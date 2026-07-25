//! CLI coverage for `xv schedule`.
//!
//! **These tests never register a real job.** `launchctl`, `systemctl --user`,
//! and `schtasks` act on the invoking user's live session regardless of `HOME`,
//! so a test that actually installed would leave a rotation job running on the
//! developer's machine. Everything here goes through `--print`, which renders and
//! writes nothing, or through argument validation that fails before any
//! scheduler is touched.
//!
//! The install/uninstall/status *logic* — command order, idempotence, error
//! mapping — is covered against a fake command runner in
//! `src/schedule/mod.rs`'s unit tests.

mod common;

use common::xv_isolated_local_with_opts;

fn xv_cmd_for(store: &std::path::Path) -> std::process::Command {
    let root = store.parent().expect("store has a parent");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_xv"));
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(root);
    cmd
}

/// Run `xv schedule install --print ...` and return stdout.
fn print_schedule(store: &std::path::Path, extra: &[&str]) -> (bool, String) {
    let mut args = vec!["schedule", "install", "--print"];
    args.extend_from_slice(extra);
    let out = xv_cmd_for(store).args(&args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn print_renders_a_unit_without_installing_anything() {
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    let (ok, out) = print_schedule(&store, &["--vault", "prod-kv"]);
    assert!(ok, "{out}");

    // The command the scheduler will run.
    assert!(
        out.contains("rotate --due --force --vault prod-kv"),
        "{out}"
    );
    // A log destination, so a failed 3am run is diagnosable.
    assert!(out.contains("rotate.log"), "{out}");
    // The default cadence.
    assert!(out.contains("daily at 03:00"), "{out}");

    // Nothing may have been written to the unit directory.
    for candidate in [
        tmp.path().join("Library/LaunchAgents"),
        tmp.path().join(".config/systemd/user"),
    ] {
        assert!(
            !candidate.exists(),
            "--print must not create {}",
            candidate.display()
        );
    }
}

#[test]
fn print_respects_interval_and_time() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);

    let (ok, out) = print_schedule(&store, &["--interval", "hourly", "--at", "00:15"]);
    assert!(ok, "{out}");
    assert!(out.contains("every hour at :15"), "{out}");

    let (ok, out) = print_schedule(&store, &["--interval", "weekly", "--at", "04:30"]);
    assert!(ok, "{out}");
    assert!(out.contains("weekly on Sunday at 04:30"), "{out}");
}

#[test]
fn print_output_contains_a_loadable_unit_for_this_platform() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let (ok, out) = print_schedule(&store, &["--vault", "v"]);
    assert!(ok, "{out}");

    if cfg!(target_os = "macos") {
        assert!(out.contains("<?xml version=\"1.0\""), "{out}");
        assert!(out.contains("com.crosstache.xv-rotate"), "{out}");
        assert!(out.contains("StartCalendarInterval"), "{out}");
        // Installing must not itself trigger a rotation.
        assert!(out.contains("<key>RunAtLoad</key>"), "{out}");
        assert!(out.contains("<false/>"), "{out}");
    } else if cfg!(target_os = "windows") {
        assert!(out.contains("schtasks"), "{out}");
        assert!(out.contains("/TN crosstache-xv-rotate"), "{out}");
    } else {
        // Linux: either systemd units, or a clear "no systemd" error handled by
        // the dedicated test below.
        assert!(
            out.contains("OnCalendar=") || out.contains("without systemd"),
            "{out}"
        );
    }
}

#[test]
fn invalid_times_are_rejected_before_touching_the_scheduler() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    for bad in ["3:0", "24:00", "03:60", "0300", "morning"] {
        let out = xv_cmd_for(&store)
            .args(["schedule", "install", "--at", bad, "--force"])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "time {bad:?} should be rejected: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("HH:MM"),
            "the error should show the expected form: {stderr}"
        );
    }
}

#[test]
fn invalid_interval_is_rejected_by_clap() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = xv_cmd_for(&store)
        .args([
            "schedule",
            "install",
            "--interval",
            "fortnightly",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hourly") && stderr.contains("weekly"),
        "clap should list the valid values: {stderr}"
    );
}

#[test]
fn the_scheduled_command_is_bounded_to_due_secrets() {
    // The single most important property: an unattended job must never rotate
    // secrets that have no policy, and must never redefine a policy.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let (ok, out) = print_schedule(&store, &["--vault", "v"]);
    assert!(ok, "{out}");

    let command_line = out
        .lines()
        .find(|l| l.starts_with("# command:"))
        .expect("print shows the command");
    assert!(command_line.contains("--due"), "{command_line}");
    assert!(command_line.contains("--force"), "{command_line}");
    assert!(
        !command_line.contains("--every"),
        "a schedule must not redefine policies: {command_line}"
    );
    assert!(!command_line.contains("--native"), "{command_line}");
}

#[test]
fn print_carries_the_config_environment_into_the_unit() {
    // The classic failure: the job runs but resolves a different config than the
    // user tested with, so it sweeps the wrong vault or none at all.
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    let (ok, out) = print_schedule(&store, &["--vault", "v"]);
    assert!(ok, "{out}");

    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        let config_home = tmp.path().join(".config");
        assert!(
            out.contains(&config_home.to_string_lossy().to_string()),
            "the unit must pin XDG_CONFIG_HOME ({}): {out}",
            config_home.display()
        );
    }
}

#[test]
fn no_unit_contains_secret_material() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    // Seed a secret so there is something that *could* leak.
    xv_cmd_for(&store)
        .args(["set", "CANARY", "--value", "canary-secret-value"])
        .status()
        .unwrap();

    let (ok, out) = print_schedule(&store, &["--vault", "default"]);
    assert!(ok, "{out}");
    assert!(!out.contains("canary-secret-value"), "{out}");
    for forbidden in ["AGE-SECRET-KEY", "client_secret", "password="] {
        assert!(!out.contains(forbidden), "unit mentions {forbidden}: {out}");
    }
}

#[test]
fn status_reports_absence_without_installing() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = xv_cmd_for(&store)
        .args(["schedule", "status"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // On a host with a supported scheduler this reports "not installed"; on one
    // without (a container with no systemd), it explains that instead. Both are
    // successful diagnoses, and neither may claim a schedule exists.
    assert!(
        combined.contains("No ")
            || combined.contains("without systemd")
            || combined.contains("not supported"),
        "{combined}"
    );
    // Must not *affirm* a schedule. Checking for the bare phrase would match
    // inside "No launchd rotation schedule is installed.", so anchor on the
    // affirmative form the success path emits.
    assert!(
        !combined.contains("rotation schedule is installed.")
            || combined.contains("No launchd rotation schedule is installed.")
            || combined.contains("No systemd user timer rotation schedule is installed.")
            || combined.contains("No Task Scheduler rotation schedule is installed."),
        "nothing was installed, so status must not affirm one: {combined}"
    );
}

#[test]
fn uninstall_is_safe_when_nothing_is_installed() {
    // Must converge on "absent" rather than erroring, so it is safe in teardown
    // scripts. This does invoke the platform scheduler's delete/bootout, which
    // is a no-op against a job that was never created.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = xv_cmd_for(&store)
        .args(["schedule", "uninstall"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        assert!(
            combined.contains("nothing to remove") || combined.contains("Removed"),
            "{combined}"
        );
    } else {
        // Only acceptable failure: this host has no scheduler we manage.
        assert!(
            combined.contains("without systemd") || combined.contains("not supported"),
            "{combined}"
        );
    }
}

#[test]
fn schedule_help_explains_it_is_not_a_daemon() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = xv_cmd_for(&store)
        .args(["schedule", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("launchd"), "{stdout}");
    assert!(stdout.contains("systemd"), "{stdout}");
    assert!(stdout.contains("Task Scheduler"), "{stdout}");
    assert!(
        stdout.contains("No daemon"),
        "help should say what it does not do: {stdout}"
    );
}
