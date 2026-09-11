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
    unregister_native_reporting, CommandRunner, DeregisterOutcome, Platform, RotationSchedule,
    ScheduleCommand, ScheduleInterval, UnitFile, UnitPaths, LAUNCHD_LABEL, SCHTASKS_NAME,
    SYSTEMD_UNIT,
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
    /// Where `manifest.json` lives, for reporting.
    fn manifest_path(&self) -> PathBuf;
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

/// Classify `manifest.json` at `path` without taking the install lock.
///
/// Shared with [`crate::schedule::ownership`] so `status` decides what is ours
/// by exactly the rules a reinstall uses to decide what it may replace. Both
/// must agree: a file `status` calls managed but `install` calls foreign would
/// send the user in a circle.
pub(crate) fn classify_owned_manifest(path: &Path) -> Result<ArtifactState> {
    Ok(match classify_path(path, "schedule manifest")? {
        ArtifactState::Owned(bytes) => classify_manifest_bytes(path, bytes),
        other => other,
    })
}

/// Classify an owned unit file at `path` without taking the install lock.
pub(crate) fn classify_owned_unit(path: &Path) -> Result<ArtifactState> {
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

impl OwnedScheduleStore for RealOwnedScheduleStore {
    fn read_manifest(&self) -> Result<ArtifactState> {
        classify_owned_manifest(&self.paths.manifest_path())
    }

    fn read_unit(&self, path: &Path) -> Result<ArtifactState> {
        classify_owned_unit(path)
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

    fn manifest_path(&self) -> PathBuf {
        self.paths.manifest_path()
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
            let properties = systemd_timer_properties(runner)?;
            Ok((systemd_is_registered(&properties), None))
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
            let properties = systemd_timer_properties(runner)?;
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
            // registered task itself has to answer for all four things. The
            // XML carries the cadence in locale-independent tag names and an
            // ISO-8601 `StartBoundary`, which the `/V /FO LIST` rendering does
            // not — it prints the schedule type and start time in the machine's
            // display language.
            let xml = runner.run("schtasks", &["/Query", "/TN", SCHTASKS_NAME, "/XML"])?;
            if !xml.ok() {
                return Err(verification_error(&format!(
                    "Task Scheduler does not report {SCHTASKS_NAME} after /Create"
                )));
            }
            verify_schtasks_cadence(&xml.stdout, plan.schedule.interval)?;

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

/// Check the registered task's trigger against the cadence we asked for.
///
/// Reads the task XML rather than `/Query /V`: tag names and the ISO-8601
/// `StartBoundary` are the same on every Windows display language, so this
/// cannot refuse a correct install because the host is not English.
///
/// `schtasks /XML` emits UTF-16LE; [`crate::schedule::decode_console_output`]
/// has already turned that into ordinary text by the time it reaches here, so
/// this matches on the tags directly.
fn verify_schtasks_cadence(xml: &str, interval: ScheduleInterval) -> Result<()> {
    let (expected_time, required): (String, Vec<&str>) = match interval {
        // `/SC HOURLY /ST 00:MM` registers a trigger that starts at :MM and
        // repeats every hour, so the repetition interval is what proves the
        // cadence, not the schedule kind.
        ScheduleInterval::Hourly { minute } => (
            format!("00:{minute:02}"),
            vec!["<Repetition", "<Interval>PT1H</Interval>"],
        ),
        ScheduleInterval::Daily { hour, minute } => (
            format!("{hour:02}:{minute:02}"),
            vec!["<CalendarTrigger", "<ScheduleByDay"],
        ),
        ScheduleInterval::Weekly {
            weekday,
            hour,
            minute,
        } => (
            format!("{hour:02}:{minute:02}"),
            vec![
                "<CalendarTrigger",
                "<ScheduleByWeek",
                "<DaysOfWeek",
                schtasks_xml_weekday(weekday),
            ],
        ),
    };

    for tag in required {
        if !xml.contains(tag) {
            return Err(verification_error(&format!(
                "the registered task's trigger is not the cadence this install asked for \
                 (no '{tag}' element in schtasks /Query /XML output)"
            )));
        }
    }

    let Some(start) = start_boundary_time_of_day(xml) else {
        return Err(verification_error(
            "the registered task has no readable <StartBoundary>, so its start time cannot be \
             confirmed",
        ));
    };
    if start != expected_time {
        return Err(verification_error(&format!(
            "the registered task starts at {start}, not the {expected_time} this install asked for"
        )));
    }
    Ok(())
}

/// `HH:MM` from the first `<StartBoundary>YYYY-MM-DDTHH:MM:SS…</StartBoundary>`.
fn start_boundary_time_of_day(xml: &str) -> Option<String> {
    let after = xml.split_once("<StartBoundary>")?.1;
    let value = after.split_once("</StartBoundary>")?.0.trim();
    let time = value.split_once('T')?.1;
    if time.len() < 5 {
        return None;
    }
    let (hhmm, _) = time.split_at(5);
    if hhmm.as_bytes()[2] == b':' {
        Some(hhmm.to_string())
    } else {
        None
    }
}

/// Task XML's empty day element, matched as an open prefix so both
/// `<Sunday/>` and `<Sunday />` count.
fn schtasks_xml_weekday(day: u32) -> &'static str {
    match day {
        0 => "<Sunday",
        1 => "<Monday",
        2 => "<Tuesday",
        3 => "<Wednesday",
        4 => "<Thursday",
        5 => "<Friday",
        _ => "<Saturday",
    }
}

/// The one systemd query both the prior-state probe and verification use.
///
/// They must not ask different questions. `is-active` alone reports an
/// enabled-but-inactive timer — a perfectly real schedule between firings, or
/// one the user stopped — as absent, and a rollback that believed that would
/// `disable --now` a schedule the user still had.
fn systemd_timer_properties(runner: &dyn CommandRunner) -> Result<HashMap<String, String>> {
    let out = runner.run(
        "systemctl",
        &[
            "--user",
            "show",
            &format!("{SYSTEMD_UNIT}.timer"),
            "--property=LoadState",
            "--property=ActiveState",
            "--property=UnitFileState",
        ],
    )?;
    Ok(parse_systemd_properties(&out.stdout))
}

/// Whether systemd currently has our timer at all. `show` exits 0 even for a
/// unit that does not exist, so the properties are the only real answer.
pub(crate) fn systemd_is_registered(properties: &HashMap<String, String>) -> bool {
    if properties.get("LoadState").map(String::as_str) != Some("loaded") {
        return false;
    }
    let enabled = properties
        .get("UnitFileState")
        .is_some_and(|state| state.starts_with("enabled"));
    let running = matches!(
        properties.get("ActiveState").map(String::as_str),
        Some("active") | Some("activating")
    );
    enabled || running
}

pub(crate) fn parse_systemd_properties(stdout: &str) -> HashMap<String, String> {
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

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

/// What uninstall removed, kept and could not do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UninstallReport {
    /// Owned unit files that were removed.
    pub(crate) removed_units: Vec<PathBuf>,
    /// Whether `manifest.json` was removed.
    pub(crate) removed_manifest: bool,
    /// Whether the scheduler actually deregistered something.
    pub(crate) deregistered: bool,
    /// Owned paths holding something `xv` did not write. Reported, retained,
    /// never touched.
    pub(crate) foreign: Vec<PathBuf>,
    /// A scheduler command that failed for a reason other than "no such job".
    /// Sanitized to the command name and its exit status.
    pub(crate) scheduler_error: Option<String>,
}

impl UninstallReport {
    /// Whether anything at all was removed.
    pub(crate) fn removed_anything(&self) -> bool {
        self.deregistered || self.removed_manifest || !self.removed_units.is_empty()
    }
}

/// Remove the schedule, and only the schedule.
///
/// The owned set is fixed by the design's "Files and ownership" table: the
/// platform's unit file(s) or task entry, plus `manifest.json`. Everything else
/// in the schedule state directory is somebody's evidence — `last-run.json` is
/// the record of what the last sweep did, `run.lock` and `install.lock` are
/// lock *inodes* that other processes may be holding right now, `recovery/`
/// holds the bytes of an install that could not be undone, and the log is what
/// a person reads to find out why a rotation failed. None of it is recreated by
/// reinstalling, so uninstall must not take it.
///
/// A foreign or symlinked artifact at an owned path is reported and left
/// exactly as it is: uninstall removes files `xv` wrote, and it decides that
/// from the bytes, not from the path. Classification happens *before* any
/// scheduler command runs, and a single foreign artifact suppresses
/// deregistration entirely — a registration is state too, and tearing it down
/// while calling the file it points at "retained" would not be retaining
/// anything.
///
/// Absence is success — teardown scripts run this against hosts that never had
/// a schedule — but a *scheduler failure* is not absence and is carried back in
/// [`UninstallReport::scheduler_error`] for the caller to report.
pub(crate) fn uninstall_owned(
    platform: Platform,
    unit_paths: &UnitPaths,
    store: &mut dyn OwnedScheduleStore,
    runner: &dyn CommandRunner,
) -> Result<UninstallReport> {
    let mut report = UninstallReport::default();

    // Classify BEFORE touching the scheduler. Deregistration is not reversible
    // from here, so "reported and retained" has to mean the registration too:
    // tearing down a job whose unit file somebody else wrote, and then
    // reporting that file as retained, is the opposite of leaving foreign
    // content alone. So every owned path is read first, and a single foreign
    // artifact anywhere in the owned set means the scheduler is not touched at
    // all. (Schtasks has no unit file to classify, so only its manifest can
    // hold foreign content.)
    let mut unit_states = Vec::new();
    for path in unit_paths_for(platform, unit_paths) {
        let state = store.read_unit(&path)?;
        unit_states.push((path, state));
    }
    let manifest_state = store.read_manifest()?;

    let any_foreign = unit_states
        .iter()
        .any(|(_, state)| matches!(state, ArtifactState::Foreign(_)))
        || matches!(manifest_state, ArtifactState::Foreign(_));

    if !any_foreign {
        // Deregister before removing files: a unit file removed while the
        // scheduler still holds the job leaves a registration pointing at
        // nothing.
        match unregister_native_reporting(platform, runner)? {
            DeregisterOutcome::Removed => report.deregistered = true,
            DeregisterOutcome::Absent => {}
            DeregisterOutcome::Failed(detail) => report.scheduler_error = Some(detail),
        }
    }

    for (path, state) in unit_states {
        match state {
            ArtifactState::Absent => {}
            ArtifactState::Owned(_) => {
                store.remove_unit(&path)?;
                report.removed_units.push(path);
            }
            ArtifactState::Foreign(_) => report.foreign.push(path),
        }
    }

    match manifest_state {
        ArtifactState::Absent => {}
        ArtifactState::Owned(_) => {
            store.remove_manifest()?;
            report.removed_manifest = true;
        }
        ArtifactState::Foreign(_) => report.foreign.push(store.manifest_path()),
    }

    if platform == Platform::Systemd && !report.removed_units.is_empty() {
        // Best effort: the units are gone either way, and a reload that fails
        // does not make them come back.
        let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
    }

    Ok(report)
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
    use crate::schedule::{fixture_abs, CommandOutput};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    // -- fixtures -----------------------------------------------------------

    /// Taking `install.lock` must work under a state root that came out of
    /// `fs::canonicalize`. On Windows that is a verbatim path (`\\?\C:\...`),
    /// and the private-file helper used to stat every path component including
    /// the bare volume prefix — which opens the volume device and fails with
    /// `ERROR_INVALID_FUNCTION` ("Incorrect function. (os error 1)"), so
    /// `xv schedule uninstall` refused before it classified anything.
    #[test]
    fn the_store_opens_under_a_canonicalized_state_root() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let paths = crate::schedule::manifest::test_paths_in(&canonical);

        let store =
            RealOwnedScheduleStore::open(&paths).expect("the install lock must be takeable");

        assert!(paths.install_lock_path().exists());
        drop(store);
    }

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

        fn manifest_path(&self) -> PathBuf {
            self.manifest_path.clone()
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
        /// systemd `ActiveState` of the *prior* timer, before this install
        /// registers anything. An enabled timer between firings is `inactive`.
        prior_active: String,
        /// systemd `UnitFileState` of the prior timer.
        prior_unit_file_state: String,
        /// The trigger element Task Scheduler reports for the installed task.
        reports_trigger: String,
        /// The `<StartBoundary>` Task Scheduler reports for it.
        reports_start_boundary: String,
        /// Emit `/Query /XML` output the way real `schtasks` does — UTF-16LE
        /// with a BOM, decoded by the runner seam.
        emits_utf16_xml: bool,
        /// stderr a failing call emits. Uninstall reads it to tell "no such
        /// job" apart from a scheduler that is actually broken.
        failure_stderr: String,
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
                prior_active: "active".to_string(),
                prior_unit_file_state: "enabled".to_string(),
                reports_trigger: "<CalendarTrigger>…<ScheduleByDay><DaysInterval>1</DaysInterval>\
                                  </ScheduleByDay></CalendarTrigger>"
                    .to_string(),
                reports_start_boundary: "2026-09-10T03:30:00".to_string(),
                emits_utf16_xml: false,
                failure_stderr: "not found".to_string(),
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
        /// The task XML `schtasks /Query /XML` prints for the installed task.
        fn task_xml(&self) -> String {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task><Triggers>{}\
                 <StartBoundary>{}</StartBoundary></Triggers><Actions><Exec><Command>{}\
                 </Command></Exec></Actions></Task>\n",
                self.reports_trigger, self.reports_start_boundary, self.reports_binary
            )
        }
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
                    stderr: self.failure_stderr.clone(),
                });
            }

            // How many calls of this shape have already happened decides
            // whether we are answering "before" or "after" registration.
            let registration_happened = self.calls.lock().unwrap().iter().any(|c| {
                c.contains("bootstrap") || c.contains("enable --now") || c.contains("/Create")
            });
            let already_registered = self.prior_registered || registration_happened;

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
                ("systemctl", j) if j.contains("show") && j.contains("timer") => (
                    0,
                    if registration_happened {
                        format!(
                            "LoadState={}\nActiveState={}\nUnitFileState=enabled\n",
                            self.reports_load, self.reports_active
                        )
                    } else if self.prior_registered {
                        format!(
                            "LoadState=loaded\nActiveState={}\nUnitFileState={}\n",
                            self.prior_active, self.prior_unit_file_state
                        )
                    } else {
                        "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n".to_string()
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
                    if self.emits_utf16_xml {
                        (
                            0,
                            crate::schedule::decode_console_output(&utf16le_with_bom(
                                &self.task_xml(),
                            )),
                        )
                    } else if registration_happened {
                        (0, self.task_xml())
                    } else if self.prior_registered {
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
                    self.failure_stderr.clone()
                },
            })
        }
    }

    // -- uninstall ----------------------------------------------------------

    /// A store holding a complete prior schedule *plus* everything uninstall
    /// must leave behind.
    fn store_with_schedule_and_evidence(platform: Platform) -> (FakeStore, Vec<PathBuf>) {
        let mut store = FakeStore::with_prior(platform);
        let dir = manifest_path().parent().unwrap().to_path_buf();
        let retained = vec![
            dir.join("last-run.json"),
            dir.join("run.lock"),
            dir.join("install.lock"),
            dir.join("recovery").join("20260909T000000Z-manifest.json"),
            dir.join("notes-the-user-left.txt"),
            schedule().log_path,
        ];
        for (n, path) in retained.iter().enumerate() {
            store
                .files
                .insert(path.clone(), format!("evidence {n}").into_bytes());
        }
        (store, retained)
    }

    /// A scheduler that answers "no such job" the way each platform does.
    fn runner_with_nothing_registered() -> FakeRunner {
        FakeRunner {
            fail_containing: Some(String::new()),
            failure_stderr: "No such process".to_string(),
            ..FakeRunner::default()
        }
    }

    #[test]
    fn uninstall_removes_the_owned_artifacts_and_keeps_every_other_file() {
        for platform in platforms() {
            let (mut store, retained) = store_with_schedule_and_evidence(platform);
            let runner = FakeRunner::with_prior();

            let report = uninstall_owned(platform, &unit_dir(), &mut store, &runner)
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(report.removed_anything(), "{platform:?}");
            assert!(report.removed_manifest, "{platform:?}");
            assert!(report.deregistered, "{platform:?}");
            assert_eq!(
                report.removed_units,
                unit_paths_for(platform, &unit_dir()),
                "{platform:?}"
            );
            assert!(store.get(&manifest_path()).is_none(), "{platform:?}");
            for unit in unit_paths_for(platform, &unit_dir()) {
                assert!(store.get(&unit).is_none(), "{platform:?}: {unit:?}");
            }
            for (n, path) in retained.iter().enumerate() {
                assert_eq!(
                    store.get(path),
                    Some(&format!("evidence {n}").into_bytes()),
                    "{platform:?}: uninstall took {path:?}"
                );
            }
            assert!(report.foreign.is_empty(), "{platform:?}");
            assert!(report.scheduler_error.is_none(), "{platform:?}");
        }
    }

    #[test]
    fn uninstall_retains_and_reports_a_foreign_artifact() {
        let platform = Platform::Systemd;
        let plan = plan(platform);
        let mut store = FakeStore::with_prior(platform);
        let victim = plan.units[0].path.clone();
        store
            .files
            .insert(victim.clone(), b"# my own timer\n".to_vec());
        store
            .foreign
            .insert(victim.clone(), "not written by xv".to_string());
        let runner = FakeRunner::with_prior();

        let report = uninstall_owned(platform, &unit_dir(), &mut store, &runner).unwrap();

        assert_eq!(report.foreign, vec![victim.clone()]);
        assert_eq!(
            store.get(&victim),
            Some(&b"# my own timer\n".to_vec()),
            "uninstall touched a file xv did not write"
        );
        assert!(!report.removed_units.contains(&victim));
        // The other, genuinely owned unit is still removed.
        assert!(store.get(&plan.units[1].path).is_none());
    }

    /// Retention covers the registration, not just the bytes: classification
    /// runs before any scheduler command, so a foreign unit at an owned path
    /// means `launchctl bootout` / `systemctl --user disable --now` never runs.
    /// Reporting "Retained" after tearing the job down retains nothing.
    #[test]
    fn uninstall_does_not_deregister_when_an_owned_path_is_foreign() {
        for platform in [Platform::Launchd, Platform::Systemd] {
            let plan = plan(platform);
            let mut store = FakeStore::with_prior(platform);
            let victim = plan.units[0].path.clone();
            store
                .files
                .insert(victim.clone(), b"# my own job\n".to_vec());
            store
                .foreign
                .insert(victim.clone(), "not written by xv".to_string());
            let runner = FakeRunner::with_prior();

            let report = uninstall_owned(platform, &unit_dir(), &mut store, &runner)
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(
                !report.deregistered,
                "{platform:?}: reported a deregistration it must not have performed"
            );
            let calls = runner.calls.lock().unwrap().clone();
            let deregistration = match platform {
                Platform::Launchd => "bootout",
                Platform::Systemd => "disable",
                Platform::Schtasks => "/Delete",
            };
            assert!(
                !calls.iter().any(|call| call.contains(deregistration)),
                "{platform:?}: tore down the registration of a foreign unit: {calls:?}"
            );
            assert_eq!(report.foreign, vec![victim], "{platform:?}");
        }
    }

    /// The same run with nothing foreign still deregisters — the guard above
    /// must be about foreign content, not about uninstall having stopped
    /// calling the scheduler.
    #[test]
    fn uninstall_still_deregisters_when_every_owned_path_is_ours() {
        for platform in [Platform::Launchd, Platform::Systemd] {
            let mut store = FakeStore::with_prior(platform);
            let runner = FakeRunner::with_prior();

            let report = uninstall_owned(platform, &unit_dir(), &mut store, &runner)
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(report.deregistered, "{platform:?}");
            assert!(report.foreign.is_empty(), "{platform:?}");
        }
    }

    #[test]
    fn uninstall_converges_on_absent_when_nothing_is_installed() {
        for platform in platforms() {
            let mut store = FakeStore::new();
            let runner = runner_with_nothing_registered();

            let report = uninstall_owned(platform, &unit_dir(), &mut store, &runner)
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(!report.removed_anything(), "{platform:?}");
            assert!(report.scheduler_error.is_none(), "{platform:?}");
            assert!(report.foreign.is_empty(), "{platform:?}");
        }
    }

    #[test]
    fn uninstall_reports_a_broken_scheduler_instead_of_claiming_absence() {
        let mut store = FakeStore::new();
        let runner = FakeRunner {
            fail_containing: Some(String::new()),
            failure_stderr: "Bad request.".to_string(),
            ..FakeRunner::default()
        };
        let report = uninstall_owned(Platform::Launchd, &unit_dir(), &mut store, &runner).unwrap();
        assert_eq!(
            report.scheduler_error,
            Some("launchctl bootout failed (exit 1)".to_string())
        );
        assert!(!report.deregistered);
    }

    #[test]
    fn uninstall_removes_a_legacy_unit_the_way_it_always_did() {
        let platform = Platform::Systemd;
        let mut store = FakeStore::new();
        for (path, bytes) in legacy_units(platform) {
            store.files.insert(path, bytes);
        }
        let runner = FakeRunner::with_prior();

        let report = uninstall_owned(platform, &unit_dir(), &mut store, &runner).unwrap();

        assert_eq!(report.removed_units, unit_paths_for(platform, &unit_dir()));
        assert!(store.files.is_empty(), "{:?}", store.files);
        assert!(!report.removed_manifest, "there was no manifest to remove");
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

    /// The units an older `xv` installed: the same renderer, the legacy
    /// command. Used as the *prior* state a reinstall has to replace.
    fn legacy_units(platform: Platform) -> Vec<(PathBuf, Vec<u8>)> {
        let legacy = RotationSchedule {
            command: ScheduleCommand::LegacyRotateDue {
                vault: Some("payments-production".into()),
            },
            ..schedule()
        };
        render(platform, &legacy, &unit_dir())
            .into_iter()
            .map(|unit| (unit.path, unit.contents.into_bytes()))
            .collect()
    }

    #[test]
    fn a_reinstall_replaces_a_legacy_unit_that_has_no_manifest() {
        // The supported migration: an explicit `xv schedule install` over a
        // pre-manifest schedule. The legacy unit carries the managed marker,
        // so it is a *prior owned* artifact — replaced through the ordinary
        // transaction, never adopted and never migrated behind the user's back.
        for platform in [Platform::Launchd, Platform::Systemd] {
            let plan = plan(platform);
            let mut store = FakeStore::new();
            for (path, bytes) in legacy_units(platform) {
                store.files.insert(path, bytes);
            }
            let runner = FakeRunner::with_prior();

            let report = install_transactional(&plan, &mut store, &runner, now())
                .unwrap_or_else(|e| panic!("{platform:?}: {e}"));

            assert!(
                report.replaced_prior_schedule,
                "{platform:?}: a legacy unit is a prior schedule"
            );
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
                    "{platform:?}: the legacy unit was not replaced"
                );
                assert!(
                    !store
                        .get(&unit.path)
                        .map(|b| String::from_utf8_lossy(b).contains("--due"))
                        .unwrap_or(false),
                    "{platform:?}: the legacy command survived the reinstall"
                );
            }
        }
    }

    #[test]
    fn a_failed_reinstall_over_a_legacy_unit_puts_the_legacy_bytes_back() {
        // Replacing a legacy schedule must be no more destructive than
        // replacing a pinned one: if the new install cannot be registered, the
        // user is left with the schedule they had.
        let platform = Platform::Systemd;
        let plan = plan(platform);
        let mut store = FakeStore::new();
        for (path, bytes) in legacy_units(platform) {
            store.files.insert(path, bytes);
        }
        // Verification fails rather than registration: the rollback's own
        // re-registration must be able to succeed, or this would test the
        // incomplete-rollback path instead.
        let runner = FakeRunner {
            reports_active: "failed".to_string(),
            ..FakeRunner::with_prior()
        };

        let err = install_transactional(&plan, &mut store, &runner, now()).expect_err("must fail");
        assert!(!err.to_string().contains("also failed"), "{err}");
        for (path, bytes) in legacy_units(platform) {
            assert_eq!(
                store.get(&path),
                Some(&bytes),
                "the legacy unit was not restored byte for byte"
            );
        }
        assert!(
            store.get(&manifest_path()).is_none(),
            "a manifest survived a rolled-back install over a legacy schedule"
        );
        assert!(store.snapshots.is_empty());
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

    #[test]
    fn schtasks_verification_accepts_the_cadence_it_registered() {
        // Daily is the fixture; the trigger and start time the fake reports
        // are the ones `/SC DAILY /ST 03:30` produces.
        let plan = plan(Platform::Schtasks);
        let mut store = FakeStore::new();
        install_transactional(&plan, &mut store, &FakeRunner::default(), now()).unwrap();
        assert_eq!(store.get(&manifest_path()), Some(&MANIFEST_BYTES.to_vec()));
    }

    #[test]
    fn schtasks_verification_rejects_a_task_that_starts_at_another_time() {
        let plan = plan(Platform::Schtasks);
        let mut store = FakeStore::new();
        let runner = FakeRunner {
            reports_start_boundary: "2026-09-10T23:30:00".to_string(),
            ..FakeRunner::default()
        };
        let err =
            install_transactional(&plan, &mut store, &runner, now()).expect_err("must refuse");
        assert!(err.to_string().contains("starts at 23:30"), "{err}");
        assert!(store.files.is_empty(), "{:?}", store.files);
    }

    #[test]
    fn schtasks_verification_rejects_a_task_with_another_kind_of_trigger() {
        let plan = plan(Platform::Schtasks);
        let mut store = FakeStore::new();
        // A weekly trigger where a daily one was asked for: the job would fire
        // once a week and nobody would notice until a rotation was six days
        // late.
        let runner = FakeRunner {
            reports_trigger: "<CalendarTrigger><ScheduleByWeek><DaysOfWeek><Sunday/></DaysOfWeek>\
                              </ScheduleByWeek></CalendarTrigger>"
                .to_string(),
            ..FakeRunner::default()
        };
        let err =
            install_transactional(&plan, &mut store, &runner, now()).expect_err("must refuse");
        assert!(err.to_string().contains("ScheduleByDay"), "{err}");
        assert!(store.files.is_empty(), "{:?}", store.files);
    }

    /// The bytes `schtasks /Query /XML` actually writes: UTF-16LE with a BOM.
    fn utf16le_with_bom(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn schtasks_cadence_is_read_from_utf16_xml_and_matched_per_interval() {
        // schtasks emits UTF-16LE; the runner decodes it before it gets here.
        let decoded = |xml: &str| -> String {
            crate::schedule::decode_console_output(&utf16le_with_bom(xml))
        };

        let hourly = "<Task><Triggers><CalendarTrigger><Repetition><Interval>PT1H</Interval>\
                      </Repetition><StartBoundary>2026-09-10T00:15:00</StartBoundary>\
                      </CalendarTrigger></Triggers></Task>";
        verify_schtasks_cadence(&decoded(hourly), ScheduleInterval::Hourly { minute: 15 })
            .expect("an hourly repetition starting at :15 is what /SC HOURLY /ST 00:15 makes");
        assert!(
            verify_schtasks_cadence(hourly, ScheduleInterval::Hourly { minute: 45 }).is_err(),
            "a different minute must not pass"
        );

        let weekly = "<Task><Triggers><CalendarTrigger><ScheduleByWeek><DaysOfWeek><Sunday/>\
                      </DaysOfWeek></ScheduleByWeek><StartBoundary>2026-09-13T04:00:00\
                      </StartBoundary></CalendarTrigger></Triggers></Task>";
        verify_schtasks_cadence(
            weekly,
            ScheduleInterval::Weekly {
                weekday: 0,
                hour: 4,
                minute: 0,
            },
        )
        .expect("Sunday at 04:00");
        assert!(
            verify_schtasks_cadence(
                weekly,
                ScheduleInterval::Weekly {
                    weekday: 3,
                    hour: 4,
                    minute: 0
                }
            )
            .is_err(),
            "a different day must not pass"
        );

        // No trigger at all, and an unreadable boundary.
        assert!(verify_schtasks_cadence(
            "<Task><Triggers><LogonTrigger/></Triggers></Task>",
            ScheduleInterval::Daily {
                hour: 3,
                minute: 30
            }
        )
        .is_err());
        let err = verify_schtasks_cadence(
            "<Task><CalendarTrigger><ScheduleByDay/></CalendarTrigger></Task>",
            ScheduleInterval::Daily {
                hour: 3,
                minute: 30,
            },
        )
        .expect_err("no StartBoundary");
        assert!(err.to_string().contains("StartBoundary"), "{err}");
    }

    #[test]
    fn the_prior_task_definition_is_stored_as_clean_utf8_xml() {
        // The rollback hands this straight back to `schtasks /Create /XML`. If
        // the UTF-16LE that Task Scheduler emits were stored as lossily
        // decoded bytes, the restored file would be NUL-riddled and the undo
        // would fail exactly when it is needed most.
        let runner = FakeRunner {
            emits_utf16_xml: true,
            ..FakeRunner::default()
        };
        let (registered, definition) =
            capture_registration(Platform::Schtasks, &runner).expect("query must succeed");
        assert!(registered);
        let definition = definition.expect("Task Scheduler's definition must be captured");
        assert!(
            !definition.contains(&0u8),
            "the stored definition still carries UTF-16 NUL bytes"
        );
        assert_eq!(
            String::from_utf8(definition).expect("clean UTF-8"),
            runner.task_xml()
        );
    }

    #[test]
    fn an_enabled_but_inactive_prior_timer_still_counts_as_a_schedule() {
        // A systemd timer between firings is `inactive`; `is-active` alone
        // would call that "no schedule" and the rollback would disable a
        // schedule the user still had.
        let plan = plan(Platform::Systemd);
        let mut store = FakeStore::with_prior(Platform::Systemd);
        let runner = FakeRunner {
            prior_active: "inactive".to_string(),
            prior_unit_file_state: "enabled".to_string(),
            ..FakeRunner::with_prior()
        };
        // Fail a unit write so the transaction rolls back with the scheduler
        // itself healthy.
        let mut failing = FakeStore::with_prior(Platform::Systemd).failing(StoreOp::WriteUnit, 1);

        let err =
            install_transactional(&plan, &mut failing, &runner, now()).expect_err("must fail");
        assert!(!err.to_string().contains("also failed"), "{err}");
        let flat = runner.flat();
        assert!(
            !flat.iter().any(|c| c.contains("disable --now")),
            "the previous schedule was disabled instead of restored: {flat:?}"
        );
        assert!(
            flat.iter().filter(|c| c.contains("enable --now")).count() >= 1,
            "the previous timer was not re-enabled: {flat:?}"
        );
        // And the untouched control: an absent prior timer is still absent.
        let absent = FakeRunner::default();
        let mut store2 = FakeStore::new().failing(StoreOp::WriteUnit, 1);
        let _ = install_transactional(&plan, &mut store2, &absent, now());
        assert!(
            absent.flat().iter().any(|c| c.contains("disable --now")),
            "{:?}",
            absent.flat()
        );
        let _ = &mut store;
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
