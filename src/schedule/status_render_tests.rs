//! Golden tests for the `xv schedule status` block.
//!
//! Every expectation here is written out literally from
//! `docs/superpowers/specs/2026-09-09-scheduled-target-manifest-goldens.md`
//! (lines 94-206). Nothing is re-derived from the renderer — a test that built
//! its expectation the way the code does could not catch the code drifting
//! away from the contract.
//!
//! Two of the goldens are complete blocks (the two healthy cases and the
//! legacy unit) and are asserted byte for byte. The rest are written in the
//! spec as excerpts of the lines that example is about, so they are asserted
//! as an ordered subsequence: every golden line must appear, in the golden's
//! order.

use super::*;

use crate::schedule::drift::{DriftReason, DriftReport, DriftVerdict};
use crate::schedule::manifest::{ManifestCadence, ManifestExecution, ManifestTarget};
use crate::schedule::outcome::{RunDiagnostic, RunSummary};
use crate::schedule::status::{UnitDriftReport, UNIT_COMMAND};

const CONFIG_PATH: &str = "/home/alice/.config/xv/xv.conf";
const PROJECT_PATH: &str = "/home/alice/work/service/.xv.toml";
const CWD: &str = "/home/alice/work/service";
const BINARY: &str = "/home/alice/bin/xv";
const LOG: &str = "/home/alice/.local/state/xv/rotate.log";
const VERSION: &str = "0.39.0";
const CONFIG_DIGEST: &str =
    "sha256:9c1e9c85ec2f2701ac6f8feccdc5b9d12f9ba72f15cc32eb590cd724e34b8e92";
const PROJECT_DIGEST: &str =
    "sha256:83ad20d5db8cc85526a362d78c7436bb5d144733eb3a16b44132620313098c4f";
const BACKEND_IDENTITY: &str =
    "sha256:55db8b6f5e64ef7c0af4c5b1f9b4d40d45994e8dcfb7d1f7b995f55f7cad2213";
const MANIFEST_DIGEST: &str =
    "sha256:4d38e06cbb685364a6500f808d8793840d5eb219d4e6b72f06d0f501c8eb3658";

/// The manifest behind every golden in the spec.
fn manifest() -> ScheduleManifestV1 {
    ScheduleManifestV1 {
        schema_version: 1,
        schedule_id: "rotation-default".to_string(),
        installed_at: "2026-09-09T15:04:05Z".to_string(),
        cadence: ManifestCadence {
            kind: "daily".to_string(),
            hour: 3,
            minute: 0,
        },
        execution: ManifestExecution {
            binary_path: BINARY.to_string(),
            installed_version: VERSION.to_string(),
            working_directory: CWD.to_string(),
            log_path: LOG.to_string(),
        },
        target: ManifestTarget {
            config_path: CONFIG_PATH.to_string(),
            config_digest: CONFIG_DIGEST.to_string(),
            project_path: Some(PROJECT_PATH.to_string()),
            project_digest: Some(PROJECT_DIGEST.to_string()),
            environment: Some("production".to_string()),
            context_path: None,
            context_digest: None,
            workspace_source: "project".to_string(),
            workspace_alias: Some("payments".to_string()),
            backend_name: "aws-prod".to_string(),
            backend_kind: "aws".to_string(),
            backend_identity: BACKEND_IDENTITY.to_string(),
            vault: "payments-production".to_string(),
            vault_selection: "explicit".to_string(),
        },
    }
}

fn executable() -> ExecutableStatus {
    ExecutableStatus {
        recorded_path: BINARY.to_string(),
        installed_version: VERSION.to_string(),
        current_version: VERSION.to_string(),
        current_matches_path: true,
        invoking_path: BINARY.to_string(),
    }
}

fn valid_drift() -> DriftReport {
    DriftReport {
        verdict: DriftVerdict::Valid,
        reasons: Vec::new(),
        warnings: Vec::new(),
    }
}

/// A healthy managed report with no run behind it.
fn healthy() -> ScheduleStatusReport {
    ScheduleStatusReport {
        scheduler: SchedulerState::Installed,
        next_run: NextRun::At("2026-09-10T03:00:00Z".to_string()),
        ownership: Ownership::Managed,
        manifest: Some((manifest(), MANIFEST_DIGEST.to_string())),
        manifest_error: None,
        drift: Some(valid_drift()),
        unit_drift: Some(UnitDriftReport::default()),
        executable: Some(executable()),
        last_run: LastRunStatus::Never,
        log: LogStatus::NotYetWritten,
    }
}

fn outcome(state: RunState) -> RunOutcomeV1 {
    RunOutcomeV1 {
        schema_version: 1,
        schedule_id: "rotation-default".to_string(),
        manifest_digest: MANIFEST_DIGEST.to_string(),
        started_at: "2026-09-10T03:00:00Z".to_string(),
        finished_at: Some("2026-09-10T03:00:02Z".to_string()),
        state,
        exit_code: Some(0),
        summary: Some(RunSummary {
            policy_managed: 4,
            due: 2,
            rotated: 2,
            failed: 0,
        }),
        diagnostic: None,
    }
}

fn render(report: &ScheduleStatusReport) -> String {
    render_status(report, Platform::Systemd, false)
}

/// Assert that every golden line appears, in the golden's order.
fn assert_in_order(rendered: &str, expected: &[&str]) {
    let mut lines = rendered.lines();
    for wanted in expected {
        assert!(
            lines.any(|line| line == *wanted),
            "missing (or out of order) golden line {wanted:?} in:\n{rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// Complete goldens, asserted byte for byte
// ---------------------------------------------------------------------------

#[test]
fn a_healthy_schedule_before_its_first_run_matches_the_golden() {
    let expected = "\
[ok] A systemd user timer rotation schedule is installed.
  Ownership: managed
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Backend:   aws-prod (aws)
  Config:    /home/alice/.config/xv/xv.conf
  Project:   /home/alice/work/service/.xv.toml (environment production)
  Cwd:       /home/alice/work/service
  Drift:     valid
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.39.0)
  Last run:  never
  Next run:  2026-09-10T03:00:00Z
  Log:       /home/alice/.local/state/xv/rotate.log (not yet written)";

    assert_eq!(render(&healthy()), expected);
}

#[test]
fn a_healthy_schedule_after_a_successful_run_matches_the_golden() {
    let mut report = healthy();
    report.last_run = LastRunStatus::Outcome {
        outcome: outcome(RunState::Success),
        previous_install: false,
    };
    report.next_run = NextRun::Unknown;
    report.log = LogStatus::Present;

    let expected = "\
[ok] A systemd user timer rotation schedule is installed.
  Ownership: managed
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Backend:   aws-prod (aws)
  Config:    /home/alice/.config/xv/xv.conf
  Project:   /home/alice/work/service/.xv.toml (environment production)
  Cwd:       /home/alice/work/service
  Drift:     valid
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.39.0)
  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; 2 due, 2 rotated, 0 failed
  Next run:  unknown (scheduler did not report a next run)
  Log:       /home/alice/.local/state/xv/rotate.log (present)";

    assert_eq!(render(&report), expected);
}

#[test]
fn a_legacy_unit_matches_the_golden() {
    let command = "/home/alice/bin/xv rotate --due --force --vault payments-production";
    let report = ScheduleStatusReport {
        scheduler: SchedulerState::Installed,
        next_run: NextRun::Unknown,
        ownership: Ownership::LegacyUnpinned {
            command_line: command.to_string(),
        },
        manifest: None,
        manifest_error: None,
        drift: None,
        unit_drift: None,
        executable: None,
        last_run: LastRunStatus::Never,
        log: LogStatus::Unknown,
    };

    let expected = format!(
        "\
[warn] A legacy systemd user timer rotation schedule is installed.
  Ownership: legacy-unpinned
  Command:   {command}
  Target:    unverified (the legacy unit does not record backend or account identity)
[hint] Replace it explicitly with 'xv schedule install --vault <alias-or-vault>'."
    );

    assert_eq!(render(&report), expected);
}

// ---------------------------------------------------------------------------
// Excerpt goldens, asserted as an ordered subsequence
// ---------------------------------------------------------------------------

#[test]
fn a_drift_refusal_renders_every_difference_and_the_accept_hint() {
    let mut report = healthy();
    report.drift = Some(DriftReport {
        verdict: DriftVerdict::Refuse,
        reasons: vec![
            DriftReason::new(
                "project_digest",
                format!("project_digest changed; review {PROJECT_PATH} and reinstall"),
            ),
            DriftReason::new(
                "backend_identity",
                "backend_identity changed for aws-prod; review the account/provider and reinstall",
            ),
        ],
        warnings: Vec::new(),
    });
    report.last_run = LastRunStatus::Outcome {
        outcome: RunOutcomeV1 {
            finished_at: Some("2026-09-10T03:00:00Z".to_string()),
            state: RunState::RefusedDrift,
            exit_code: Some(3),
            summary: None,
            diagnostic: Some(RunDiagnostic::new(
                "target_drift",
                "project_digest and backend_identity changed; review the recorded target and \
                 reinstall",
            )),
            ..outcome(RunState::RefusedDrift)
        },
        previous_install: false,
    };
    report.next_run = NextRun::At("2026-09-11T03:00:00Z".to_string());

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[error] The installed systemd user timer rotation schedule is unsafe to run.",
            "  Ownership: managed",
            "  Target:    payments -> aws-prod/payments-production",
            "  Drift:     refused",
            "  - project_digest changed; review /home/alice/work/service/.xv.toml and reinstall",
            "  - backend_identity changed for aws-prod; review the account/provider and reinstall",
            "  Last run:  refused_drift; 2026-09-10T03:00:00Z; exit 3; target_drift",
            "  Next run:  2026-09-11T03:00:00Z",
            "[hint] Review the changes, then run 'xv schedule install --vault payments' to accept \
             the new target.",
        ],
    );
    assert_eq!(
        status_failure(&report, Platform::Systemd).as_deref(),
        Some("the installed rotation schedule would refuse its next run"),
        "a refusing schedule must fail the command"
    );
}

#[test]
fn an_orphaned_manifest_matches_the_golden() {
    let mut report = healthy();
    report.ownership = Ownership::OrphanedManifest;
    report.scheduler = SchedulerState::Absent;
    report.next_run = NextRun::Unknown;

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[warn] A rotation manifest exists but no systemd user timer is installed.",
            "  Ownership: orphaned-manifest",
            "  Target:    payments -> aws-prod/payments-production",
            "  Drift:     valid",
            "[hint] Run 'xv schedule install --vault payments' to repair the schedule, or 'xv \
             schedule uninstall' to remove the manifest.",
        ],
    );
    assert!(
        status_failure(&report, Platform::Systemd).is_none(),
        "an orphaned manifest is an accurate diagnosis, not a failure"
    );
}

// ---------------------------------------------------------------------------
// Last-run shapes
// ---------------------------------------------------------------------------

#[test]
fn a_partial_failure_reports_its_counts_its_exit_and_its_code() {
    let mut report = healthy();
    report.last_run = LastRunStatus::Outcome {
        outcome: RunOutcomeV1 {
            finished_at: Some("2026-09-10T03:00:05Z".to_string()),
            state: RunState::PartialFailure,
            exit_code: Some(3),
            summary: Some(RunSummary {
                policy_managed: 5,
                due: 3,
                rotated: 2,
                failed: 1,
            }),
            diagnostic: Some(RunDiagnostic::new("rotation-failed", "one secret failed")),
            ..outcome(RunState::PartialFailure)
        },
        previous_install: false,
    };

    assert!(
        render(&report).contains(
            "  Last run:  partial_failure; 2026-09-10T03:00:00Z to 2026-09-10T03:00:05Z; \
             3 due, 2 rotated, 1 failed; exit 3; rotation-failed"
        ),
        "{}",
        render(&report)
    );
}

#[test]
fn a_whole_run_failure_reports_its_exit_code_and_diagnostic_code() {
    let mut report = healthy();
    report.last_run = LastRunStatus::Outcome {
        outcome: RunOutcomeV1 {
            state: RunState::Failed,
            exit_code: Some(3),
            summary: None,
            diagnostic: Some(RunDiagnostic::new("backend-unavailable", "redacted")),
            ..outcome(RunState::Failed)
        },
        previous_install: false,
    };

    assert!(
        render(&report).contains(
            "  Last run:  failed; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; exit 3; \
             backend-unavailable"
        ),
        "{}",
        render(&report)
    );
}

#[test]
fn a_retained_outcome_from_an_earlier_install_is_labelled() {
    let mut report = healthy();
    report.last_run = LastRunStatus::Outcome {
        outcome: outcome(RunState::Success),
        previous_install: true,
    };

    assert!(
        render(&report).contains(
            "  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; \
             2 due, 2 rotated, 0 failed (previous install)"
        ),
        "{}",
        render(&report)
    );
}

#[test]
fn a_live_run_and_an_interrupted_one_read_differently() {
    let mut report = healthy();
    report.last_run = LastRunStatus::RunningHeld {
        started_at: "2026-09-10T03:00:00Z".to_string(),
    };
    assert!(
        render(&report).contains("  Last run:  running since 2026-09-10T03:00:00Z"),
        "{}",
        render(&report)
    );

    report.last_run = LastRunStatus::Interrupted {
        started_at: "2026-09-10T03:00:00Z".to_string(),
    };
    assert!(
        render(&report).contains(
            "  Last run:  interrupted after 2026-09-10T03:00:00Z (no runner holds the lock)"
        ),
        "{}",
        render(&report)
    );
}

#[test]
fn an_unreadable_last_run_record_says_so_without_losing_the_rest() {
    let mut report = healthy();
    report.last_run = LastRunStatus::Unreadable("last-run.json is not valid JSON".to_string());

    let rendered = render(&report);
    assert!(
        rendered.contains("  Last run:  unreadable (last-run.json is not valid JSON)"),
        "{rendered}"
    );
    assert!(rendered.contains("  Drift:     valid"), "{rendered}");
}

// ---------------------------------------------------------------------------
// Scheduler dimension
// ---------------------------------------------------------------------------

#[test]
fn a_scheduler_that_gave_no_readable_answer_is_not_absence() {
    let report = ScheduleStatusReport {
        scheduler: SchedulerState::Unknown,
        next_run: NextRun::Unknown,
        ownership: Ownership::Absent,
        manifest: None,
        manifest_error: None,
        drift: None,
        unit_drift: None,
        executable: None,
        last_run: LastRunStatus::Never,
        log: LogStatus::Unknown,
    };

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[warn] Could not confirm whether a systemd user timer rotation schedule is installed.",
            "  Scheduler: unknown (the scheduler gave no readable answer)",
        ],
    );
    assert!(
        !rendered.contains("No systemd user timer rotation schedule is installed."),
        "an unreadable answer may not be reported as absence: {rendered}"
    );
    assert!(
        status_failure(&report, Platform::Systemd).is_none(),
        "an unproven answer is a warning, not a failure"
    );
}

#[test]
fn a_scheduler_command_failure_is_an_error_with_a_sanitized_detail() {
    let report = ScheduleStatusReport {
        scheduler: SchedulerState::Error("systemctl --user show exited 1".to_string()),
        next_run: NextRun::Unknown,
        ownership: Ownership::Absent,
        manifest: None,
        manifest_error: None,
        drift: None,
        unit_drift: None,
        executable: None,
        last_run: LastRunStatus::Never,
        log: LogStatus::Unknown,
    };

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[error] Could not determine whether a systemd user timer rotation schedule is \
             installed.",
            "  Scheduler: error (systemctl --user show exited 1)",
        ],
    );
    assert_eq!(
        status_failure(&report, Platform::Systemd).as_deref(),
        Some("the scheduler could not be queried: systemctl --user show exited 1")
    );
}

// ---------------------------------------------------------------------------
// The two findings this task closes
// ---------------------------------------------------------------------------

#[test]
fn an_unreadable_manifest_gets_an_error_headline_not_the_healthy_one() {
    let mut report = healthy();
    report.manifest = None;
    report.manifest_error = Some("manifest.json is not valid JSON".to_string());
    report.drift = None;
    report.unit_drift = None;
    report.executable = None;

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[error] The recorded target of the systemd user timer rotation schedule could not be \
             read.",
            "  Ownership: managed",
            "  Target:    unreadable (manifest.json is not valid JSON)",
            "[hint] Reinstall the schedule with 'xv schedule install' to regenerate it.",
        ],
    );
    assert!(
        !rendered.contains("[ok]"),
        "a manifest that cannot be read is not a healthy schedule: {rendered}"
    );
    assert!(status_failure(&report, Platform::Systemd).is_some());
}

#[test]
fn status_run_from_another_binary_reports_no_current_version_at_all() {
    let mut report = healthy();
    report.executable = Some(ExecutableStatus {
        current_version: "0.40.0".to_string(),
        current_matches_path: false,
        invoking_path: "/tmp/build/xv".to_string(),
        ..executable()
    });

    let rendered = render(&report);
    assert!(
        rendered.contains(
            "  Binary:    /home/alice/bin/xv (installed 0.39.0, current unknown (status run from \
             /tmp/build/xv))"
        ),
        "{rendered}"
    );
    assert!(
        !rendered.contains("0.40.0"),
        "this process's version says nothing about the scheduled binary: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Unit drift
// ---------------------------------------------------------------------------

#[test]
fn unit_drift_alone_refuses_and_prints_its_reason_verbatim() {
    let mut report = healthy();
    report.unit_drift = Some(UnitDriftReport {
        reasons: vec![DriftReason::new(
            UNIT_COMMAND,
            "the installed unit could not be read; check its permissions, then reinstall",
        )],
    });

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[error] The installed systemd user timer rotation schedule is unsafe to run.",
            "  Drift:     refused",
            "  - the installed unit could not be read; check its permissions, then reinstall",
        ],
    );
}

#[test]
fn a_warning_verdict_still_prints_its_reason() {
    let mut report = healthy();
    report.drift = Some(DriftReport {
        verdict: DriftVerdict::Warning,
        reasons: Vec::new(),
        warnings: vec![DriftReason::new(
            "installed_version",
            "installed_version changed from 0.39.0 to 0.40.0 at the same binary path; reinstall \
             the schedule ('xv schedule install') to refresh the rendered unit",
        )],
    });

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[ok] A systemd user timer rotation schedule is installed.",
            "  Drift:     warning",
            "  - installed_version changed from 0.39.0 to 0.40.0 at the same binary path; \
             reinstall the schedule ('xv schedule install') to refresh the rendered unit",
        ],
    );
    assert!(status_failure(&report, Platform::Systemd).is_none());
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// The renderer must print only the fields it is allowed to print.
///
/// Every field the block *does* show legitimately carries user text — paths, a
/// backend name, an alias, a drift sentence — so "no canary anywhere" would be
/// unprovable. This tests **field selection** instead: a distinct canary is
/// planted in every manifest / outcome / diagnostic field the renderer is *not*
/// allowed to print, and the block must contain none of them, while the fields
/// it *is* allowed to print must still appear (so the test cannot pass by
/// rendering nothing).
///
/// The excluded set is the interesting one. `config_digest`, `project_digest`,
/// `backend_identity` and `context_digest` are hashes of the user's files and
/// account — nothing a diagnosis needs. `diagnostic.message` is the only
/// free-form string in an outcome; its `code` comes from a closed set, so the
/// code is printed and the message is not. `manifest_digest` and `installed_at`
/// are binding metadata, not a dimension.
#[test]
fn the_block_prints_the_permitted_fields_and_no_others() {
    const LEAK: &str = "CANARY-MUST-NOT-APPEAR";
    let mut manifest = manifest();
    // Fields the renderer must never print.
    manifest.installed_at = format!("2026-09-09T15:04:05Z{LEAK}-installed-at");
    manifest.schedule_id = format!("rotation-default{LEAK}-schedule-id");
    manifest.target.config_digest = format!("{CONFIG_DIGEST}{LEAK}-config-digest");
    manifest.target.project_digest = Some(format!("{PROJECT_DIGEST}{LEAK}-project-digest"));
    manifest.target.backend_identity = format!("{BACKEND_IDENTITY}{LEAK}-backend-identity");
    manifest.target.context_path = Some(format!("/home/alice/.xv/context{LEAK}-context-path"));
    manifest.target.context_digest = Some(format!("sha256:aa{LEAK}-context-digest"));
    manifest.target.workspace_source = format!("project{LEAK}-workspace-source");
    manifest.target.vault_selection = format!("explicit{LEAK}-vault-selection");

    let mut report = healthy();
    report.manifest = Some((manifest, format!("{MANIFEST_DIGEST}{LEAK}-manifest-digest")));
    report.last_run = LastRunStatus::Outcome {
        outcome: RunOutcomeV1 {
            manifest_digest: format!("{MANIFEST_DIGEST}{LEAK}-outcome-digest"),
            schedule_id: format!("rotation-default{LEAK}-outcome-schedule-id"),
            state: RunState::Failed,
            exit_code: Some(3),
            summary: None,
            diagnostic: Some(RunDiagnostic::new(
                "backend-unavailable",
                // The one free-form string an outcome carries. A provider error
                // body would land here, so it may never be rendered.
                format!("the provider said: {LEAK}-diagnostic-message"),
            )),
            ..outcome(RunState::Failed)
        },
        previous_install: false,
    };

    let rendered = render(&report);
    assert!(
        !rendered.contains(LEAK),
        "the renderer printed a field outside its permitted set:\n{rendered}"
    );

    // …and it really did render the dimensions, so the assertion above is not
    // vacuous. These are the fields the block is allowed to show.
    for permitted in [
        "daily at 03:00",                           // cadence.kind/hour/minute
        "payments -> aws-prod/payments-production", // alias, backend_name, vault
        "aws-prod (aws)",                           // backend_name, backend_kind
        CONFIG_PATH,                                // target.config_path
        PROJECT_PATH,                               // target.project_path
        "(environment production)",                 // target.environment
        CWD,                                        // execution.working_directory
        BINARY,                                     // execution.binary_path
        "installed 0.39.0",                         // execution.installed_version
        LOG,                                        // execution.log_path
        "failed",                                   // outcome.state
        "2026-09-10T03:00:00Z",                     // outcome.started_at
        "2026-09-10T03:00:02Z",                     // outcome.finished_at
        "exit 3",                                   // outcome.exit_code
        "backend-unavailable",                      // outcome.diagnostic.code
    ] {
        assert!(
            rendered.contains(permitted),
            "the block no longer renders {permitted:?}, so the leak check above proves nothing:\n{rendered}"
        );
    }

    // The goldens' own canaries, for good measure: nothing the renderer adds
    // can introduce them.
    for canary in [
        "AKIAIOSFODNN7EXAMPLE",
        "aws-session-token-canary",
        "azure-client-secret-canary",
        "AGE-SECRET-KEY-1CANARY",
        "super-secret-value-canary",
    ] {
        assert!(!rendered.contains(canary), "{canary} leaked:\n{rendered}");
    }
}

// ---------------------------------------------------------------------------
// The one classification behind the prefix and the exit code
// ---------------------------------------------------------------------------

/// Every report the renderer can be handed, as a matrix: the `[error]` prefix
/// and the non-zero exit must be the same decision, always.
#[test]
fn an_error_headline_always_fails_and_nothing_else_does() {
    let mut cases: Vec<(&str, ScheduleStatusReport)> = Vec::new();

    cases.push(("healthy", healthy()));

    let mut refused = healthy();
    refused.drift = Some(DriftReport {
        verdict: DriftVerdict::Refuse,
        reasons: vec![DriftReason::new("config_digest", "config_digest changed")],
        warnings: Vec::new(),
    });
    cases.push(("managed + refused target", refused));

    let mut unit = healthy();
    unit.unit_drift = Some(UnitDriftReport {
        reasons: vec![DriftReason::new(UNIT_COMMAND, "unit_command differs")],
    });
    cases.push(("managed + unit drift", unit));

    let mut warned = healthy();
    warned.drift = Some(DriftReport {
        verdict: DriftVerdict::Warning,
        reasons: Vec::new(),
        warnings: vec![DriftReason::new(
            "installed_version",
            "installed_version changed",
        )],
    });
    cases.push(("managed + warning", warned));

    let mut unreadable = healthy();
    unreadable.manifest = None;
    unreadable.manifest_error = Some("not valid JSON".to_string());
    unreadable.drift = None;
    unreadable.unit_drift = None;
    unreadable.executable = None;
    cases.push(("managed + unreadable manifest", unreadable));

    let mut orphan_unreadable = healthy();
    orphan_unreadable.ownership = Ownership::OrphanedManifest;
    orphan_unreadable.scheduler = SchedulerState::Absent;
    orphan_unreadable.manifest = None;
    orphan_unreadable.manifest_error = Some("not valid JSON".to_string());
    orphan_unreadable.drift = None;
    orphan_unreadable.unit_drift = None;
    orphan_unreadable.executable = None;
    cases.push(("orphaned + unreadable manifest", orphan_unreadable));

    let mut orphan = healthy();
    orphan.ownership = Ownership::OrphanedManifest;
    orphan.scheduler = SchedulerState::Absent;
    cases.push(("orphaned", orphan));

    for (label, scheduler) in [
        ("managed", SchedulerState::Installed),
        ("managed + scheduler unknown", SchedulerState::Unknown),
        (
            "managed + scheduler error",
            SchedulerState::Error("systemctl --user show exited 1".to_string()),
        ),
    ] {
        let mut report = healthy();
        report.scheduler = scheduler;
        cases.push((label, report));
    }

    for (label, ownership) in [
        (
            "legacy",
            Ownership::LegacyUnpinned {
                command_line: "/bin/xv rotate --due --force".to_string(),
            },
        ),
        (
            "foreign",
            Ownership::Foreign {
                paths: vec![std::path::PathBuf::from("/tmp/theirs.plist")],
            },
        ),
    ] {
        for (suffix, scheduler) in [
            ("", SchedulerState::Installed),
            (
                " + scheduler error",
                SchedulerState::Error("launchctl print exited 5".to_string()),
            ),
        ] {
            let mut report = healthy();
            report.ownership = ownership.clone();
            report.scheduler = scheduler;
            report.manifest = None;
            report.manifest_error = None;
            report.drift = None;
            report.unit_drift = None;
            report.executable = None;
            cases.push((
                Box::leak(format!("{label}{suffix}").into_boxed_str()),
                report,
            ));
        }
    }

    for (label, scheduler) in [
        ("absent", SchedulerState::Absent),
        ("absent + scheduler unknown", SchedulerState::Unknown),
        (
            "absent + scheduler error",
            SchedulerState::Error("schtasks /Query exited 1".to_string()),
        ),
    ] {
        cases.push((
            label,
            ScheduleStatusReport {
                scheduler,
                next_run: NextRun::Unknown,
                ownership: Ownership::Absent,
                manifest: None,
                manifest_error: None,
                drift: None,
                unit_drift: None,
                executable: None,
                last_run: LastRunStatus::Never,
                log: LogStatus::Unknown,
            },
        ));
    }

    for (label, report) in &cases {
        let rendered = render(report);
        let headline = rendered.lines().next().expect("a headline");
        let fails = status_failure(report, Platform::Systemd).is_some();
        assert_eq!(
            headline.starts_with("[error]"),
            fails,
            "{label}: an `[error]` headline must fail and nothing else may \
             (headline {headline:?}, fails {fails})"
        );
    }
}

#[test]
fn a_managed_schedule_whose_scheduler_would_not_answer_is_not_healthy() {
    let mut report = healthy();
    report.scheduler = SchedulerState::Error("systemctl --user show exited 1".to_string());

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[error] The systemd user timer rotation schedule could not be confirmed.",
            "  Ownership: managed",
            "  Scheduler: error (systemctl --user show exited 1)",
            "  Drift:     valid",
        ],
    );
    assert!(
        !rendered.contains("[ok]"),
        "whether the job is registered is unknown; that is not `[ok]`: {rendered}"
    );
    assert_eq!(
        status_failure(&report, Platform::Systemd).as_deref(),
        Some("the scheduler could not be queried: systemctl --user show exited 1")
    );
}

#[test]
fn an_unreadable_orphaned_manifest_is_an_error_and_fails() {
    let mut report = healthy();
    report.ownership = Ownership::OrphanedManifest;
    report.scheduler = SchedulerState::Absent;
    report.manifest = None;
    report.manifest_error = Some("manifest.json does not match schema version 1".to_string());
    report.drift = None;
    report.unit_drift = None;
    report.executable = None;

    let rendered = render(&report);
    assert_in_order(
        &rendered,
        &[
            "[error] A rotation manifest exists but could not be read, and no systemd user timer \
             is installed.",
            "  Ownership: orphaned-manifest",
            "  Target:    unreadable (manifest.json does not match schema version 1)",
            "[hint] Reinstall the schedule with 'xv schedule install' to regenerate it.",
        ],
    );
    assert_eq!(
        status_failure(&report, Platform::Systemd).as_deref(),
        Some("the orphaned rotation manifest could not be read"),
        "`install` cannot repair a manifest it cannot read"
    );
}

// ---------------------------------------------------------------------------
// Drift reason ordering
// ---------------------------------------------------------------------------

/// Goldens line 133: "every difference in manifest-field order". Refusals and
/// warnings are one merged list ranked by manifest field, not two blocks —
/// otherwise an `installed_version` warning would print before a `config_path`
/// refusal.
#[test]
fn refusals_and_warnings_interleave_in_manifest_field_order() {
    let mut report = healthy();
    report.drift = Some(DriftReport {
        verdict: DriftVerdict::Refuse,
        // Deliberately out of order within each vector, to prove the renderer
        // ranks rather than trusting the arrival order.
        reasons: vec![
            DriftReason::new("vault", "vault changed"),
            DriftReason::new("config_path", "config_path changed"),
        ],
        warnings: vec![
            DriftReason::new("installed_version", "installed_version changed"),
            DriftReason::new("project_path", "project_path changed"),
        ],
    });
    report.unit_drift = Some(UnitDriftReport {
        reasons: vec![DriftReason::new(UNIT_COMMAND, "unit_command differs")],
    });

    let rendered = render(&report);
    let reasons: Vec<&str> = rendered
        .lines()
        .filter(|line| line.starts_with("  - "))
        .map(|line| line.trim_start_matches("  - "))
        .collect();
    assert_eq!(
        reasons,
        vec![
            "config_path changed",
            "project_path changed",
            "vault changed",
            "installed_version changed",
            // Unit drift is a difference from the unit, not from a manifest
            // field, so it has no rank in that list and comes last.
            "unit_command differs",
        ]
    );
}
