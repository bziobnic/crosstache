//! `last-run.json` — what the scheduled runner did, and the lock that keeps
//! two runners from doing it at once.
//!
//! ## Why a result file at all
//!
//! A scheduled sweep runs unattended. Nobody watches its exit code, and its
//! log is a growing text file nothing parses. `xv schedule status` has to be
//! able to answer "did last night's rotation work?" without contacting a
//! provider, so the runner writes one small, versioned, machine-readable
//! record of every run.
//!
//! ## What may appear in it
//!
//! Counts, a fixed state token, a stable diagnostic code, and a sanitized
//! message. Never a secret name, a secret value, a provider error body, a
//! token, or command output — those belong (under their existing security
//! model) to the human-facing log, not to a file `status` prints and other
//! tools may ship elsewhere. Per-secret failures contribute only to counts.
//!
//! ## Why the outcome is bound to a manifest digest
//!
//! `uninstall` retains `last-run.json` and a later `install` writes a new
//! manifest. Without a binding, the retained record would be presented as the
//! current installation's last run, which it is not. `manifest_digest` is the
//! SHA-256 of the exact manifest bytes the run parsed, so status can label a
//! mismatched record `previous install` instead of lying about it.
//!
//! ## Why the lock is a separate persistent inode
//!
//! `run.lock` is never deleted — a lock file removed while another process
//! holds it stops excluding anything, because the next process creates a new
//! inode and both "hold the lock". `install.lock` follows the same rule; this
//! module reuses its opening helper verbatim.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CrosstacheError, Result};
use crate::schedule::manifest::{
    reject_if_symlink, validate_digest, ScheduleStatePaths, SCHEDULE_ID,
};
use crate::utils::helpers::{
    atomic_write_file_no_follow, create_private_dir, open_private_lock_file_no_follow,
    read_file_no_follow,
};

/// Outcome files are capped at this size before they are ever parsed, matching
/// the manifest's cap.
const MAX_OUTCOME_BYTES: usize = 64 * 1024;

/// A diagnostic message is limited to this many Unicode **scalar values** —
/// not bytes, so a multi-byte message is never cut mid-character. Writers
/// truncate; the loader refuses a longer one rather than silently accepting a
/// file some other process grew.
pub(crate) const MAX_DIAGNOSTIC_SCALARS: usize = 1024;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// The `state` field of a scheduled run's outcome.
///
/// `running` is written the moment the lock is taken and replaced on every
/// normal return path. A record still reading `running` therefore means the
/// process died between the two writes — which is exactly what status needs
/// to distinguish a live run (lock held) from an interrupted one (lock free).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Success,
    PartialFailure,
    Failed,
    RefusedDrift,
}

impl RunState {
    /// The literal written to `last-run.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Success => "success",
            RunState::PartialFailure => "partial_failure",
            RunState::Failed => "failed",
            RunState::RefusedDrift => "refused_drift",
        }
    }

    /// Every state except [`RunState::Running`]: the run has returned and the
    /// record must carry a finish time and an exit code.
    pub fn is_terminal(self) -> bool {
        !matches!(self, RunState::Running)
    }
}

/// Aggregate counts for one sweep. Names never appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSummary {
    pub policy_managed: u64,
    pub due: u64,
    pub rotated: u64,
    pub failed: u64,
}

/// A redacted diagnostic: a stable code from a closed set plus a sanitized
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDiagnostic {
    pub code: String,
    pub message: String,
}

impl RunDiagnostic {
    /// Build a diagnostic, truncating the message to
    /// [`MAX_DIAGNOSTIC_SCALARS`] Unicode scalar values.
    ///
    /// Truncation is by `chars()`, not by bytes: a byte-sliced message could
    /// split a multi-byte scalar and produce a file that is not valid UTF-8 —
    /// or, worse, panic on the slice boundary while writing an unattended
    /// run's only record.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let message: String = message.into();
        Self {
            code: code.into(),
            message: message.chars().take(MAX_DIAGNOSTIC_SCALARS).collect(),
        }
    }
}

/// Version 1 of `last-run.json`.
///
/// Field order matches the design's JSON exactly, because the file is read by
/// people as often as by `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutcomeV1 {
    pub schema_version: u32,
    pub schedule_id: String,
    /// SHA-256 of the exact manifest bytes this run parsed.
    pub manifest_digest: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub state: RunState,
    pub exit_code: Option<i32>,
    pub summary: Option<RunSummary>,
    pub diagnostic: Option<RunDiagnostic>,
}

/// A loaded, version-dispatched outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    V1(RunOutcomeV1),
}

/// Minimal shape used only to peek `schema_version` before committing to a
/// concrete version's strict (`deny_unknown_fields`) deserialization.
#[derive(Debug, Deserialize)]
struct SchemaVersionPeek {
    schema_version: u32,
}

impl RunOutcomeV1 {
    /// The `running` record written immediately after the lock is taken.
    pub fn running(manifest_digest: &str, started_at: &str) -> Self {
        Self {
            schema_version: 1,
            schedule_id: SCHEDULE_ID.to_string(),
            manifest_digest: manifest_digest.to_string(),
            started_at: started_at.to_string(),
            finished_at: None,
            state: RunState::Running,
            exit_code: None,
            summary: None,
            diagnostic: None,
        }
    }
}

/// RFC 3339, UTC, second resolution — the spelling every timestamp in this
/// module and the manifest uses.
pub fn now_rfc3339_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn validate_timestamp(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(CrosstacheError::config(format!(
            "schedule outcome field '{field}' must not be empty"
        )));
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value).map_err(|e| {
        CrosstacheError::config(format!(
            "schedule outcome field '{field}' must be an RFC 3339 timestamp: {value} ({e})"
        ))
    })?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(CrosstacheError::config(format!(
            "schedule outcome field '{field}' must be in UTC: {value}"
        )));
    }
    Ok(())
}

/// Validate a v1 outcome.
///
/// Run on every load *and* before every write, so a record this build would
/// refuse to read is never written in the first place.
pub fn validate_v1(outcome: &RunOutcomeV1) -> Result<()> {
    if outcome.schema_version != 1 {
        return Err(CrosstacheError::config(format!(
            "schedule outcome field 'schema_version' must be 1: {}",
            outcome.schema_version
        )));
    }
    if outcome.schedule_id != SCHEDULE_ID {
        return Err(CrosstacheError::config(format!(
            "schedule outcome field 'schedule_id' must be '{SCHEDULE_ID}': {}",
            outcome.schedule_id
        )));
    }
    validate_digest("manifest_digest", &outcome.manifest_digest)?;
    validate_timestamp("started_at", &outcome.started_at)?;
    if let Some(finished_at) = &outcome.finished_at {
        validate_timestamp("finished_at", finished_at)?;
    }

    if outcome.state.is_terminal() {
        if outcome.finished_at.is_none() {
            return Err(CrosstacheError::config(
                "schedule outcome field 'finished_at' must be set once the run has finished"
                    .to_string(),
            ));
        }
        if outcome.exit_code.is_none() {
            return Err(CrosstacheError::config(
                "schedule outcome field 'exit_code' must be set once the run has finished"
                    .to_string(),
            ));
        }
    } else {
        // A `running` record makes exactly one claim: a run started. Anything
        // else in it would be a result that does not exist yet.
        if outcome.finished_at.is_some()
            || outcome.exit_code.is_some()
            || outcome.summary.is_some()
            || outcome.diagnostic.is_some()
        {
            return Err(CrosstacheError::config(
                "schedule outcome fields 'finished_at', 'exit_code', 'summary' and 'diagnostic' \
                 must be null while the state is 'running'"
                    .to_string(),
            ));
        }
    }

    if let Some(diagnostic) = &outcome.diagnostic {
        if diagnostic.code.is_empty() {
            return Err(CrosstacheError::config(
                "schedule outcome field 'diagnostic.code' must not be empty".to_string(),
            ));
        }
        let scalars = diagnostic.message.chars().count();
        if scalars > MAX_DIAGNOSTIC_SCALARS {
            return Err(CrosstacheError::config(format!(
                "schedule outcome field 'diagnostic.message' must be at most \
                 {MAX_DIAGNOSTIC_SCALARS} Unicode scalar values: {scalars}"
            )));
        }
    }

    Ok(())
}

/// Deterministic pretty JSON bytes for the on-disk outcome, terminated with a
/// trailing newline.
pub fn serialize_outcome(outcome: &RunOutcomeV1) -> Vec<u8> {
    let mut bytes =
        serde_json::to_vec_pretty(outcome).expect("schedule outcome is always valid JSON");
    bytes.push(b'\n');
    bytes
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

fn reject_symlink_components(paths: &ScheduleStatePaths) -> Result<()> {
    reject_if_symlink(paths.root())?;
    reject_if_symlink(&paths.last_run_path())?;
    Ok(())
}

fn read_outcome_bytes(path: &Path, paths: &ScheduleStatePaths) -> Result<Vec<u8>> {
    reject_symlink_components(paths)?;

    // Size-check via metadata before reading, so a bloated file is refused
    // without pulling arbitrary bytes into memory first.
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to inspect schedule run outcome '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.len() > MAX_OUTCOME_BYTES as u64 {
        return Err(CrosstacheError::config(format!(
            "schedule run outcome '{}' exceeds the {MAX_OUTCOME_BYTES} byte limit",
            path.display()
        )));
    }

    let bytes = read_file_no_follow(path)?;
    if bytes.len() > MAX_OUTCOME_BYTES {
        return Err(CrosstacheError::config(format!(
            "schedule run outcome '{}' exceeds the {MAX_OUTCOME_BYTES} byte limit",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Load and validate `last-run.json`, dispatching on its `schema_version`.
///
/// `Ok(None)` means no run has been recorded (or the record was removed);
/// that is a normal state, not an error. An unknown `schema_version` produces
/// a targeted error rather than a generic deserialization failure.
pub fn load_outcome(paths: &ScheduleStatePaths) -> Result<Option<RunOutcome>> {
    let path = paths.last_run_path();
    // Distinguish absent from unreadable *before* the symlink/size checks, so
    // "no run yet" never surfaces as a scary refusal.
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CrosstacheError::config(format!(
                "Failed to inspect schedule run outcome '{}': {error}",
                path.display()
            )))
        }
        Ok(_) => {}
    }

    let bytes = read_outcome_bytes(&path, paths)?;

    let peek: SchemaVersionPeek = serde_json::from_slice(&bytes).map_err(|error| {
        CrosstacheError::config(format!(
            "schedule run outcome '{}' is not valid JSON: {error}",
            path.display()
        ))
    })?;

    match peek.schema_version {
        1 => {
            let outcome: RunOutcomeV1 = serde_json::from_slice(&bytes).map_err(|error| {
                CrosstacheError::config(format!(
                    "schedule run outcome '{}' does not match schema version 1: {error}",
                    path.display()
                ))
            })?;
            validate_v1(&outcome)?;
            Ok(Some(RunOutcome::V1(outcome)))
        }
        other => Err(CrosstacheError::config(format!(
            "schedule run outcome '{}' has schema_version {other}, which this version of xv does \
             not support; it will be replaced by the next completed run",
            path.display()
        ))),
    }
}

/// Validate and atomically write `last-run.json`, creating the owning private
/// directory (`0700` on Unix) if needed. The file is written private (`0600`
/// on Unix).
pub fn write_outcome_atomic(paths: &ScheduleStatePaths, outcome: &RunOutcomeV1) -> Result<()> {
    validate_v1(outcome)?;
    reject_if_symlink(paths.root())?;
    create_private_dir(paths.root()).map_err(|error| {
        CrosstacheError::config(format!(
            "Failed to create schedule state directory '{}': {error}",
            paths.root().display()
        ))
    })?;
    atomic_write_file_no_follow(&paths.last_run_path(), &serialize_outcome(outcome), true)
}

// ---------------------------------------------------------------------------
// run.lock
// ---------------------------------------------------------------------------

/// How many times the runner re-attempts a *contended* exclusive acquire
/// before concluding another runner owns the sweep.
///
/// `xv schedule status` takes the lock for an instant to tell a live run from
/// an interrupted one. Without a retry, a health check that happens to run on
/// the same cadence as the job could make the firing skip until the next
/// interval. [`RUN_LOCK_ATTEMPTS`] × [`RUN_LOCK_RETRY_DELAY`] ≈ 2s, which is
/// far longer than any probe holds it and far shorter than any cadence.
const RUN_LOCK_ATTEMPTS: u32 = 10;
const RUN_LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// What a non-blocking lock attempt meant.
#[derive(Debug)]
pub(crate) enum LockAttempt {
    Acquired,
    /// Somebody else holds it. Not an error.
    Contended,
    /// The lock could not be evaluated at all — `ENOLCK`, `EIO`, a Windows
    /// quota or permission failure. A run that cannot take the lock has not
    /// been excluded by anything; it has failed.
    Failed(std::io::Error),
}

/// Whether a failed non-blocking lock attempt means "another process holds
/// it".
///
/// Unix reports contention as `EWOULDBLOCK`/`EAGAIN`; Windows reports
/// `ERROR_LOCK_VIOLATION`, which does not map to `WouldBlock`. `fs2` exposes
/// the platform's own contention error, so the portable test compares raw OS
/// codes against it.
pub(crate) fn is_lock_contention(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    match (
        error.raw_os_error(),
        fs2::lock_contended_error().raw_os_error(),
    ) {
        (Some(actual), Some(contended)) => actual == contended,
        _ => false,
    }
}

/// Map a raw `try_lock_*` result onto the three outcomes the callers act on.
pub(crate) fn classify_lock(result: std::io::Result<()>) -> LockAttempt {
    match result {
        Ok(()) => LockAttempt::Acquired,
        Err(error) if is_lock_contention(&error) => LockAttempt::Contended,
        Err(error) => LockAttempt::Failed(error),
    }
}

/// The error a lock failure reports: it names `run.lock` and the OS error, so
/// the run that failed is diagnosable from the scheduler's own log.
fn lock_failure(path: &Path, error: &std::io::Error) -> CrosstacheError {
    CrosstacheError::config(format!(
        "Failed to take the schedule run lock '{}': {error}",
        path.display()
    ))
}

/// An exclusive hold on `run.lock` for the lifetime of the value.
///
/// The lock is advisory and process-scoped (`flock`/`LockFileEx`), released
/// when the file handle drops — including when the process dies, which is
/// what makes an interrupted `running` record detectable: status can take the
/// lock, and taking it proves no runner still owns that run.
#[derive(Debug)]
pub struct RunGuard {
    _lock: std::fs::File,
}

impl RunGuard {
    /// Take the exclusive run lock without blocking.
    ///
    /// `Ok(None)` means another runner holds it — after
    /// [`RUN_LOCK_ATTEMPTS`] contended attempts, so a momentary shared probe
    /// from `xv schedule status` cannot make a firing skip. That is not an
    /// error: a scheduled job that fires while the previous firing is still
    /// sweeping must log and leave, not queue up a second sweep of the same
    /// vault.
    ///
    /// `Err` means the lock could not be evaluated at all. That is a failed
    /// run, not a skipped one: nothing excluded this process, so reporting
    /// success would hide a rotation that never happened.
    pub fn try_acquire(paths: &ScheduleStatePaths) -> Result<Option<Self>> {
        let path = paths.run_lock_path();
        reject_if_symlink(paths.root())?;
        create_private_dir(paths.root()).map_err(|error| {
            CrosstacheError::config(format!(
                "Failed to create schedule state directory '{}': {error}",
                paths.root().display()
            ))
        })?;
        let lock = open_private_lock_file_no_follow(&path)?;
        for attempt in 0..RUN_LOCK_ATTEMPTS {
            match classify_lock(fs2::FileExt::try_lock_exclusive(&lock)) {
                LockAttempt::Acquired => return Ok(Some(Self { _lock: lock })),
                LockAttempt::Failed(error) => return Err(lock_failure(&path, &error)),
                LockAttempt::Contended => {
                    if attempt + 1 < RUN_LOCK_ATTEMPTS {
                        std::thread::sleep(RUN_LOCK_RETRY_DELAY);
                    }
                }
            }
        }
        Ok(None)
    }

    /// Whether some process currently holds the run lock, **without creating
    /// anything**.
    ///
    /// `Ok(None)` means there is no `run.lock` yet, so no runner has ever
    /// started here and there is nothing to hold. Otherwise the lock is taken
    /// and immediately released, and the answer is whether that succeeded.
    ///
    /// This exists instead of a plain `probe` because `xv schedule status` is
    /// read-only: a diagnosis that materialized the state directory and a lock
    /// inode would change the very thing it was asked to describe — and would
    /// leave `run.lock` behind on a machine that has no schedule installed.
    pub fn probe_existing(paths: &ScheduleStatePaths) -> Result<Option<bool>> {
        let path = paths.run_lock_path();
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(CrosstacheError::config(format!(
                    "Failed to inspect schedule run lock '{}': {error}",
                    path.display()
                )))
            }
            Ok(_) => {}
        }
        reject_if_symlink(&path)?;
        let lock = open_private_lock_file_no_follow(&path)?;
        // *Shared*, not exclusive: a probe must be able to observe the lock
        // without competing for it. A shared attempt still contends with an
        // exclusive holder — which is the question being asked — but two
        // concurrent probes, and a probe that overlaps a runner's retry
        // window, no longer take anything the runner needs.
        match classify_lock(fs2::FileExt::try_lock_shared(&lock)) {
            // Taking it proves no exclusive holder; the handle drops here.
            LockAttempt::Acquired => Ok(Some(false)),
            LockAttempt::Contended => Ok(Some(true)),
            // Not "someone holds it" — we do not know. Same distinction the
            // runner makes, for the same reason.
            LockAttempt::Failed(error) => Err(lock_failure(&path, &error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::manifest::test_paths_in;

    const DIGEST: &str = "sha256:4d38e06cbb685364a6500f808d8793840d5eb219d4e6b72f06d0f501c8eb3658";

    fn terminal(state: RunState) -> RunOutcomeV1 {
        RunOutcomeV1 {
            schema_version: 1,
            schedule_id: SCHEDULE_ID.to_string(),
            manifest_digest: DIGEST.to_string(),
            started_at: "2026-09-10T03:00:00Z".to_string(),
            finished_at: Some("2026-09-10T03:00:02Z".to_string()),
            state,
            exit_code: Some(0),
            summary: Some(RunSummary {
                policy_managed: 12,
                due: 2,
                rotated: 2,
                failed: 0,
            }),
            diagnostic: None,
        }
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    // -- schema -------------------------------------------------------------

    #[test]
    fn every_state_round_trips_through_disk() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        for state in [
            RunState::Running,
            RunState::Success,
            RunState::PartialFailure,
            RunState::Failed,
            RunState::RefusedDrift,
        ] {
            let outcome = if state == RunState::Running {
                RunOutcomeV1::running(DIGEST, "2026-09-10T03:00:00Z")
            } else {
                terminal(state)
            };
            write_outcome_atomic(&paths, &outcome).expect("write");
            let loaded = load_outcome(&paths).expect("load").expect("present");
            assert_eq!(loaded, RunOutcome::V1(outcome), "state {state:?}");
        }
    }

    #[test]
    fn state_literals_match_the_persisted_spelling() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        for state in [
            RunState::Success,
            RunState::PartialFailure,
            RunState::Failed,
            RunState::RefusedDrift,
        ] {
            write_outcome_atomic(&paths, &terminal(state)).expect("write");
            let body = std::fs::read_to_string(paths.last_run_path()).expect("read");
            assert!(
                body.contains(&format!("\"state\": \"{}\"", state.as_str())),
                "{body}"
            );
        }
    }

    #[test]
    fn the_persisted_field_order_matches_the_design() {
        let bytes = serialize_outcome(&terminal(RunState::Success));
        let body = String::from_utf8(bytes).expect("utf8");
        let order: Vec<&str> = [
            "\"schema_version\"",
            "\"schedule_id\"",
            "\"manifest_digest\"",
            "\"started_at\"",
            "\"finished_at\"",
            "\"state\"",
            "\"exit_code\"",
            "\"summary\"",
            "\"diagnostic\"",
        ]
        .to_vec();
        let mut cursor = 0usize;
        for field in order {
            let at = body[cursor..]
                .find(field)
                .unwrap_or_else(|| panic!("{field} missing or out of order in {body}"));
            cursor += at + field.len();
        }
    }

    #[test]
    fn no_outcome_yet_is_not_an_error() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        assert_eq!(load_outcome(&paths).expect("load"), None);
    }

    // -- invariants ---------------------------------------------------------

    #[test]
    fn a_running_record_may_not_carry_a_result() {
        let mut outcome = RunOutcomeV1::running(DIGEST, "2026-09-10T03:00:00Z");
        outcome.exit_code = Some(0);
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = RunOutcomeV1::running(DIGEST, "2026-09-10T03:00:00Z");
        outcome.finished_at = Some("2026-09-10T03:00:02Z".to_string());
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = RunOutcomeV1::running(DIGEST, "2026-09-10T03:00:00Z");
        outcome.summary = Some(RunSummary {
            policy_managed: 1,
            due: 0,
            rotated: 0,
            failed: 0,
        });
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = RunOutcomeV1::running(DIGEST, "2026-09-10T03:00:00Z");
        outcome.diagnostic = Some(RunDiagnostic::new("target_drift", "x"));
        assert!(validate_v1(&outcome).is_err());
    }

    #[test]
    fn a_terminal_record_must_carry_a_finish_and_an_exit_code() {
        let mut outcome = terminal(RunState::Success);
        outcome.finished_at = None;
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = terminal(RunState::Success);
        outcome.exit_code = None;
        assert!(validate_v1(&outcome).is_err());
    }

    #[test]
    fn identifiers_timestamps_and_digests_are_validated() {
        let mut outcome = terminal(RunState::Success);
        outcome.schedule_id = "something-else".to_string();
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = terminal(RunState::Success);
        outcome.manifest_digest = "not-a-digest".to_string();
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = terminal(RunState::Success);
        outcome.started_at = "2026-09-10T03:00:00+02:00".to_string();
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = terminal(RunState::Success);
        outcome.finished_at = Some("yesterday".to_string());
        assert!(validate_v1(&outcome).is_err());

        let mut outcome = terminal(RunState::Success);
        outcome.schema_version = 2;
        assert!(validate_v1(&outcome).is_err());
    }

    #[test]
    fn unknown_fields_and_unknown_versions_are_refused() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        create_private_dir(paths.root()).expect("dir");

        std::fs::write(
            paths.last_run_path(),
            r#"{"schema_version":1,"schedule_id":"rotation-default","manifest_digest":"sha256:4d38e06cbb685364a6500f808d8793840d5eb219d4e6b72f06d0f501c8eb3658","started_at":"2026-09-10T03:00:00Z","finished_at":null,"state":"running","exit_code":null,"summary":null,"diagnostic":null,"extra":1}"#,
        )
        .expect("write");
        let error = load_outcome(&paths).expect_err("unknown field").to_string();
        assert!(error.contains("schema version 1"), "{error}");

        std::fs::write(
            paths.last_run_path(),
            r#"{"schema_version":99,"schedule_id":"rotation-default"}"#,
        )
        .expect("write");
        let error = load_outcome(&paths)
            .expect_err("unknown version")
            .to_string();
        assert!(error.contains("schema_version 99"), "{error}");
    }

    #[test]
    fn an_oversized_outcome_is_refused_before_parsing() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        create_private_dir(paths.root()).expect("dir");
        std::fs::write(paths.last_run_path(), vec![b'a'; MAX_OUTCOME_BYTES + 1]).expect("write");
        let error = load_outcome(&paths).expect_err("too large").to_string();
        assert!(error.contains("byte limit"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_outcome_is_refused() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        create_private_dir(paths.root()).expect("dir");
        let real = dir.path().join("elsewhere.json");
        std::fs::write(&real, "{}").expect("write");
        std::os::unix::fs::symlink(&real, paths.last_run_path()).expect("symlink");

        let error = load_outcome(&paths).expect_err("symlink").to_string();
        assert!(error.contains("Refusing symlinked"), "{error}");
        let error = write_outcome_atomic(&paths, &terminal(RunState::Success))
            .expect_err("symlink write")
            .to_string();
        assert!(!error.is_empty());
        // The write must not have followed the link.
        assert_eq!(std::fs::read_to_string(&real).expect("read"), "{}");
    }

    #[cfg(unix)]
    #[test]
    fn the_outcome_and_its_directory_are_owner_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        write_outcome_atomic(&paths, &terminal(RunState::Success)).expect("write");
        let file = std::fs::metadata(paths.last_run_path()).expect("file");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let root = std::fs::metadata(paths.root()).expect("root");
        assert_eq!(root.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_replacing_write_leaves_exactly_one_record() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        write_outcome_atomic(
            &paths,
            &RunOutcomeV1::running(DIGEST, "2026-09-10T03:00:00Z"),
        )
        .expect("running");
        write_outcome_atomic(&paths, &terminal(RunState::Success)).expect("terminal");
        let loaded = load_outcome(&paths).expect("load").expect("present");
        let RunOutcome::V1(loaded) = loaded;
        assert_eq!(loaded.state, RunState::Success);
    }

    // -- diagnostics --------------------------------------------------------

    #[test]
    fn a_long_diagnostic_truncates_on_a_scalar_boundary() {
        // 1030 multi-byte scalars: a byte-wise truncation would split one.
        let long: String = "é".repeat(1030);
        let diagnostic = RunDiagnostic::new("rotation-failed", long);
        assert_eq!(diagnostic.message.chars().count(), MAX_DIAGNOSTIC_SCALARS);
        assert!(diagnostic.message.chars().all(|c| c == 'é'));
        // Every scalar is intact: the string round-trips through UTF-8.
        assert_eq!(
            String::from_utf8(diagnostic.message.clone().into_bytes()).expect("utf8"),
            diagnostic.message
        );

        let mut outcome = terminal(RunState::Failed);
        outcome.diagnostic = Some(diagnostic);
        validate_v1(&outcome).expect("a truncated diagnostic is valid");
    }

    #[test]
    fn a_short_diagnostic_is_left_alone() {
        let diagnostic = RunDiagnostic::new("target_drift", "config_digest changed");
        assert_eq!(diagnostic.message, "config_digest changed");
    }

    #[test]
    fn the_loader_refuses_an_overlong_diagnostic_message() {
        let mut outcome = terminal(RunState::Failed);
        // Bypass `RunDiagnostic::new`'s truncation the way a hand-edited file
        // would.
        outcome.diagnostic = Some(RunDiagnostic {
            code: "rotation-failed".to_string(),
            message: "a".repeat(MAX_DIAGNOSTIC_SCALARS + 1),
        });
        let error = validate_v1(&outcome).expect_err("too long").to_string();
        assert!(error.contains("Unicode scalar values"), "{error}");

        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        create_private_dir(paths.root()).expect("dir");
        std::fs::write(
            paths.last_run_path(),
            serde_json::to_vec(&outcome).expect("json"),
        )
        .expect("write");
        assert!(load_outcome(&paths).is_err());
    }

    #[test]
    fn an_empty_diagnostic_code_is_refused() {
        let mut outcome = terminal(RunState::Failed);
        outcome.diagnostic = Some(RunDiagnostic::new("", "something"));
        assert!(validate_v1(&outcome).is_err());
    }

    // -- run.lock -----------------------------------------------------------

    #[test]
    fn the_run_lock_excludes_a_second_holder_and_is_released_on_drop() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());

        let first = RunGuard::try_acquire(&paths)
            .expect("acquire")
            .expect("free lock");
        assert!(
            RunGuard::try_acquire(&paths).expect("contend").is_none(),
            "a second acquisition must fail while the first is held"
        );
        assert_eq!(RunGuard::probe_existing(&paths).expect("probe"), Some(true));

        drop(first);
        assert!(
            RunGuard::try_acquire(&paths).expect("reacquire").is_some(),
            "the lock must be free once the guard drops"
        );
        assert_eq!(
            RunGuard::probe_existing(&paths).expect("probe"),
            Some(false)
        );
    }

    /// Status is read-only. Probing a machine with no schedule installed must
    /// not conjure the state directory or the lock inode into existence.
    #[test]
    fn probing_creates_nothing_when_no_run_has_ever_started() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        assert!(!paths.root().exists(), "fixture starts with no state root");

        assert_eq!(RunGuard::probe_existing(&paths).expect("probe"), None);

        assert!(!paths.root().exists(), "probing created the state root");
        assert!(!paths.run_lock_path().exists(), "probing created the lock");
        assert!(!paths.last_run_path().exists());
    }

    #[test]
    fn probing_an_existing_free_lock_leaves_it_empty_and_unheld() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        drop(
            RunGuard::try_acquire(&paths)
                .expect("acquire")
                .expect("free"),
        );

        assert_eq!(
            RunGuard::probe_existing(&paths).expect("probe"),
            Some(false)
        );
        assert!(!paths.last_run_path().exists());
        assert_eq!(
            std::fs::read(paths.run_lock_path()).expect("read"),
            Vec::<u8>::new()
        );
        // The probe released what it took: a real runner can still start.
        assert!(RunGuard::try_acquire(&paths).expect("acquire").is_some());
    }

    /// Only contention means "another runner holds it". Every other lock
    /// failure is a failed run, and must not be laundered into a skip.
    #[test]
    fn only_contention_counts_as_another_holder() {
        assert!(is_lock_contention(&std::io::Error::from(
            std::io::ErrorKind::WouldBlock
        )));
        assert!(is_lock_contention(&fs2::lock_contended_error()));

        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::Other,
        ] {
            assert!(
                !is_lock_contention(&std::io::Error::from(kind)),
                "{kind:?} is a failed run, not a busy one"
            );
        }
        // ENOLCK: a synthetic OS error that is not the contention error.
        assert!(!is_lock_contention(&std::io::Error::from_raw_os_error(77)));
    }

    /// The seam finding 1 asks for: the mapping from a raw lock result to the
    /// runner's three outcomes, unit-testable on a synthetic `io::Error`.
    #[test]
    fn a_non_contention_lock_error_is_a_failure_not_a_skip() {
        assert!(matches!(classify_lock(Ok(())), LockAttempt::Acquired));
        assert!(matches!(
            classify_lock(Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))),
            LockAttempt::Contended
        ));
        match classify_lock(Err(std::io::Error::from_raw_os_error(77))) {
            LockAttempt::Failed(error) => {
                assert_eq!(error.raw_os_error(), Some(77));
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// The error a failed lock reports has to be diagnosable from a log
    /// nobody was watching: it names the file and the OS error.
    #[test]
    fn a_lock_failure_names_the_lock_file_and_the_os_error() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        let message = lock_failure(
            &paths.run_lock_path(),
            &std::io::Error::from_raw_os_error(77),
        )
        .to_string();
        assert!(message.contains("run.lock"), "{message}");
        assert!(
            message.contains(&std::io::Error::from_raw_os_error(77).to_string()),
            "{message}"
        );
    }

    /// A `status` probe holds `run.lock` *shared* for an instant. The runner
    /// retries, so an overlapping probe delays the sweep — it never skips it.
    #[test]
    fn a_briefly_held_shared_lock_delays_the_runner_but_does_not_skip_it() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        // Materialize the lock inode so the probing thread opens the same one.
        drop(
            RunGuard::try_acquire(&paths)
                .expect("acquire")
                .expect("free"),
        );

        let lock_path = paths.run_lock_path();
        // Handshake: the runner must not attempt the lock until the probe
        // holds it, or the test would race its own fixture.
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let held = std::thread::spawn(move || {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open");
            fs2::FileExt::try_lock_shared(&file).expect("shared");
            held_tx.send(()).expect("signal");
            std::thread::sleep(std::time::Duration::from_millis(300));
            fs2::FileExt::unlock(&file).expect("unlock");
        });
        held_rx.recv().expect("the probe took the shared lock");

        let guard = RunGuard::try_acquire(&paths).expect("acquire");
        assert!(
            guard.is_some(),
            "a shared probe released inside the retry window must not make the runner skip"
        );
        held.join().expect("join");
    }

    /// A shared probe must not evict a live runner's exclusive hold.
    #[test]
    fn probing_reports_a_live_runner_and_does_not_take_its_lock() {
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        let _guard = RunGuard::try_acquire(&paths)
            .expect("acquire")
            .expect("free");
        assert_eq!(RunGuard::probe_existing(&paths).expect("probe"), Some(true));
        assert_eq!(RunGuard::probe_existing(&paths).expect("probe"), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn the_run_lock_is_owner_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let paths = test_paths_in(dir.path());
        let _guard = RunGuard::try_acquire(&paths)
            .expect("acquire")
            .expect("free");
        let mode = std::fs::metadata(paths.run_lock_path())
            .expect("lock")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
