//! Deterministic drift validation for the scheduled-rotation manifest.
//!
//! A scheduled run is unattended: nobody sees the target it picked before it
//! mutates secrets. So before any backend is constructed, the runner recomputes
//! the target *from the inputs the manifest recorded* and compares the result
//! with the manifest. A difference refuses the sweep — invariant 4 of
//! `docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`.
//!
//! Two properties matter more than the comparisons themselves:
//!
//! 1. **Only recorded inputs participate.** The recorded config path, the
//!    recorded `.xv.toml` path and environment name, the recorded context file,
//!    and the recorded working directory. Nothing here reads `XV_BACKEND`,
//!    `XV_ENV`, `XV_CONTEXT_DIR`, `XV_NO_PARENT_CONFIG`, the ambient context
//!    file, or the process's current directory — the "Ignore" row of the
//!    design's drift table. Project discovery states its traversal explicitly
//!    (always walking up); installation refuses to pin a target
//!    `XV_NO_PARENT_CONFIG` is hiding, so the two always agree. The
//!    workspace layer is reached through
//!    [`crate::workspace::resolve_workspace_snapshot_replay`] rather than the
//!    interactive resolver for exactly this reason: the interactive one
//!    re-runs `project::resolve_env`, which consults `XV_ENV` first.
//! 2. **Reasons are sanitized.** A reason carries a manifest field name, the
//!    paths and routing names the manifest itself already records (and which
//!    already appear in ordinary CLI output), and nothing else. No file
//!    contents, no digests of anything but what the manifest stores, no
//!    backend error bodies, no secret names or values. The refusal is written
//!    to a log a person reads later.
//!
//! Reasons are emitted in manifest-field order regardless of the order the
//! recomputation discovers them, so a refusal reads the same way every time
//! and `status` can print it verbatim.

use std::path::{Path, PathBuf};

use crate::config::project::{EnvProfile, ResolvedProject};
use crate::config::ContextManager;
use crate::schedule::manifest::VAULT_SELECTION_IMPLICIT;
use crate::schedule::manifest::{ManifestTarget, ScheduleManifestV1};
use crate::schedule::target::{selected_backend_identity, workspace_source_label};
use crate::workspace::{Workspace, WorkspaceEntry};

/// The outcome of validating a recorded target against the world as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriftVerdict {
    /// Everything the manifest pinned still resolves to the same target.
    Valid,
    /// The target is unchanged, but something worth telling a person about
    /// differs — today only an in-place upgrade of the same binary path. The
    /// run proceeds.
    Warning,
    /// The target changed, is unresolvable, or the recorded executable is not
    /// usable. The sweep is refused before any backend is constructed.
    Refuse,
}

/// One difference between the manifest and the recomputed target.
///
/// `field` is the manifest field name, used for ordering and for tests;
/// `detail` is the whole human-readable line, which *starts* with that field
/// name so callers can print it as-is:
///
/// ```text
/// - config_digest changed; review /home/alice/.config/xv/xv.conf and reinstall
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriftReason {
    pub(crate) field: &'static str,
    pub(crate) detail: String,
}

impl DriftReason {
    pub(crate) fn new(field: &'static str, detail: impl Into<String>) -> Self {
        Self {
            field,
            detail: detail.into(),
        }
    }

    /// `<field> changed; review <path> and reinstall` — the golden wording.
    fn changed_at(field: &'static str, path: &str) -> Self {
        Self::new(
            field,
            format!("{field} changed; review {path} and reinstall"),
        )
    }

    pub(crate) fn missing_at(field: &'static str, path: &str) -> Self {
        Self::new(
            field,
            format!("{field} is missing or unreadable; review {path} and reinstall"),
        )
    }
}

/// Every drift reason found, ordered by manifest field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriftReport {
    pub(crate) verdict: DriftVerdict,
    /// Refusal reasons, in manifest-field order. Empty unless the verdict is
    /// [`DriftVerdict::Refuse`].
    pub(crate) reasons: Vec<DriftReason>,
    /// Non-blocking differences, in manifest-field order.
    pub(crate) warnings: Vec<DriftReason>,
}

impl DriftReport {
    pub(crate) fn is_refused(&self) -> bool {
        matches!(self.verdict, DriftVerdict::Refuse)
    }

    /// Field names of every refusal reason, in order. Convenience for callers
    /// and tests that care about *which* fields drifted, not the wording.
    #[cfg(test)]
    fn fields(&self) -> Vec<&'static str> {
        self.reasons.iter().map(|r| r.field).collect()
    }
}

/// The order reasons are reported in: every `target` field in the order the
/// manifest declares them, then the `execution` fields that can drift
/// (`working_directory`, `binary_path`, `installed_version`).
///
/// Target fields come first because they are what a person acts on, and
/// because the goldens
/// (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-goldens.md`,
/// "Drift refusal") show target-field refusals in exactly this order. It is
/// therefore *not* the manifest's own top-level order, which puts `execution`
/// before `target`; the execution fields are appended rather than interleaved.
///
/// This is also the authoritative list of fields drift validation may report
/// on: a new manifest field that belongs in a refusal must be added here, or
/// its reason would sort first by accident.
const FIELD_ORDER: [&str; 16] = [
    "config_path",
    "config_digest",
    "project_path",
    "project_digest",
    "environment",
    "context_path",
    "context_digest",
    "workspace_source",
    "workspace_alias",
    "backend_name",
    "backend_kind",
    "backend_identity",
    "vault",
    "working_directory",
    "binary_path",
    "installed_version",
];

pub(crate) fn field_rank(field: &str) -> usize {
    FIELD_ORDER
        .iter()
        .position(|candidate| *candidate == field)
        .unwrap_or(FIELD_ORDER.len())
}

/// Validate a loaded manifest against the current filesystem and configuration.
///
/// `now_binary` is the running executable's path shaped exactly the way
/// installation shaped `execution.binary_path` (absolute, lexically
/// normalized, verbatim prefix stripped, **symlinks not resolved**);
/// `now_version` is this build's `CARGO_PKG_VERSION`. Both are parameters
/// rather than reads so the whole function is testable without a second
/// process.
///
/// Never constructs a backend and never mutates anything. A recomputation
/// failure becomes a refusal reason, not an `Err`: an unattended run needs a
/// verdict, and every failure mode here is "this target no longer resolves",
/// which is drift.
pub(crate) async fn validate_recorded_target(
    manifest: &ScheduleManifestV1,
    now_binary: &Path,
    now_version: &str,
) -> DriftReport {
    let mut reasons: Vec<DriftReason> = Vec::new();
    let mut warnings: Vec<DriftReason> = Vec::new();

    validate_execution(
        manifest,
        now_binary,
        now_version,
        &mut reasons,
        &mut warnings,
    );

    // The recorded working directory anchors project discovery, so it is
    // resolved before the target layers even though it sorts after them.
    let cwd = match canonical_working_directory(&manifest.execution.working_directory) {
        Ok(cwd) => Some(cwd),
        Err(reason) => {
            reasons.push(reason);
            None
        }
    };

    if let Some(cwd) = cwd {
        recompute_target(&manifest.target, &cwd, &mut reasons).await;
    }

    reasons.sort_by_key(|reason| field_rank(reason.field));
    warnings.sort_by_key(|reason| field_rank(reason.field));

    let verdict = if !reasons.is_empty() {
        DriftVerdict::Refuse
    } else if warnings.is_empty() {
        DriftVerdict::Valid
    } else {
        DriftVerdict::Warning
    };

    DriftReport {
        verdict,
        reasons,
        warnings,
    }
}

// ---------------------------------------------------------------------------
// execution: the recorded binary
// ---------------------------------------------------------------------------

/// Compare the running executable with the recorded one.
///
/// A *different* path refuses: the unit was rendered for one binary and is
/// being served by another. The *same* path reporting a different version is
/// the ordinary in-place upgrade (`brew upgrade`, a package manager, `xv
/// upgrade`) and is only a warning — refusing there would break every
/// scheduled run on the day the user updates xv.
fn validate_execution(
    manifest: &ScheduleManifestV1,
    now_binary: &Path,
    now_version: &str,
    reasons: &mut Vec<DriftReason>,
    warnings: &mut Vec<DriftReason>,
) {
    let recorded = Path::new(&manifest.execution.binary_path);
    if now_binary != recorded {
        reasons.push(DriftReason::changed_at(
            "binary_path",
            &manifest.execution.binary_path,
        ));
        return;
    }
    if let Err(reason) = check_executable(recorded) {
        reasons.push(reason);
        return;
    }
    if now_version != manifest.execution.installed_version {
        warnings.push(DriftReason::new(
            "installed_version",
            format!(
                "installed_version changed from {} to {now_version} at the same binary path; \
                 reinstall the schedule ('xv schedule install') to refresh the rendered unit",
                manifest.execution.installed_version
            ),
        ));
    }
}

/// The recorded path must still be a regular file with an execute bit.
fn check_executable(path: &Path) -> Result<(), DriftReason> {
    let display = path.display().to_string();
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| DriftReason::missing_at("binary_path", &display))?;
    // A symlink at the recorded path is followed deliberately: the recorded
    // spelling is a shim path on purpose (see `recorded_binary_path`), so what
    // matters is that it *leads to* a regular executable file.
    let metadata = if metadata.file_type().is_symlink() {
        std::fs::metadata(path).map_err(|_| DriftReason::missing_at("binary_path", &display))?
    } else {
        metadata
    };
    if !metadata.is_file() {
        return Err(DriftReason::new(
            "binary_path",
            format!("binary_path is not a regular file; review {display} and reinstall"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(DriftReason::new(
                "binary_path",
                format!("binary_path is not executable; review {display} and reinstall"),
            ));
        }
    }
    Ok(())
}

fn canonical_working_directory(recorded: &str) -> Result<PathBuf, DriftReason> {
    let recorded_path = Path::new(recorded);
    let canonical = crate::utils::helpers::canonicalize_without_verbatim_prefix(recorded_path)
        .map_err(|_| DriftReason::missing_at("working_directory", recorded))?;
    if !canonical.is_dir() {
        return Err(DriftReason::missing_at("working_directory", recorded));
    }
    if canonical != recorded_path {
        return Err(DriftReason::changed_at("working_directory", recorded));
    }
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// target: config -> project -> context -> workspace -> backend -> vault
// ---------------------------------------------------------------------------

async fn recompute_target(target: &ManifestTarget, cwd: &Path, reasons: &mut Vec<DriftReason>) {
    // 1. The exact global config file, read from the recorded path. The
    //    ambient config home is deliberately not consulted.
    let config_path = Path::new(&target.config_path);
    let Ok((file_config, config_bytes)) =
        crate::config::settings::load_config_file_at_with_bytes(config_path).await
    else {
        reasons.push(DriftReason::missing_at("config_path", &target.config_path));
        return;
    };
    if crate::config::content_digest(&config_bytes) != target.config_digest {
        reasons.push(DriftReason::changed_at(
            "config_digest",
            &target.config_path,
        ));
    }

    // 2. The exact `.xv.toml`, replayed at its recorded path with the recorded
    //    environment name. `load_project_at` consults neither `XV_ENV` nor
    //    `default_env`.
    let project = recompute_project(target, cwd, reasons).await;

    // 3. The exact context file, when one participated. When none did, the
    //    replay uses a context that read nothing from disk rather than the
    //    ambient one.
    let context = recompute_context(target, reasons).await;

    // 4. The workspace, from those inputs only.
    let mut effective = file_config.clone();
    effective.env_flag = target.environment.clone();
    if let Some(backend) = project
        .as_ref()
        .and_then(ResolvedProject::profile)
        .and_then(|profile| profile.backend.clone())
    {
        if crate::config::project::validate_env_profile_backend(&backend).is_ok() {
            effective.backend = Some(backend);
        }
    }
    let profile: Option<&EnvProfile> = project.as_ref().and_then(ResolvedProject::profile);

    let snapshot =
        match crate::workspace::resolve_workspace_snapshot_replay(&effective, &context, profile)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(_) => {
                // The error body can name a backend and quote provider text;
                // the field and the recorded source are enough.
                reasons.push(DriftReason::new(
                    "workspace_source",
                    "workspace_source changed: the recorded workspace no longer resolves; \
                     review the workspace and reinstall",
                ));
                return;
            }
        };
    let workspace = snapshot.workspace;

    if workspace_source_label(workspace.source) != target.workspace_source {
        reasons.push(DriftReason::new(
            "workspace_source",
            format!(
                "workspace_source changed from {} to {}; review the workspace and reinstall",
                target.workspace_source,
                workspace_source_label(workspace.source)
            ),
        ));
    }

    // 5. The selected entry. A recorded alias names an attached entry; a null
    //    alias is the degenerate workspace-of-one, where the recorded vault is
    //    a raw vault on the effective backend (exactly what installation
    //    built from `--vault`, and what it read out of the degenerate default
    //    otherwise — both of which are functions of the config/project/context
    //    bytes this validation already pins).
    let entry = match &target.workspace_alias {
        Some(alias) => match workspace.entry(alias) {
            Some(entry) => entry.clone(),
            None => {
                reasons.push(DriftReason::new(
                    "workspace_alias",
                    format!(
                        "workspace_alias '{alias}' is no longer attached; review the workspace \
                         and reinstall"
                    ),
                ));
                return;
            }
        },
        None => {
            // An implicit install (no `--vault`) pinned *the degenerate
            // workspace's default vault*, not a name. If that default has
            // since moved, the recorded name would silently keep rotating a
            // vault the resolution chain no longer chooses — so re-derive it
            // and refuse on a difference. An explicit `--vault X` pinned the
            // name `X`, which a moving default does not affect.
            if let Some(reason) = implicit_default_vault_reason(&workspace, target) {
                reasons.push(reason);
            }
            WorkspaceEntry {
                alias: crate::workspace::degenerate_alias_for(&effective, &target.vault),
                backend: effective.effective_backend_name().to_string(),
                vault: target.vault.clone(),
                default: true,
            }
        }
    };

    // 6. The backend the entry names, and the account it points at.
    if entry.backend != target.backend_name {
        reasons.push(DriftReason::new(
            "backend_name",
            format!(
                "backend_name changed from {} to {}; review the workspace and reinstall",
                target.backend_name, entry.backend
            ),
        ));
    }
    match selected_backend_identity(&effective, &entry.backend) {
        Ok(identity) => {
            if identity.kind != target.backend_kind {
                reasons.push(DriftReason::new(
                    "backend_kind",
                    format!(
                        "backend_kind changed from {} to {}; review the account/provider and \
                         reinstall",
                        target.backend_kind, identity.kind
                    ),
                ));
            }
            if identity.digest != target.backend_identity {
                reasons.push(DriftReason::new(
                    "backend_identity",
                    format!(
                        "backend_identity changed for {}; review the account/provider and \
                         reinstall",
                        entry.backend
                    ),
                ));
            }
        }
        Err(_) => reasons.push(DriftReason::new(
            "backend_name",
            format!(
                "backend_name '{}' is no longer a configured backend; review the account/provider \
                 and reinstall",
                entry.backend
            ),
        )),
    }

    // 7. The real vault behind the entry.
    if entry.vault != target.vault {
        reasons.push(DriftReason::new(
            "vault",
            format!(
                "vault changed from {} to {}; review the vault and reinstall",
                target.vault, entry.vault
            ),
        ));
    }
}

/// The `vault` reason an **implicit** degenerate target owes, if any.
///
/// An install with no `--vault` pinned *the degenerate workspace's default
/// vault*, not a name. When that default moves, the recorded name would
/// silently keep rotating a vault the resolution chain no longer chooses, so
/// the run must refuse. An explicit `--vault X` pinned the name `X`, which a
/// moving default does not affect — and a configured workspace records an
/// alias, which the caller checks instead.
///
/// Split out of [`recompute_target`] so the failure branch is reachable from a
/// test: a workspace whose `default_alias` names no entry cannot be produced
/// by `build_workspace`, only handed in.
fn implicit_default_vault_reason(
    workspace: &Workspace,
    target: &ManifestTarget,
) -> Option<DriftReason> {
    if target.workspace_source != "degenerate" || target.vault_selection != VAULT_SELECTION_IMPLICIT
    {
        return None;
    }
    match workspace.default_entry() {
        Ok(entry) if entry.vault == target.vault => None,
        // Names are deliberately absent from the wording: this reason is
        // written to an unattended log.
        Ok(_) => Some(DriftReason::new(
            "vault",
            "vault changed; review the workspace default and reinstall",
        )),
        // Fail closed. This branch exists precisely to catch a default that no
        // longer resolves the way it did; a workspace that cannot name a
        // default at all is a stronger version of that, not a reason to
        // proceed.
        Err(_) => Some(DriftReason::new(
            "vault",
            "the workspace default could not be resolved; review the workspace and reinstall",
        )),
    }
}

/// Replay the recorded project file and report project drift.
///
/// Returns the loaded project when it still resolves, so the workspace layer
/// can use its profile. Discovery from `cwd` runs too, but only to notice a
/// project file appearing, disappearing, or moving closer to the working
/// directory — the *contents* always come from the recorded path.
async fn recompute_project(
    target: &ManifestTarget,
    cwd: &Path,
    reasons: &mut Vec<DriftReason>,
) -> Option<ResolvedProject> {
    // Explicit walk-up: `find_project_config` would consult
    // `XV_NO_PARENT_CONFIG`, and a scheduled run inherits none of the
    // invoking shell's environment. Installation refuses to pin a target that
    // variable is currently hiding (`target::suppressed_parent_project`), so
    // the two discoveries agree.
    let discovered = crate::config::project::find_project_config_walking(cwd, true)
        .await
        .ok()
        .flatten()
        .map(|(path, _)| path)
        .and_then(|path| crate::utils::helpers::canonicalize_without_verbatim_prefix(&path).ok());

    let Some(recorded) = target.project_path.as_deref() else {
        if let Some(found) = discovered {
            reasons.push(DriftReason::new(
                "project_path",
                format!(
                    "project_path changed: no project file was recorded but {} now governs the \
                     recorded working directory; review the project selection and reinstall",
                    found.display()
                ),
            ));
        }
        return None;
    };

    let recorded_path = Path::new(recorded);
    let Ok(bytes) = tokio::fs::read(recorded_path).await else {
        reasons.push(DriftReason::missing_at("project_path", recorded));
        return None;
    };
    if discovered.as_deref() != Some(recorded_path) {
        reasons.push(DriftReason::changed_at("project_path", recorded));
    }
    let digest_matches = Some(crate::config::content_digest(&bytes)) == target.project_digest;
    if !digest_matches {
        reasons.push(DriftReason::changed_at("project_digest", recorded));
    }

    match crate::config::project::load_project_at(recorded_path, target.environment.as_deref())
        .await
    {
        Ok(project) => Some(project),
        Err(_) => {
            // The file parsed at install time and its digest still matches, so
            // the only remaining failure is the recorded environment no longer
            // being defined. When the digest already drifted the file may also
            // be unparseable; that is reported as `project_digest` above.
            if let Some(environment) = target.environment.as_deref() {
                reasons.push(DriftReason::new(
                    "environment",
                    format!(
                        "environment '{environment}' is no longer defined; review {recorded} and \
                         reinstall"
                    ),
                ));
            } else if digest_matches {
                reasons.push(DriftReason::changed_at("project_digest", recorded));
            }
            None
        }
    }
}

/// Replay the recorded context file, or hand back a context that read nothing
/// when the manifest recorded none. Never falls back to the ambient context.
async fn recompute_context(
    target: &ManifestTarget,
    reasons: &mut Vec<DriftReason>,
) -> ContextManager {
    let Some(recorded) = target.context_path.as_deref() else {
        return ContextManager::default();
    };
    match ContextManager::load_at(Path::new(recorded)).await {
        Ok(context) => {
            if context.source_digest() != target.context_digest.as_deref() {
                reasons.push(DriftReason::changed_at("context_digest", recorded));
            }
            context
        }
        Err(_) => {
            reasons.push(DriftReason::missing_at("context_path", recorded));
            ContextManager::default()
        }
    }
}

#[cfg(test)]
#[path = "drift_tests.rs"]
mod tests;
