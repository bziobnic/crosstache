//! The install/reinstall transaction.
//!
//! `xv schedule install` publishes two things that must agree: the pinned
//! `manifest.json` — which *is* the target — and a native scheduler entry that
//! points at it. Writing them one after another is not enough. A failure
//! between the two leaves a job with no manifest (it refuses itself at 3am) or
//! a manifest describing a job that was never registered, and a *reinstall*
//! that fails halfway leaves the user with neither the old schedule they had
//! nor the new one they asked for.
//!
//! So installation is a transaction. The design's six stages are:
//!
//! 1. Resolve and verify the target without mutation. (The caller does this and
//!    hands the result over in an [`InstallPlan`].)
//! 2. Render the manifest and every native unit fully in memory. ([`InstallPlan::new`])
//! 3. Read the exact bytes of any owned artifact that already exists into
//!    memory — this is the undo log. A foreign or symlinked artifact is
//!    refused, never adopted and never overwritten.
//! 4. Atomically publish `manifest.json`, privately. The manifest goes first:
//!    a job that exists must always have a manifest to read.
//! 5. Write the rendered unit files and register with the native scheduler.
//! 6. Query the scheduler and verify the owned entry is installed *and* points
//!    at this executable, this manifest, this cadence and this log path.
//!    Presence alone proves nothing — a stale entry from a previous install is
//!    also "present".
//!
//! If stage 5 or 6 fails, stage 3's bytes are put back and the previous
//! scheduler registration is restored. If the rollback itself fails, the prior
//! bytes are written to owner-private snapshots under `recovery/` and one error
//! carrying *both* failures is returned, because at that point the only honest
//! thing to tell the user is what broke, what could not be undone, and where
//! the evidence is.
//!
//! Everything runs under an exclusive `install.lock` held by
//! [`RealOwnedScheduleStore`] for the life of the transaction, so two
//! concurrent installs cannot interleave their stages.
//!
//! ## Why a store trait
//!
//! [`OwnedScheduleStore`] exists so the transaction's *ordering and rollback*
//! logic can be tested against injected failures at every single step. There is
//! no other way to reach the interesting branches: making a real `write` fail
//! at exactly the fourth call, and then making its undo fail too, is not
//! something a test can arrange on a real filesystem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt;

use crate::error::{CrosstacheError, Result};
use crate::schedule::manifest::{self, ScheduleStatePaths, SCHEDULE_ID};
use crate::schedule::{
    launchd_domain_target, register_native, render, unit_paths_for, unregister_native,
    CommandRunner, Platform, RotationSchedule, ScheduleCommand, UnitFile, UnitPaths, LAUNCHD_LABEL,
    SCHTASKS_NAME, SYSTEMD_UNIT,
};
use crate::utils::helpers::{
    atomic_write_file_no_follow, create_private_dir, open_private_lock_file_no_follow,
    read_file_no_follow, write_private_file_no_follow_create_new,
};

/// Cap on any single owned artifact we read back, matching the manifest cap.
/// A unit file or manifest larger than this is not something `xv` wrote.
const MAX_OWNED_BYTES: usize = 64 * 1024;

/// Marker every rendered unit carries. Its presence is what distinguishes a
/// unit `xv` owns from a same-named job a user wrote by hand, which we must
/// refuse rather than silently overwrite.
const MANAGED_MARKER: &str = "Managed by crosstache (xv schedule)";

/// Name of the temporary file used to hand a saved Task Scheduler definition
/// back to `schtasks /Create /XML`. Fixed, so no user-controlled component
/// ever reaches a path we create.
const TASK_RESTORE_FILE: &str = "restore-task.xml";

// ---------------------------------------------------------------------------
// Store seam
// ---------------------------------------------------------------------------

/// What an owned path currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArtifactState {
    /// Nothing is there.
    Absent,
    /// A file `xv` recognizes as its own, with its exact bytes.
    Owned(Vec<u8>),
    /// Something we must not touch, and why.
    Foreign(String),
}

/// The filesystem side of the install transaction.
///
/// Every method is an operation the transaction may have to undo, which is why
/// they are on a trait: the tests inject a failure at each one in turn and
/// assert the resulting on-disk state byte for byte.
pub(crate) trait OwnedScheduleStore {
    /// Exact bytes of `manifest.json`, if it is ours.
    fn read_manifest(&self) -> Result<ArtifactState>;
    /// Exact bytes of an owned unit file, if it is ours.
    fn read_unit(&self, path: &Path) -> Result<ArtifactState>;
    /// Atomically publish `manifest.json`, owner-private.
    fn write_manifest(&mut self, bytes: &[u8]) -> Result<()>;
    /// Atomically publish one unit file.
    fn write_unit(&mut self, path: &Path, bytes: &[u8]) -> Result<()>;
    /// Remove `manifest.json`. A missing file is not an error.
    fn remove_manifest(&mut self) -> Result<()>;
    /// Remove one owned unit file. A missing file is not an error.
    fn remove_unit(&mut self, path: &Path) -> Result<()>;
    /// Create the directory the scheduler appends the run log to. launchd
    /// fails a job with no visible reason when this is missing.
    fn ensure_log_dir(&mut self, log_path: &Path) -> Result<()>;
    /// Write an owner-private snapshot of prior bytes under `recovery/`.
    fn write_recovery_snapshot(&mut self, name: &str, bytes: &[u8]) -> Result<PathBuf>;
    /// Materialize a saved Task Scheduler definition so `schtasks /Create /XML`
    /// can read it back.
    fn write_task_definition_temp(&mut self, bytes: &[u8]) -> Result<PathBuf>;
    /// Best-effort removal of that temporary file.
    fn remove_task_definition_temp(&mut self);
}

/// The real store: owned paths under the schedule state directory plus the
/// platform's unit directory, holding `install.lock` for its whole lifetime.
#[derive(Debug)]
pub(crate) struct RealOwnedScheduleStore {
    paths: ScheduleStatePaths,
    /// Held, not used: dropping it releases the exclusive install lock.
    _lock: std::fs::File,
}

impl RealOwnedScheduleStore {
    /// Create the owned state directory if needed and take the exclusive
    /// `install.lock`.
    ///
    /// The lock inode is persistent — it is never removed, by this function or
    /// by uninstall — because a lock file that is deleted while another
    /// process holds it stops excluding anything.
    pub(crate) fn open(paths: &ScheduleStatePaths) -> Result<Self> {
        reject_symlinked_dir(paths.root())?;
        create_private_dir(paths.root()).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to create schedule state directory '{}': {error}",
                paths.root().display()
            ))
        })?;
        let lock = open_private_lock_file_no_follow(&paths.install_lock_path())?;
        lock.try_lock_exclusive().map_err(|error| {
            CrosstacheError::config(format!(
                "another 'xv schedule' install or uninstall is already running (could not take \
                 the exclusive lock on '{}': {error}). Wait for it to finish and try again.",
                paths.install_lock_path().display()
            ))
        })?;
        Ok(Self {
            paths: paths.clone(),
            _lock: lock,
        })
    }
}

fn reject_symlinked_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(CrosstacheError::config(format!(
            "Refusing symlinked schedule state directory '{}'",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CrosstacheError::config(format!(
            "Failed to inspect schedule state directory '{}': {error}",
            path.display()
        ))),
    }
}

/// Read an owned path without following its final link, bounded, and without
/// deciding yet whether the content is ours.
fn classify_path(path: &Path, kind: &str) -> Result<ArtifactState> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ArtifactState::Absent)
        }
        Err(error) => {
            return Err(CrosstacheError::config(format!(
                "Failed to inspect {kind} '{}': {error}",
                path.display()
            )))
        }
    };
    if metadata.file_type().is_symlink() {
        return Ok(ArtifactState::Foreign(format!(
            "'{}' is a symlink",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Ok(ArtifactState::Foreign(format!(
            "'{}' is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_OWNED_BYTES as u64 {
        return Ok(ArtifactState::Foreign(format!(
            "'{}' is larger than the {MAX_OWNED_BYTES} byte limit",
            path.display()
        )));
    }
    let bytes = read_file_no_follow(path)?;
    if bytes.len() > MAX_OWNED_BYTES {
        return Ok(ArtifactState::Foreign(format!(
            "'{}' is larger than the {MAX_OWNED_BYTES} byte limit",
            path.display()
        )));
    }
    Ok(ArtifactState::Owned(bytes))
}

/// Decide whether manifest bytes are ours.
///
/// Deliberately lenient about *corruption*: bytes that are not JSON at all sit
/// at a path inside `xv`'s own private directory, so the honest reading is "we
/// wrote this and it got damaged", and a reinstall must be able to replace it.
/// What we refuse is JSON that names a *different* schedule — that is somebody
/// else's file at our path, and adopting it would put their bytes in our undo
/// log.
fn classify_manifest_bytes(path: &Path, bytes: Vec<u8>) -> ArtifactState {
    #[derive(serde::Deserialize)]
    struct IdPeek {
        schedule_id: Option<String>,
    }
    if let Ok(peek) = serde_json::from_slice::<IdPeek>(&bytes) {
        if let Some(id) = peek.schedule_id {
            if id != SCHEDULE_ID {
                return ArtifactState::Foreign(format!(
                    "'{}' is a manifest for schedule '{id}', not '{SCHEDULE_ID}'",
                    path.display()
                ));
            }
        }
    }
    ArtifactState::Owned(bytes)
}

impl OwnedScheduleStore for RealOwnedScheduleStore {
    fn read_manifest(&self) -> Result<ArtifactState> {
        let path = self.paths.manifest_path();
        Ok(match classify_path(&path, "schedule manifest")? {
            ArtifactState::Owned(bytes) => classify_manifest_bytes(&path, bytes),
            other => other,
        })
    }

    fn read_unit(&self, path: &Path) -> Result<ArtifactState> {
        Ok(match classify_path(path, "schedule unit file")? {
            ArtifactState::Owned(bytes) => {
                if String::from_utf8_lossy(&bytes).contains(MANAGED_MARKER) {
                    ArtifactState::Owned(bytes)
                } else {
                    ArtifactState::Foreign(format!(
                        "'{}' was not written by xv (it carries no '{MANAGED_MARKER}' marker)",
                        path.display()
                    ))
                }
            }
            other => other,
        })
    }

    fn write_manifest(&mut self, bytes: &[u8]) -> Result<()> {
        manifest::write_manifest_atomic(&self.paths, bytes)
    }

    fn write_unit(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        if let Some(parent) = path.parent() {
            // The unit directory is the platform's own (`~/Library/LaunchAgents`,
            // `~/.config/systemd/user`); it is not xv-private, so it is created
            // with normal permissions. The unit file itself carries no secrets.
            std::fs::create_dir_all(parent).map_err(|error| {
                CrosstacheError::config(format!("failed to create {}: {error}", parent.display()))
            })?;
        }
        atomic_write_file_no_follow(path, bytes, false)
    }

    fn remove_manifest(&mut self) -> Result<()> {
        manifest::remove_owned_manifest(&self.paths)
    }

    fn remove_unit(&mut self, path: &Path) -> Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(CrosstacheError::config(format!(
                        "Refusing to remove symlinked schedule unit '{}'",
                        path.display()
                    )));
                }
                std::fs::remove_file(path).map_err(|error| {
                    CrosstacheError::config(format!("failed to remove {}: {error}", path.display()))
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(CrosstacheError::config(format!(
                "Failed to inspect schedule unit '{}': {error}",
                path.display()
            ))),
        }
    }

    fn ensure_log_dir(&mut self, log_path: &Path) -> Result<()> {
        let Some(parent) = log_path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent).map_err(|error| {
            CrosstacheError::config(format!(
                "failed to create the log directory {}: {error}",
                parent.display()
            ))
        })
    }

    fn write_recovery_snapshot(&mut self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let dir = self.paths.recovery_dir();
        create_private_dir(&dir).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to create schedule recovery directory '{}': {error}",
                dir.display()
            ))
        })?;
        let path = dir.join(name);
        write_private_file_no_follow_create_new(&path, bytes)?;
        Ok(path)
    }

    fn write_task_definition_temp(&mut self, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.paths.root().join(TASK_RESTORE_FILE);
        atomic_write_file_no_follow(&path, bytes, true)?;
        Ok(path)
    }

    fn remove_task_definition_temp(&mut self) {
        let _ = std::fs::remove_file(self.paths.root().join(TASK_RESTORE_FILE));
    }
}

// ---------------------------------------------------------------------------
// Plan, prior state, report
// ---------------------------------------------------------------------------

/// Everything stages 4-6 need, rendered and validated up front (stages 1-2).
#[derive(Debug, Clone)]
pub(crate) struct InstallPlan {
    pub(crate) platform: Platform,
    pub(crate) schedule: RotationSchedule,
    pub(crate) unit_paths: UnitPaths,
    /// The exact bytes `manifest.json` will contain, already stamped and
    /// validated by the caller.
    pub(crate) manifest_bytes: Vec<u8>,
    /// The rendered unit files. Empty on Task Scheduler, which keeps its own
    /// registry.
    pub(crate) units: Vec<UnitFile>,
}

impl InstallPlan {
    /// Render everything in memory.
    ///
    /// Only [`ScheduleCommand::ManifestRun`] may be installed. The refusal
    /// lives here, at the single door into installation, so no caller can
    /// reintroduce an unpinned job by constructing the legacy variant.
    pub(crate) fn new(
        platform: Platform,
        schedule: RotationSchedule,
        unit_paths: UnitPaths,
        manifest_bytes: Vec<u8>,
    ) -> Result<Self> {
        if !matches!(schedule.command, ScheduleCommand::ManifestRun { .. }) {
            return Err(CrosstacheError::config(
                "refusing to install an unpinned rotation schedule: a scheduled job must invoke \
                 'xv schedule run --manifest <path>' so its target is the one recorded at install \
                 time, not whatever the environment resolves to when it fires",
            ));
        }
        let units = render(platform, &schedule, &unit_paths);
        Ok(Self {
            platform,
            schedule,
            unit_paths,
            manifest_bytes,
            units,
        })
    }

    /// Unit files this platform owns, whether or not they exist.
    fn owned_unit_paths(&self) -> Vec<PathBuf> {
        unit_paths_for(self.platform, &self.unit_paths)
    }

    /// The manifest path the installed unit will name.
    fn manifest_arg(&self) -> &Path {
        match &self.schedule.command {
            ScheduleCommand::ManifestRun { manifest, .. } => manifest,
            // Unreachable: `new` refuses the legacy shape.
            ScheduleCommand::LegacyRotateDue { .. } => Path::new(""),
        }
    }
}

/// Stage 3's undo log: the exact bytes and registration that existed before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PriorState {
    /// Prior `manifest.json` bytes, when one was there.
    pub(crate) manifest: Option<Vec<u8>>,
    /// Prior bytes of each owned unit file that existed.
    pub(crate) units: Vec<(PathBuf, Vec<u8>)>,
    /// Whether the native scheduler already had our entry registered.
    pub(crate) registered: bool,
    /// Task Scheduler's saved XML definition of the prior task. Only
    /// Task Scheduler needs this: launchd and systemd re-register from the
    /// unit file bytes we restore, but a task definition lives only inside
    /// the scheduler, so it has to be carried out and back.
    pub(crate) task_definition: Option<Vec<u8>>,
}

impl PriorState {
    /// Whether a schedule existed at all before this install.
    pub(crate) fn existed(&self) -> bool {
        self.manifest.is_some() || !self.units.is_empty() || self.registered
    }
}

/// What a successful install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallReport {
    pub(crate) manifest_path: PathBuf,
    pub(crate) unit_paths: Vec<PathBuf>,
    /// True when this replaced an existing schedule (a reinstall).
    pub(crate) replaced_prior_schedule: bool,
}

// ---------------------------------------------------------------------------
// The transaction
// ---------------------------------------------------------------------------

/// Run the six-stage install/reinstall transaction.
///
/// `now` timestamps recovery snapshots and nothing else; `installed_at` is
/// already inside `plan.manifest_bytes`.
pub(crate) fn install_transactional(
    plan: &InstallPlan,
    store: &mut dyn OwnedScheduleStore,
    runner: &dyn CommandRunner,
    now: DateTime<Utc>,
) -> Result<InstallReport> {
    // Stage 3: read the undo log before anything changes.
    let prior = capture_prior(plan, store, runner)?;

    // Stage 4: publish the manifest. It is written atomically, so a failure
    // here leaves the previous manifest exactly as it was and nothing else has
    // been touched yet — there is nothing to roll back.
    store.write_manifest(&plan.manifest_bytes)?;

    // Stages 5 and 6.
    match publish_and_verify(plan, store, runner) {
        Ok(()) => Ok(InstallReport {
            manifest_path: plan.manifest_arg().to_path_buf(),
            unit_paths: plan.owned_unit_paths(),
            replaced_prior_schedule: prior.existed(),
        }),
        Err(primary) => match roll_back(plan, &prior, store, runner) {
            Ok(()) => Err(primary),
            Err(rollback_error) => Err(incomplete_rollback_error(
                &prior,
                store,
                now,
                &primary,
                &rollback_error,
            )),
        },
    }
}

/// Stage 3.
fn capture_prior(
    plan: &InstallPlan,
    store: &dyn OwnedScheduleStore,
    runner: &dyn CommandRunner,
) -> Result<PriorState> {
    let manifest = match store.read_manifest()? {
        ArtifactState::Absent => None,
        ArtifactState::Owned(bytes) => Some(bytes),
        ArtifactState::Foreign(reason) => return Err(foreign_refusal(&reason)),
    };

    let mut units = Vec::new();
    for path in plan.owned_unit_paths() {
        match store.read_unit(&path)? {
            ArtifactState::Absent => {}
            ArtifactState::Owned(bytes) => units.push((path, bytes)),
            ArtifactState::Foreign(reason) => return Err(foreign_refusal(&reason)),
        }
    }

    let (registered, task_definition) = capture_registration(plan.platform, runner)?;

    Ok(PriorState {
        manifest,
        units,
        registered,
        task_definition,
    })
}

fn foreign_refusal(reason: &str) -> CrosstacheError {
    CrosstacheError::config(format!(
        "refusing to install the rotation schedule: {reason}. xv will not overwrite or adopt a \
         file it did not write. Move it aside and re-run 'xv schedule install'."
    ))
}

/// Ask the scheduler what is registered *now*, before we change it.
fn capture_registration(
    platform: Platform,
    runner: &dyn CommandRunner,
) -> Result<(bool, Option<Vec<u8>>)> {
    match platform {
        Platform::Launchd => Ok((
            runner
                .run("launchctl", &["print", &launchd_domain_target()])?
                .ok(),
            None,
        )),
        Platform::Systemd => {
            let out = runner.run(
                "systemctl",
                &["--user", "is-active", &format!("{SYSTEMD_UNIT}.timer")],
            )?;
            Ok((out.stdout.trim() == "active", None))
        }
        Platform::Schtasks => {
            // The definition lives only inside Task Scheduler, so rollback has
            // to carry it out and hand it back.
            let out = runner.run("schtasks", &["/Query", "/TN", SCHTASKS_NAME, "/XML"])?;
            if out.ok() {
                Ok((true, Some(out.stdout.into_bytes())))
            } else {
                Ok((false, None))
            }
        }
    }
}

/// Stages 5 and 6.
fn publish_and_verify(
    plan: &InstallPlan,
    store: &mut dyn OwnedScheduleStore,
    runner: &dyn CommandRunner,
) -> Result<()> {
    store.ensure_log_dir(&plan.schedule.log_path)?;
    for unit in &plan.units {
        store.write_unit(&unit.path, unit.contents.as_bytes())?;
    }
    register_native(plan.platform, &plan.schedule, &plan.unit_paths, runner)?;
    verify_registration(plan, store, runner)
}

/// Stage 6: the scheduler's entry must be *this* schedule, not merely present.
///
/// Two independent checks, because neither alone is enough. The unit file we
/// just wrote is read back and compared byte for byte — that is what pins the
/// executable, the manifest path, the cadence and the log path, since all four
/// are rendered into it. Then the scheduler is asked what it actually loaded,
/// because a unit file on disk that was never picked up is not a schedule.
fn verify_registration(
    plan: &InstallPlan,
    store: &dyn OwnedScheduleStore,
    runner: &dyn CommandRunner,
) -> Result<()> {
    for unit in &plan.units {
        match store.read_unit(&unit.path)? {
            ArtifactState::Owned(bytes) if bytes == unit.contents.as_bytes() => {}
            ArtifactState::Owned(_) => {
                return Err(verification_error(&format!(
                    "the unit file '{}' does not contain what this install rendered",
                    unit.path.display()
                )))
            }
            ArtifactState::Absent => {
                return Err(verification_error(&format!(
                    "the unit file '{}' is missing after installation",
                    unit.path.display()
                )))
            }
            ArtifactState::Foreign(reason) => return Err(verification_error(&reason)),
        }
    }

    let binary = plan.schedule.binary.to_string_lossy().to_string();
    let manifest_arg = plan.manifest_arg().to_string_lossy().to_string();

    match plan.platform {
        Platform::Launchd => {
            let out = runner.run("launchctl", &["print", &launchd_domain_target()])?;
            if !out.ok() {
                return Err(verification_error(&format!(
                    "launchd does not report {LAUNCHD_LABEL} as loaded after bootstrap"
                )));
            }
            require_contains(&out.stdout, &binary, "the xv executable", "launchctl print")?;
            require_contains(
                &out.stdout,
                &manifest_arg,
                "this schedule's manifest",
                "launchctl print",
            )?;
        }
        Platform::Systemd => {
            let timer = format!("{SYSTEMD_UNIT}.timer");
            let shown = runner.run(
                "systemctl",
                &[
                    "--user",
                    "show",
                    &timer,
                    "--property=LoadState",
                    "--property=ActiveState",
                ],
            )?;
            let properties = parse_systemd_properties(&shown.stdout);
            let load_state = properties
                .get("LoadState")
                .map(String::as_str)
                .unwrap_or("");
            if load_state != "loaded" {
                return Err(verification_error(&format!(
                    "systemd reports {timer} as LoadState={load_state}, not loaded"
                )));
            }
            let active = properties
                .get("ActiveState")
                .map(String::as_str)
                .unwrap_or("");
            if active != "active" && active != "activating" {
                return Err(verification_error(&format!(
                    "systemd reports {timer} as ActiveState={active}"
                )));
            }
            let service = runner.run(
                "systemctl",
                &[
                    "--user",
                    "show",
                    &format!("{SYSTEMD_UNIT}.service"),
                    "--property=ExecStart",
                ],
            )?;
            require_contains(
                &service.stdout,
                &binary,
                "the xv executable",
                "systemctl show ExecStart",
            )?;
            require_contains(
                &service.stdout,
                &manifest_arg,
                "this schedule's manifest",
                "systemctl show ExecStart",
            )?;
        }
        Platform::Schtasks => {
            // Task Scheduler has no unit file to compare against, so the
            // registered command line carries the whole check. Cadence is not
            // re-read: `schtasks /Query` renders the schedule type and start
            // time in the machine's display language, and refusing a correct
            // install because the host is not English would be worse than the
            // gap. `/TR` is verbatim what we sent.
            let out = runner.run(
                "schtasks",
                &["/Query", "/TN", SCHTASKS_NAME, "/V", "/FO", "LIST"],
            )?;
            if !out.ok() {
                return Err(verification_error(&format!(
                    "Task Scheduler does not report {SCHTASKS_NAME} after /Create"
                )));
            }
            require_contains(
                &out.stdout,
                &plan.schedule.command_line(),
                "this schedule's command line",
                "schtasks /Query /V",
            )?;
            require_contains(
                &out.stdout,
                &plan.schedule.log_path.to_string_lossy(),
                "this schedule's log path",
                "schtasks /Query /V",
            )?;
        }
    }
    Ok(())
}

fn parse_systemd_properties(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn require_contains(haystack: &str, needle: &str, what: &str, source: &str) -> Result<()> {
    if haystack.contains(needle) {
        return Ok(());
    }
    Err(verification_error(&format!(
        "the registered job does not name {what} ('{needle}' is absent from {source} output)"
    )))
}

fn verification_error(detail: &str) -> CrosstacheError {
    CrosstacheError::config(format!(
        "the rotation schedule was written but the scheduler did not accept it: {detail}"
    ))
}

/// Put back exactly what stage 3 recorded.
fn roll_back(
    plan: &InstallPlan,
    prior: &PriorState,
    store: &mut dyn OwnedScheduleStore,
    runner: &dyn CommandRunner,
) -> Result<()> {
    match &prior.manifest {
        Some(bytes) => store.write_manifest(bytes)?,
        None => store.remove_manifest()?,
    }

    for path in plan.owned_unit_paths() {
        match prior
            .units
            .iter()
            .find(|(prior_path, _)| *prior_path == path)
        {
            Some((_, bytes)) => store.write_unit(&path, bytes)?,
            None => store.remove_unit(&path)?,
        }
    }

    if !prior.registered {
        // Nothing was registered before, so converge on absent. A scheduler
        // that says "no such job" is the outcome we want, not a failure.
        unregister_native(plan.platform, runner)?;
        return Ok(());
    }

    match (plan.platform, &prior.task_definition) {
        (Platform::Schtasks, Some(definition)) => {
            let path = store.write_task_definition_temp(definition)?;
            let result = runner.run(
                "schtasks",
                &[
                    "/Create",
                    "/TN",
                    SCHTASKS_NAME,
                    "/XML",
                    &path.to_string_lossy(),
                    "/F",
                ],
            )?;
            store.remove_task_definition_temp();
            if !result.ok() {
                return Err(CrosstacheError::config(format!(
                    "failed to restore the previous Task Scheduler entry (exit {}): {}",
                    result.status,
                    result.stderr.trim()
                )));
            }
            Ok(())
        }
        // launchd and systemd re-register from the unit file bytes that were
        // just put back, so the previous registration is fully described by
        // what is on disk again.
        _ => register_native(plan.platform, &plan.schedule, &plan.unit_paths, runner),
    }
}

/// One error carrying both failures, plus where the prior bytes were saved.
fn incomplete_rollback_error(
    prior: &PriorState,
    store: &mut dyn OwnedScheduleStore,
    now: DateTime<Utc>,
    primary: &CrosstacheError,
    rollback_error: &CrosstacheError,
) -> CrosstacheError {
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let mut saved: Vec<String> = Vec::new();
    let mut snapshot_failures: Vec<String> = Vec::new();

    let snapshot = |store: &mut dyn OwnedScheduleStore,
                    name: &str,
                    bytes: &[u8],
                    saved: &mut Vec<String>,
                    failures: &mut Vec<String>| {
        match store.write_recovery_snapshot(&format!("{stamp}-{name}"), bytes) {
            Ok(path) => saved.push(path.display().to_string()),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    };

    if let Some(bytes) = &prior.manifest {
        snapshot(
            store,
            "manifest.json",
            bytes,
            &mut saved,
            &mut snapshot_failures,
        );
    }
    for (path, bytes) in &prior.units {
        snapshot(
            store,
            owned_artifact_name(path),
            bytes,
            &mut saved,
            &mut snapshot_failures,
        );
    }
    if let Some(bytes) = &prior.task_definition {
        snapshot(
            store,
            "scheduled-task.xml",
            bytes,
            &mut saved,
            &mut snapshot_failures,
        );
    }

    let mut message = format!(
        "failed to install the rotation schedule: {primary}\n  \
         rolling back to the previous schedule also failed: {rollback_error}"
    );
    if !saved.is_empty() {
        message.push_str("\n  the previous state was saved to:");
        for path in saved {
            message.push_str(&format!("\n    {path}"));
        }
    }
    if !snapshot_failures.is_empty() {
        message.push_str("\n  and these could not be saved:");
        for failure in snapshot_failures {
            message.push_str(&format!("\n    {failure}"));
        }
    }
    message.push_str(
        "\n  Run 'xv schedule status' to see what the scheduler currently has, then reinstall \
         with 'xv schedule install' to get back to a known state.",
    );
    CrosstacheError::config(message)
}

/// The fixed snapshot name for an owned unit path.
///
/// Derived from the platform's constants rather than the path itself, so no
/// part of a recovery filename can come from anywhere a user controls.
fn owned_artifact_name(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name == format!("{LAUNCHD_LABEL}.plist") {
        "com.crosstache.xv-rotate.plist"
    } else if name == format!("{SYSTEMD_UNIT}.service") {
        "xv-rotate.service"
    } else if name == format!("{SYSTEMD_UNIT}.timer") {
        "xv-rotate.timer"
    } else {
        "unit"
    }
}

/// Test-only counters for the fake store, kept out of the test module so the
/// fake can be declared next to the trait it implements.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StoreOp {
    ReadManifest,
    ReadUnit,
    WriteManifest,
    WriteUnit,
    RemoveManifest,
    RemoveUnit,
    EnsureLogDir,
    Snapshot,
    WriteTaskTemp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{fixture_abs, CommandOutput, ScheduleInterval};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    // -- fixtures -----------------------------------------------------------

    fn manifest_path() -> PathBuf {
        PathBuf::from(fixture_abs(
            "/home/u/.local/state/xv/schedules/rotation-default/manifest.json",
        ))
    }

    fn unit_dir() -> UnitPaths {
        UnitPaths {
            dir: PathBuf::from(fixture_abs("/home/u/units")),
        }
    }

    fn schedule() -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily {
                hour: 3,
                minute: 30,
            },
            command: ScheduleCommand::ManifestRun {
                manifest: manifest_path(),
                working_directory: PathBuf::from(fixture_abs("/home/u/work/service")),
            },
            binary: PathBuf::from(fixture_abs("/usr/local/bin/xv")),
            log_path: PathBuf::from(fixture_abs("/home/u/.local/state/xv/rotate.log")),
            home: PathBuf::from(fixture_abs("/home/u")),
            state_home: None,
        }
    }

    const MANIFEST_BYTES: &[u8] = b"{\n  \"schedule_id\": \"rotation-default\"\n}\n";
    const OLD_MANIFEST_BYTES: &[u8] =
        b"{\n  \"schedule_id\": \"rotation-default\",\n  \"old\": 1\n}\n";

    fn plan(platform: Platform) -> InstallPlan {
        InstallPlan::new(platform, schedule(), unit_dir(), MANIFEST_BYTES.to_vec())
            .expect("the pinned runner is installable")
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-10T04:15:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // -- fake store ---------------------------------------------------------

    /// In-memory owned filesystem with per-operation failure injection.
    #[derive(Debug)]
    struct FakeStore {
        manifest_path: PathBuf,
        files: BTreeMap<PathBuf, Vec<u8>>,
        /// Paths that must report `Foreign`, with the reason.
        foreign: BTreeMap<PathBuf, String>,
        /// Bytes a read returns instead of what is stored — used to make
        /// verification see something other than what was written.
        read_as: BTreeMap<PathBuf, Vec<u8>>,
        /// `(op, nth)`: fail the nth (1-based) call of `op`.
        fail: Vec<(StoreOp, u32)>,
        counts: RefCell<HashMap<StoreOp, u32>>,
        log_dirs: RefCell<Vec<PathBuf>>,
        snapshots: BTreeMap<PathBuf, Vec<u8>>,
        task_temp: Option<Vec<u8>>,
        recovery_dir: PathBuf,
    }

    impl FakeStore {
        fn new() -> Self {
            Self {
                manifest_path: manifest_path(),
                files: BTreeMap::new(),
                foreign: BTreeMap::new(),
                read_as: BTreeMap::new(),
                fail: Vec::new(),
                counts: RefCell::new(HashMap::new()),
                log_dirs: RefCell::new(Vec::new()),
                snapshots: BTreeMap::new(),
                task_temp: None,
                recovery_dir: PathBuf::from(fixture_abs(
                    "/home/u/.local/state/xv/schedules/rotation-default/recovery",
                )),
            }
        }

        /// A store that already holds a complete prior systemd schedule.
        fn with_prior(platform: Platform) -> Self {
            let mut store = Self::new();
            store
                .files
                .insert(manifest_path(), OLD_MANIFEST_BYTES.to_vec());
            for path in unit_paths_for(platform, &unit_dir()) {
                store.files.insert(
                    path,
                    b"# Managed by crosstache (xv schedule). previous\n".to_vec(),
                );
            }
            store
        }

        fn failing(mut self, op: StoreOp, nth: u32) -> Self {
            self.fail.push((op, nth));
            self
        }

        fn check(&self, op: StoreOp) -> Result<()> {
            let mut counts = self.counts.borrow_mut();
            let seen = counts.entry(op).or_insert(0);
            *seen += 1;
            if self.fail.iter().any(|(o, n)| *o == op && *n == *seen) {
                return Err(CrosstacheError::config(format!("injected {op:?} failure")));
            }
            Ok(())
        }

        fn classify(&self, path: &Path, managed: bool) -> ArtifactState {
            if let Some(reason) = self.foreign.get(path) {
                return ArtifactState::Foreign(reason.clone());
            }
            match self.read_as.get(path).or_else(|| self.files.get(path)) {
                None => ArtifactState::Absent,
                Some(bytes) => {
                    if managed && !String::from_utf8_lossy(bytes).contains(MANAGED_MARKER) {
                        ArtifactState::Foreign(format!("'{}' is not ours", path.display()))
                    } else {
                        ArtifactState::Owned(bytes.clone())
                    }
                }
            }
        }

        fn get(&self, path: &Path) -> Option<&Vec<u8>> {
            self.files.get(path)
        }
    }

    impl OwnedScheduleStore for FakeStore {
        fn read_manifest(&self) -> Result<ArtifactState> {
            self.check(StoreOp::ReadManifest)?;
            Ok(self.classify(&self.manifest_path.clone(), false))
        }
        fn read_unit(&self, path: &Path) -> Result<ArtifactState> {
            self.check(StoreOp::ReadUnit)?;
            Ok(self.classify(path, true))
        }
        fn write_manifest(&mut self, bytes: &[u8]) -> Result<()> {
            self.check(StoreOp::WriteManifest)?;
            self.files
                .insert(self.manifest_path.clone(), bytes.to_vec());
            Ok(())
        }
        fn write_unit(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
            self.check(StoreOp::WriteUnit)?;
            self.files.insert(path.to_path_buf(), bytes.to_vec());
            Ok(())
        }
        fn remove_manifest(&mut self) -> Result<()> {
            self.check(StoreOp::RemoveManifest)?;
            self.files.remove(&self.manifest_path);
            Ok(())
        }
        fn remove_unit(&mut self, path: &Path) -> Result<()> {
            self.check(StoreOp::RemoveUnit)?;
            self.files.remove(path);
            Ok(())
        }
        fn ensure_log_dir(&mut self, log_path: &Path) -> Result<()> {
            self.check(StoreOp::EnsureLogDir)?;
            self.log_dirs
                .borrow_mut()
                .push(log_path.parent().unwrap_or(log_path).to_path_buf());
            Ok(())
        }
        fn write_recovery_snapshot(&mut self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
            self.check(StoreOp::Snapshot)?;
            let path = self.recovery_dir.join(name);
            self.snapshots.insert(path.clone(), bytes.to_vec());
            Ok(path)
        }
        fn write_task_definition_temp(&mut self, bytes: &[u8]) -> Result<PathBuf> {
            self.check(StoreOp::WriteTaskTemp)?;
            self.task_temp = Some(bytes.to_vec());
            Ok(PathBuf::from(fixture_abs(
                "/home/u/.local/state/xv/schedules/rotation-default/restore-task.xml",
            )))
        }
        fn remove_task_definition_temp(&mut self) {
            self.task_temp = None;
        }
    }

    // -- fake runner --------------------------------------------------------

    /// Scheduler that answers as a healthy host would, with optional failure
    /// injection by argument substring.
    #[derive(Debug)]
    struct FakeRunner {
        calls: Mutex<Vec<String>>,
        /// Any call whose flattened args contain this fails.
        fail_containing: Option<String>,
        /// Whether a prior job is registered when the transaction starts.
        prior_registered: bool,
        /// Manifest path the scheduler reports for the installed job.
        reports_manifest: String,
        /// Binary path the scheduler reports for the installed job.
        reports_binary: String,
        /// Log path Task Scheduler reports.
        reports_log: String,
        /// systemd `ActiveState` after enabling.
        reports_active: String,
        /// systemd `LoadState` after enabling.
        reports_load: String,
    }

    impl Default for FakeRunner {
        fn default() -> Self {
            let s = schedule();
            Self {
                calls: Mutex::new(Vec::new()),
                fail_containing: None,
                prior_registered: false,
                reports_manifest: manifest_path().to_string_lossy().to_string(),
                reports_binary: s.binary.to_string_lossy().to_string(),
                reports_log: s.log_path.to_string_lossy().to_string(),
                reports_active: "active".to_string(),
                reports_load: "loaded".to_string(),
            }
        }
    }

    impl FakeRunner {
        fn with_prior() -> Self {
            Self {
                prior_registered: true,
                ..Self::default()
            }
        }
        fn flat(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        /// The `/TR` line `schtasks /Query /V` prints, built from what this
        /// fake claims is registered rather than from the plan — so a test can
        /// make the scheduler report a job pointing somewhere else.
        fn task_command(&self) -> String {
            format!(
                "Task To Run:  cmd /c {} schedule run --manifest {} >> \"{}\" 2>&1",
                self.reports_binary, self.reports_manifest, self.reports_log
            )
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
            let joined = args.join(" ");
            self.calls
                .lock()
                .unwrap()
                .push(format!("{program} {joined}"));
            let forced_failure = self
                .fail_containing
                .as_ref()
                .is_some_and(|needle| joined.contains(needle));
            if forced_failure {
                return Ok(CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "boom".to_string(),
                });
            }

            // How many calls of this shape have already happened decides
            // whether we are answering "before" or "after" registration.
            let already_registered = self.prior_registered
                || self.calls.lock().unwrap().iter().any(|c| {
                    c.contains("bootstrap") || c.contains("enable") || c.contains("/Create")
                });

            let (status, stdout) = match (program, joined.as_str()) {
                ("launchctl", j) if j.starts_with("print") => {
                    if already_registered {
                        (
                            0,
                            format!(
                                "com.crosstache.xv-rotate = {{\n\tstate = waiting\n\tprogram = {}\n\
                                 \targuments = {{\n\t\t{}\n\t\tschedule\n\t\trun\n\t\t--manifest\n\
                                 \t\t{}\n\t}}\n}}\n",
                                self.reports_binary, self.reports_binary, self.reports_manifest
                            ),
                        )
                    } else {
                        (1, String::new())
                    }
                }
                ("systemctl", j) if j.contains("is-active") => (
                    0,
                    if already_registered {
                        "active\n".to_string()
                    } else {
                        "inactive\n".to_string()
                    },
                ),
                ("systemctl", j) if j.contains("show") && j.contains("timer") => (
                    0,
                    if already_registered {
                        format!(
                            "LoadState={}\nActiveState={}\n",
                            self.reports_load, self.reports_active
                        )
                    } else {
                        "LoadState=not-found\nActiveState=inactive\n".to_string()
                    },
                ),
                ("systemctl", j) if j.contains("show") && j.contains("service") => (
                    0,
                    format!(
                        "ExecStart={{ path={} ; argv[]={} schedule run --manifest {} ; ignore_errors=no }}\n",
                        self.reports_binary, self.reports_binary, self.reports_manifest
                    ),
                ),
                ("schtasks", j) if j.contains("/XML") && j.contains("/Query") => {
                    if self.prior_registered {
                        (0, "<Task><Exec>previous</Exec></Task>\n".to_string())
                    } else {
                        (1, String::new())
                    }
                }
                ("schtasks", j) if j.contains("/Query") => {
                    if already_registered {
                        (0, self.task_command())
                    } else {
                        (1, String::new())
                    }
                }
                _ => (0, String::new()),
            };
            Ok(CommandOutput {
                status,
                stdout,
                stderr: if status == 0 {
                    String::new()
                } else {
                    "not found".to_string()
                },
            })
        }
    }

    fn platforms() -> [Platform; 3] {
        [Platform::Launchd, Platform::Systemd, Platform::Schtasks]
    }

    // -- happy paths --------------------------------------------------------

    #[test]
    fn a_first_install_publishes_the_manifest_and_every_unit() {
        for platform in platforms() {
            let plan = plan(platform);
            let mut store = FakeStore::new();
            let runner = FakeRunner::default();

            let report = install_transactional(&plan, &mut store, &runner, now())
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(!report.replaced_prior_schedule, "{platform:?}");
            assert_eq!(
                store.get(&manifest_path()),
                Some(&MANIFEST_BYTES.to_vec()),
                "{platform:?}"
            );
            for unit in &plan.units {
                assert_eq!(
                    store
                        .get(&unit.path)
                        .map(|b| String::from_utf8_lossy(b).to_string()),
                    Some(unit.contents.clone()),
                    "{platform:?}"
                );
            }
            assert!(
                store.snapshots.is_empty(),
                "{platform:?} wrote recovery evidence for a clean install"
            );
            // The log directory is created before the scheduler can append.
            assert_eq!(store.log_dirs.borrow().len(), 1, "{platform:?}");
        }
    }

    #[test]
    fn the_manifest_is_published_before_the_scheduler_is_touched() {
        // A registered job that has no manifest to read refuses itself at 3am;
        // a manifest with no job is inert. So the manifest goes first.
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        let runner = FakeRunner::default();
        // Fail the very first unit write: the manifest must already be there.
        let mut failing = FakeStore::new().failing(StoreOp::WriteUnit, 1);
        let _ = install_transactional(&plan, &mut failing, &runner, now());
        assert!(
            failing.get(&manifest_path()).is_none(),
            "a failed first install must not leave its manifest behind"
        );
        install_transactional(&plan, &mut store, &runner, now()).unwrap();
        assert!(store.get(&manifest_path()).is_some());
    }

    #[test]
    fn a_reinstall_replaces_prior_bytes_and_keeps_unowned_state() {
        for platform in platforms() {
            let plan = plan(platform);
            let mut store = FakeStore::with_prior(platform);
            // Files the transaction must never touch.
            let last_run = PathBuf::from(fixture_abs(
                "/home/u/.local/state/xv/schedules/rotation-default/last-run.json",
            ));
            let run_lock = PathBuf::from(fixture_abs(
                "/home/u/.local/state/xv/schedules/rotation-default/run.lock",
            ));
            let log = schedule().log_path;
            store
                .files
                .insert(last_run.clone(), b"{\"outcome\":1}".to_vec());
            store.files.insert(run_lock.clone(), Vec::new());
            store
                .files
                .insert(log.clone(), b"previous run output\n".to_vec());
            let runner = FakeRunner::with_prior();

            let report = install_transactional(&plan, &mut store, &runner, now())
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(report.replaced_prior_schedule, "{platform:?}");
            assert_eq!(
                store.get(&manifest_path()),
                Some(&MANIFEST_BYTES.to_vec()),
                "{platform:?} kept the old manifest"
            );
            for unit in &plan.units {
                assert_eq!(
                    store
                        .get(&unit.path)
                        .map(|b| String::from_utf8_lossy(b).to_string()),
                    Some(unit.contents.clone()),
                    "{platform:?}"
                );
            }
            assert_eq!(
                store.get(&last_run),
                Some(&b"{\"outcome\":1}".to_vec()),
                "{platform:?} clobbered last-run.json"
            );
            assert_eq!(
                store.get(&run_lock),
                Some(&Vec::new()),
                "{platform:?} clobbered run.lock"
            );
            assert_eq!(
                store.get(&log),
                Some(&b"previous run output\n".to_vec()),
                "{platform:?} clobbered the log"
            );
        }
    }

    #[test]
    fn an_unpinned_legacy_command_is_refused_before_anything_is_rendered() {
        let legacy = RotationSchedule {
            command: ScheduleCommand::LegacyRotateDue {
                vault: Some("prod-kv".into()),
            },
            ..schedule()
        };
        let err = InstallPlan::new(
            Platform::Systemd,
            legacy,
            unit_dir(),
            MANIFEST_BYTES.to_vec(),
        )
        .expect_err("must refuse");
        assert!(err.to_string().contains("unpinned"), "{err}");
    }

    // -- ownership refusals -------------------------------------------------

    #[test]
    fn a_foreign_unit_at_an_owned_path_is_refused_and_left_alone() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        let victim = plan.units[0].path.clone();
        store
            .files
            .insert(victim.clone(), b"# my own timer\n".to_vec());
        let runner = FakeRunner::default();

        let err = install_transactional(&plan, &mut store, &runner, now())
            .expect_err("a unit xv did not write must not be overwritten");
        assert!(err.to_string().contains("will not overwrite"), "{err}");
        assert_eq!(
            store.get(&victim),
            Some(&b"# my own timer\n".to_vec()),
            "the foreign file was modified"
        );
        assert!(
            store.get(&manifest_path()).is_none(),
            "the refusal happened after the manifest was published"
        );
        assert!(
            runner.flat().iter().all(|c| !c.contains("enable")),
            "{:?}",
            runner.flat()
        );
    }

    #[test]
    fn a_symlinked_owned_path_is_refused() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        let victim = plan.units[1].path.clone();
        store.foreign.insert(
            victim.clone(),
            format!("'{}' is a symlink", victim.display()),
        );
        let runner = FakeRunner::default();

        let err = install_transactional(&plan, &mut store, &runner, now())
            .expect_err("a symlinked owned path must be refused");
        assert!(err.to_string().contains("is a symlink"), "{err}");
        assert!(store.get(&manifest_path()).is_none());
    }

    #[test]
    fn a_manifest_for_another_schedule_is_refused() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        store.foreign.insert(
            manifest_path(),
            "'manifest.json' is a manifest for schedule 'other', not 'rotation-default'"
                .to_string(),
        );
        let err = install_transactional(&plan, &mut store, &FakeRunner::default(), now())
            .expect_err("must refuse");
        assert!(err.to_string().contains("schedule 'other'"), "{err}");
    }

    // -- failure injection: no prior schedule -------------------------------

    #[test]
    fn a_failed_manifest_publish_leaves_nothing_behind() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new().failing(StoreOp::WriteManifest, 1);
        let runner = FakeRunner::default();
        let err = install_transactional(&plan, &mut store, &runner, now()).expect_err("must fail");
        assert!(err.to_string().contains("injected WriteManifest"), "{err}");
        assert!(store.files.is_empty(), "{:?}", store.files);
        assert!(runner.flat().iter().all(|c| !c.contains("enable")));
    }

    #[test]
    fn a_failure_at_each_native_step_rolls_the_first_install_all_the_way_back() {
        // Each of the writes and the registration, in turn.
        /// `(what failed, injected store failure, injected scheduler failure)`.
        type Injection = (&'static str, Option<(StoreOp, u32)>, Option<&'static str>);
        let injections: Vec<Injection> = vec![
            ("log directory", Some((StoreOp::EnsureLogDir, 1)), None),
            ("first unit write", Some((StoreOp::WriteUnit, 1)), None),
            ("second unit write", Some((StoreOp::WriteUnit, 2)), None),
            ("registration", None, Some("enable")),
            ("daemon reload", None, Some("daemon-reload")),
        ];
        for (what, store_failure, runner_failure) in injections {
            let plan = plan(Platform::Systemd);
            let mut store = FakeStore::new();
            if let Some((op, nth)) = store_failure {
                store = store.failing(op, nth);
            }
            let runner = FakeRunner {
                fail_containing: runner_failure.map(str::to_string),
                ..FakeRunner::default()
            };

            let err = install_transactional(&plan, &mut store, &runner, now())
                .expect_err(&format!("{what}: must fail"));
            assert!(
                !err.to_string().contains("also failed"),
                "{what}: rollback should have succeeded: {err}"
            );
            assert!(
                store.files.is_empty(),
                "{what}: rollback left files behind: {:?}",
                store.files.keys().collect::<Vec<_>>()
            );
            assert!(
                store.snapshots.is_empty(),
                "{what}: wrote needless recovery evidence"
            );
            assert!(
                runner.flat().iter().any(|c| c.contains("disable --now")),
                "{what}: the new registration was not withdrawn: {:?}",
                runner.flat()
            );
        }
    }

    #[test]
    fn a_failed_verification_rolls_the_first_install_back() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        let runner = FakeRunner {
            reports_active: "failed".to_string(),
            ..FakeRunner::default()
        };
        let err = install_transactional(&plan, &mut store, &runner, now()).expect_err("must fail");
        assert!(err.to_string().contains("ActiveState=failed"), "{err}");
        assert!(store.files.is_empty(), "{:?}", store.files);
    }

    // -- failure injection: reinstall over a prior schedule -----------------

    fn prior_bytes(platform: Platform) -> Vec<(PathBuf, Vec<u8>)> {
        unit_paths_for(platform, &unit_dir())
            .into_iter()
            .map(|p| {
                (
                    p,
                    b"# Managed by crosstache (xv schedule). previous\n".to_vec(),
                )
            })
            .collect()
    }

    #[test]
    fn a_failed_reinstall_restores_the_previous_manifest_and_units_byte_for_byte() {
        // The scheduler itself stays healthy here, so re-registering the
        // previous units succeeds; a failure of the registration *and* its undo
        // is the incomplete-rollback case tested below.
        for (what, op, nth) in [
            ("first unit write", StoreOp::WriteUnit, 1),
            ("second unit write", StoreOp::WriteUnit, 2),
        ] {
            let plan = plan(Platform::Systemd);
            let mut store = FakeStore::with_prior(Platform::Systemd).failing(op, nth);
            let runner = FakeRunner::with_prior();

            let err = install_transactional(&plan, &mut store, &runner, now())
                .expect_err(&format!("{what}: must fail"));
            assert!(!err.to_string().contains("also failed"), "{what}: {err}");
            assert_eq!(
                store.get(&manifest_path()),
                Some(&OLD_MANIFEST_BYTES.to_vec()),
                "{what}: the previous manifest was not restored"
            );
            for (path, bytes) in prior_bytes(Platform::Systemd) {
                assert_eq!(store.get(&path), Some(&bytes), "{what}: {path:?}");
            }
            assert!(store.snapshots.is_empty(), "{what}");
        }
    }

    #[test]
    fn a_failure_in_each_rollback_step_reports_both_errors_and_saves_the_prior_bytes() {
        let rollback_failures = [
            StoreOp::WriteManifest,
            StoreOp::WriteUnit,
            StoreOp::RemoveUnit,
        ];
        for op in rollback_failures {
            let plan = plan(Platform::Systemd);
            // The rollback's writes are the *second* of each kind: one publish
            // happened first.
            let nth = if op == StoreOp::RemoveUnit { 1 } else { 2 };
            let mut store = FakeStore::with_prior(Platform::Systemd).failing(op, nth);
            if op == StoreOp::RemoveUnit {
                // Nothing to remove during a reinstall rollback, so exercise
                // this one against a first install instead.
                store = FakeStore::new().failing(op, nth);
                store
                    .files
                    .insert(manifest_path(), OLD_MANIFEST_BYTES.to_vec());
            }
            let runner = FakeRunner {
                fail_containing: Some("enable".to_string()),
                ..FakeRunner::with_prior()
            };

            let err = install_transactional(&plan, &mut store, &runner, now())
                .expect_err(&format!("{op:?}: must fail"));
            let message = err.to_string();
            assert!(
                message.contains("systemctl --user enable"),
                "{op:?}: {message}"
            );
            assert!(message.contains("also failed"), "{op:?}: {message}");
            assert!(message.contains("xv schedule status"), "{op:?}: {message}");
            assert!(
                message.contains("previous state was saved to"),
                "{op:?}: {message}"
            );
            // The snapshots are the prior bytes, exactly — nothing summarized,
            // nothing added.
            let snapshot_bytes: Vec<Vec<u8>> = store.snapshots.values().cloned().collect();
            assert!(
                snapshot_bytes.contains(&OLD_MANIFEST_BYTES.to_vec()),
                "{op:?}: {:?}",
                store.snapshots.keys().collect::<Vec<_>>()
            );
            for path in store.snapshots.keys() {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                assert!(name.starts_with("20260910T041500Z-"), "{name}");
            }
        }
    }

    #[test]
    fn a_failed_rollback_registration_also_reports_both_errors() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::with_prior(Platform::Systemd);
        // Enabling the timer is what fails, so putting the previous units back
        // and re-enabling them fails for the same reason.
        let runner = FakeRunner {
            fail_containing: Some("enable".to_string()),
            ..FakeRunner::with_prior()
        };
        let err = install_transactional(&plan, &mut store, &runner, now()).expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("also failed"), "{message}");
        assert!(message.contains("xv schedule status"), "{message}");
    }

    #[test]
    fn a_failed_recovery_snapshot_still_reports_both_errors() {
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::with_prior(Platform::Systemd)
            .failing(StoreOp::WriteManifest, 2)
            .failing(StoreOp::Snapshot, 1);
        let runner = FakeRunner {
            fail_containing: Some("enable".to_string()),
            ..FakeRunner::with_prior()
        };
        let err = install_transactional(&plan, &mut store, &runner, now()).expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("also failed"), "{message}");
        assert!(message.contains("could not be saved"), "{message}");
    }

    // -- verification -------------------------------------------------------

    #[test]
    fn verification_rejects_a_job_registered_against_another_manifest() {
        for platform in [Platform::Launchd, Platform::Systemd, Platform::Schtasks] {
            let plan = plan(platform);
            let mut store = FakeStore::new();
            let runner = FakeRunner {
                reports_manifest: fixture_abs("/tmp/somebody-elses/manifest.json"),
                reports_log: fixture_abs("/home/u/.local/state/xv/rotate.log"),
                ..FakeRunner::default()
            };
            let err = install_transactional(&plan, &mut store, &runner, now())
                .expect_err(&format!("{platform:?}: must refuse"));
            assert!(
                err.to_string().contains("scheduler did not accept it"),
                "{platform:?}: {err}"
            );
            assert!(store.files.is_empty(), "{platform:?}: {:?}", store.files);
        }
    }

    #[test]
    fn verification_rejects_a_job_registered_against_another_executable() {
        for platform in [Platform::Launchd, Platform::Systemd] {
            let plan = plan(platform);
            let mut store = FakeStore::new();
            let runner = FakeRunner {
                reports_binary: fixture_abs("/opt/other/bin/xv"),
                ..FakeRunner::default()
            };
            let err = install_transactional(&plan, &mut store, &runner, now())
                .expect_err(&format!("{platform:?}: must refuse"));
            assert!(
                err.to_string().contains("the xv executable"),
                "{platform:?}: {err}"
            );
        }
    }

    #[test]
    fn verification_rejects_a_task_registered_against_another_log_path() {
        let plan = plan(Platform::Schtasks);
        let mut store = FakeStore::new();
        let runner = FakeRunner {
            reports_log: fixture_abs("/var/tmp/elsewhere.log"),
            ..FakeRunner::default()
        };
        let err =
            install_transactional(&plan, &mut store, &runner, now()).expect_err("must refuse");
        assert!(err.to_string().contains("log path"), "{err}");
    }

    #[test]
    fn verification_rejects_a_unit_whose_bytes_changed_under_us() {
        // Same cadence in the manifest, a different one in the unit that
        // actually got written: the schedule would fire at the wrong time.
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        let timer = plan.units[1].path.clone();
        let tampered = plan.units[1]
            .contents
            .replace("03:30:00", "23:30:00")
            .into_bytes();
        store.read_as.insert(timer, tampered);
        let runner = FakeRunner::default();
        let err =
            install_transactional(&plan, &mut store, &runner, now()).expect_err("must refuse");
        assert!(
            err.to_string()
                .contains("does not contain what this install rendered"),
            "{err}"
        );
    }

    #[test]
    fn verification_rejects_a_unit_that_systemd_never_loaded() {
        // `systemctl show` exits 0 even for a unit that does not exist, so the
        // call succeeding proves nothing; LoadState is the real answer.
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::new();
        let runner = FakeRunner {
            reports_load: "not-found".to_string(),
            ..FakeRunner::default()
        };
        let err =
            install_transactional(&plan, &mut store, &runner, now()).expect_err("must refuse");
        assert!(err.to_string().contains("LoadState=not-found"), "{err}");
        assert!(store.files.is_empty(), "{:?}", store.files);
    }

    // -- Task Scheduler rollback -------------------------------------------

    #[test]
    fn a_failed_reinstall_restores_the_previous_scheduled_task_definition() {
        let plan = plan(Platform::Schtasks);
        let mut store = FakeStore::new();
        store
            .files
            .insert(manifest_path(), OLD_MANIFEST_BYTES.to_vec());
        let runner = FakeRunner {
            fail_containing: Some("/SC".to_string()),
            ..FakeRunner::with_prior()
        };

        let err = install_transactional(&plan, &mut store, &runner, now()).expect_err("must fail");
        assert!(!err.to_string().contains("also failed"), "{err}");
        assert_eq!(
            store.get(&manifest_path()),
            Some(&OLD_MANIFEST_BYTES.to_vec()),
            "the previous manifest was not restored"
        );
        let flat = runner.flat();
        assert!(
            flat.iter()
                .any(|c| c.contains("/XML") && c.contains("/Create")),
            "the saved task definition was not handed back: {flat:?}"
        );
        assert!(
            store.task_temp.is_none(),
            "the temporary definition was left behind"
        );
    }

    // -- the real store -----------------------------------------------------

    #[test]
    fn the_real_store_round_trips_owned_bytes_and_refuses_foreign_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = manifest::test_paths_in(tmp.path());
        let mut store = RealOwnedScheduleStore::open(&paths).unwrap();

        assert_eq!(store.read_manifest().unwrap(), ArtifactState::Absent);
        store.write_manifest(MANIFEST_BYTES).unwrap();
        assert_eq!(
            store.read_manifest().unwrap(),
            ArtifactState::Owned(MANIFEST_BYTES.to_vec())
        );

        let unit = tmp.path().join("units/xv-rotate.timer");
        assert_eq!(store.read_unit(&unit).unwrap(), ArtifactState::Absent);
        store
            .write_unit(&unit, b"# Managed by crosstache (xv schedule).\n[Timer]\n")
            .unwrap();
        assert!(matches!(
            store.read_unit(&unit).unwrap(),
            ArtifactState::Owned(_)
        ));

        // A same-named job the user wrote themselves.
        let foreign = tmp.path().join("units/foreign.timer");
        std::fs::write(&foreign, b"[Timer]\nOnCalendar=daily\n").unwrap();
        assert!(matches!(
            store.read_unit(&foreign).unwrap(),
            ArtifactState::Foreign(_)
        ));

        // A manifest for somebody else's schedule.
        store
            .write_manifest(b"{\"schedule_id\":\"other\"}\n")
            .unwrap();
        match store.read_manifest().unwrap() {
            ArtifactState::Foreign(reason) => assert!(reason.contains("'other'"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_real_store_refuses_a_symlinked_owned_path() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = manifest::test_paths_in(tmp.path());
        let store = RealOwnedScheduleStore::open(&paths).unwrap();
        let target = tmp.path().join("elsewhere.timer");
        std::fs::write(&target, b"# Managed by crosstache (xv schedule).\n").unwrap();
        let link = tmp.path().join("linked.timer");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        match store.read_unit(&link).unwrap() {
            ArtifactState::Foreign(reason) => assert!(reason.contains("symlink"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_second_installer_cannot_take_the_install_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = manifest::test_paths_in(tmp.path());
        let first = RealOwnedScheduleStore::open(&paths).unwrap();
        let err = RealOwnedScheduleStore::open(&paths).expect_err("the lock must exclude");
        assert!(err.to_string().contains("already running"), "{err}");
        drop(first);
        // The lock inode itself is persistent, and releasing it lets the next
        // installer in.
        assert!(paths.install_lock_path().exists());
        RealOwnedScheduleStore::open(&paths).expect("the lock is released on drop");
    }

    #[test]
    fn recovery_snapshots_are_owner_private_and_hold_the_prior_bytes_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = manifest::test_paths_in(tmp.path());
        let mut store = RealOwnedScheduleStore::open(&paths).unwrap();
        let path = store
            .write_recovery_snapshot("20260910T041500Z-manifest.json", OLD_MANIFEST_BYTES)
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), OLD_MANIFEST_BYTES);
        assert!(path.starts_with(paths.recovery_dir()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(paths.recovery_dir())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn the_recovery_directory_is_not_created_when_nothing_needs_recovering() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = manifest::test_paths_in(tmp.path());
        let mut store = RealOwnedScheduleStore::open(&paths).unwrap();
        store.write_manifest(MANIFEST_BYTES).unwrap();
        assert!(
            !paths.recovery_dir().exists(),
            "recovery/ exists only as evidence of an incomplete rollback"
        );
    }

    #[test]
    fn recovery_snapshot_names_carry_no_caller_supplied_path_component() {
        for platform in platforms() {
            for path in unit_paths_for(platform, &unit_dir()) {
                let name = owned_artifact_name(&path);
                assert!(!name.contains('/') && !name.contains('\\'), "{name}");
                assert_ne!(name, "unit", "{path:?} has no fixed snapshot name");
            }
        }
    }
}
