//! What `xv schedule status` may say, as a typed value.
//!
//! `status` is the one schedule command that answers rather than acts, and the
//! design
//! (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`,
//! "Status contract") lists the dimensions it must keep apart: what the
//! scheduler says, who owns what is installed, what target was pinned, whether
//! that target still resolves, whether the *unit* still agrees with the
//! manifest, which executable is recorded, how the last run ended, and when the
//! next one is due.
//!
//! Three rules shape everything here:
//!
//! 1. **Nothing is guessed.** A next-run time is reported only when the
//!    scheduler printed one in a form that cannot mean two different instants;
//!    everything else is [`NextRun::Unknown`]. A scheduler that could not
//!    answer is [`SchedulerState::Error`] or [`SchedulerState::Unknown`], never
//!    absence.
//! 2. **Nothing is written.** No lock is created, no state directory is
//!    materialized, no repair is attempted — a diagnosis that changes what it
//!    describes is not a diagnosis. `collect_status` is covered by a snapshot
//!    test that fails if a single byte under the state or unit directories
//!    changes.
//! 3. **No provider is contacted.** Drift is recomputed from recorded inputs
//!    and files only; vault verification belongs to install and run.
//!
//! Raw command output never leaves this module. Scheduler failures are reported
//! as the command name plus its exit status, because `schtasks` speaks the
//! machine's display language and `launchctl` will happily quote another
//! user's job.

use std::path::{Path, PathBuf};

use chrono::{NaiveDateTime, TimeZone, Utc};

use crate::error::Result;
use crate::schedule::drift::{self, DriftReason, DriftReport};
use crate::schedule::install::{classify_owned_unit, verify_schtasks_cadence, ArtifactState};
use crate::schedule::manifest::{
    ManifestCadence, ScheduleManifest, ScheduleManifestV1, ScheduleStatePaths,
};
use crate::schedule::outcome::{load_outcome, RunGuard, RunOutcome, RunOutcomeV1, RunState};
use crate::schedule::ownership::{
    inspect_ownership, plist_program_arguments, schtasks_task_to_run, systemd_exec_start,
    xml_unescape, Ownership, SchedulerState,
};
use crate::schedule::{
    launchd_calendar_pairs, launchd_domain_target, systemd_on_calendar, CommandRunner, Platform,
    ScheduleInterval, UnitPaths, SCHTASKS_NAME, SYSTEMD_UNIT,
};

// ---------------------------------------------------------------------------
// Dimensions
// ---------------------------------------------------------------------------

/// When the scheduler says the job fires next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NextRun {
    /// An instant the scheduler itself reported, normalized to RFC 3339 UTC.
    At(String),
    /// The platform exposes no next-run field, printed one in a locale-
    /// dependent form, or said `n/a`. Never a time computed from the cadence:
    /// a guessed "next run" that disagrees with the scheduler is worse than no
    /// answer at all.
    Unknown,
}

/// Which executable the manifest pinned, and what is at that path now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutableStatus {
    pub(crate) recorded_path: String,
    pub(crate) installed_version: String,
    pub(crate) current_version: String,
    /// Whether the `xv` asking is the `xv` the unit will run.
    ///
    /// `false` means `current_version` describes *this* process, not the
    /// binary the scheduler invokes, so it may not be reported as the
    /// installed binary's version.
    pub(crate) current_matches_path: bool,
    /// The path of the `xv` that ran `status`, for the "current unknown"
    /// rendering when it is not the recorded one.
    pub(crate) invoking_path: String,
}

/// How the last recorded run ended, if there was one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LastRunStatus {
    /// No `last-run.json`.
    Never,
    /// A completed (or still-running, see the other variants) record.
    Outcome {
        outcome: RunOutcomeV1,
        /// The record was written by a *different* installation than the one
        /// installed now, so it does not describe the current schedule.
        previous_install: bool,
    },
    /// The record says `running` and the run lock is still held: a sweep is
    /// happening right now.
    RunningHeld { started_at: String },
    /// The record says `running` but nobody holds the lock: the runner died
    /// without recording an ending.
    Interrupted { started_at: String },
    /// `last-run.json` exists but could not be read or did not validate. The
    /// string is our own message, which names paths and fields only.
    Unreadable(String),
}

/// Whether the rotation log the manifest names exists yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogStatus {
    Present,
    NotYetWritten,
    /// No manifest to name a log path, or the path could not be inspected.
    Unknown,
}

/// Differences between the *installed unit* and the manifest.
///
/// Distinct from [`DriftReport`], which compares the manifest with the world.
/// This compares the manifest with what the scheduler will actually invoke, and
/// the design's drift table makes both of its rows refusals in status:
/// a unit that names a different manifest or executable, and a unit whose
/// cadence or log path disagrees with the manifest.
///
/// Nothing here repairs anything: an install is the only thing that writes a
/// unit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UnitDriftReport {
    pub(crate) reasons: Vec<DriftReason>,
}

impl UnitDriftReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.reasons.is_empty()
    }

    /// One reason per field: two mismatching arguments of the same command are
    /// one problem with one repair.
    fn push(&mut self, field: &'static str) {
        self.push_detail(
            field,
            format!("{field} differs between the installed unit and the manifest; reinstall"),
        );
    }

    /// A reason whose wording is not the plain "differs" sentence — today only
    /// a unit that could not be read at all.
    fn push_detail(&mut self, field: &'static str, detail: String) {
        if self.reasons.iter().any(|reason| reason.field == field) {
            return;
        }
        self.reasons.push(DriftReason::new(field, detail));
    }

    /// Sort into the fixed field order, so the same three problems read the
    /// same way whichever platform found them — the property `DriftReport`
    /// already guarantees for target drift.
    fn sorted(mut self) -> Self {
        self.reasons.sort_by_key(|reason| {
            UNIT_FIELD_ORDER
                .iter()
                .position(|field| *field == reason.field)
                .unwrap_or(UNIT_FIELD_ORDER.len())
        });
        self
    }
}

/// The order unit-drift reasons are reported in: what it runs, when it runs,
/// where it writes.
const UNIT_FIELD_ORDER: [&str; 3] = [UNIT_COMMAND, UNIT_CADENCE, UNIT_LOG_PATH];

/// Field name for a unit whose executable or manifest argument disagrees.
pub(crate) const UNIT_COMMAND: &str = "unit_command";
/// Field name for a unit whose trigger disagrees with the recorded cadence.
pub(crate) const UNIT_CADENCE: &str = "unit_cadence";
/// Field name for a unit that logs somewhere else.
pub(crate) const UNIT_LOG_PATH: &str = "unit_log_path";

/// Everything `xv schedule status` knows, before any of it is rendered.
#[derive(Debug)]
pub(crate) struct ScheduleStatusReport {
    pub(crate) scheduler: SchedulerState,
    pub(crate) next_run: NextRun,
    pub(crate) ownership: Ownership,
    /// The manifest and the digest of the exact bytes it was parsed from.
    pub(crate) manifest: Option<(ScheduleManifestV1, String)>,
    /// Why the manifest could not be used, when one is present but unusable.
    pub(crate) manifest_error: Option<String>,
    pub(crate) drift: Option<DriftReport>,
    pub(crate) unit_drift: Option<UnitDriftReport>,
    pub(crate) executable: Option<ExecutableStatus>,
    pub(crate) last_run: LastRunStatus,
    pub(crate) log: LogStatus,
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// The systemd property that carries the next elapse, and the invocation this
/// module pins for it.
///
/// `--timestamp=utc` is what makes the answer parseable: without it systemctl
/// renders the timestamp in the host's timezone and locale-ish weekday, and a
/// value like `Thu 2026-09-10 05:00:00 CEST` cannot be turned into an instant
/// without a timezone database we do not carry. Systemd older than v247 does
/// not know the option, so a failed call is retried once without it — that
/// host then reports a time only when its timestamps are already UTC.
const SYSTEMD_NEXT_ELAPSE: &str = "--property=NextElapseUSecRealtime";
const SYSTEMD_TIMESTAMP_UTC: &str = "--timestamp=utc";

/// Collect every status dimension. Reads files and runs the platform's own
/// query commands; writes nothing, locks nothing, and constructs no backend.
pub(crate) async fn collect_status(
    platform: Platform,
    unit_paths: &UnitPaths,
    state: &ScheduleStatePaths,
    runner: &dyn CommandRunner,
    now_binary: &Path,
    now_version: &str,
) -> Result<ScheduleStatusReport> {
    let ownership = inspect_ownership(platform, unit_paths, state, runner)?;
    let next_run = probe_next_run(platform, runner);

    let (manifest, manifest_error) = match load_manifest_if_present(state) {
        Ok(Some((ScheduleManifest::V1(manifest), bytes))) => (
            Some((manifest, crate::config::content_digest(&bytes))),
            None,
        ),
        Ok(None) => (None, None),
        Err(error) => (None, Some(error.to_string())),
    };

    let mut drift = None;
    let mut unit_drift = None;
    let mut executable = None;
    let mut log = LogStatus::Unknown;
    if let Some((manifest, _)) = &manifest {
        // Validate the executable the *scheduler* will run, not the one that
        // happens to be asking. `status` may be invoked from any `xv` on
        // PATH — a build tree, another version, a copy — and feeding that
        // path into `validate_execution` made every such run report
        // `binary_path` drift for a schedule that would have run perfectly.
        // So the recorded path is compared with itself (which leaves the
        // existence / regular-file / executable-bit checks doing the real
        // work), and the version comparison is only meaningful when this
        // process *is* the recorded binary; otherwise the recorded version is
        // passed back in so the in-place-upgrade warning cannot fire on
        // evidence we do not have.
        let recorded_binary = PathBuf::from(&manifest.execution.binary_path);
        let current_matches_path = now_binary == recorded_binary.as_path();
        let version_for_drift = if current_matches_path {
            now_version
        } else {
            manifest.execution.installed_version.as_str()
        };
        drift = Some(
            drift::validate_recorded_target(manifest, &recorded_binary, version_for_drift).await,
        );
        unit_drift = Some(inspect_unit_drift(
            platform, unit_paths, state, manifest, runner,
        )?);
        executable = Some(ExecutableStatus {
            recorded_path: manifest.execution.binary_path.clone(),
            installed_version: manifest.execution.installed_version.clone(),
            current_version: now_version.to_string(),
            current_matches_path,
            invoking_path: now_binary.display().to_string(),
        });
        log = inspect_log(Path::new(&manifest.execution.log_path));
    }

    let last_run = collect_last_run(state, manifest.as_ref().map(|(_, digest)| digest.as_str()))?;

    Ok(ScheduleStatusReport {
        scheduler: ownership.scheduler,
        next_run,
        ownership: ownership.state,
        manifest,
        manifest_error,
        drift,
        unit_drift,
        executable,
        last_run,
        log,
    })
}

/// `Ok(None)` when there is simply no manifest — the ordinary state of a host
/// with no schedule, which must not be reported as an error.
fn load_manifest_if_present(
    state: &ScheduleStatePaths,
) -> Result<Option<(ScheduleManifest, Vec<u8>)>> {
    match std::fs::symlink_metadata(state.manifest_path()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        _ => {}
    }
    crate::schedule::manifest::load_manifest_with_bytes(state).map(Some)
}

/// Existence only, and without following a final symlink: status reports what
/// is at the path, it does not open it.
fn inspect_log(path: &Path) -> LogStatus {
    match std::fs::symlink_metadata(path) {
        Ok(_) => LogStatus::Present,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LogStatus::NotYetWritten,
        Err(_) => LogStatus::Unknown,
    }
}

/// Read `last-run.json` and decide what it means *now*.
///
/// A `running` record is only a running sweep while somebody holds the run
/// lock. Without that check a runner killed at 3am would leave `xv schedule
/// status` claiming a rotation is in progress forever.
fn collect_last_run(
    state: &ScheduleStatePaths,
    manifest_digest: Option<&str>,
) -> Result<LastRunStatus> {
    let outcome = match load_outcome(state) {
        Ok(Some(RunOutcome::V1(outcome))) => outcome,
        Ok(None) => return Ok(LastRunStatus::Never),
        Err(error) => return Ok(LastRunStatus::Unreadable(error.to_string())),
    };

    if outcome.state == RunState::Running {
        // `probe_existing` never creates the lock, so a status on a host with
        // no schedule cannot leave one behind.
        let held = RunGuard::probe_existing(state)?;
        let started_at = outcome.started_at.clone();
        return Ok(match held {
            Some(true) => LastRunStatus::RunningHeld { started_at },
            _ => LastRunStatus::Interrupted { started_at },
        });
    }

    // A retained record from an earlier installation is history, not the
    // current schedule's last run.
    let previous_install = manifest_digest != Some(outcome.manifest_digest.as_str());
    Ok(LastRunStatus::Outcome {
        outcome,
        previous_install,
    })
}

// ---------------------------------------------------------------------------
// Next run
// ---------------------------------------------------------------------------

/// Ask the scheduler when the job fires next. Any failure is
/// [`NextRun::Unknown`]: the scheduler dimension already reports command
/// trouble, and a missing next-run time is not itself a fault.
pub(crate) fn probe_next_run(platform: Platform, runner: &dyn CommandRunner) -> NextRun {
    match platform {
        Platform::Launchd => match runner.run("launchctl", &["print", &launchd_domain_target()]) {
            Ok(out) if out.ok() => parse_launchctl_next_run(&out.stdout),
            _ => NextRun::Unknown,
        },
        Platform::Systemd => {
            let timer = format!("{SYSTEMD_UNIT}.timer");
            let utc = runner.run(
                "systemctl",
                &[
                    "--user",
                    "show",
                    &timer,
                    SYSTEMD_TIMESTAMP_UTC,
                    SYSTEMD_NEXT_ELAPSE,
                ],
            );
            match utc {
                Ok(out) if out.ok() => parse_systemd_next_run(&out.stdout),
                // Pre-v247 systemctl rejects `--timestamp`; ask again without
                // it and accept the answer only if it is already UTC.
                _ => match runner.run(
                    "systemctl",
                    &["--user", "show", &timer, SYSTEMD_NEXT_ELAPSE],
                ) {
                    Ok(out) if out.ok() => parse_systemd_next_run(&out.stdout),
                    _ => NextRun::Unknown,
                },
            }
        }
        Platform::Schtasks => match runner.run(
            "schtasks",
            &["/Query", "/TN", SCHTASKS_NAME, "/V", "/FO", "LIST"],
        ) {
            Ok(out) if out.ok() => parse_schtasks_next_run(&out.stdout),
            _ => NextRun::Unknown,
        },
    }
}

/// `launchctl print` output → the next fire date, when it prints one.
///
/// launchd only emits `next fire date` for a loaded calendar/interval trigger,
/// and several macOS versions print nothing of the sort; that is the normal
/// case and yields [`NextRun::Unknown`]. When it does print one the value
/// carries a numeric UTC offset (`2026-09-10 03:00:00 -0700`), which is
/// unambiguous — anything else, including the human "in 4 hours" forms, is
/// refused rather than guessed.
pub(crate) fn parse_launchctl_next_run(output: &str) -> NextRun {
    let Some(value) = field_value(output, "next fire date") else {
        return NextRun::Unknown;
    };
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&value) {
        return NextRun::At(rfc3339_utc(parsed.with_timezone(&Utc)));
    }
    for format in ["%Y-%m-%d %H:%M:%S %z", "%Y-%m-%d %H:%M:%S%.f %z"] {
        if let Ok(parsed) = chrono::DateTime::parse_from_str(&value, format) {
            return NextRun::At(rfc3339_utc(parsed.with_timezone(&Utc)));
        }
    }
    NextRun::Unknown
}

/// `systemctl --user show ... --property=NextElapseUSecRealtime` → the next
/// elapse.
///
/// Two accepted forms: the rendered `Thu 2026-09-10 03:00:00 UTC`, taken only
/// when the zone token is literally `UTC` (any other zone abbreviation is
/// ambiguous without a timezone database), and a bare microsecond epoch, which
/// some systemd versions print for the raw property. `n/a`, `0` and an empty
/// value all mean the timer has no next elapse.
pub(crate) fn parse_systemd_next_run(output: &str) -> NextRun {
    let value = field_value(output, "NextElapseUSecRealtime").unwrap_or_default();
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("n/a") || value == "0" {
        return NextRun::Unknown;
    }
    if value.chars().all(|c| c.is_ascii_digit()) {
        let micros: i64 = match value.parse() {
            Ok(micros) => micros,
            Err(_) => return NextRun::Unknown,
        };
        return match Utc.timestamp_micros(micros) {
            chrono::LocalResult::Single(parsed) => NextRun::At(rfc3339_utc(parsed)),
            _ => NextRun::Unknown,
        };
    }
    let tokens: Vec<&str> = value.split_whitespace().collect();
    let (date, time, zone) = match tokens.as_slice() {
        [_weekday, date, time, zone] => (*date, *time, *zone),
        [date, time, zone] => (*date, *time, *zone),
        _ => return NextRun::Unknown,
    };
    if zone != "UTC" {
        return NextRun::Unknown;
    }
    match NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S") {
        Ok(naive) => NextRun::At(rfc3339_utc(Utc.from_utc_datetime(&naive))),
        Err(_) => NextRun::Unknown,
    }
}

/// `schtasks /Query /TN <name> /V /FO LIST` → `Next Run Time:`.
///
/// Two independent ways this value can mean more than one instant, and both
/// end in [`NextRun::Unknown`] rather than a guess:
///
/// 1. **The date is locale-formatted.** `schtasks` renders in the machine's
///    display language and short-date format, and `3/9/2026` is two different
///    days depending on where the host was set up.
/// 2. **The time carries no zone.** The usual `Next Run Time` is a bare local
///    wall clock. Attributing the host's *current* UTC offset to a time in the
///    future is wrong across a DST boundary — `03:00` the night the clocks move
///    is an hour away from where that arithmetic lands it — and status would
///    print a confident instant that the scheduler never said.
///
/// So only a value that names its own offset is accepted: RFC 3339
/// (`2026-09-10T03:00:00Z`, `2026-09-10T03:00:00+02:00`) or the same date and
/// time with a numeric `±hhmm`. `N/A`, `Disabled`, every locale short-date form
/// and every zoneless time yield [`NextRun::Unknown`].
pub(crate) fn parse_schtasks_next_run(output: &str) -> NextRun {
    let Some(value) = field_value(output, "next run time") else {
        return NextRun::Unknown;
    };
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("n/a")
        || value.eq_ignore_ascii_case("disabled")
    {
        return NextRun::Unknown;
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(value) {
        return NextRun::At(rfc3339_utc(parsed.with_timezone(&Utc)));
    }
    for format in ["%Y-%m-%d %H:%M:%S %z", "%Y-%m-%dT%H:%M:%S %z"] {
        if let Ok(parsed) = chrono::DateTime::parse_from_str(value, format) {
            return NextRun::At(rfc3339_utc(parsed.with_timezone(&Utc)));
        }
    }
    NextRun::Unknown
}

/// The value of a `Key: value` / `Key=value` line, matched case-insensitively
/// on the key. Splits on the *first* separator only, so a timestamp's own
/// colons survive.
fn field_value(output: &str, key: &str) -> Option<String> {
    let key = key.to_ascii_lowercase();
    output.lines().map(str::trim).find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let rest = lower.strip_prefix(&key)?;
        let separator = rest.trim_start().chars().next()?;
        if separator != ':' && separator != '=' {
            return None;
        }
        let index = line.len() - rest.len();
        let value = line[index..].trim_start();
        Some(value[separator.len_utf8()..].trim().to_string())
    })
}

fn rfc3339_utc(when: chrono::DateTime<Utc>) -> String {
    when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Unit drift
// ---------------------------------------------------------------------------

/// Compare what the scheduler will invoke with what the manifest recorded.
///
/// The four rendered facts are the four checked here: the executable, the
/// manifest argument, the cadence and the log path. The comparison reads the
/// owned unit files (and, on Windows, the registered task, which is the only
/// artifact there) through the same classifiers install uses, so a foreign or
/// absent unit produces no reasons — that is ownership's story to tell, not
/// drift's.
pub(crate) fn inspect_unit_drift(
    platform: Platform,
    unit_paths: &UnitPaths,
    state: &ScheduleStatePaths,
    manifest: &ScheduleManifestV1,
    runner: &dyn CommandRunner,
) -> Result<UnitDriftReport> {
    let mut report = UnitDriftReport::default();
    let manifest_path = state.manifest_path();
    let interval = manifest_interval(&manifest.cadence);

    match platform {
        Platform::Launchd => {
            let text = match owned_unit_text(&unit_paths.launchd_plist()) {
                UnitText::Owned(text) => text,
                UnitText::NotOurs => return Ok(report),
                UnitText::Unreadable => {
                    report.push_detail(UNIT_COMMAND, unreadable_unit_detail());
                    return Ok(report.sorted());
                }
            };
            match plist_program_arguments(&text) {
                Some(argv) => check_command(&mut report, platform, &argv, manifest, &manifest_path),
                None => report.push(UNIT_COMMAND),
            }
            if let Some(interval) = interval {
                let expected: Vec<(String, String)> = launchd_calendar_pairs(interval)
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect();
                let actual = plist_calendar_fields(&text);
                if actual != expected {
                    report.push(UNIT_CADENCE);
                }
            }
            match plist_string_value(&text, "StandardOutPath") {
                Some(log) if same_path(platform, &log, &manifest.execution.log_path) => {}
                _ => report.push(UNIT_LOG_PATH),
            }
        }
        Platform::Systemd => {
            match owned_unit_text(&unit_paths.systemd_service()) {
                UnitText::Owned(text) => {
                    match systemd_exec_start(&text) {
                        Some(argv) => {
                            check_command(&mut report, platform, &argv, manifest, &manifest_path)
                        }
                        None => report.push(UNIT_COMMAND),
                    }
                    match systemd_append_target(&text) {
                        Some(log) if same_path(platform, &log, &manifest.execution.log_path) => {}
                        _ => report.push(UNIT_LOG_PATH),
                    }
                }
                UnitText::NotOurs => {}
                UnitText::Unreadable => report.push_detail(UNIT_COMMAND, unreadable_unit_detail()),
            }
            match owned_unit_text(&unit_paths.systemd_timer()) {
                UnitText::Owned(text) => {
                    if let Some(interval) = interval {
                        let expected = systemd_on_calendar(interval);
                        match field_value(&text, "OnCalendar") {
                            Some(actual) if actual == expected => {}
                            _ => report.push(UNIT_CADENCE),
                        }
                    }
                }
                UnitText::NotOurs => {}
                UnitText::Unreadable => report.push_detail(UNIT_CADENCE, unreadable_unit_detail()),
            }
        }
        Platform::Schtasks => {
            // Task Scheduler holds the whole definition, so both queries are
            // against the registered task rather than a file.
            if let Ok(out) = runner.run(
                "schtasks",
                &["/Query", "/TN", SCHTASKS_NAME, "/V", "/FO", "LIST"],
            ) {
                if out.ok() {
                    match schtasks_task_to_run(&out.stdout) {
                        Some(command) => {
                            let invocation = parse_schtasks_invocation(&command);
                            let argv: Vec<String> = invocation
                                .binary
                                .into_iter()
                                .chain(
                                    invocation
                                        .manifest
                                        .into_iter()
                                        .flat_map(|manifest| ["--manifest".to_string(), manifest]),
                                )
                                .collect();
                            check_command(&mut report, platform, &argv, manifest, &manifest_path);
                            match invocation.log {
                                Some(log)
                                    if same_path(platform, &log, &manifest.execution.log_path) => {}
                                _ => report.push(UNIT_LOG_PATH),
                            }
                        }
                        None => report.push(UNIT_COMMAND),
                    }
                }
            }
            if let Some(interval) = interval {
                if let Ok(xml) = runner.run("schtasks", &["/Query", "/TN", SCHTASKS_NAME, "/XML"]) {
                    if xml.ok() && verify_schtasks_cadence(&xml.stdout, interval).is_err() {
                        report.push(UNIT_CADENCE);
                    }
                }
            }
        }
    }

    Ok(report.sorted())
}

/// One sentence for a unit that is there but will not open. Says nothing about
/// *why* beyond that: the OS error text can quote another user's path.
fn unreadable_unit_detail() -> String {
    "the installed unit could not be read; check its permissions, then reinstall".to_string()
}

/// The pieces of a registered task's `Task To Run` command line.
///
/// Task Scheduler hands back one string, and the four facts that must match the
/// manifest are buried in it. Substring containment is not a comparison —
/// `rotate.log.old` contains `rotate.log` — so the string is tokenized with
/// `cmd`'s quoting rules and each element is compared as a path.
#[derive(Debug, Default, PartialEq, Eq)]
struct SchtasksInvocation {
    binary: Option<String>,
    manifest: Option<String>,
    log: Option<String>,
}

/// Parse the `/TR` shape [`crate::schedule::schtasks_create_args`] renders:
/// `cmd /c [set "VAR=v" && ][cd /d "<dir>" && ]"<xv>" schedule run --manifest
/// "<manifest>" >> "<log>" 2>&1`.
fn parse_schtasks_invocation(command: &str) -> SchtasksInvocation {
    let tokens = windows_tokenize(command);
    // The executable is whatever runs `schedule run`, wherever the `set`/`cd`
    // prelude ends.
    let binary = tokens
        .windows(2)
        .position(|pair| pair[0] == "schedule" && pair[1] == "run")
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| tokens.get(index).cloned());
    let manifest = manifest_argument_of(&tokens).map(str::to_string);
    let log = tokens.iter().enumerate().find_map(|(index, token)| {
        // Both `>> "log"` and `>>"log"` reach us, and `1>>` is the same
        // redirect spelled out.
        let rest = token
            .strip_prefix(">>")
            .or_else(|| token.strip_prefix("1>>"))?;
        if rest.is_empty() {
            tokens.get(index + 1).cloned()
        } else {
            Some(rest.to_string())
        }
    });
    SchtasksInvocation {
        binary,
        manifest,
        log,
    }
}

/// `cmd`'s tokenization, enough of it: whitespace separates, double quotes
/// group, and there is no backslash escape inside them (a Windows path ends in
/// `\\` often enough that treating it as an escape would corrupt paths).
fn windows_tokenize(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
}

/// The executable and the manifest argument, from an argv the unit carries.
///
/// Compared element by element. An earlier version joined the argv back into a
/// single string and re-parsed it, which turned `--manifest /s p/manifest.json`
/// into the token `/s` and refused a perfectly healthy install whose state
/// directory happened to contain a space. An argv is already the answer; there
/// is nothing to re-lex.
fn check_command(
    report: &mut UnitDriftReport,
    platform: Platform,
    argv: &[String],
    manifest: &ScheduleManifestV1,
    manifest_path: &Path,
) {
    match argv.first() {
        Some(binary) if same_path(platform, binary, &manifest.execution.binary_path) => {}
        _ => report.push(UNIT_COMMAND),
    }
    match manifest_argument_of(argv) {
        Some(recorded) if same_path(platform, recorded, &manifest_path.to_string_lossy()) => {}
        _ => report.push(UNIT_COMMAND),
    }
}

/// The value of `--manifest` in an argv: the following element, or the tail of
/// a `--manifest=<path>` element. Never a substring of anything else.
fn manifest_argument_of(argv: &[String]) -> Option<&str> {
    for (index, argument) in argv.iter().enumerate() {
        if argument == "--manifest" {
            return argv
                .get(index + 1)
                .map(String::as_str)
                .filter(|v| !v.is_empty());
        }
        if let Some(value) = argument.strip_prefix("--manifest=") {
            return (!value.is_empty()).then_some(value);
        }
    }
    None
}

/// What reading an owned unit path produced.
enum UnitText {
    /// A unit carrying our marker.
    Owned(String),
    /// Absent, or something we did not write: ownership's story, not drift's.
    NotOurs,
    /// Present but unreadable — a permission problem, usually. `status` must
    /// still answer every other dimension, so this is reported rather than
    /// raised.
    Unreadable,
}

fn owned_unit_text(path: &Path) -> UnitText {
    match classify_owned_unit(path) {
        Ok(ArtifactState::Owned(bytes)) => {
            UnitText::Owned(String::from_utf8_lossy(&bytes).into_owned())
        }
        Ok(ArtifactState::Absent) | Ok(ArtifactState::Foreign(_)) => UnitText::NotOurs,
        Err(_) => UnitText::Unreadable,
    }
}

/// Path equality as the *inspected platform* defines it.
///
/// Windows compares paths case-insensitively and accepts either separator, so
/// a task registered as `C:/Users/alice/bin/XV.EXE` names the same executable
/// the manifest recorded as `C:\\Users\\alice\\bin\\xv.exe`; refusing that would be a
/// false accusation. Unix paths are bytes and are compared as such.
///
/// The platform is a parameter rather than a `cfg`, because the caller may be
/// inspecting a platform it is not running on — every renderer test does.
fn same_path(platform: Platform, left: &str, right: &str) -> bool {
    match platform {
        Platform::Schtasks => windows_path_key(left) == windows_path_key(right),
        _ => Path::new(left) == Path::new(right),
    }
}

fn windows_path_key(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// The recorded cadence as an interval, when its `kind` is one this build
/// renders. An unrecognized kind is a manifest problem, not a unit problem, so
/// the cadence comparison is skipped rather than reported here.
pub(crate) fn manifest_interval(cadence: &ManifestCadence) -> Option<ScheduleInterval> {
    let hour = u32::from(cadence.hour);
    let minute = u32::from(cadence.minute);
    match cadence.kind.as_str() {
        "hourly" => Some(ScheduleInterval::Hourly { minute }),
        "daily" => Some(ScheduleInterval::Daily { hour, minute }),
        // The installer only ever records Sunday for weekly, and the manifest
        // has no weekday field to record anything else.
        "weekly" => Some(ScheduleInterval::Weekly {
            weekday: 0,
            hour,
            minute,
        }),
        _ => None,
    }
}

/// `<key>K</key> <string>V</string>` from a plist, unescaped.
///
/// The unescaping matters: a log path containing `&` is written as `&amp;`,
/// and comparing the escaped text with the manifest would refuse a healthy
/// install.
fn plist_string_value(text: &str, key: &str) -> Option<String> {
    let after = text.split_once(&format!("<key>{key}</key>"))?.1;
    let value = after.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(xml_unescape(value.trim()))
}

/// The `StartCalendarInterval` dict as ordered key/value pairs.
fn plist_calendar_fields(text: &str) -> Vec<(String, String)> {
    let Some(after) = text.split_once("<key>StartCalendarInterval</key>") else {
        return Vec::new();
    };
    let Some(body) = after
        .1
        .split_once("<dict>")
        .and_then(|(_, rest)| rest.split_once("</dict>"))
    else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    let mut rest = body.0;
    while let Some((_, tail)) = rest.split_once("<key>") {
        let Some((key, tail)) = tail.split_once("</key>") else {
            break;
        };
        let Some((_, tail)) = tail.split_once("<integer>") else {
            break;
        };
        let Some((value, tail)) = tail.split_once("</integer>") else {
            break;
        };
        pairs.push((key.trim().to_string(), value.trim().to_string()));
        rest = tail;
    }
    pairs
}

/// The path in `StandardOutput=append:<path>`.
fn systemd_append_target(text: &str) -> Option<String> {
    let value = field_value(text, "StandardOutput")?;
    value
        .strip_prefix("append:")
        .map(|path| path.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::manifest::{
        test_paths_in, ManifestExecution, ManifestTarget, SCHEDULE_ID,
    };
    use crate::schedule::outcome::{write_outcome_atomic, RunSummary};
    use crate::schedule::{
        fixture_abs, render, CommandOutput, RotationSchedule, ScheduleCommand, UnitFile,
    };
    use std::collections::{BTreeMap, HashMap};
    use std::path::PathBuf;
    use std::sync::Mutex;

    // -----------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------

    #[derive(Debug, Default)]
    struct FakeRunner {
        answers: HashMap<String, CommandOutput>,
        default: Option<CommandOutput>,
        spawn_fails: bool,
        calls: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self::default()
        }

        fn answering(mut self, contains: &str, status: i32, stdout: &str, stderr: &str) -> Self {
            self.answers.insert(
                contains.to_string(),
                CommandOutput {
                    status,
                    stdout: stdout.to_string(),
                    stderr: stderr.to_string(),
                },
            );
            self
        }

        fn otherwise(mut self, status: i32, stdout: &str, stderr: &str) -> Self {
            self.default = Some(CommandOutput {
                status,
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            });
            self
        }

        fn spawn_failure() -> Self {
            Self {
                spawn_fails: true,
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.lock().unwrap().push(line.clone());
            if self.spawn_fails {
                return Err(crate::error::CrosstacheError::config(format!(
                    "failed to run {program}"
                )));
            }
            for (needle, answer) in &self.answers {
                if line.contains(needle) {
                    return Ok(answer.clone());
                }
            }
            Ok(self.default.clone().unwrap_or_else(|| CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            }))
        }
    }

    fn registered() -> FakeRunner {
        FakeRunner::new()
            .answering(
                "systemctl --user show",
                0,
                "LoadState=loaded\nActiveState=active\nUnitFileState=enabled\n",
                "",
            )
            .otherwise(0, "", "")
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        units: UnitPaths,
        state: ScheduleStatePaths,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let units = UnitPaths {
            dir: tmp.path().join("units"),
        };
        let state = test_paths_in(&tmp.path().join("state"));
        std::fs::create_dir_all(&units.dir).unwrap();
        Fixture {
            _tmp: tmp,
            units,
            state,
        }
    }

    fn pinned_schedule(manifest: &Path, interval: ScheduleInterval, log: &str) -> RotationSchedule {
        RotationSchedule {
            interval,
            command: ScheduleCommand::ManifestRun {
                manifest: manifest.to_path_buf(),
                working_directory: PathBuf::from(fixture_abs("/home/alice/work")),
            },
            binary: PathBuf::from(fixture_abs("/home/alice/bin/xv")),
            log_path: PathBuf::from(fixture_abs(log)),
            home: PathBuf::from(fixture_abs("/home/alice")),
            state_home: None,
        }
    }

    fn digest() -> String {
        format!("sha256:{}", "0".repeat(64))
    }

    /// The manifest an install of `schedule` would have written.
    fn manifest_for(schedule: &RotationSchedule) -> ScheduleManifestV1 {
        let (kind, hour, minute) = match schedule.interval {
            ScheduleInterval::Hourly { minute } => ("hourly", 0, minute),
            ScheduleInterval::Daily { hour, minute } => ("daily", hour, minute),
            ScheduleInterval::Weekly { hour, minute, .. } => ("weekly", hour, minute),
        };
        ScheduleManifestV1 {
            schema_version: 1,
            schedule_id: SCHEDULE_ID.to_string(),
            installed_at: "2026-09-10T03:00:00Z".to_string(),
            cadence: ManifestCadence {
                kind: kind.to_string(),
                hour: hour as u8,
                minute: minute as u8,
            },
            execution: ManifestExecution {
                binary_path: schedule.binary.to_string_lossy().to_string(),
                installed_version: "0.39.0".to_string(),
                working_directory: fixture_abs("/home/alice/work"),
                log_path: schedule.log_path.to_string_lossy().to_string(),
            },
            target: ManifestTarget {
                config_path: fixture_abs("/home/alice/.config/xv/xv.conf"),
                config_digest: digest(),
                project_path: None,
                project_digest: None,
                environment: None,
                context_path: None,
                context_digest: None,
                workspace_source: "degenerate".to_string(),
                workspace_alias: None,
                backend_name: "local".to_string(),
                backend_kind: "local".to_string(),
                backend_identity: digest(),
                vault: "payments".to_string(),
                vault_selection: "explicit".to_string(),
            },
        }
    }

    fn seed_units(platform: Platform, schedule: &RotationSchedule, units: &UnitPaths) {
        std::fs::create_dir_all(&units.dir).unwrap();
        for UnitFile { path, contents } in render(platform, schedule, units) {
            std::fs::write(path, contents).unwrap();
        }
    }

    /// Write `manifest` where the state paths say it lives, and return the
    /// digest of the bytes actually written.
    fn seed_manifest(state: &ScheduleStatePaths, manifest: &ScheduleManifestV1) -> String {
        std::fs::create_dir_all(state.root()).unwrap();
        let bytes = crate::schedule::manifest::serialize_manifest(manifest);
        std::fs::write(state.manifest_path(), &bytes).unwrap();
        crate::config::content_digest(&bytes)
    }

    /// Every file under `root`, with its bytes: the "status wrote nothing"
    /// evidence.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        files
    }

    // -----------------------------------------------------------------------
    // Scheduler state
    // -----------------------------------------------------------------------

    #[test]
    fn the_scheduler_dimension_separates_installed_absent_error_and_unreadable() {
        use crate::schedule::ownership::probe_scheduler;

        assert_eq!(
            probe_scheduler(Platform::Launchd, &registered()),
            SchedulerState::Installed
        );
        assert_eq!(
            probe_scheduler(
                Platform::Launchd,
                &FakeRunner::new().otherwise(113, "", "Could not find service \"x\"")
            ),
            SchedulerState::Absent
        );
        assert_eq!(
            probe_scheduler(
                Platform::Launchd,
                &FakeRunner::new().otherwise(74, "secret /Users/alice/token", "Bad request.")
            ),
            SchedulerState::Error("launchctl print failed (exit 74)".to_string())
        );
        assert!(matches!(
            probe_scheduler(Platform::Schtasks, &FakeRunner::spawn_failure()),
            SchedulerState::Error(detail) if detail.contains("could not be run")
        ));
    }

    #[test]
    fn a_systemctl_answer_we_cannot_read_is_unknown_not_absent() {
        use crate::schedule::ownership::probe_scheduler;

        // `show` exited 0 and printed nothing we recognize: that is not the
        // same as it telling us the timer does not exist.
        let runner = FakeRunner::new().answering("systemctl --user show", 0, "\n\n", "");
        assert_eq!(
            probe_scheduler(Platform::Systemd, &runner),
            SchedulerState::Unknown
        );
        // ... whereas an answer that *does* say so stays absence.
        let runner = FakeRunner::new().answering(
            "systemctl --user show",
            0,
            "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
            "",
        );
        assert_eq!(
            probe_scheduler(Platform::Systemd, &runner),
            SchedulerState::Absent
        );
    }

    // -----------------------------------------------------------------------
    // Next-run parsers
    // -----------------------------------------------------------------------

    #[test]
    fn systemd_reports_a_next_run_only_when_the_value_is_utc() {
        assert_eq!(
            parse_systemd_next_run("NextElapseUSecRealtime=Thu 2026-09-10 03:00:00 UTC\n"),
            NextRun::At("2026-09-10T03:00:00Z".to_string())
        );
        // A zone abbreviation is not an instant without a timezone database.
        assert_eq!(
            parse_systemd_next_run("NextElapseUSecRealtime=Thu 2026-09-10 05:00:00 CEST\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_systemd_next_run("NextElapseUSecRealtime=n/a\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_systemd_next_run("NextElapseUSecRealtime=\n"),
            NextRun::Unknown
        );
        assert_eq!(parse_systemd_next_run(""), NextRun::Unknown);
        // The raw microsecond form some versions print.
        assert_eq!(
            parse_systemd_next_run("NextElapseUSecRealtime=1789016400000000\n"),
            NextRun::At("2026-09-10T05:00:00Z".to_string())
        );
    }

    #[test]
    fn launchctl_reports_a_next_run_only_from_an_offset_bearing_date() {
        assert_eq!(
            parse_launchctl_next_run("\tnext fire date = 2026-09-10 03:00:00 -0700\n"),
            NextRun::At("2026-09-10T10:00:00Z".to_string())
        );
        assert_eq!(
            parse_launchctl_next_run("\tnext fire date = 2026-09-10T03:00:00Z\n"),
            NextRun::At("2026-09-10T03:00:00Z".to_string())
        );
        // The common case: launchd printed no such field at all.
        assert_eq!(
            parse_launchctl_next_run("com.crosstache.xv-rotate = {\n\tactive count = 0\n}\n"),
            NextRun::Unknown
        );
        // A human phrasing is not a time.
        assert_eq!(
            parse_launchctl_next_run("\tnext fire date = in 4 hours\n"),
            NextRun::Unknown
        );
    }

    #[test]
    fn schtasks_never_guesses_a_locale_formatted_next_run() {
        // A value that names its own offset is unambiguous, in either form.
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 2026-09-10T03:00:00Z\r\n"),
            NextRun::At("2026-09-10T03:00:00Z".to_string())
        );
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 2026-09-10T03:00:00-07:00\r\n"),
            NextRun::At("2026-09-10T10:00:00Z".to_string())
        );
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 2026-09-10 03:00:00 -0700\r\n"),
            NextRun::At("2026-09-10T10:00:00Z".to_string())
        );
        // The shape `schtasks` actually prints: a bare local wall clock. The
        // host's *current* UTC offset is not the offset that will be in force
        // on the far side of a DST boundary, so this is refused rather than
        // guessed.
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 2026-09-10 03:00:00\r\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 2026-09-10T03:00:00\r\n"),
            NextRun::Unknown
        );
        // `3/9/2026` is two different days depending on the host's locale.
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 3/9/2026 3:00:00 AM\r\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: 10.09.2026 03:00:00\r\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: N/A\r\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_schtasks_next_run("Next Run Time: Disabled\r\n"),
            NextRun::Unknown
        );
        assert_eq!(
            parse_schtasks_next_run("Status: Ready\r\n"),
            NextRun::Unknown
        );
    }

    #[test]
    fn the_systemd_next_run_invocation_is_pinned_and_falls_back_once() {
        // The `--timestamp=utc` form is what makes the answer parseable, so it
        // is asked for first...
        let runner = FakeRunner::new().otherwise(1, "", "Unknown option --timestamp");
        assert_eq!(probe_next_run(Platform::Systemd, &runner), NextRun::Unknown);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].contains("--timestamp=utc"), "{calls:?}");
        assert!(
            calls[0].contains("--property=NextElapseUSecRealtime"),
            "{calls:?}"
        );
        // ... and a systemd too old to know it is asked again without it.
        assert!(!calls[1].contains("--timestamp"), "{calls:?}");
    }

    #[test]
    fn a_scheduler_that_cannot_be_run_yields_an_unknown_next_run() {
        for platform in [Platform::Launchd, Platform::Systemd, Platform::Schtasks] {
            assert_eq!(
                probe_next_run(platform, &FakeRunner::spawn_failure()),
                NextRun::Unknown,
                "{platform:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Unit drift
    // -----------------------------------------------------------------------

    fn unix_platforms() -> [Platform; 2] {
        [Platform::Launchd, Platform::Systemd]
    }

    #[test]
    fn a_unit_that_matches_its_manifest_has_no_unit_drift() {
        for platform in unix_platforms() {
            let f = fixture();
            let schedule = pinned_schedule(
                &f.state.manifest_path(),
                ScheduleInterval::Daily { hour: 3, minute: 0 },
                "/home/alice/rotate.log",
            );
            let manifest = manifest_for(&schedule);
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert!(report.is_empty(), "{platform:?}: {report:?}");
        }
    }

    #[test]
    fn a_unit_naming_another_manifest_is_command_drift() {
        for platform in unix_platforms() {
            let f = fixture();
            let elsewhere = f.state.root().join("other-manifest.json");
            let schedule = pinned_schedule(
                &elsewhere,
                ScheduleInterval::Daily { hour: 3, minute: 0 },
                "/home/alice/rotate.log",
            );
            let mut manifest = manifest_for(&schedule);
            manifest.execution.log_path = fixture_abs("/home/alice/rotate.log");
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert_eq!(fields(&report), vec![UNIT_COMMAND], "{platform:?}");
        }
    }

    #[test]
    fn a_unit_running_another_executable_is_command_drift() {
        for platform in unix_platforms() {
            let f = fixture();
            let schedule = pinned_schedule(
                &f.state.manifest_path(),
                ScheduleInterval::Daily { hour: 3, minute: 0 },
                "/home/alice/rotate.log",
            );
            let mut manifest = manifest_for(&schedule);
            manifest.execution.binary_path = fixture_abs("/opt/other/xv");
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert_eq!(fields(&report), vec![UNIT_COMMAND], "{platform:?}");
        }
    }

    #[test]
    fn a_unit_with_another_cadence_is_cadence_drift() {
        for platform in unix_platforms() {
            let f = fixture();
            let schedule = pinned_schedule(
                &f.state.manifest_path(),
                ScheduleInterval::Daily { hour: 3, minute: 0 },
                "/home/alice/rotate.log",
            );
            let mut manifest = manifest_for(&schedule);
            manifest.cadence = ManifestCadence {
                kind: "daily".to_string(),
                hour: 17,
                minute: 45,
            };
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert_eq!(fields(&report), vec![UNIT_CADENCE], "{platform:?}");
        }
    }

    #[test]
    fn a_unit_logging_somewhere_else_is_log_path_drift() {
        for platform in unix_platforms() {
            let f = fixture();
            let schedule = pinned_schedule(
                &f.state.manifest_path(),
                ScheduleInterval::Daily { hour: 3, minute: 0 },
                "/home/alice/rotate.log",
            );
            let mut manifest = manifest_for(&schedule);
            manifest.execution.log_path = fixture_abs("/home/alice/elsewhere.log");
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert_eq!(fields(&report), vec![UNIT_LOG_PATH], "{platform:?}");
        }
    }

    #[test]
    fn a_registered_task_naming_another_manifest_is_command_drift() {
        let f = fixture();
        let schedule = pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        );
        let manifest = manifest_for(&schedule);
        let listing = format!(
            "TaskName:      \\crosstache-xv-rotate\r\n\
             Task To Run:   cmd /c {} schedule run --manifest C:\\elsewhere\\manifest.json >> \"{}\" 2>&1\r\n",
            manifest.execution.binary_path, manifest.execution.log_path
        );
        let runner = registered()
            .answering("/V /FO LIST", 0, &listing, "")
            .answering("/XML", 1, "", "ERROR");
        let report =
            inspect_unit_drift(Platform::Schtasks, &f.units, &f.state, &manifest, &runner).unwrap();
        assert_eq!(fields(&report), vec![UNIT_COMMAND]);
    }

    #[test]
    fn a_unit_drift_reason_names_the_field_and_asks_for_a_reinstall() {
        let mut report = UnitDriftReport::default();
        report.push(UNIT_COMMAND);
        // One reason per field: two bad arguments are one repair.
        report.push(UNIT_COMMAND);
        assert_eq!(report.reasons.len(), 1);
        assert_eq!(
            report.reasons[0].detail,
            "unit_command differs between the installed unit and the manifest; reinstall"
        );
    }

    /// A manifest, a binary and a log under paths that contain spaces — and,
    /// for the plist, an `&` that has to survive XML escaping. The earlier
    /// joined-string comparison refused every one of these.
    fn spaced_schedule(manifest: &Path) -> RotationSchedule {
        RotationSchedule {
            interval: ScheduleInterval::Daily { hour: 3, minute: 0 },
            command: ScheduleCommand::ManifestRun {
                manifest: manifest.to_path_buf(),
                working_directory: PathBuf::from(fixture_abs("/home/alice/my work")),
            },
            binary: PathBuf::from(fixture_abs("/home/alice/my bin/xv")),
            log_path: PathBuf::from(fixture_abs("/home/alice/my logs/rotate & audit.log")),
            home: PathBuf::from(fixture_abs("/home/alice")),
            state_home: None,
        }
    }

    /// A fixture whose state root and unit directory both contain a space.
    fn spaced_fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let units = UnitPaths {
            dir: tmp.path().join("my units"),
        };
        let state = test_paths_in(&tmp.path().join("my state"));
        std::fs::create_dir_all(&units.dir).unwrap();
        Fixture {
            _tmp: tmp,
            units,
            state,
        }
    }

    #[test]
    fn a_healthy_install_under_spaced_paths_is_not_drift() {
        for platform in unix_platforms() {
            let f = spaced_fixture();
            assert!(
                f.state.manifest_path().to_string_lossy().contains(' '),
                "the fixture must actually exercise a spaced path"
            );
            let schedule = spaced_schedule(&f.state.manifest_path());
            let mut manifest = manifest_for(&schedule);
            manifest.execution.working_directory = fixture_abs("/home/alice/my work");
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert!(report.is_empty(), "{platform:?}: {report:?}");
        }
    }

    #[test]
    fn a_spaced_manifest_path_that_really_differs_is_still_drift() {
        for platform in unix_platforms() {
            let f = spaced_fixture();
            // The same spaced prefix, a different file: a comparison that
            // truncated at the first space would call this healthy.
            let installed = f.state.manifest_path().with_file_name("manifest.json.old");
            let schedule = spaced_schedule(&installed);
            let mut manifest = manifest_for(&schedule);
            manifest.execution.working_directory = fixture_abs("/home/alice/my work");
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert_eq!(fields(&report), vec![UNIT_COMMAND], "{platform:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Task Scheduler command parsing
    // -----------------------------------------------------------------------

    /// The `/TR` string `schtasks_create_args` renders, for a given executable,
    /// manifest, working directory and log.
    fn task_command(binary: &str, manifest: &str, dir: &str, log: &str) -> String {
        format!("cmd /c cd /d \"{dir}\" && \"{binary}\" schedule run --manifest \"{manifest}\" >> \"{log}\" 2>&1")
    }

    fn schtasks_report(
        state: &ScheduleStatePaths,
        manifest: &ScheduleManifestV1,
        units: &UnitPaths,
        command: &str,
    ) -> UnitDriftReport {
        let listing = format!(
            "TaskName:      \\crosstache-xv-rotate\r\nTask To Run:   {command}\r\nStatus:        Ready\r\n"
        );
        let runner = registered()
            .answering("/V /FO LIST", 0, &listing, "")
            // The cadence is checked from the XML, which this fixture does not
            // provide; a failed query contributes no reason.
            .answering("/XML", 1, "", "ERROR");
        inspect_unit_drift(Platform::Schtasks, units, state, manifest, &runner).unwrap()
    }

    fn windows_fixture() -> (Fixture, ScheduleManifestV1) {
        let f = fixture();
        let mut manifest = manifest_for(&pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        ));
        manifest.execution.binary_path = "C:\\Program Files\\xv\\xv.exe".to_string();
        manifest.execution.log_path = "C:\\Users\\alice\\state\\rotate.log".to_string();
        (f, manifest)
    }

    #[test]
    fn a_registered_task_matching_the_manifest_has_no_unit_drift() {
        let (f, manifest) = windows_fixture();
        let command = task_command(
            &manifest.execution.binary_path,
            &f.state.manifest_path().to_string_lossy(),
            "C:\\Users\\alice\\my work",
            &manifest.execution.log_path,
        );
        let report = schtasks_report(&f.state, &manifest, &f.units, &command);
        assert!(report.is_empty(), "{report:?}");
    }

    #[test]
    fn a_registered_task_differing_only_in_case_or_separator_is_not_drift() {
        let (f, manifest) = windows_fixture();
        let command = task_command(
            "C:/Program Files/XV/XV.EXE",
            &f.state.manifest_path().to_string_lossy(),
            "C:\\Users\\alice\\my work",
            "C:/Users/Alice/state/Rotate.log",
        );
        let report = schtasks_report(&f.state, &manifest, &f.units, &command);
        assert!(report.is_empty(), "{report:?}");
    }

    #[test]
    fn a_registered_task_logging_to_a_longer_path_is_drift() {
        // The containment bug in one test: `rotate.log.old` contains
        // `rotate.log`.
        let (f, manifest) = windows_fixture();
        let command = task_command(
            &manifest.execution.binary_path,
            &f.state.manifest_path().to_string_lossy(),
            "C:\\Users\\alice\\my work",
            "C:\\Users\\alice\\state\\rotate.log.old",
        );
        let report = schtasks_report(&f.state, &manifest, &f.units, &command);
        assert_eq!(fields(&report), vec![UNIT_LOG_PATH]);
    }

    #[test]
    fn a_registered_task_running_another_executable_is_drift() {
        let (f, manifest) = windows_fixture();
        let command = task_command(
            "C:\\Program Files\\xv\\xv.exe.bak",
            &f.state.manifest_path().to_string_lossy(),
            "C:\\Users\\alice\\my work",
            &manifest.execution.log_path,
        );
        let report = schtasks_report(&f.state, &manifest, &f.units, &command);
        assert_eq!(fields(&report), vec![UNIT_COMMAND]);
    }

    #[test]
    fn the_task_command_is_parsed_into_its_pieces() {
        let parsed = parse_schtasks_invocation(&task_command(
            "C:\\Program Files\\xv\\xv.exe",
            "C:\\Users\\alice\\my state\\manifest.json",
            "C:\\work dir",
            "C:\\logs\\rotate & audit.log",
        ));
        assert_eq!(
            parsed,
            SchtasksInvocation {
                binary: Some("C:\\Program Files\\xv\\xv.exe".to_string()),
                manifest: Some("C:\\Users\\alice\\my state\\manifest.json".to_string()),
                log: Some("C:\\logs\\rotate & audit.log".to_string()),
            }
        );
        // The state-root `set` prelude and an unspaced redirect both parse.
        let parsed = parse_schtasks_invocation(
            "cmd /c set \"XV_STATE_HOME=C:\\s\" && \"C:\\xv.exe\" schedule run --manifest \"C:\\m.json\" >>\"C:\\r.log\" 2>&1",
        );
        assert_eq!(parsed.binary.as_deref(), Some("C:\\xv.exe"));
        assert_eq!(parsed.manifest.as_deref(), Some("C:\\m.json"));
        assert_eq!(parsed.log.as_deref(), Some("C:\\r.log"));
    }

    // -----------------------------------------------------------------------
    // Ordering and unreadable units
    // -----------------------------------------------------------------------

    #[test]
    fn unit_drift_reasons_come_out_in_a_fixed_order() {
        for platform in unix_platforms() {
            let f = fixture();
            let schedule = pinned_schedule(
                &f.state.root().join("other.json"),
                ScheduleInterval::Daily { hour: 3, minute: 0 },
                "/home/alice/elsewhere.log",
            );
            let mut manifest = manifest_for(&schedule);
            manifest.execution.log_path = fixture_abs("/home/alice/rotate.log");
            manifest.cadence.hour = 17;
            seed_units(platform, &schedule, &f.units);
            let report =
                inspect_unit_drift(platform, &f.units, &f.state, &manifest, &registered()).unwrap();
            assert_eq!(
                fields(&report),
                vec![UNIT_COMMAND, UNIT_CADENCE, UNIT_LOG_PATH],
                "{platform:?}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_unit_is_reported_instead_of_aborting_status() {
        use std::os::unix::fs::PermissionsExt;

        let f = fixture();
        let schedule = pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        );
        let manifest = manifest_for(&schedule);
        seed_manifest(&f.state, &manifest);
        seed_units(Platform::Systemd, &schedule, &f.units);
        let service = f.units.dir.join("xv-rotate.service");
        std::fs::set_permissions(&service, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&service).is_ok() {
            // Running as root: the mode proves nothing, so there is nothing to
            // assert here.
            return;
        }

        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&manifest.execution.binary_path),
            "0.39.0",
        )
        .await
        .unwrap();

        // Every other dimension still answered...
        assert_eq!(report.scheduler, SchedulerState::Installed);
        assert!(report.manifest.is_some());
        // ... and the unreadable unit is reported, not raised.
        let unit_drift = report.unit_drift.expect("a manifest was loaded");
        assert_eq!(fields(&unit_drift), vec![UNIT_COMMAND]);
        assert!(
            unit_drift.reasons[0].detail.contains("could not be read"),
            "{:?}",
            unit_drift.reasons[0]
        );
        // Ownership cannot claim a unit it could not open.
        assert!(
            matches!(report.ownership, Ownership::Foreign { ref paths } if paths == &vec![service.clone()]),
            "{:?}",
            report.ownership
        );

        std::fs::set_permissions(&service, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn fields(report: &UnitDriftReport) -> Vec<&'static str> {
        report.reasons.iter().map(|reason| reason.field).collect()
    }

    // -----------------------------------------------------------------------
    // collect_status
    // -----------------------------------------------------------------------

    fn outcome_at(digest: &str, state: RunState) -> RunOutcomeV1 {
        RunOutcomeV1 {
            schema_version: 1,
            schedule_id: SCHEDULE_ID.to_string(),
            manifest_digest: digest.to_string(),
            started_at: "2026-09-10T03:00:00Z".to_string(),
            finished_at: match state {
                RunState::Running => None,
                _ => Some("2026-09-10T03:00:02Z".to_string()),
            },
            state,
            exit_code: match state {
                RunState::Running => None,
                _ => Some(0),
            },
            summary: match state {
                RunState::Running => None,
                _ => Some(RunSummary {
                    policy_managed: 12,
                    due: 2,
                    rotated: 2,
                    failed: 0,
                }),
            },
            diagnostic: None,
        }
    }

    #[tokio::test]
    async fn nothing_installed_reports_absence_without_inventing_anything() {
        let f = fixture();
        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &FakeRunner::new().answering(
                "systemctl --user show",
                0,
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
                "",
            ),
            Path::new(&fixture_abs("/home/alice/bin/xv")),
            "0.40.0",
        )
        .await
        .unwrap();
        assert_eq!(report.ownership, Ownership::Absent);
        assert_eq!(report.scheduler, SchedulerState::Absent);
        assert_eq!(report.next_run, NextRun::Unknown);
        assert!(report.manifest.is_none());
        assert!(report.manifest_error.is_none());
        assert!(report.drift.is_none());
        assert!(report.unit_drift.is_none());
        assert!(report.executable.is_none());
        assert_eq!(report.last_run, LastRunStatus::Never);
        assert_eq!(report.log, LogStatus::Unknown);
    }

    #[tokio::test]
    async fn a_managed_install_reports_its_executable_and_its_last_run() {
        let f = fixture();
        let schedule = pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        );
        let manifest = manifest_for(&schedule);
        let digest = seed_manifest(&f.state, &manifest);
        seed_units(Platform::Systemd, &schedule, &f.units);
        write_outcome_atomic(&f.state, &outcome_at(&digest, RunState::Success)).unwrap();

        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&manifest.execution.binary_path),
            "0.40.0",
        )
        .await
        .unwrap();

        assert_eq!(report.ownership, Ownership::Managed);
        assert_eq!(report.scheduler, SchedulerState::Installed);
        let executable = report.executable.expect("a manifest records an executable");
        assert_eq!(executable.recorded_path, manifest.execution.binary_path);
        assert_eq!(executable.installed_version, "0.39.0");
        assert_eq!(executable.current_version, "0.40.0");
        assert!(executable.current_matches_path);
        assert!(report.unit_drift.expect("a unit was seeded").is_empty());
        // The recorded target points at files that do not exist here, so drift
        // is computed and refuses — the point is that it was computed at all,
        // from files only.
        assert!(report.drift.is_some());
        assert_eq!(report.log, LogStatus::NotYetWritten);
        match report.last_run {
            LastRunStatus::Outcome {
                previous_install, ..
            } => assert!(!previous_install),
            other => panic!("expected a completed outcome, got {other:?}"),
        }
    }

    /// `systemctl --user disable --now` leaves our files exactly where install
    /// wrote them, so ownership still reads `managed` from the bytes while the
    /// scheduler has no record of the timer. `collect_status` must keep the two
    /// dimensions apart and report the second honestly — the rendered block
    /// turns that into an `[error]`, and nothing here may launder it into
    /// "installed".
    #[tokio::test]
    async fn a_managed_unit_the_scheduler_deregistered_is_managed_and_not_registered() {
        let f = fixture();
        let schedule = pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        );
        let manifest = manifest_for(&schedule);
        seed_manifest(&f.state, &manifest);
        seed_units(Platform::Systemd, &schedule, &f.units);

        // The timer file is gone from the user manager's view; the unit files
        // on disk are untouched.
        let deregistered = FakeRunner::new()
            .answering(
                "systemctl --user show",
                0,
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
                "",
            )
            .otherwise(0, "", "");

        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &deregistered,
            Path::new(&manifest.execution.binary_path),
            "0.39.0",
        )
        .await
        .unwrap();

        assert_eq!(report.ownership, Ownership::Managed);
        assert_eq!(report.scheduler, SchedulerState::Absent);
        // The recorded target names files this fixture does not have, so drift
        // refuses too and owns the headline; the deregistration is still
        // visible as its own dimension, and the command still fails.
        let rendered =
            crate::schedule::status_render::render_status(&report, Platform::Systemd, false);
        assert!(rendered.contains("  Ownership: managed"), "{rendered}");
        assert!(
            rendered.contains("  Scheduler: not registered"),
            "a job the scheduler has never heard of must say so:\n{rendered}"
        );
        assert!(
            !rendered.contains("[ok]"),
            "a schedule that cannot fire is not a healthy one:\n{rendered}"
        );
        assert!(
            crate::schedule::status_render::status_failure(&report, Platform::Systemd).is_some()
        );
    }

    #[tokio::test]
    async fn an_outcome_from_another_installation_is_labelled_previous_install() {
        let f = fixture();
        let schedule = pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        );
        let manifest = manifest_for(&schedule);
        seed_manifest(&f.state, &manifest);
        write_outcome_atomic(
            &f.state,
            &outcome_at(&format!("sha256:{}", "1".repeat(64)), RunState::Success),
        )
        .unwrap();

        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&manifest.execution.binary_path),
            "0.39.0",
        )
        .await
        .unwrap();
        match report.last_run {
            LastRunStatus::Outcome {
                previous_install, ..
            } => assert!(previous_install),
            other => panic!("expected an outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_running_record_is_running_only_while_the_lock_is_held() {
        let f = fixture();
        write_outcome_atomic(&f.state, &outcome_at(&digest(), RunState::Running)).unwrap();

        // Nobody holds the lock: the runner died without recording an ending.
        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&fixture_abs("/home/alice/bin/xv")),
            "0.39.0",
        )
        .await
        .unwrap();
        assert_eq!(
            report.last_run,
            LastRunStatus::Interrupted {
                started_at: "2026-09-10T03:00:00Z".to_string()
            }
        );

        // With the lock actually held, the same record means a live sweep.
        let held = RunGuard::try_acquire(&f.state)
            .unwrap()
            .expect("the lock is free in this test");
        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&fixture_abs("/home/alice/bin/xv")),
            "0.39.0",
        )
        .await
        .unwrap();
        assert_eq!(
            report.last_run,
            LastRunStatus::RunningHeld {
                started_at: "2026-09-10T03:00:00Z".to_string()
            }
        );
        drop(held);
    }

    #[tokio::test]
    async fn an_unreadable_last_run_record_is_reported_not_fatal() {
        let f = fixture();
        std::fs::create_dir_all(f.state.root()).unwrap();
        std::fs::write(f.state.last_run_path(), b"{ not json").unwrap();
        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&fixture_abs("/home/alice/bin/xv")),
            "0.39.0",
        )
        .await
        .unwrap();
        assert!(matches!(report.last_run, LastRunStatus::Unreadable(_)));
    }

    #[tokio::test]
    async fn a_malformed_manifest_is_reported_without_losing_the_rest() {
        let f = fixture();
        std::fs::create_dir_all(f.state.root()).unwrap();
        std::fs::write(f.state.manifest_path(), b"{\"schema_version\":1}").unwrap();
        let report = collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&fixture_abs("/home/alice/bin/xv")),
            "0.39.0",
        )
        .await
        .unwrap();
        assert!(report.manifest.is_none());
        assert!(report.manifest_error.is_some());
        assert!(report.drift.is_none());
        assert_eq!(report.scheduler, SchedulerState::Installed);
    }

    #[tokio::test]
    async fn status_writes_nothing() {
        let f = fixture();
        let schedule = pinned_schedule(
            &f.state.manifest_path(),
            ScheduleInterval::Daily { hour: 3, minute: 0 },
            "/home/alice/rotate.log",
        );
        let manifest = manifest_for(&schedule);
        let digest = seed_manifest(&f.state, &manifest);
        seed_units(Platform::Systemd, &schedule, &f.units);
        write_outcome_atomic(&f.state, &outcome_at(&digest, RunState::Success)).unwrap();

        let before = (snapshot(f.state.root()), snapshot(&f.units.dir));
        collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&manifest.execution.binary_path),
            "0.39.0",
        )
        .await
        .unwrap();
        let after = (snapshot(f.state.root()), snapshot(&f.units.dir));
        assert_eq!(
            before, after,
            "status changed what it was asked to describe"
        );
    }

    #[tokio::test]
    async fn status_on_a_bare_host_creates_no_state_directory() {
        // The nastiest version of the same rule: a machine with no schedule at
        // all must not gain a state directory or a `run.lock` inode just
        // because somebody asked.
        let f = fixture();
        let root = f.state.root().to_path_buf();
        collect_status(
            Platform::Systemd,
            &f.units,
            &f.state,
            &registered(),
            Path::new(&fixture_abs("/home/alice/bin/xv")),
            "0.39.0",
        )
        .await
        .unwrap();
        assert!(!root.exists(), "{} was created", root.display());
    }
}
