//! Schedule state paths and the versioned target manifest.
//!
//! `manifest.json` is the private, owned record of exactly what a scheduled
//! `xv rotate --due --force` run is allowed to touch: which config, which
//! project/environment, which backend registry entry, and which vault. It is
//! written once at install/reinstall time and never mutated by a scheduled
//! run; the runner only ever reads it back and refuses if the pinned target
//! has drifted (that comparison is implemented by a later task).
//!
//! This module owns three things: locating the per-platform state directory
//! ([`ScheduleStatePaths`]), the versioned on-disk schema
//! ([`ScheduleManifestV1`] / [`ScheduleManifest`]), and the bounded,
//! symlink-safe storage primitives ([`load_manifest`], [`write_manifest_atomic`],
//! [`remove_owned_manifest`]) built on top of the shared helpers in
//! `crate::utils::helpers`.
//!
//! The manifest stores only names and routing metadata that already appear in
//! config or CLI output. It must never contain access keys, session tokens,
//! client secrets, credential file contents, environment values, local age
//! identities, secret names or secret values.
//!
//! Install writes and validates the manifest and the runner loads it back;
//! the last-run, lock and recovery paths belong to the runner and install
//! transaction still to come. Those still-unconsumed items carry a per-item
//! `allow(dead_code)` rather than the module carrying a blanket one, so a
//! genuinely unused item added later still shows up as a warning.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CrosstacheError, Result};
use crate::utils::helpers::{atomic_write_file_no_follow, create_private_dir, read_file_no_follow};

/// Fixed identifier for the single schedule xv currently manages.
pub const SCHEDULE_ID: &str = "rotation-default";

/// Manifest files are capped at this size before they are ever parsed.
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// State directory resolution
// ---------------------------------------------------------------------------

/// Explicit, testable inputs to [`resolve`]. Empty strings are treated the
/// same as `None`. `XV_STATE_HOME` is an internal test/embedding override
/// honored on every platform; the remaining fields feed the per-platform
/// native fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleEnv {
    /// `XV_STATE_HOME` — overrides the state root on every platform.
    pub xv_state_home: Option<String>,
    /// `XDG_STATE_HOME` — Unix native state root.
    pub xdg_state_home: Option<String>,
    /// `HOME` — used for the Unix `~/.local/state` fallback.
    pub home: Option<String>,
    /// The platform "local app data" directory (Windows `%LOCALAPPDATA%`,
    /// resolved elsewhere via `dirs::data_local_dir()`).
    pub windows_local_data_dir: Option<PathBuf>,
}

/// Which native fallback rule to apply when no override is present.
///
/// [`resolve`] picks this from the compile-time target so production builds
/// always use their own platform's rule; tests exercise both branches
/// directly regardless of the host they run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlatform {
    Unix,
    Windows,
}

/// Which input selected the state root.
///
/// Install has to know this, not just the resulting path. A scheduled process
/// inherits none of the installing shell's environment, so if an environment
/// variable chose the root, the installed unit must carry that variable or the
/// run will recompute a *different* root and refuse its own manifest. The
/// native fallbacks need no pinning: they derive from `HOME`, which the unit
/// already sets, or from the Windows local-data directory, which is a property
/// of the account rather than the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateRootSource {
    /// `XV_STATE_HOME`, with the value that selected the root.
    XvStateHome(String),
    /// `XDG_STATE_HOME`, with the value that selected the root.
    XdgStateHome(String),
    /// The Unix `$HOME/.local/state` fallback.
    Home,
    /// The Windows local-data directory.
    WindowsLocalData,
}

/// Resolved, owned paths for the `rotation-default` schedule's state
/// directory and the fixed files inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleStatePaths {
    root: PathBuf,
    source: StateRootSource,
}

impl ScheduleStatePaths {
    /// `<state root>/xv/schedules/rotation-default/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Which input selected [`Self::root`].
    // Production reads the derived `pinned_state_home()`; the raw source is
    // what the resolution tests assert on, and what a later status/diagnostic
    // task needs to explain where the state directory came from.
    #[allow(dead_code)]
    pub fn source(&self) -> &StateRootSource {
        &self.source
    }

    /// The `(variable, value)` pair an installed unit must set so the
    /// scheduled process resolves this same state root, or `None` when the
    /// root came from a native fallback the unit already reproduces.
    ///
    /// This is not target selection: the manifest path is written into the
    /// unit as an absolute string either way, and `xv schedule run` compares
    /// what it was handed against what it recomputes. Pinning the variable is
    /// what makes those two agree for a user whose shell profile sets one.
    pub fn pinned_state_home(&self) -> Option<(&'static str, PathBuf)> {
        match &self.source {
            StateRootSource::XvStateHome(value) => Some(("XV_STATE_HOME", PathBuf::from(value))),
            StateRootSource::XdgStateHome(value) => Some(("XDG_STATE_HOME", PathBuf::from(value))),
            StateRootSource::Home | StateRootSource::WindowsLocalData => None,
        }
    }

    /// `manifest.json` — the pinned target, owned by install/reinstall.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    /// `last-run.json` — owned by the scheduled runner.
    // The scheduled runner and the install transaction consume these; the
    // renderer/runner scaffolding does not.
    #[allow(dead_code)]
    pub fn last_run_path(&self) -> PathBuf {
        self.root.join("last-run.json")
    }

    /// `run.lock` — persistent lock inode owned by the scheduled runner.
    // The scheduled runner and the install transaction consume these; the
    // renderer/runner scaffolding does not.
    #[allow(dead_code)]
    pub fn run_lock_path(&self) -> PathBuf {
        self.root.join("run.lock")
    }

    /// `install.lock` — persistent lock inode owned by install/reinstall/uninstall.
    // The scheduled runner and the install transaction consume these; the
    // renderer/runner scaffolding does not.
    #[allow(dead_code)]
    pub fn install_lock_path(&self) -> PathBuf {
        self.root.join("install.lock")
    }

    /// `recovery/` — created only when an install rollback is incomplete.
    // The scheduled runner and the install transaction consume these; the
    // renderer/runner scaffolding does not.
    #[allow(dead_code)]
    pub fn recovery_dir(&self) -> PathBuf {
        self.root.join("recovery")
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

/// Resolve state paths from real process environment. Not pure — this is the
/// thin wrapper production callers use; tests should call [`resolve`] with an
/// explicit [`ScheduleEnv`] instead.
pub fn resolve_from_process_env() -> Result<ScheduleStatePaths> {
    let env = ScheduleEnv {
        xv_state_home: non_empty(std::env::var("XV_STATE_HOME").ok()),
        xdg_state_home: non_empty(std::env::var("XDG_STATE_HOME").ok()),
        home: non_empty(std::env::var("HOME").ok()),
        windows_local_data_dir: dirs::data_local_dir(),
    };
    resolve(&env)
}

/// Resolve state paths for the current build's platform. Pure function of
/// `env`; performs no environment or filesystem access itself.
pub fn resolve(env: &ScheduleEnv) -> Result<ScheduleStatePaths> {
    let platform = if cfg!(windows) {
        HostPlatform::Windows
    } else {
        HostPlatform::Unix
    };
    resolve_for(env, platform)
}

fn resolve_for(env: &ScheduleEnv, platform: HostPlatform) -> Result<ScheduleStatePaths> {
    let (state_root, source) = if let Some(overridden) = non_empty(env.xv_state_home.clone()) {
        (
            PathBuf::from(&overridden),
            StateRootSource::XvStateHome(overridden),
        )
    } else {
        match platform {
            HostPlatform::Unix => {
                if let Some(xdg) = non_empty(env.xdg_state_home.clone()) {
                    (PathBuf::from(&xdg), StateRootSource::XdgStateHome(xdg))
                } else if let Some(home) = non_empty(env.home.clone()) {
                    (
                        PathBuf::from(home).join(".local").join("state"),
                        StateRootSource::Home,
                    )
                } else {
                    return Err(CrosstacheError::config(
                        "Cannot determine the schedule state directory: HOME is not set and no XDG_STATE_HOME or XV_STATE_HOME override was provided",
                    ));
                }
            }
            HostPlatform::Windows => match env.windows_local_data_dir.clone() {
                Some(local_data) => (local_data, StateRootSource::WindowsLocalData),
                None => {
                    return Err(CrosstacheError::config(
                        "Cannot determine the schedule state directory: the Windows local-data directory could not be resolved and no XV_STATE_HOME override was provided",
                    ));
                }
            },
        }
    };

    Ok(ScheduleStatePaths {
        root: state_root.join("xv").join("schedules").join(SCHEDULE_ID),
        source,
    })
}

// ---------------------------------------------------------------------------
// Manifest schema
// ---------------------------------------------------------------------------

/// Rotation cadence as recorded at install time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestCadence {
    pub kind: String,
    pub hour: u8,
    pub minute: u8,
}

/// The pinned executable and run-time environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestExecution {
    pub binary_path: String,
    pub installed_version: String,
    pub working_directory: String,
    pub log_path: String,
}

/// The pinned resolution target: which config/project/context participated,
/// which backend registry entry, and which vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTarget {
    pub config_path: String,
    pub config_digest: String,
    pub project_path: Option<String>,
    pub project_digest: Option<String>,
    pub environment: Option<String>,
    pub context_path: Option<String>,
    pub context_digest: Option<String>,
    pub workspace_source: String,
    pub workspace_alias: Option<String>,
    pub backend_name: String,
    pub backend_kind: String,
    pub backend_identity: String,
    pub vault: String,
}

/// Version 1 of `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleManifestV1 {
    pub schema_version: u32,
    pub schedule_id: String,
    pub installed_at: String,
    pub cadence: ManifestCadence,
    pub execution: ManifestExecution,
    pub target: ManifestTarget,
}

/// A loaded, version-dispatched manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleManifest {
    V1(ScheduleManifestV1),
}

/// Minimal shape used only to peek `schema_version` before committing to a
/// concrete version's strict (`deny_unknown_fields`) deserialization.
#[derive(Debug, Deserialize)]
struct SchemaVersionPeek {
    schema_version: u32,
}

// ---------------------------------------------------------------------------
// Path/digest validation
// ---------------------------------------------------------------------------

/// `installed_at` must be a non-empty RFC 3339 timestamp in UTC.
///
/// The stamp is what tells a person (and a later drift check) *when* this
/// target was pinned, so an empty or unparseable value is a corrupt manifest,
/// not a cosmetic defect. Only UTC is accepted: two manifests written in
/// different local offsets must still compare and sort as written.
///
/// [`validate_v1`] runs on load and — since install validates before
/// serializing — on write too, but never on the preview: the preview
/// manifest's `installed_at` is deliberately empty and is rendered as
/// `<set-at-install>` by [`serialize_manifest_preview`], which does not
/// validate.
fn validate_installed_at(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(CrosstacheError::config(
            "schedule manifest field 'installed_at' must not be empty".to_string(),
        ));
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value).map_err(|e| {
        CrosstacheError::config(format!(
            "schedule manifest field 'installed_at' must be an RFC 3339 timestamp: {value} ({e})"
        ))
    })?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(CrosstacheError::config(format!(
            "schedule manifest field 'installed_at' must be in UTC: {value}"
        )));
    }
    Ok(())
}

pub(crate) fn validate_absolute_normalized_path(field: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(CrosstacheError::config(format!(
            "schedule manifest field '{field}' must be an absolute path: {value}"
        )));
    }
    for component in path.components() {
        if matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        ) {
            return Err(CrosstacheError::config(format!(
                "schedule manifest field '{field}' must be lexically normalized (no '.' or '..' components): {value}"
            )));
        }
    }
    Ok(())
}

fn validate_digest(field: &str, value: &str) -> Result<()> {
    let invalid = || {
        CrosstacheError::config(format!(
            "schedule manifest field '{field}' must be a sha256 digest of the form 'sha256:<64 lowercase hex chars>': {value}"
        ))
    };
    let hex = value.strip_prefix("sha256:").ok_or_else(invalid)?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    Ok(())
}

/// Validate a v1 manifest.
///
/// Called on every load *and* by install before serializing, so a manifest
/// this build would refuse to read is never written in the first place — the
/// alternative is discovering the defect at 3am, from a job that cannot run.
pub(crate) fn validate_v1(manifest: &ScheduleManifestV1) -> Result<()> {
    if manifest.schedule_id != SCHEDULE_ID {
        return Err(CrosstacheError::config(format!(
            "schedule manifest field 'schedule_id' must be '{SCHEDULE_ID}': {}",
            manifest.schedule_id
        )));
    }

    validate_absolute_normalized_path("execution.binary_path", &manifest.execution.binary_path)?;
    validate_absolute_normalized_path(
        "execution.working_directory",
        &manifest.execution.working_directory,
    )?;
    validate_absolute_normalized_path("execution.log_path", &manifest.execution.log_path)?;

    validate_absolute_normalized_path("target.config_path", &manifest.target.config_path)?;
    validate_digest("target.config_digest", &manifest.target.config_digest)?;

    if let Some(project_path) = &manifest.target.project_path {
        validate_absolute_normalized_path("target.project_path", project_path)?;
    }
    if let Some(project_digest) = &manifest.target.project_digest {
        validate_digest("target.project_digest", project_digest)?;
    }
    if let Some(context_path) = &manifest.target.context_path {
        validate_absolute_normalized_path("target.context_path", context_path)?;
    }
    if let Some(context_digest) = &manifest.target.context_digest {
        validate_digest("target.context_digest", context_digest)?;
    }
    validate_digest("target.backend_identity", &manifest.target.backend_identity)?;

    validate_installed_at(&manifest.installed_at)?;

    if manifest.cadence.hour > 23 {
        return Err(CrosstacheError::config(format!(
            "schedule manifest field 'cadence.hour' must be 0-23: {}",
            manifest.cadence.hour
        )));
    }
    if manifest.cadence.minute > 59 {
        return Err(CrosstacheError::config(format!(
            "schedule manifest field 'cadence.minute' must be 0-59: {}",
            manifest.cadence.minute
        )));
    }

    match manifest.target.workspace_source.as_str() {
        "project" | "context" | "degenerate" => {}
        other => {
            return Err(CrosstacheError::config(format!(
                "schedule manifest field 'target.workspace_source' must be one of 'project', 'context', 'degenerate': {other}"
            )));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Deterministic pretty JSON preview of a manifest for confirmation prompts,
/// with `installed_at` replaced by the literal `<set-at-install>` since the
/// real value is not known until the write actually happens.
pub fn serialize_manifest_preview(manifest: &ScheduleManifestV1) -> String {
    let mut preview = manifest.clone();
    preview.installed_at = "<set-at-install>".to_string();
    // `ScheduleManifestV1` contains only plain strings/numbers/options, so
    // this cannot fail.
    serde_json::to_string_pretty(&preview).expect("schedule manifest preview is always valid JSON")
}

/// Deterministic pretty JSON bytes for the real on-disk manifest, terminated
/// with a trailing newline.
pub fn serialize_manifest(manifest: &ScheduleManifestV1) -> Vec<u8> {
    let mut bytes =
        serde_json::to_vec_pretty(manifest).expect("schedule manifest is always valid JSON");
    bytes.push(b'\n');
    bytes
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Reject a path if it exists and is itself a symlink, without following it.
fn reject_if_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(CrosstacheError::config(format!(
            "Refusing symlinked schedule state path '{}'",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CrosstacheError::config(format!(
            "Failed to inspect schedule state path '{}': {error}",
            path.display()
        ))),
    }
}

/// Reject any existing path component *inside the owned schedule directory*
/// that is a symlink, without following it. Complements
/// `read_file_no_follow`'s final-component `O_NOFOLLOW` check by also
/// covering the owning directory itself. Deliberately does not walk
/// ancestors above `paths.root()`: those are outside xv's ownership and may
/// legitimately be symlinks (e.g. macOS's `/var` -> `/private/var`, or a
/// symlinked home directory).
fn reject_symlink_components(paths: &ScheduleStatePaths) -> Result<()> {
    reject_if_symlink(paths.root())?;
    reject_if_symlink(&paths.manifest_path())?;
    Ok(())
}

fn read_manifest_bytes(paths: &ScheduleStatePaths) -> Result<Vec<u8>> {
    let path = paths.manifest_path();
    reject_symlink_components(paths)?;

    // Check the size via metadata before reading so a bloated file is
    // refused without pulling arbitrary bytes into memory first.
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to inspect schedule manifest '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(CrosstacheError::config(format!(
            "schedule manifest '{}' exceeds the {MAX_MANIFEST_BYTES} byte limit",
            path.display()
        )));
    }

    let bytes = read_file_no_follow(&path)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(CrosstacheError::config(format!(
            "schedule manifest '{}' exceeds the {MAX_MANIFEST_BYTES} byte limit",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Load and validate `manifest.json`, dispatching on its `schema_version`.
///
/// An unknown `schema_version` produces a targeted error naming reinstall,
/// rather than a generic deserialization failure.
pub fn load_manifest(paths: &ScheduleStatePaths) -> Result<ScheduleManifest> {
    let bytes = read_manifest_bytes(paths)?;

    let peek: SchemaVersionPeek = serde_json::from_slice(&bytes).map_err(|error| {
        CrosstacheError::config(format!(
            "schedule manifest '{}' is not valid JSON: {error}",
            paths.manifest_path().display()
        ))
    })?;

    match peek.schema_version {
        1 => {
            let manifest: ScheduleManifestV1 = serde_json::from_slice(&bytes).map_err(|error| {
                CrosstacheError::config(format!(
                    "schedule manifest '{}' does not match schema version 1: {error}",
                    paths.manifest_path().display()
                ))
            })?;
            validate_v1(&manifest)?;
            Ok(ScheduleManifest::V1(manifest))
        }
        other => Err(CrosstacheError::config(format!(
            "schedule manifest '{}' has schema_version {other}, which this version of xv does not support; reinstall the schedule (xv schedule install) to regenerate it",
            paths.manifest_path().display()
        ))),
    }
}

/// Atomically write `manifest.json`, creating the owning private directory
/// (`0700` on Unix) if needed. The file is written private (`0600` on Unix).
pub fn write_manifest_atomic(paths: &ScheduleStatePaths, bytes: &[u8]) -> Result<()> {
    create_private_dir(paths.root()).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to create schedule state directory '{}': {error}",
            paths.root().display()
        ))
    })?;
    atomic_write_file_no_follow(&paths.manifest_path(), bytes, true)
}

/// Remove only `manifest.json`. Refuses if it is a symlink; a missing file
/// is not an error.
// Consumed by uninstall and the install transaction, still to come.
#[allow(dead_code)]
pub fn remove_owned_manifest(paths: &ScheduleStatePaths) -> Result<()> {
    let path = paths.manifest_path();
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(CrosstacheError::config(format!(
                    "Refusing to remove symlinked schedule manifest '{}'",
                    path.display()
                )));
            }
            std::fs::remove_file(&path).map_err(|error| {
                CrosstacheError::config(format!(
                    "Failed to remove schedule manifest '{}': {error}",
                    path.display()
                ))
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CrosstacheError::config(format!(
            "Failed to inspect schedule manifest '{}': {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::fixture_abs;

    fn fixture_manifest() -> ScheduleManifestV1 {
        ScheduleManifestV1 {
            schema_version: 1,
            schedule_id: SCHEDULE_ID.to_string(),
            installed_at: "2026-09-09T15:04:05Z".to_string(),
            cadence: ManifestCadence {
                kind: "daily".to_string(),
                hour: 3,
                minute: 0,
            },
            execution: ManifestExecution {
                binary_path: fixture_abs("/opt/homebrew/bin/xv"),
                installed_version: "0.39.0".to_string(),
                working_directory: fixture_abs("/Users/alice/work/service"),
                log_path: fixture_abs("/Users/alice/.local/state/xv/rotate.log"),
            },
            target: ManifestTarget {
                config_path: fixture_abs("/Users/alice/.config/xv/xv.conf"),
                config_digest: format!("sha256:{}", "9c".repeat(32)),
                project_path: Some(fixture_abs("/Users/alice/work/service/.xv.toml")),
                project_digest: Some(format!("sha256:{}", "83".repeat(32))),
                environment: Some("production".to_string()),
                context_path: None,
                context_digest: None,
                workspace_source: "project".to_string(),
                workspace_alias: Some("payments".to_string()),
                backend_name: "aws-prod".to_string(),
                backend_kind: "aws".to_string(),
                backend_identity: format!("sha256:{}", "55".repeat(32)),
                vault: "payments-production".to_string(),
            },
        }
    }

    fn degenerate_manifest() -> ScheduleManifestV1 {
        let mut manifest = fixture_manifest();
        manifest.target.project_path = None;
        manifest.target.project_digest = None;
        manifest.target.environment = None;
        manifest.target.context_path = None;
        manifest.target.context_digest = None;
        manifest.target.workspace_alias = None;
        manifest.target.workspace_source = "degenerate".to_string();
        manifest
    }

    // -- ScheduleStatePaths::resolve --------------------------------------

    #[test]
    fn resolve_unix_prefers_xv_state_home_override() {
        let env = ScheduleEnv {
            xv_state_home: Some("/override/state".to_string()),
            xdg_state_home: Some("/xdg/state".to_string()),
            home: Some("/home/alice".to_string()),
            windows_local_data_dir: None,
        };
        let paths = resolve_for(&env, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("/override/state/xv/schedules/rotation-default")
        );
    }

    #[test]
    fn resolve_unix_uses_xdg_state_home_when_no_override() {
        let env = ScheduleEnv {
            xv_state_home: None,
            xdg_state_home: Some("/xdg/state".to_string()),
            home: Some("/home/alice".to_string()),
            windows_local_data_dir: None,
        };
        let paths = resolve_for(&env, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("/xdg/state/xv/schedules/rotation-default")
        );
    }

    #[test]
    fn resolve_unix_falls_back_to_home_local_state() {
        let env = ScheduleEnv {
            xv_state_home: None,
            xdg_state_home: None,
            home: Some("/home/alice".to_string()),
            windows_local_data_dir: None,
        };
        let paths = resolve_for(&env, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("/home/alice/.local/state/xv/schedules/rotation-default")
        );
    }

    /// The root alone is not enough: install has to tell the unit which
    /// variable produced it, or a scheduled run that inherits none of them
    /// recomputes a different root and refuses its own manifest.
    #[test]
    fn resolve_reports_which_input_selected_the_root() {
        let overridden = ScheduleEnv {
            xv_state_home: Some("/override/state".to_string()),
            xdg_state_home: Some("/xdg/state".to_string()),
            home: Some("/home/alice".to_string()),
            windows_local_data_dir: None,
        };
        let paths = resolve_for(&overridden, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.source(),
            &StateRootSource::XvStateHome("/override/state".to_string())
        );
        assert_eq!(
            paths.pinned_state_home(),
            Some(("XV_STATE_HOME", PathBuf::from("/override/state")))
        );

        let xdg = ScheduleEnv {
            xv_state_home: None,
            ..overridden.clone()
        };
        let paths = resolve_for(&xdg, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.source(),
            &StateRootSource::XdgStateHome("/xdg/state".to_string())
        );
        assert_eq!(
            paths.pinned_state_home(),
            Some(("XDG_STATE_HOME", PathBuf::from("/xdg/state")))
        );

        // The native fallbacks need no pin: the unit already sets HOME, and
        // the Windows local-data directory is a property of the account.
        let home_only = ScheduleEnv {
            xv_state_home: None,
            xdg_state_home: None,
            ..overridden.clone()
        };
        let paths = resolve_for(&home_only, HostPlatform::Unix).unwrap();
        assert_eq!(paths.source(), &StateRootSource::Home);
        assert_eq!(paths.pinned_state_home(), None);

        let windows = ScheduleEnv {
            xv_state_home: None,
            xdg_state_home: None,
            home: None,
            windows_local_data_dir: Some(PathBuf::from(r"C:\Users\alice\AppData\Local")),
        };
        let paths = resolve_for(&windows, HostPlatform::Windows).unwrap();
        assert_eq!(paths.source(), &StateRootSource::WindowsLocalData);
        assert_eq!(paths.pinned_state_home(), None);
    }

    /// An empty override is ignored for the *source* too, not just the path —
    /// otherwise the unit would pin an empty variable that resolves nowhere.
    #[test]
    fn an_empty_override_pins_nothing() {
        let env = ScheduleEnv {
            xv_state_home: Some(String::new()),
            xdg_state_home: Some(String::new()),
            home: Some("/home/alice".to_string()),
            windows_local_data_dir: None,
        };
        let paths = resolve_for(&env, HostPlatform::Unix).unwrap();
        assert_eq!(paths.source(), &StateRootSource::Home);
        assert_eq!(paths.pinned_state_home(), None);
    }

    #[test]
    fn resolve_unix_errors_without_any_input() {
        let env = ScheduleEnv::default();
        let error = resolve_for(&env, HostPlatform::Unix).unwrap_err();
        assert!(error.to_string().contains("HOME"));
    }

    #[test]
    fn resolve_windows_uses_local_data_dir() {
        let env = ScheduleEnv {
            xv_state_home: None,
            xdg_state_home: None,
            home: None,
            windows_local_data_dir: Some(PathBuf::from("C:\\Users\\alice\\AppData\\Local")),
        };
        let paths = resolve_for(&env, HostPlatform::Windows).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("C:\\Users\\alice\\AppData\\Local")
                .join("xv")
                .join("schedules")
                .join("rotation-default")
        );
    }

    #[test]
    fn resolve_windows_prefers_xv_state_home_override() {
        let env = ScheduleEnv {
            xv_state_home: Some("C:\\override".to_string()),
            xdg_state_home: None,
            home: None,
            windows_local_data_dir: Some(PathBuf::from("C:\\Users\\alice\\AppData\\Local")),
        };
        let paths = resolve_for(&env, HostPlatform::Windows).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("C:\\override")
                .join("xv")
                .join("schedules")
                .join("rotation-default")
        );
    }

    #[test]
    fn resolve_windows_errors_when_local_data_dir_unavailable() {
        let env = ScheduleEnv {
            xv_state_home: None,
            xdg_state_home: None,
            home: None,
            windows_local_data_dir: None,
        };
        let error = resolve_for(&env, HostPlatform::Windows).unwrap_err();
        assert!(error.to_string().contains("local-data"));
    }

    #[test]
    fn resolve_ignores_empty_string_overrides() {
        let env = ScheduleEnv {
            xv_state_home: Some(String::new()),
            xdg_state_home: Some(String::new()),
            home: Some("/home/alice".to_string()),
            windows_local_data_dir: None,
        };
        let paths = resolve_for(&env, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.root(),
            Path::new("/home/alice/.local/state/xv/schedules/rotation-default")
        );
    }

    #[test]
    fn state_paths_expose_fixed_filenames() {
        let env = ScheduleEnv {
            xv_state_home: Some("/state".to_string()),
            ..ScheduleEnv::default()
        };
        let paths = resolve_for(&env, HostPlatform::Unix).unwrap();
        assert_eq!(
            paths.manifest_path(),
            Path::new("/state/xv/schedules/rotation-default/manifest.json")
        );
        assert_eq!(
            paths.last_run_path(),
            Path::new("/state/xv/schedules/rotation-default/last-run.json")
        );
        assert_eq!(
            paths.run_lock_path(),
            Path::new("/state/xv/schedules/rotation-default/run.lock")
        );
        assert_eq!(
            paths.install_lock_path(),
            Path::new("/state/xv/schedules/rotation-default/install.lock")
        );
        assert_eq!(
            paths.recovery_dir(),
            Path::new("/state/xv/schedules/rotation-default/recovery")
        );
    }

    // -- schema round trip / validation -----------------------------------

    #[test]
    fn json_round_trip_preserves_the_manifest() {
        let manifest = fixture_manifest();
        let bytes = serialize_manifest(&manifest);
        assert!(bytes.ends_with(b"\n"));
        let parsed: ScheduleManifestV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, manifest);
    }

    #[test]
    fn round_trip_preserves_null_project_and_context_fields() {
        let manifest = degenerate_manifest();
        let bytes = serialize_manifest(&manifest);
        let parsed: ScheduleManifestV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(parsed.target.workspace_source, "degenerate");
    }

    #[test]
    fn deserialize_rejects_unknown_fields() {
        let mut value = serde_json::to_value(fixture_manifest()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected_field".to_string(), serde_json::json!(true));
        let result: std::result::Result<ScheduleManifestV1, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_rejects_missing_fields() {
        let mut value = serde_json::to_value(fixture_manifest()).unwrap();
        value.as_object_mut().unwrap().remove("schedule_id");
        let result: std::result::Result<ScheduleManifestV1, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn serialize_manifest_preview_masks_installed_at() {
        let manifest = fixture_manifest();
        let preview = serialize_manifest_preview(&manifest);
        assert!(preview.contains("\"<set-at-install>\""));
        assert!(!preview.contains(&manifest.installed_at));
        // Preview is stable pretty JSON, not tied to the real bytes.
        let again = serialize_manifest_preview(&manifest);
        assert_eq!(preview, again);
    }

    #[test]
    fn load_manifest_reports_unknown_schema_version_for_reinstall() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ScheduleStatePaths {
            root: dir.path().to_path_buf(),
            source: StateRootSource::Home,
        };
        std::fs::create_dir_all(paths.root()).unwrap();
        std::fs::write(
            paths.manifest_path(),
            serde_json::json!({ "schema_version": 99 }).to_string(),
        )
        .unwrap();

        let error = load_manifest(&paths).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("99"));
        assert!(message.contains("reinstall"));
    }

    #[test]
    fn validate_v1_rejects_relative_paths() {
        let mut manifest = fixture_manifest();
        manifest.target.config_path = "relative/xv.conf".to_string();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("absolute"));
    }

    #[test]
    fn validate_v1_rejects_dot_dot_components() {
        let mut manifest = fixture_manifest();
        manifest.target.config_path = fixture_abs("/Users/alice/../alice/xv.conf");
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("normalized"));
    }

    #[test]
    fn validate_v1_rejects_wrong_schedule_id() {
        let mut manifest = fixture_manifest();
        manifest.schedule_id = "rotation-other".to_string();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("schedule_id"));
    }

    #[test]
    fn validate_v1_rejects_malformed_digest() {
        let mut manifest = fixture_manifest();
        manifest.target.config_digest = "sha256:not-hex".to_string();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("digest"));
    }

    #[test]
    fn validate_v1_rejects_unknown_workspace_source() {
        let mut manifest = fixture_manifest();
        manifest.target.workspace_source = "bogus".to_string();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("workspace_source"));
    }

    #[test]
    fn validate_v1_rejects_an_empty_installed_at() {
        let mut manifest = fixture_manifest();
        manifest.installed_at = String::new();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("installed_at"), "{error}");
    }

    #[test]
    fn validate_v1_rejects_a_non_rfc3339_installed_at() {
        let mut manifest = fixture_manifest();
        manifest.installed_at = "2026-09-09 15:04:05".to_string();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("RFC 3339"), "{error}");
    }

    #[test]
    fn validate_v1_rejects_a_non_utc_installed_at() {
        let mut manifest = fixture_manifest();
        manifest.installed_at = "2026-09-09T15:04:05+02:00".to_string();
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("UTC"), "{error}");
    }

    #[test]
    fn validate_v1_accepts_an_offset_zero_installed_at() {
        let mut manifest = fixture_manifest();
        manifest.installed_at = "2026-09-09T15:04:05.123+00:00".to_string();
        validate_v1(&manifest).unwrap();
    }

    #[test]
    fn validate_v1_rejects_an_out_of_range_cadence() {
        let mut manifest = fixture_manifest();
        manifest.cadence.hour = 24;
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("cadence.hour"), "{error}");

        let mut manifest = fixture_manifest();
        manifest.cadence.minute = 60;
        let error = validate_v1(&manifest).unwrap_err();
        assert!(error.to_string().contains("cadence.minute"), "{error}");
    }

    /// The preview manifest is built before the write stamps `installed_at`,
    /// so validation must stay on the load/write path only — rendering a
    /// preview of a manifest with an empty stamp has to keep working.
    #[test]
    fn serialize_manifest_preview_works_on_an_unstamped_manifest() {
        let mut manifest = fixture_manifest();
        manifest.installed_at = String::new();
        let preview = serialize_manifest_preview(&manifest);
        assert!(
            preview.contains("\"installed_at\": \"<set-at-install>\""),
            "{preview}"
        );
    }

    #[test]
    fn validate_v1_accepts_the_fixture_and_degenerate_manifests() {
        validate_v1(&fixture_manifest()).unwrap();
        validate_v1(&degenerate_manifest()).unwrap();
    }

    // -- storage ------------------------------------------------------------

    fn temp_paths(dir: &tempfile::TempDir) -> ScheduleStatePaths {
        ScheduleStatePaths {
            root: dir.path().join("xv").join("schedules").join(SCHEDULE_ID),
            source: StateRootSource::Home,
        }
    }

    #[test]
    fn write_then_load_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        let manifest = fixture_manifest();

        write_manifest_atomic(&paths, &serialize_manifest(&manifest)).unwrap();
        let loaded = load_manifest(&paths).unwrap();
        assert_eq!(loaded, ScheduleManifest::V1(manifest));
    }

    #[cfg(unix)]
    #[test]
    fn write_manifest_atomic_sets_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        write_manifest_atomic(&paths, &serialize_manifest(&fixture_manifest())).unwrap();

        let dir_mode = std::fs::metadata(paths.root())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);

        let file_mode = std::fs::metadata(paths.manifest_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
    }

    #[test]
    fn load_manifest_rejects_files_over_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        std::fs::create_dir_all(paths.root()).unwrap();

        let oversized = vec![b' '; MAX_MANIFEST_BYTES + 1];
        std::fs::write(paths.manifest_path(), oversized).unwrap();

        let error = load_manifest(&paths).unwrap_err();
        assert!(error.to_string().contains("byte limit"));
    }

    #[cfg(unix)]
    #[test]
    fn load_manifest_rejects_a_symlinked_manifest_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        std::fs::create_dir_all(paths.root()).unwrap();

        let real_target = dir.path().join("elsewhere.json");
        std::fs::write(&real_target, serialize_manifest(&fixture_manifest())).unwrap();
        std::os::unix::fs::symlink(&real_target, paths.manifest_path()).unwrap();

        let error = load_manifest(&paths).unwrap_err();
        assert!(error.to_string().contains("symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn load_manifest_rejects_a_symlinked_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real-root");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(
            real_dir.join("manifest.json"),
            serialize_manifest(&fixture_manifest()),
        )
        .unwrap();

        let linked_root = dir.path().join("linked-root");
        std::os::unix::fs::symlink(&real_dir, &linked_root).unwrap();
        let paths = ScheduleStatePaths {
            root: linked_root,
            source: StateRootSource::Home,
        };

        let error = load_manifest(&paths).unwrap_err();
        assert!(error.to_string().contains("symlink"));
    }

    #[test]
    fn load_manifest_reports_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        std::fs::create_dir_all(paths.root()).unwrap();

        assert!(load_manifest(&paths).is_err());
    }

    #[test]
    fn remove_owned_manifest_deletes_only_the_manifest_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        write_manifest_atomic(&paths, &serialize_manifest(&fixture_manifest())).unwrap();
        std::fs::write(paths.last_run_path(), b"{}").unwrap();

        remove_owned_manifest(&paths).unwrap();

        assert!(!paths.manifest_path().exists());
        assert!(paths.last_run_path().exists());
    }

    #[test]
    fn remove_owned_manifest_is_ok_when_already_missing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        std::fs::create_dir_all(paths.root()).unwrap();

        remove_owned_manifest(&paths).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn remove_owned_manifest_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        std::fs::create_dir_all(paths.root()).unwrap();

        let real_target = dir.path().join("elsewhere.json");
        std::fs::write(&real_target, b"{}").unwrap();
        std::os::unix::fs::symlink(&real_target, paths.manifest_path()).unwrap();

        let error = remove_owned_manifest(&paths).unwrap_err();
        assert!(error.to_string().contains("symlink"));
        assert!(real_target.exists());
    }
}
