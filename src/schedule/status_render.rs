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

use crate::schedule::drift::{DriftReport, DriftVerdict};
use crate::schedule::manifest::ScheduleManifestV1;
use crate::schedule::outcome::{RunOutcomeV1, RunState};
use crate::schedule::ownership::{unverified_target_note, Ownership};
use crate::schedule::status::manifest_interval;
use crate::schedule::status::{
    ExecutableStatus, LastRunStatus, LogStatus, NextRun, ScheduleStatusReport, SchedulerState,
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

/// The message `xv schedule status` should fail with, when it should fail.
///
/// Exit code policy, decided from the goldens: a `[warn]` or `[info]` state is
/// a successful diagnosis and exits `0` — an orphaned manifest, a legacy unit,
/// a foreign file and "nothing is installed" are all things `status` reports
/// accurately. An `[error]` state exits with the configuration-error code
/// (`3`), the same code the scheduled run itself uses when it refuses: a
/// managed schedule that would refuse tonight, a managed manifest that cannot
/// be read, and a scheduler that could not be queried at all. That makes
/// `xv schedule status` usable as a health check in a wrapper script without
/// parsing its text.
pub(crate) fn status_failure(report: &ScheduleStatusReport) -> Option<String> {
    let managed = matches!(report.ownership, Ownership::Managed);
    if managed && report.manifest_error.is_some() {
        return Some(
            "the recorded target of the installed rotation schedule could not be read".into(),
        );
    }
    if managed && status_refuses(report) {
        return Some("the installed rotation schedule would refuse its next run".into());
    }
    if let SchedulerState::Error(detail) = &report.scheduler {
        return Some(format!("the scheduler could not be queried: {detail}"));
    }
    None
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
    let mut head = |level: Level, message: String| lines.push(format_line(level, &message, rich));

    let refuses = status_refuses(report);
    let unreadable_manifest = report.manifest_error.is_some();

    match &report.ownership {
        // A manifest that cannot be read is not a healthy schedule: the run it
        // is pinned to will refuse tonight, and status may not open with the
        // healthy headline and contradict itself three lines later.
        Ownership::Managed if unreadable_manifest => head(
            Level::Error,
            format!("The recorded target of the {scheduler} rotation schedule could not be read."),
        ),
        Ownership::Managed if refuses => head(
            Level::Error,
            format!("The installed {scheduler} rotation schedule is unsafe to run."),
        ),
        Ownership::Managed => head(
            Level::Success,
            format!("A {scheduler} rotation schedule is installed."),
        ),
        Ownership::LegacyUnpinned { .. } => head(
            Level::Warn,
            format!("A legacy {scheduler} rotation schedule is installed."),
        ),
        Ownership::OrphanedManifest if unreadable_manifest => head(
            Level::Error,
            format!("A rotation manifest exists but could not be read, and no {scheduler} is installed."),
        ),
        Ownership::OrphanedManifest => head(
            Level::Warn,
            format!("A rotation manifest exists but no {scheduler} is installed."),
        ),
        Ownership::Foreign { .. } => head(
            Level::Warn,
            format!("Something xv did not write is at a path the {scheduler} rotation schedule owns."),
        ),
        // A scheduler that would not answer is not evidence of absence.
        Ownership::Absent => match &report.scheduler {
            SchedulerState::Error(_) => head(
                Level::Error,
                format!("Could not determine whether a {scheduler} rotation schedule is installed."),
            ),
            SchedulerState::Unknown => head(
                Level::Warn,
                format!("Could not confirm whether a {scheduler} rotation schedule is installed."),
            ),
            _ => head(
                Level::Info,
                format!("No {scheduler} rotation schedule is installed."),
            ),
        },
    }

    if let Some(label) = report.ownership.label() {
        lines.push(dimension("Ownership:", label));
    }
    // The scheduler dimension earns a line only when it says something the
    // headline does not: `installed` is what "A ... schedule is installed."
    // already means, and `not registered` is what every non-managed headline
    // already says.
    if matches!(
        report.scheduler,
        SchedulerState::Unknown | SchedulerState::Error(_)
    ) {
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
/// even when the recorded target itself still recomputes. Reason `detail`
/// strings are printed verbatim: one of them is not the "differs" sentence
/// (an unreadable unit says so in its own words), and rebuilding a sentence
/// from `field` would lose that.
fn render_drift(lines: &mut Vec<String>, report: &ScheduleStatusReport) {
    let Some(drift) = &report.drift else {
        return;
    };
    let unit_reasons: &[crate::schedule::drift::DriftReason] = report
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
    for reason in drift
        .warnings
        .iter()
        .chain(drift.reasons.iter())
        .chain(unit_reasons.iter())
    {
        lines.push(format!("  - {}", reason.detail));
    }
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
