//! Turning a [`ScheduleStatusReport`] into the block a person reads.
//!
//! The layout is a contract:
//! `docs/superpowers/specs/2026-09-09-scheduled-target-manifest-goldens.md`
//! lines 94-206 fix the headlines, the dimension labels, their column and
//! their order; the design's "Status contract" fixes which dimensions exist at
//! all. The goldens' own preamble says the examples "define output structure
//! and wording" — so the wording and the ordering here are taken literally
//! from them, while a state whose golden is written as an excerpt (the drift
//! refusal and the orphaned manifest each show only the lines that example is
//! about) still renders the full dimension set. Suppressing `Config:` and
//! `Project:` in exactly the state where a config or project file changed
//! would be a worse diagnosis, not a more faithful one.
//!
//! Everything in this module is pure: it reads a collected report and returns
//! a string. No probing, no clock, no filesystem — `collect_status` already
//! did all of that, which is what makes every case below testable as a fixed
//! value.

use crate::schedule::drift::{field_rank, DriftReason, DriftReport, DriftVerdict};
use crate::schedule::manifest::ScheduleManifestV1;
use crate::schedule::outcome::{RunOutcomeV1, RunState};
use crate::schedule::ownership::{unverified_target_note, Ownership, SchedulerState};
use crate::schedule::status::manifest_interval;
use crate::schedule::status::{
    ExecutableStatus, LastRunStatus, LogStatus, NextRun, ScheduleStatusReport,
};
use crate::schedule::{quote_if_needed, Platform};
use crate::utils::output::{format_line, Level};

/// Width of the `Label:` column, including its colon. Every dimension line is
/// `"  " + label padded to this + value`, which is what makes the goldens'
/// values line up at one column.
const LABEL_WIDTH: usize = 11;

/// The placeholder used when the name the schedule was installed with is not
/// knowable.
const VAULT_PLACEHOLDER: &str = "<alias-or-vault>";

/// One indented dimension line.
fn dimension(label: &str, value: impl AsRef<str>) -> String {
    format!("  {label:<LABEL_WIDTH$}{}", value.as_ref())
}

/// Whether this report describes a schedule that would refuse its next run.
///
/// Two independent sources, both refusals per the design's drift table: the
/// recorded target no longer recomputing to the same thing, and the installed
/// unit no longer agreeing with the manifest it was rendered from.
pub(crate) fn status_refuses(report: &ScheduleStatusReport) -> bool {
    let target = report.drift.as_ref().is_some_and(DriftReport::is_refused);
    let unit = report
        .unit_drift
        .as_ref()
        .is_some_and(|drift| !drift.is_empty());
    target || unit
}

/// The headline, its severity, and whether the command fails — decided once.
///
/// One classification, so the prefix and the exit code cannot disagree. The
/// invariant is `failure.is_some() == (level == Level::Error)`, asserted by
/// `an_error_headline_always_fails_and_nothing_else_does`: every `[error]`
/// exits with the configuration-error code `3`, and every `[ok]`, `[warn]` and
/// `[info]` exits `0`.
struct Classification {
    level: Level,
    headline: String,
    /// The message `xv schedule status` fails with. `Some` iff `level` is
    /// [`Level::Error`].
    failure: Option<String>,
}

impl Classification {
    fn ok(headline: String) -> Self {
        Self {
            level: Level::Success,
            headline,
            failure: None,
        }
    }

    fn info(headline: String) -> Self {
        Self {
            level: Level::Info,
            headline,
            failure: None,
        }
    }

    fn warn(headline: String) -> Self {
        Self {
            level: Level::Warn,
            headline,
            failure: None,
        }
    }

    fn error(headline: String, failure: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            headline,
            failure: Some(failure.into()),
        }
    }
}

/// Decide the headline and the exit behavior from the whole report.
///
/// Exit-code policy, decided from the goldens: a `[warn]` or `[info]` state is a
/// successful diagnosis and exits `0` — an orphaned manifest, a legacy unit, a
/// foreign file and "nothing is installed" are all things `status` reports
/// accurately. An `[error]` state exits with the configuration-error code `3`,
/// the same code the scheduled run itself uses when it refuses: a schedule that
/// would refuse tonight, a manifest that cannot be read, a managed unit the
/// scheduler says it has never heard of, and a scheduler that could not be
/// queried at all. That makes `xv schedule status` usable as a
/// health check in a wrapper script without parsing its text.
///
/// The order of the arms is precedence, not taxonomy. A manifest that cannot be
/// read and a refusing target are more specific than the two scheduler-dimension
/// findings ("it has no such job" and "it would not answer"), and all of them
/// are fatal, so which one gets to name the headline never changes the exit
/// code.
fn classify(report: &ScheduleStatusReport, scheduler: &str) -> Classification {
    let scheduler_failure = match &report.scheduler {
        SchedulerState::Error(detail) => {
            Some(format!("the scheduler could not be queried: {detail}"))
        }
        _ => None,
    };

    // Nothing of ours on disk and a scheduler that would not answer: presence
    // is genuinely unknown, and that is the whole finding.
    if matches!(report.ownership, Ownership::Absent) {
        return match (&scheduler_failure, &report.scheduler) {
            (Some(failure), _) => Classification::error(
                format!(
                    "Could not determine whether a {scheduler} rotation schedule is installed."
                ),
                failure.clone(),
            ),
            (None, SchedulerState::Unknown) => Classification::warn(format!(
                "Could not confirm whether a {scheduler} rotation schedule is installed."
            )),
            _ => Classification::info(format!("No {scheduler} rotation schedule is installed.")),
        };
    }

    // A manifest that cannot be read is not a healthy schedule: the run it is
    // pinned to will refuse tonight, and status may not open with the healthy
    // headline and contradict itself three lines later. An *orphaned* unreadable
    // manifest is equally unusable — `install` cannot repair what it cannot
    // read — so it fails too.
    if report.manifest_error.is_some() {
        return match report.ownership {
            Ownership::OrphanedManifest => Classification::error(
                format!(
                    "A rotation manifest exists but could not be read, and no {scheduler} is \
                     installed."
                ),
                "the orphaned rotation manifest could not be read",
            ),
            _ => Classification::error(
                format!(
                    "The recorded target of the {scheduler} rotation schedule could not be read."
                ),
                "the recorded target of the installed rotation schedule could not be read",
            ),
        };
    }

    if matches!(report.ownership, Ownership::Managed) && status_refuses(report) {
        return Classification::error(
            format!("The installed {scheduler} rotation schedule is unsafe to run."),
            "the installed rotation schedule would refuse its next run",
        );
    }

    // Ownership came from the bytes on disk, which the scheduler probe does not
    // participate in — so a managed (or legacy, or foreign) artifact can sit
    // next to a `launchctl`/`systemctl`/`schtasks` that refused to answer. That
    // is not a healthy schedule: whether the job is actually registered is
    // unknown, and the `Scheduler:` line below says which command failed.
    // Ownership is read from the bytes on disk; registration is a separate
    // question, and a managed unit the scheduler does not know about will not
    // fire at all. `systemctl --user disable --now` and `launchctl bootout`
    // both leave the files exactly where install wrote them, so this is the one
    // state where `[ok] ... is installed.` would be a lie with a working-looking
    // block under it.
    if matches!(report.ownership, Ownership::Managed) && report.scheduler == SchedulerState::Absent
    {
        return Classification::error(
            format!("The {scheduler} rotation schedule is not registered."),
            "the installed rotation schedule is not registered with the scheduler",
        );
    }

    if let Some(failure) = scheduler_failure {
        return Classification::error(
            format!("The {scheduler} rotation schedule could not be confirmed."),
            failure,
        );
    }

    match &report.ownership {
        Ownership::Managed => {
            Classification::ok(format!("A {scheduler} rotation schedule is installed."))
        }
        Ownership::LegacyUnpinned { .. } => Classification::warn(format!(
            "A legacy {scheduler} rotation schedule is installed."
        )),
        Ownership::OrphanedManifest => Classification::warn(format!(
            "A rotation manifest exists but no {scheduler} is installed."
        )),
        Ownership::Foreign { .. } => Classification::warn(format!(
            "Something xv did not write is at a path the {scheduler} rotation schedule owns."
        )),
        // Handled above, before any other arm could claim it.
        Ownership::Absent => {
            Classification::info(format!("No {scheduler} rotation schedule is installed."))
        }
    }
}

/// The message `xv schedule status` should fail with, when it should fail.
///
/// Reads the same classification the headline does, so the `[error]` prefix and
/// the non-zero exit are the same decision.
pub(crate) fn status_failure(report: &ScheduleStatusReport, platform: Platform) -> Option<String> {
    classify(report, platform.name()).failure
}

/// Render the whole status block.
///
/// `rich` selects the emoji/colour headline prefixes for a terminal; the
/// goldens (and every test here) are the plain `[ok]` / `[warn]` / `[error]` /
/// `[hint]` form. The indented dimension lines are never decorated in either
/// mode — their alignment is the point.
pub(crate) fn render_status(
    report: &ScheduleStatusReport,
    platform: Platform,
    rich: bool,
) -> String {
    let scheduler = platform.name();
    let mut lines: Vec<String> = Vec::new();

    let refuses = status_refuses(report);
    let unregistered = matches!(report.ownership, Ownership::Managed)
        && report.scheduler == SchedulerState::Absent;
    let classification = classify(report, scheduler);
    lines.push(format_line(
        classification.level,
        &classification.headline,
        rich,
    ));

    if let Some(label) = report.ownership.label() {
        lines.push(dimension("Ownership:", label));
    }
    // The scheduler dimension earns a line whenever it says something the
    // headline does not. `installed` is what "A ... schedule is installed."
    // already means, so it stays silent; everything else is printed. `not
    // registered` in particular must be visible for *any* ownership we found on
    // disk — an artifact exists, and the scheduler does not know about it — and
    // is suppressed only when there is no artifact either, where the headline
    // has already said nothing is installed.
    if matches!(
        report.scheduler,
        SchedulerState::Unknown | SchedulerState::Error(_)
    ) || (report.scheduler == SchedulerState::Absent
        && !matches!(report.ownership, Ownership::Absent))
    {
        lines.push(dimension("Scheduler:", report.scheduler.describe()));
    }

    let mut hints: Vec<String> = Vec::new();
    match &report.ownership {
        Ownership::LegacyUnpinned { command_line } => {
            lines.push(dimension(
                "Command:",
                if command_line.is_empty() {
                    "unknown (the scheduler did not report one)"
                } else {
                    command_line
                },
            ));
            lines.push(dimension("Target:", unverified_target_note(command_line)));
            hints.push(format!(
                "Replace it explicitly with 'xv schedule install --vault {VAULT_PLACEHOLDER}'."
            ));
        }
        Ownership::Managed | Ownership::OrphanedManifest => {
            let orphaned = matches!(report.ownership, Ownership::OrphanedManifest);
            match (&report.manifest, &report.manifest_error) {
                (_, Some(detail)) => {
                    lines.push(dimension("Target:", format!("unreadable ({detail})")));
                    render_last_run(&mut lines, &report.last_run);
                    render_next_run(&mut lines, &report.next_run);
                    hints.push(
                        "Reinstall the schedule with 'xv schedule install' to regenerate it."
                            .to_string(),
                    );
                }
                (Some((manifest, _)), None) => {
                    render_manifest_dimensions(&mut lines, report, manifest);
                    let alias = install_argument(manifest);
                    if orphaned {
                        hints.push(format!(
                            "Run 'xv schedule install --vault {alias}' to repair the schedule, \
                             or 'xv schedule uninstall' to remove the manifest."
                        ));
                    } else if refuses {
                        hints.push(format!(
                            "Review the changes, then run 'xv schedule install --vault {alias}' \
                             to accept the new target."
                        ));
                    } else if unregistered {
                        // The files are ours and intact; what is missing is the
                        // registration. Reinstall is the only command that
                        // re-registers it, and it must be aimed at the name the
                        // schedule was installed with. Ordered after the drift
                        // hint deliberately, so the hint always answers the
                        // headline `classify` chose.
                        hints.push(format!(
                            "Run 'xv schedule install --vault {alias}' to register it again."
                        ));
                    } else if has_warnings(report) {
                        // The design's drift table: a same-path binary at a new
                        // version is a warning, the run is allowed, and status
                        // *recommends a reinstall* so the rendered unit and the
                        // recorded schema are refreshed. The reason line already
                        // says what changed; this is the action, in the paste-able
                        // form every other hint uses.
                        hints.push(format!(
                            "Reinstall the schedule with 'xv schedule install --vault {alias}' \
                             to refresh the rendered unit and the recorded version."
                        ));
                    }
                }
                // Ownership said a manifest is there and the loader found
                // neither a manifest nor an error. Nothing to claim.
                (None, None) => {
                    lines.push(dimension(
                        "Target:",
                        "unknown (the manifest disappeared while status was reading it)",
                    ));
                }
            }
        }
        Ownership::Foreign { paths } => {
            for path in paths {
                lines.push(dimension("Path:", path.display().to_string()));
            }
            hints.push(
                "xv will not overwrite or remove a file it did not write. Move it aside, then \
                 run 'xv schedule install --vault <alias-or-vault>'."
                    .to_string(),
            );
        }
        Ownership::Absent => {
            // Uninstall retains `last-run.json`, so there may still be history
            // here — and history nothing renders is retention the user cannot
            // see. `collect_last_run` has already labelled it `(previous
            // install)`, because the installation that wrote it is gone. A host
            // that never ran the sweep says nothing: `Last run:  never` under
            // "nothing is installed" is noise.
            if !matches!(report.last_run, LastRunStatus::Never) {
                render_last_run(&mut lines, &report.last_run);
            }
            hints.push(format!(
                "Install one with 'xv schedule install --vault {VAULT_PLACEHOLDER}'."
            ));
        }
    }

    for hint in hints {
        lines.push(format_line(Level::Hint, &hint, rich));
    }
    lines.join("\n")
}

/// The `--vault` value a reinstall hint must echo: the name the schedule was
/// installed with, which is the recorded workspace alias — or the real vault
/// in the degenerate (no workspace) case. Telling someone to rerun
/// `--vault <the real vault>` when they installed `--vault payments` would
/// aim them at a different target.
fn install_argument(manifest: &ScheduleManifestV1) -> String {
    quote_if_needed(
        manifest
            .target
            .workspace_alias
            .as_deref()
            .unwrap_or(manifest.target.vault.as_str()),
    )
}

/// Every dimension a loaded manifest supplies, in golden order.
fn render_manifest_dimensions(
    lines: &mut Vec<String>,
    report: &ScheduleStatusReport,
    manifest: &ScheduleManifestV1,
) {
    lines.push(dimension(
        "Schedule:",
        manifest_interval(&manifest.cadence).map_or_else(
            || format!("unknown (unrecognized cadence '{}')", manifest.cadence.kind),
            |interval| interval.describe(),
        ),
    ));
    lines.push(dimension(
        "Target:",
        format!(
            "{} -> {}/{}",
            manifest
                .target
                .workspace_alias
                .as_deref()
                .unwrap_or(manifest.target.vault.as_str()),
            manifest.target.backend_name,
            manifest.target.vault
        ),
    ));
    lines.push(dimension(
        "Backend:",
        format!(
            "{} ({})",
            manifest.target.backend_name, manifest.target.backend_kind
        ),
    ));
    lines.push(dimension("Config:", &manifest.target.config_path));
    lines.push(dimension(
        "Project:",
        match (&manifest.target.project_path, &manifest.target.environment) {
            (Some(path), Some(environment)) => format!("{path} (environment {environment})"),
            (Some(path), None) => path.clone(),
            // No `.xv.toml` took part in the recorded resolution. Saying so is
            // a fact about the target, not a missing line.
            (None, _) => "none".to_string(),
        },
    ));
    lines.push(dimension("Cwd:", &manifest.execution.working_directory));
    render_drift(lines, report);
    if let Some(executable) = &report.executable {
        lines.push(dimension("Binary:", render_binary(executable)));
    }
    render_last_run(lines, &report.last_run);
    render_next_run(lines, &report.next_run);
    lines.push(dimension(
        "Log:",
        format!(
            "{} ({})",
            manifest.execution.log_path,
            match report.log {
                LogStatus::Present => "present",
                LogStatus::NotYetWritten => "not yet written",
                LogStatus::Unknown => "unknown",
            }
        ),
    ));
}

/// The drift verdict and every reason behind it, target drift first.
///
/// Unit drift is a refusal too, so a report with unit reasons reads `refused`
/// even when the recorded target itself still recomputes.
///
/// **Ordering** (goldens line 133, "every difference in manifest-field order"):
/// the refusals and the warnings are *merged* and sorted by manifest-field rank,
/// not printed as two blocks. `DriftReport` keeps them in separate vectors
/// because they mean different things to the runner, but to a reader they are
/// one list of differences, and a warning about `installed_version` belongs
/// after a refusal about `config_digest` rather than before it. Unit-drift
/// reasons follow, in their own fixed `unit_command, unit_cadence,
/// unit_log_path` order — they are differences from the *unit*, not from the
/// manifest's fields, so they have no rank in that list.
///
/// `detail` strings are printed verbatim: one of them is not the "differs"
/// sentence (an unreadable unit says so in its own words), and rebuilding a
/// sentence from `field` would lose that.
fn render_drift(lines: &mut Vec<String>, report: &ScheduleStatusReport) {
    let Some(drift) = &report.drift else {
        return;
    };
    let unit_reasons: &[DriftReason] = report
        .unit_drift
        .as_ref()
        .map_or(&[], |unit| unit.reasons.as_slice());
    let verdict = if drift.is_refused() || !unit_reasons.is_empty() {
        "refused"
    } else {
        match drift.verdict {
            DriftVerdict::Valid => "valid",
            DriftVerdict::Warning => "warning",
            DriftVerdict::Refuse => "refused",
        }
    };
    lines.push(dimension("Drift:", verdict));

    let mut target: Vec<&DriftReason> = drift.reasons.iter().chain(drift.warnings.iter()).collect();
    // Stable, so two reasons on the same field keep the refusal-before-warning
    // order the report arrived in.
    target.sort_by_key(|reason| field_rank(reason.field));
    for reason in target.into_iter().chain(unit_reasons.iter()) {
        lines.push(format!("  - {}", reason.detail));
    }
}

/// Whether the recorded target still recomputes but with something worth
/// saying — today, only an in-place version change at the same binary path.
fn has_warnings(report: &ScheduleStatusReport) -> bool {
    report
        .drift
        .as_ref()
        .is_some_and(|drift| !drift.warnings.is_empty())
}

/// `<recorded path> (installed X, current Y)`.
///
/// When `status` was not run from the recorded path there is no honest "current
/// version" to report: this process's version says nothing about the binary
/// the scheduler will invoke. That case names the binary that is asking
/// instead, so the reader can see why the comparison was skipped.
fn render_binary(executable: &ExecutableStatus) -> String {
    let current = if executable.current_matches_path {
        executable.current_version.clone()
    } else {
        format!("unknown (status run from {})", executable.invoking_path)
    };
    format!(
        "{} (installed {}, current {current})",
        executable.recorded_path, executable.installed_version
    )
}

fn render_last_run(lines: &mut Vec<String>, last_run: &LastRunStatus) {
    lines.push(dimension("Last run:", describe_last_run(last_run)));
}

fn render_next_run(lines: &mut Vec<String>, next_run: &NextRun) {
    lines.push(dimension(
        "Next run:",
        match next_run {
            NextRun::At(instant) => instant.clone(),
            NextRun::Unknown => "unknown (scheduler did not report a next run)".to_string(),
        },
    ));
}

/// The `Last run:` value.
///
/// Shape: `<state>; <when>[; <counts>][; exit <n>][; <code>]`, with the
/// trailing `(previous install)` marker when the record was written by an
/// earlier installation than the one on disk now. A successful run carries no
/// exit code or diagnostic — its counts are the whole story.
pub(crate) fn describe_last_run(last_run: &LastRunStatus) -> String {
    match last_run {
        LastRunStatus::Never => "never".to_string(),
        LastRunStatus::RunningHeld { started_at } => format!("running since {started_at}"),
        LastRunStatus::Interrupted { started_at } => {
            format!("interrupted after {started_at} (no runner holds the lock)")
        }
        LastRunStatus::Unreadable(detail) => format!("unreadable ({detail})"),
        LastRunStatus::Outcome {
            outcome,
            previous_install,
        } => {
            let mut rendered = describe_outcome(outcome);
            if *previous_install {
                rendered.push_str(" (previous install)");
            }
            rendered
        }
    }
}

fn describe_outcome(outcome: &RunOutcomeV1) -> String {
    let mut parts = vec![outcome.state.as_str().to_string()];
    parts.push(match &outcome.finished_at {
        // A refusal starts and ends in the same second; printing the same
        // instant twice reads as a bug, not as precision.
        Some(finished) if finished != &outcome.started_at => {
            format!("{} to {finished}", outcome.started_at)
        }
        _ => outcome.started_at.clone(),
    });
    if let Some(summary) = &outcome.summary {
        parts.push(format!(
            "{} due, {} rotated, {} failed",
            summary.due, summary.rotated, summary.failed
        ));
    }
    if outcome.state != RunState::Success {
        if let Some(code) = outcome.exit_code {
            parts.push(format!("exit {code}"));
        }
        if let Some(diagnostic) = &outcome.diagnostic {
            parts.push(diagnostic.code.clone());
        }
    }
    parts.join("; ")
}

#[cfg(test)]
#[path = "status_render_tests.rs"]
mod tests;
