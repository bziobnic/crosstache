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

// ---------------------------------------------------------------------------
// Release-binary guard
//
// `xv` honors `XV_SCHEDULE_RUNNER` only under `cfg(debug_assertions)`
// (`src/schedule/testing.rs`, and `schedule_runner()` in
// `src/cli/schedule_ops.rs`). In a **release** test binary the fake scheduler is
// therefore compiled out, and `xv schedule install`/`uninstall`/`status` would
// reach the developer's own launchd, systemd or Task Scheduler — under a fixed
// global job name that `HOME` does not sandbox — and deregister or overwrite a
// rotation schedule they actually rely on. `cargo test --release` must not be
// able to do that.
//
// The test crate is built with the same profile as the binary it spawns, so
// `cfg!(debug_assertions)` here answers for that binary too. Every test that
// issues a scheduler-touching subcommand begins with `skip_if_release!()`, and
// every spawn of one goes through [`scheduler_output`], which refuses to spawn
// at all when the switch is not compiled in — so forgetting the macro is a loud
// test failure rather than a silent visit to the real scheduler.
// ---------------------------------------------------------------------------

/// Whether the `xv` binary these tests spawn honors `XV_SCHEDULE_RUNNER`.
///
/// A function rather than a bare `cfg!()` at each call site so the two guards
/// state the same fact once — and so the assertion in [`scheduler_output`] stays
/// a *runtime* check: a compile-time one would stop the test crate from building
/// under `--release` instead of skipping.
#[inline]
fn fake_scheduler_is_compiled_in() -> bool {
    cfg!(debug_assertions)
}

/// Return early (with a line on stderr) when the binary under test would not
/// honor the fake scheduler.
macro_rules! skip_if_release {
    () => {
        if !fake_scheduler_is_compiled_in() {
            eprintln!(
                "skipping the scheduler-touching test at {}:{}: XV_SCHEDULE_RUNNER is \
                 compiled out of a release binary, and this test would reach the real \
                 scheduler",
                file!(),
                line!()
            );
            return;
        }
    };
}

/// Spawn a command that will reach the platform scheduler.
///
/// Refuses to spawn when the fake switch is not compiled in: a release binary
/// would act on the developer's live session. See the module comment above.
fn scheduler_output(cmd: &mut std::process::Command) -> std::process::Output {
    assert!(
        fake_scheduler_is_compiled_in(),
        "BUG: a scheduler-touching xv invocation was about to run against a release \
         binary, where XV_SCHEDULE_RUNNER is compiled out. The test must begin with \
         skip_if_release!()."
    );
    cmd.output().unwrap()
}

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
        // See `fake_scheduler`: nothing here may reach the real launchd,
        // systemd or Task Scheduler.
        .env("XV_SCHEDULE_RUNNER", "fake")
        .current_dir(root);
    cmd
}

/// Point a spawned `xv` at a fake scheduler that records what it was asked to
/// run, and return the log path.
///
/// `launchctl`, `systemctl --user` and `schtasks` act on the invoking user's
/// live session under a fixed global job name — `HOME` does not sandbox them —
/// so a test that let `xv schedule uninstall` reach the real scheduler would
/// deregister the developer's own rotation schedule. The binary honors
/// `XV_SCHEDULE_RUNNER=fake` in debug builds only; the switch is compiled out
/// of a release build (`src/schedule/testing.rs`, `schedule_runner()` in
/// `src/cli/schedule_ops.rs`), so it cannot change what a shipped `xv` does.
fn fake_scheduler(cmd: &mut std::process::Command, log: &std::path::Path) {
    cmd.env("XV_SCHEDULE_RUNNER", "fake")
        .env("XV_SCHEDULE_RUNNER_LOG", log);
}

/// Every line the fake scheduler recorded.
fn scheduler_calls(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The deregistration invocation `uninstall` must issue on this platform.
fn expected_deregistration(platform: Platform) -> &'static str {
    match platform {
        Platform::Launchd => "launchctl bootout gui/",
        Platform::Systemd => "systemctl --user disable --now xv-rotate.timer",
        Platform::Schtasks => "schtasks /Delete /TN crosstache-xv-rotate /F",
    }
}

/// Bring the isolated local store into existence the way a user does: by
/// using it. A schedule is only installed against a target that already
/// exists, so resolution refuses a store that has never been opened.
fn use_the_store_once(store: &std::path::Path) {
    let out = xv_cmd_for(store).args(["list"]).output().unwrap();
    assert!(
        out.status.success(),
        "fixture setup failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
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

/// The value of a `# label:` header line in the preview, trimmed.
fn header_value<'a>(out: &'a str, label: &str) -> &'a str {
    let prefix = format!("# {label}:");
    out.lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("preview has no '{prefix}' line:\n{out}"))
        .split_once(':')
        .expect("header line has a colon")
        .1
        .trim()
}

/// Every regular file under `root`, as `(relative path, len, mtime, contents
/// hash)`. Used to prove `--print` is byte-for-byte write-free: a preview that
/// created, touched or rewrote anything shows up as a difference here.
fn snapshot_tree(root: &std::path::Path) -> Vec<(String, u64, std::time::SystemTime, u64)> {
    fn walk(
        dir: &std::path::Path,
        root: &std::path::Path,
        out: &mut Vec<(String, u64, std::time::SystemTime, u64)>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                walk(&path, root, out);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let hash = std::fs::read(&path)
                .map(|bytes| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut h);
                    h.finish()
                })
                .unwrap_or(0);
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            out.push((rel, meta.len(), mtime, hash));
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn print_renders_a_unit_without_installing_anything() {
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let (ok, out) = print_schedule(&store, &["--vault", "prod-kv"]);
    assert!(ok, "{out}");

    // The command the scheduler will run: the pinned manifest runner, never an
    // ambient `rotate --due`.
    let command = header_value(&out, "command");
    assert!(
        command.contains("schedule run --manifest "),
        "{command}\n{out}"
    );
    assert!(command.ends_with("manifest.json"), "{command}\n{out}");
    assert!(!out.contains("rotate --due --force"), "{out}");
    // A log destination, so a failed 3am run is diagnosable.
    assert!(out.contains("rotate.log"), "{out}");
    // The default cadence.
    assert!(out.contains("daily at 03:00"), "{out}");
    // The manifest that would be written, previewed verbatim.
    assert!(
        out.contains("# --- manifest.json (preview; installed_at is assigned during install) ---"),
        "{out}"
    );
    assert!(
        out.contains("\"installed_at\": \"<set-at-install>\""),
        "{out}"
    );
    assert!(out.contains("\"schema_version\": 1"), "{out}");

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
    use_the_store_once(&store);

    let (ok, out) = print_schedule(&store, &["--interval", "hourly", "--at", "00:15"]);
    assert!(ok, "{out}");
    assert!(out.contains("every hour at :15"), "{out}");

    let (ok, out) = print_schedule(&store, &["--interval", "weekly", "--at", "04:30"]);
    assert!(ok, "{out}");
    assert!(out.contains("weekly on Sunday at 04:30"), "{out}");
}

/// A relative `--log-file` must reach both the `# log:` header and the
/// manifest preview as an *absolute* path. Left raw, it produced a preview
/// carrying a relative `execution.log_path` — a manifest the schema rejects
/// at write time, after the user had already reviewed and approved it.
#[test]
fn a_relative_log_file_is_previewed_as_an_absolute_path() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);

    let (ok, out) = print_schedule(&store, &["--log-file", "logs/rotate.log"]);
    assert!(ok, "{out}");

    let logged = header_value(&out, "log");
    assert!(
        std::path::Path::new(logged).is_absolute(),
        "the log header is still relative: {logged}\n{out}"
    );
    assert!(logged.ends_with("rotate.log"), "{logged}\n{out}");
    assert!(!logged.contains(".."), "{logged}\n{out}");

    // And the manifest preview carries the same absolute value, since that is
    // the string `validate_v1` will see at install time.
    let escaped = logged.replace('\\', "\\\\");
    assert!(
        out.contains(&format!("\"log_path\": \"{escaped}\"")),
        "manifest preview does not carry {logged:?}\n{out}"
    );
}

#[test]
fn print_output_contains_a_loadable_unit_for_this_platform() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
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
    skip_if_release!();
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    for bad in ["3:0", "24:00", "03:60", "0300", "morning"] {
        let out = scheduler_output(
            xv_cmd_for(&store).args(["schedule", "install", "--at", bad, "--force"]),
        );
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
    skip_if_release!();
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = scheduler_output(xv_cmd_for(&store).args([
        "schedule",
        "install",
        "--interval",
        "fortnightly",
        "--force",
    ]));
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hourly") && stderr.contains("weekly"),
        "clap should list the valid values: {stderr}"
    );
}

#[test]
fn the_previewed_command_runs_the_pinned_manifest() {
    // The single most important property: an unattended job acts only on the
    // target recorded in the manifest, and never redefines a rotation policy.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let (ok, out) = print_schedule(&store, &["--vault", "v"]);
    assert!(ok, "{out}");

    let command_line = header_value(&out, "command");
    assert!(
        command_line.contains("schedule run --manifest "),
        "{command_line}"
    );
    assert!(
        !command_line.contains("--every"),
        "a schedule must not redefine policies: {command_line}"
    );
    assert!(!command_line.contains("--native"), "{command_line}");
    assert!(!command_line.contains("--due"), "{command_line}");
}

#[test]
fn print_headers_follow_the_golden_order_and_labels() {
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let (ok, out) = print_schedule(&store, &["--vault", "prod-kv"]);
    assert!(ok, "{out}");

    let headers: Vec<&str> = out
        .lines()
        .take_while(|l| l.starts_with("# ") && !l.starts_with("# ---"))
        .collect();
    let labels: Vec<String> = headers
        .iter()
        .map(|l| {
            l.split_once(':')
                .unwrap()
                .0
                .trim_start_matches("# ")
                .to_string()
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "scheduler",
            "schedule",
            "target",
            "backend",
            "config",
            "project",
            "cwd",
            "command",
            "log"
        ],
        "{out}"
    );
    // Values are column-aligned: every label pads to the same width.
    for line in &headers {
        let value_col = line.find(':').unwrap() + 1;
        let padding = line[value_col..].len() - line[value_col..].trim_start().len();
        assert_eq!(value_col + padding, 13, "misaligned header: {line}");
    }

    // Degenerate workspace: no alias, so the real vault names both sides.
    assert_eq!(
        header_value(&out, "target"),
        "prod-kv -> local/prod-kv",
        "{out}"
    );
    let backend = header_value(&out, "backend");
    assert!(
        backend.starts_with("local (local, sha256:"),
        "{backend}\n{out}"
    );
    assert_eq!(header_value(&out, "project"), "(none)", "{out}");
    assert!(header_value(&out, "config").ends_with("xv.conf"), "{out}");
}

#[test]
fn print_writes_nothing_and_creates_no_state_root() {
    // Invariant 7: `--print` is a read-only preview. It creates no directory,
    // manifest, result, lock or native unit, and it touches no existing file.
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let root = store.parent().unwrap();
    let state_home = tmp.path().join("state-home");

    let before = snapshot_tree(tmp.path());
    assert!(!before.is_empty(), "fixture should have created files");

    let out = xv_cmd_in(root)
        .env("XV_BACKEND", "local")
        .env("XV_STATE_HOME", &state_home)
        .args(["schedule", "install", "--print", "--vault", "prod-kv"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{combined}");

    // The preview names the state root it would use...
    let command = header_value(&combined, "command");
    assert!(
        command.contains(&state_home.to_string_lossy().to_string()),
        "XV_STATE_HOME must drive the previewed manifest path: {command}"
    );
    // ...but must not have brought it into being.
    assert!(
        !state_home.exists(),
        "--print created the state root {}",
        state_home.display()
    );

    // And nothing else changed either: no unit, no log, no touched file.
    assert_eq!(
        before,
        snapshot_tree(tmp.path()),
        "--print modified the tree"
    );
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

/// Canonicalize a fixture path the way the manifest records it: resolved,
/// and without the Windows `\\?\` verbatim prefix.
fn manifest_path(path: &std::path::Path) -> std::path::PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap();
    let text = canonical.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(stripped) if cfg!(windows) => std::path::PathBuf::from(stripped),
        _ => canonical,
    }
}

/// The JSON string literal the manifest preview prints for `path`.
fn manifest_path_json(path: &std::path::Path) -> String {
    serde_json::to_string(&manifest_path(path).to_string_lossy().into_owned()).unwrap()
}

#[test]
fn print_pins_the_config_file_rather_than_a_config_environment() {
    // The classic failure: the job runs but resolves a different config than
    // the user tested with, so it sweeps the wrong vault or none at all. The
    // manifest closes that by naming the exact file and its digest — which is
    // also why the unit may no longer carry XDG_CONFIG_HOME, an environment
    // variable that would redirect config resolution out from under it.
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let (ok, out) = print_schedule(&store, &["--vault", "v"]);
    assert!(ok, "{out}");

    let config = tmp.path().join(".config").join("xv").join("xv.conf");
    let config = manifest_path(&config);
    assert!(
        out.contains(&format!("\"config_path\": {}", manifest_path_json(&config))),
        "the manifest must pin the config file ({}): {out}",
        config.display()
    );
    assert!(out.contains("\"config_digest\": \"sha256:"), "{out}");
    assert!(
        !out.contains("XDG_CONFIG_HOME"),
        "a pinned unit may not add target-selection environment variables: {out}"
    );

    // And it starts where the manifest says resolution happened.
    let cwd = manifest_path(tmp.path());
    assert!(
        out.contains(&format!(
            "\"working_directory\": {}",
            manifest_path_json(&cwd)
        )),
        "{out}"
    );
    assert_eq!(header_value(&out, "cwd"), cwd.to_string_lossy(), "{out}");
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
    skip_if_release!();
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = scheduler_output(xv_cmd_for(&store).args(["schedule", "status"]));
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
    skip_if_release!();
    // Must converge on "absent" rather than erroring, so it is safe in teardown
    // scripts. This does invoke the platform scheduler's delete/bootout, which
    // is a no-op against a job that was never created.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = scheduler_output(xv_cmd_for(&store).args(["schedule", "uninstall"]));
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
    // What the registered job actually runs. Help that still promised the
    // pre-manifest sweep would send a reader looking for a `--vault` in a unit
    // that has none, and would describe a target resolved at 3am rather than
    // one pinned at install.
    assert!(
        stdout.contains("xv schedule run --manifest"),
        "help should name the pinned manifest runner: {stdout}"
    );
    assert!(
        stdout.contains("pinned"),
        "help should say the target is pinned at install: {stdout}"
    );
    assert!(
        !stdout.contains("rotate --due --force"),
        "the scheduled job has not run the unpinned sweep since the manifest \
         landed: {stdout}"
    );
}

/// Attach a two-vault workspace to the context store the isolated command
/// reads (`<cwd>/.xv/context`), so `--vault` means "attached alias" rather
/// than "raw vault name".
fn attach_workspace(root: &std::path::Path) {
    let context_dir = root.join(".xv");
    std::fs::create_dir_all(&context_dir).expect("create context dir");
    let context = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "payments-production", "backend": "local", "alias": "payments", "default": true },
      { "vault": "billing-production", "backend": "local", "alias": "billing" }
    ]
  }
}
"#;
    std::fs::write(context_dir.join("context"), context).expect("write context");
}

#[test]
fn print_shows_the_real_vault_behind_an_attached_alias() {
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    attach_workspace(tmp.path());

    let (ok, out) = print_schedule(&store, &["--vault", "payments"]);
    assert!(ok, "{out}");
    // The manifest must pin the vault the alias resolves to — the scheduled
    // run does not re-resolve a workspace — while the summary still shows the
    // alias the user typed.
    assert_eq!(
        header_value(&out, "target"),
        "payments -> local/payments-production",
        "{out}"
    );
    assert!(out.contains("\"workspace_alias\": \"payments\""), "{out}");
    assert!(out.contains("\"vault\": \"payments-production\""), "{out}");
}

/// Point the isolated config at a *second* local store (`store-b`), open it
/// so it exists, then rewrite the config so `local` is the original store and
/// `local-b` is a named backend for `store-b`. Attaches a workspace whose
/// default entry is on the active backend and whose `stage` alias is on
/// `local-b`.
///
/// This is the shape the interim legacy install has to get right: `stage`
/// resolves to `local-b/stage-vault`, but the *active* backend is `local`, so
/// a unit that carried the real vault name would sweep `local/stage-vault`.
fn attach_two_named_local_backends(root: &std::path::Path) {
    let conf = root.join(".config").join("xv").join("xv.conf");
    let original = std::fs::read_to_string(&conf).expect("read xv.conf");
    let store_b = root.join("store-b");
    let key_b = root.join("key-b.txt");
    std::fs::create_dir_all(&store_b).expect("create store-b");
    let path_b = store_b.to_string_lossy().replace('\\', "\\\\");
    let key_b_str = key_b.to_string_lossy().replace('\\', "\\\\");

    // Open store-b the only way a user can: by pointing the active local
    // backend at it once.
    let temporary = original.replace(
        &format!(
            "store_path = \"{}\"",
            root.join("store").to_string_lossy().replace('\\', "\\\\")
        ),
        &format!("store_path = \"{path_b}\""),
    );
    let temporary = temporary.replace(
        &format!(
            "key_file = \"{}\"",
            root.join("key.txt").to_string_lossy().replace('\\', "\\\\")
        ),
        &format!("key_file = \"{key_b_str}\""),
    );
    assert_ne!(temporary, original, "fixture config shape changed");
    std::fs::write(&conf, &temporary).expect("write temp config");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(root)
        .args(["list"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "opening store-b failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Restore the original active backend and add the named one beside it.
    let final_conf = format!(
        "{original}\n[named_backends.local-b]\ntype = \"local\"\nstore_path = \"{path_b}\"\nkey_file = \"{key_b_str}\"\ndefault_vault = \"stage-vault\"\n"
    );
    std::fs::write(&conf, final_conf).expect("write final config");

    let context_dir = root.join(".xv");
    std::fs::create_dir_all(&context_dir).expect("create context dir");
    let context = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "payments-production", "backend": "local", "alias": "payments", "default": true },
      { "vault": "stage-vault", "backend": "local-b", "alias": "stage" }
    ]
  }
}
"#;
    std::fs::write(context_dir.join("context"), context).expect("write context");
}

/// An alias attached to a backend that is *not* the active one resolves to
/// that backend, not to the active backend's same-named vault.
///
/// The other half of this — that the interim legacy unit carries the alias
/// rather than the resolved real vault, so run-time re-resolution lands on the
/// same place — is unit-tested at `cli::schedule_ops::tests`, because `--print`
/// renders the manifest runner and there is no path that renders the legacy
/// unit without registering it.
#[test]
fn print_resolves_an_alias_on_a_non_active_named_backend() {
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    attach_two_named_local_backends(tmp.path());

    let (ok, out) = print_schedule(&store, &["--vault", "stage"]);
    assert!(ok, "{out}");
    assert_eq!(
        header_value(&out, "target"),
        "stage -> local-b/stage-vault",
        "{out}"
    );
    assert!(out.contains("\"backend_name\": \"local-b\""), "{out}");
    assert!(out.contains("\"workspace_alias\": \"stage\""), "{out}");
    assert!(out.contains("\"vault\": \"stage-vault\""), "{out}");
}

#[test]
fn an_unattached_vault_name_is_rejected_with_the_attached_aliases() {
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    attach_workspace(tmp.path());

    // The raw vault name behind an alias is still not an alias: a scheduled
    // target must resolve exactly.
    let out = xv_cmd_for(&store)
        .args([
            "schedule",
            "install",
            "--print",
            "--vault",
            "payments-production",
        ])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{combined}");
    assert!(combined.contains("payments"), "{combined}");
    assert!(combined.contains("billing"), "{combined}");
    // Nothing may have been rendered, let alone installed.
    assert!(!combined.contains("# --- manifest.json"), "{combined}");
}

#[test]
fn a_missing_global_config_file_fails_before_the_scheduler_is_touched() {
    let (_cmd, tmp, _store) = xv_isolated_local_with_opts(false, false);
    // Same isolated home, but pointed at a config directory with no xv.conf:
    // an unattended run replays a saved file, so there is nothing to pin.
    let empty_config = tmp.path().join("empty-config");
    std::fs::create_dir_all(empty_config.join("xv")).unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", &empty_config)
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(tmp.path())
        .args(["schedule", "install", "--print", "--vault", "prod-kv"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!out.status.success(), "{combined}");
    assert!(combined.contains("xv.conf"), "{combined}");
    assert!(!combined.contains("# --- manifest.json"), "{combined}");
}

/// Write an isolated `xv.conf` whose local store and key do **not** exist yet,
/// and return `(root, store_path, key_path)`.
fn fresh_unopened_store(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let config_dir = tmp.join(".config");
    std::fs::create_dir_all(config_dir.join("xv")).unwrap();
    let store = tmp.join("never-opened-store");
    let key = tmp.join("never-opened-key.txt");
    let config = format!(
        r#"backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = '{store}'
key_file = '{key}'
default_vault = "default"
"#,
        store = store.display(),
        key = key.display(),
    );
    std::fs::write(config_dir.join("xv").join("xv.conf"), config).unwrap();
    (store, key)
}

fn xv_cmd_in(root: &std::path::Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_xv"));
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("NO_COLOR", "1")
        .env("XV_SCHEDULE_RUNNER", "fake")
        .current_dir(root);
    cmd
}

#[test]
fn schedule_never_creates_a_local_store_or_age_key() {
    skip_if_release!();
    // Scheduling rotation must describe a target that already exists. Neither
    // the preview nor `status` may bring a store, an identity, or a vault into
    // being — an unattended job pointed at a store xv just invented would
    // rotate nothing, forever, and quietly.
    let tmp = tempfile::tempdir().unwrap();
    let (store, key) = fresh_unopened_store(tmp.path());

    let print = xv_cmd_in(tmp.path())
        .args(["schedule", "install", "--print", "--vault", "prod-kv"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&print.stdout),
        String::from_utf8_lossy(&print.stderr)
    );

    assert!(
        !store.exists() && !key.exists(),
        "preview created local state: store={} key={}\n{combined}",
        store.exists(),
        key.exists()
    );
    // It also must not pretend the target is fine: verification fails closed.
    assert!(!print.status.success(), "{combined}");
    assert!(!combined.contains("# --- manifest.json"), "{combined}");

    let status = scheduler_output(xv_cmd_in(tmp.path()).args(["schedule", "status"]));
    let _ = status;
    assert!(
        !store.exists() && !key.exists(),
        "status created local state: store={} key={}",
        store.exists(),
        key.exists()
    );
}

#[test]
fn an_unsaved_ambient_backend_is_refused() {
    // `XV_BACKEND` is set on the child process only — never on this test
    // process. A scheduled run inherits no environment, so a schedule
    // installed under an ambient backend would sweep a different one.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let root = store.parent().unwrap();

    let out = xv_cmd_in(root)
        .env("XV_BACKEND", "azure")
        .args(["schedule", "install", "--print", "--vault", "v"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!out.status.success(), "{combined}");
    assert!(combined.contains("azure"), "{combined}");
    assert!(combined.contains("XV_BACKEND"), "{combined}");
    assert!(!combined.contains("# --- manifest.json"), "{combined}");
}

// ---------------------------------------------------------------------------
// The hidden manifest runner (`xv schedule run --manifest ...`)
// ---------------------------------------------------------------------------

/// The owned manifest path under an `XV_STATE_HOME` override, with `contents`
/// already written there.
fn seed_state_manifest(state_home: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let dir = state_home
        .join("xv")
        .join("schedules")
        .join("rotation-default");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("manifest.json");
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn a_malformed_manifest_refuses_before_any_backend_is_constructed() {
    // Invariant 4: the sweep is refused before backend construction. The
    // local store here has never been opened, so a backend constructor would
    // leave a store and an age identity behind — visible, unfakeable evidence
    // that the runner reached one.
    let tmp = tempfile::tempdir().unwrap();
    let (store, key) = fresh_unopened_store(tmp.path());
    let state = tmp.path().join("state");
    let manifest = seed_state_manifest(&state, "{ this is not json");

    let out = xv_cmd_in(tmp.path())
        .env("XV_STATE_HOME", &state)
        .args(["schedule", "run", "--manifest", manifest.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert_eq!(out.status.code(), Some(3), "stdout={stdout}stderr={stderr}");
    // stdout is data; a refused run produces none.
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.to_lowercase().contains("reinstall"),
        "the refusal must say how to recover: {stderr}"
    );
    assert!(
        !store.exists() && !key.exists(),
        "the runner constructed a backend: store={} key={}\n{stderr}",
        store.exists(),
        key.exists()
    );
}

#[test]
fn a_missing_manifest_is_reported_as_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, key) = fresh_unopened_store(tmp.path());
    let state = tmp.path().join("state");
    let manifest = state
        .join("xv")
        .join("schedules")
        .join("rotation-default")
        .join("manifest.json");

    let out = xv_cmd_in(tmp.path())
        .env("XV_STATE_HOME", &state)
        .args(["schedule", "run", "--manifest", manifest.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("missing"), "{stderr}");
    assert!(stderr.to_lowercase().contains("reinstall"), "{stderr}");
    assert!(!store.exists() && !key.exists(), "{stderr}");
}

#[test]
fn the_runner_refuses_a_manifest_outside_the_owned_location() {
    // Anyone who can hand the scheduler a different `--manifest` chooses the
    // rotation target. Only the current user's own manifest path is accepted.
    let tmp = tempfile::tempdir().unwrap();
    let (store, key) = fresh_unopened_store(tmp.path());
    let state = tmp.path().join("state");
    seed_state_manifest(&state, "{}");
    let foreign = tmp.path().join("elsewhere.json");
    std::fs::write(&foreign, "{}").unwrap();

    for arg in [
        foreign.to_str().unwrap().to_string(),
        "manifest.json".to_string(),
    ] {
        let out = xv_cmd_in(tmp.path())
            .env("XV_STATE_HOME", &state)
            .args(["schedule", "run", "--manifest", &arg])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert_eq!(out.status.code(), Some(3), "{arg}: {stderr}");
        assert!(
            String::from_utf8_lossy(&out.stdout).is_empty(),
            "{arg}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(!store.exists() && !key.exists(), "{arg}: {stderr}");
    }
}

#[test]
fn schedule_run_is_hidden_from_help() {
    // It is scheduler plumbing, not a user-facing verb: the units invoke it,
    // people never should.
    let tmp = tempfile::tempdir().unwrap();
    fresh_unopened_store(tmp.path());
    let out = xv_cmd_in(tmp.path())
        .args(["schedule", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(help.contains("install"), "{help}");
    assert!(!help.contains("\n  run"), "{help}");
}

#[test]
fn an_installing_shells_state_home_is_pinned_into_the_unit() {
    // The bug: a user whose profile sets the state-home variable gets a
    // manifest under it and a unit pointing there, but launchd and systemd
    // user units export no such variable — so at fire time the runner
    // recomputes $HOME/.local/state/... and refuses the manifest it was just
    // handed.
    //
    // Which variable that is, is platform-dependent and the design says so:
    // `XDG_STATE_HOME` is a *Unix* state root
    // (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`,
    // "Files and ownership"), while `XV_STATE_HOME` overrides on every
    // platform. Setting XDG_STATE_HOME on Windows selects nothing, so the test
    // asks with the variable this platform actually resolves.
    let state_var = if cfg!(windows) {
        "XV_STATE_HOME"
    } else {
        "XDG_STATE_HOME"
    };
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let root = store.parent().unwrap();
    let state = root.join("custom-state");
    std::fs::create_dir_all(&state).unwrap();

    let out = xv_cmd_for(&store)
        .env(state_var, &state)
        .args(["schedule", "install", "--print", "--vault", "prod-kv"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{combined}");

    // The manifest the unit points at lives under the override...
    let expected = state
        .join("xv")
        .join("schedules")
        .join("rotation-default")
        .join("manifest.json");
    assert!(
        combined.contains(&expected.display().to_string()),
        "expected {} in:\n{combined}",
        expected.display()
    );
    // ...the default log must come from that same root, or the unit's two
    // halves would describe two different installs.
    let expected_log = state.join("xv").join("rotate.log");
    assert!(
        combined.contains(&expected_log.display().to_string()),
        "expected the log at {} in:\n{combined}",
        expected_log.display()
    );
    // ...so the unit must carry the variable that put them there.
    assert!(
        combined.contains(&format!("{state_var}={}", state.display()))
            || combined.contains(&format!(
                "<key>{state_var}</key>\n        <string>{}</string>",
                state.display()
            )),
        "the unit does not pin {state_var}:\n{combined}"
    );
}

#[test]
fn no_state_variable_means_no_pin_in_the_unit() {
    // The common case must stay exactly as it was: HOME picked the state root,
    // and the unit says nothing about state directories.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let (ok, out) = print_schedule(&store, &["--vault", "prod-kv"]);
    assert!(ok, "{out}");
    // The preview's unit section carries no state-home variable at all.
    let unit = out
        .split_once("# --- ")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(out.clone());
    assert!(!unit.contains("XDG_STATE_HOME"), "{out}");
    assert!(!unit.contains("XV_STATE_HOME"), "{out}");
}

// ---------------------------------------------------------------------------
// `xv schedule run`: the pinned sweep and the drift refusals
//
// These do register a manifest, but never a native job: the manifest is the
// exact JSON `--print` renders (the crate's own serializer), stamped and
// written into an `XV_STATE_HOME` sandbox. Nothing touches launchd, systemd or
// schtasks.
// ---------------------------------------------------------------------------

/// The manifest JSON block out of an `xv schedule install --print` rendering.
fn manifest_json_from_preview(preview: &str) -> String {
    let mut lines = preview
        .lines()
        .skip_while(|l| !l.starts_with("# --- manifest.json"));
    lines.next().expect("preview contains a manifest block");
    let body: Vec<&str> = lines.take_while(|l| !l.starts_with("# --- ")).collect();
    body.join("\n").trim().to_string()
}

/// Render the manifest installation would write and publish it under
/// `state_home` the way installation does, without registering a native job.
fn install_manifest(
    root: &std::path::Path,
    state: &std::path::Path,
    extra: &[&str],
) -> std::path::PathBuf {
    let mut args = vec!["schedule", "install", "--print"];
    args.extend_from_slice(extra);
    let out = xv_cmd_in(root)
        .env("XV_BACKEND", "local")
        .env("XV_STATE_HOME", state)
        .args(&args)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "install --print failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json =
        manifest_json_from_preview(&stdout).replace("<set-at-install>", "2026-09-09T15:04:05Z");
    assert!(json.contains("\"schema_version\""), "{json}");

    let path = seed_state_manifest(state, &format!("{json}\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

/// Run the pinned sweep from a directory that is *not* the recorded one, with
/// selection variables that contradict the manifest. Neither may matter.
fn run_pinned(
    elsewhere: &std::path::Path,
    root: &std::path::Path,
    state: &std::path::Path,
    manifest: &std::path::Path,
) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("NO_COLOR", "1")
        // Deliberately *not* set: `XV_NO_PARENT_CONFIG` is an ambient
        // discovery switch the scheduler's environment never carries, so a
        // runner that depended on it would refuse every night in the field
        // while passing here.
        // Deliberately wrong: the runner replays recorded inputs.
        .env("XV_BACKEND", "azure")
        .env("XV_ENV", "no-such-environment")
        .env("XV_STATE_HOME", state)
        .current_dir(elsewhere)
        .args(["schedule", "run", "--manifest", manifest.to_str().unwrap()])
        .output()
        .unwrap()
}

/// [`run_pinned`] from an environment that carries no `XV_ENV` at all — the
/// shape the rendered units actually produce, since they pin no selection
/// variable. The recorded environment therefore has to come from the manifest.
fn run_pinned_without_ambient_env(
    elsewhere: &std::path::Path,
    root: &std::path::Path,
    state: &std::path::Path,
    manifest: &std::path::Path,
) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("NO_COLOR", "1")
        .env("XV_BACKEND", "azure")
        .env("XV_STATE_HOME", state)
        .current_dir(elsewhere)
        .args(["schedule", "run", "--manifest", manifest.to_str().unwrap()])
        .output()
        .unwrap()
}

/// [`run_pinned`] with a specific `XV_ENV` inherited from the surrounding
/// user manager.
///
/// Units carry no `XV_ENV` of their own, but they also cannot *unset* one that
/// systemd `environment.d` or `launchctl setenv` exported into every job the
/// user manager starts — so this is the shape a real scheduled run can be
/// handed, and the recorded environment still has to win.
fn run_pinned_with_ambient_env(
    elsewhere: &std::path::Path,
    root: &std::path::Path,
    state: &std::path::Path,
    manifest: &std::path::Path,
    xv_env: &str,
) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join(".config"))
        .env("NO_COLOR", "1")
        .env("XV_STATE_HOME", state)
        .env("XV_ENV", xv_env)
        .current_dir(elsewhere)
        .args(["schedule", "run", "--manifest", manifest.to_str().unwrap()])
        .output()
        .unwrap()
}

/// `xv vault create <vault>` in the pinned store, under the given project
/// environment (a project file that defines environments and no `default_env`
/// makes every ordinary command name one).
fn create_vault(store: &std::path::Path, vault: &str, env: &str) {
    let out = xv_cmd_for(store)
        .args(["vault", "create", vault, "--env", env])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// [`set_secret`] into whatever vault the named environment selects.
///
/// `xv set` has no `--vault`; the project environment is how a vault other
/// than the configured default is addressed without writing a context file,
/// which would otherwise participate in the recorded target.
fn set_secret_in_env(store: &std::path::Path, env: &str, name: &str, value: &str) {
    let out = xv_cmd_for(store)
        .args(["set", name, "--value", value, "--env", env])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// [`make_due`] for a secret in whatever vault the named environment selects.
fn make_due_in_env(store: &std::path::Path, env: &str, name: &str) {
    let out = xv_cmd_for(store)
        .args([
            "update",
            name,
            "--env",
            env,
            "--tag",
            "xv:rotate_every=30d",
            "--tag",
            "xv:rotated_at=2020-01-01T00:00:00Z",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn set_secret(store: &std::path::Path, name: &str, value: &str) {
    let out = xv_cmd_for(store)
        .args(["set", name, "--value", value])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Back-date a secret's rotation stamp so `--due` picks it up.
fn make_due(store: &std::path::Path, name: &str) {
    let out = xv_cmd_for(store)
        .args([
            "update",
            name,
            "--tag",
            "xv:rotate_every=30d",
            "--tag",
            "xv:rotated_at=2020-01-01T00:00:00Z",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn secret_value(store: &std::path::Path, name: &str) -> String {
    secret_value_in_env(store, name, None)
}

/// [`secret_value`] with an explicit `--env`, for fixtures whose `.xv.toml`
/// defines environments and no `default_env`: every ordinary command in that
/// directory must name one, which is precisely the fail-closed condition the
/// runner has to satisfy from the manifest instead.
fn secret_value_in_env(store: &std::path::Path, name: &str, env: Option<&str>) -> String {
    let mut cmd = xv_cmd_for(store);
    cmd.args(["get", name, "--raw"]);
    if let Some(env) = env {
        cmd.args(["--env", env]);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fixture with one due secret in the pinned store, a same-named due secret
/// in a *second* local store attached as the named backend `local-b`, and a
/// published manifest pinning the first.
struct PinnedRun {
    tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    store: std::path::PathBuf,
    decoy_store: std::path::PathBuf,
    state: std::path::PathBuf,
    elsewhere: std::path::PathBuf,
    manifest: std::path::PathBuf,
}

impl PinnedRun {
    fn config_path(&self) -> std::path::PathBuf {
        self.root.join(".config").join("xv").join("xv.conf")
    }

    fn value(&self) -> String {
        secret_value(&self.store, "STALE")
    }

    fn decoy_snapshot(&self) -> Vec<(String, u64, std::time::SystemTime, u64)> {
        snapshot_tree(&self.decoy_store)
    }

    fn run(&self) -> std::process::Output {
        run_pinned(&self.elsewhere, &self.root, &self.state, &self.manifest)
    }

    fn run_without_ambient_env(&self) -> std::process::Output {
        run_pinned_without_ambient_env(&self.elsewhere, &self.root, &self.state, &self.manifest)
    }

    fn run_with_ambient_env(&self, xv_env: &str) -> std::process::Output {
        run_pinned_with_ambient_env(
            &self.elsewhere,
            &self.root,
            &self.state,
            &self.manifest,
            xv_env,
        )
    }
}

/// Build the fixture. `extra` are additional `schedule install` flags, and
/// `before_install` runs with the fixture assembled but the manifest not yet
/// rendered — the place to add a `.xv.toml` or a context file that must be
/// part of the recorded target.
fn pinned_run_fixture(extra: &[&str], before_install: impl FnOnce(&std::path::Path)) -> PinnedRun {
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    // Canonical, because that is the spelling the manifest records (macOS
    // hands out `/var/...` tempdirs that canonicalize to `/private/var/...`).
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    pinned_run_fixture_in(tmp, root, store, extra, before_install)
}

/// [`pinned_run_fixture`] over a root the caller chose, so a test can put the
/// whole target — config, store, state root and working directory — at a path
/// containing spaces.
fn pinned_run_fixture_in(
    tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    store: std::path::PathBuf,
    extra: &[&str],
    before_install: impl FnOnce(&std::path::Path),
) -> PinnedRun {
    use_the_store_once(&store);

    // The decoy: a second local store holding the same due secret name in the
    // same vault. A run that resolved its target from the ambient environment
    // instead of the manifest could land here.
    let conf = root.join(".config").join("xv").join("xv.conf");
    let original = std::fs::read_to_string(&conf).unwrap();
    let decoy_store = root.join("store-b");
    // The decoy's age identity gets its own directory: the recipients file is
    // derived as `<key file dir>/recipients.txt`, so two keys side by side in
    // one directory would share (and clobber) one recipients file.
    let decoy_key = root.join("key-b").join("key-b.txt");
    std::fs::create_dir_all(&decoy_store).unwrap();
    std::fs::create_dir_all(decoy_key.parent().unwrap()).unwrap();
    // The *config's own* spelling of the key path, which is the one beside the
    // store rather than `root`'s: on macOS a tempdir is handed out as
    // `/var/...` and canonicalizes to `/private/var/...`, so replacing the
    // canonical spelling silently matched nothing and left the decoy store
    // sharing the pinned identity — which made `local-b` unusable as a
    // *schedule target*, not just as a decoy.
    let pinned_key = store
        .parent()
        .expect("the store has a parent")
        .join("key.txt");
    let swapped = original
        .replace(
            &store.to_string_lossy().replace('\\', "\\\\"),
            &decoy_store.to_string_lossy().replace('\\', "\\\\"),
        )
        .replace(
            &pinned_key.to_string_lossy().replace('\\', "\\\\"),
            &decoy_key.to_string_lossy().replace('\\', "\\\\"),
        );
    assert!(
        swapped.contains(&decoy_store.to_string_lossy().replace('\\', "\\\\"))
            && swapped.contains(&decoy_key.to_string_lossy().replace('\\', "\\\\")),
        "fixture config shape changed: neither the store nor the key was repointed"
    );
    std::fs::write(&conf, &swapped).unwrap();
    set_secret(&store, "STALE", "decoy-value");
    make_due(&store, "STALE");

    // Restore the pinned store and attach the decoy as a named backend.
    let path_b = decoy_store.to_string_lossy().replace('\\', "\\\\");
    let key_b = decoy_key.to_string_lossy().replace('\\', "\\\\");
    std::fs::write(
        &conf,
        format!(
            "{original}\n[named_backends.local-b]\ntype = \"local\"\nstore_path = \"{path_b}\"\nkey_file = \"{key_b}\"\ndefault_vault = \"default\"\n"
        ),
    )
    .unwrap();

    set_secret(&store, "STALE", "pinned-value");
    make_due(&store, "STALE");

    before_install(&root);

    let state = root.join("state");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let manifest = install_manifest(&root, &state, extra);

    PinnedRun {
        tmp,
        root,
        store,
        decoy_store,
        state,
        elsewhere,
        manifest,
    }
}

#[test]
fn a_pinned_run_rotates_the_recorded_target_and_nothing_else() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    let decoy_before = fixture.decoy_snapshot();
    assert_eq!(before, "pinned-value");

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");

    assert_ne!(
        fixture.value(),
        before,
        "the pinned secret must rotate: {stderr}"
    );
    assert_eq!(
        fixture.decoy_snapshot(),
        decoy_before,
        "the run touched the decoy store"
    );
    // No secret value may appear in the run's chatter.
    assert!(!stderr.contains("pinned-value"), "{stderr}");
    drop(fixture.tmp);
}

#[test]
fn changed_config_bytes_refuse_the_run() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    let body = std::fs::read_to_string(fixture.config_path()).unwrap();
    std::fs::write(fixture.config_path(), format!("{body}\n# edited\n")).unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    // Against the path *as the manifest recorded it*, which is what the
    // refusal prints: on Windows the fixture holds the canonicalized (verbatim,
    // long-name) spelling and xv records the canonical one with the verbatim
    // prefix stripped, so comparing to `fixture.config_path()` compares two
    // spellings of the same file.
    assert!(
        stderr.contains(&format!(
            "config_digest changed; review {} and reinstall",
            recorded(&fixture.manifest, "/target/config_path")
        )),
        "{stderr}"
    );
    assert!(stderr.to_lowercase().contains("reinstall"), "{stderr}");
    assert_eq!(fixture.value(), before, "a refused run must not rotate");
}

#[test]
fn a_changed_project_file_refuses_the_run() {
    let project = "default_env = \"production\"\n\n[env.production]\nvault = \"default\"\n";
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::write(root.join(".xv.toml"), project).unwrap();
    });
    let before = fixture.value();
    std::fs::write(
        fixture.root.join(".xv.toml"),
        format!("{project}\n# edited\n"),
    )
    .unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("project_digest changed"), "{stderr}");
    assert_eq!(fixture.value(), before);
}

/// The recorded environment governs the sweep, not the project file's own
/// defaults and not the runner's (empty) environment.
///
/// The project file defines two environments and **no** `default_env`, so
/// every project-profile lookup in the rotation path fails closed unless an
/// environment was selected. Installation selects `production` with `--env`
/// and records it; the sweep runs from a directory that is not the recorded
/// one, with no `XV_ENV` — exactly what a rendered unit provides — and must
/// still rotate.
#[test]
fn the_sweep_replays_the_recorded_environment() {
    let project = "[env.production]\nvault = \"default\"\n\n\
                   [env.staging]\nvault = \"default\"\n\n\
                   [[types.deploy-token.fields]]\nname = \"token\"\nkind = \"secret\"\nprimary = true\n";
    let fixture = pinned_run_fixture(&["--env", "production"], |root| {
        std::fs::write(root.join(".xv.toml"), project).unwrap();
    });
    let value = || secret_value_in_env(&fixture.store, "STALE", Some("production"));
    let before = value();
    assert_eq!(before, "pinned-value");

    // The manifest records the environment installation resolved.
    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("\"environment\": \"production\""),
        "{recorded}"
    );

    let out = fixture.run_without_ambient_env();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("no active environment") && !stderr.contains("not defined"),
        "the sweep fell back to ambient env resolution: {stderr}"
    );
    assert_ne!(value(), before, "the pinned secret must rotate: {stderr}");
    drop(fixture.tmp);
}

/// An `XV_ENV` inherited from the user manager may not outrank the recorded
/// environment.
///
/// `project::resolve_env` reads `XV_ENV` *first* and the config's `env_flag`
/// second, and a rendered unit cannot unset a variable systemd
/// `environment.d` / `launchctl setenv` exported into every job — so the
/// runner pins `XV_ENV` in its own process to the recorded name before the
/// sweep. Here the manifest records `production` (vault `default`) while the
/// inherited environment says `staging` (vault `staging`): the production
/// vault must rotate, and the staging vault must be left exactly as it was.
#[test]
fn an_ambient_xv_env_does_not_override_the_recorded_environment() {
    let project = "[env.production]\nvault = \"default\"\n\n\
                   [env.staging]\nvault = \"staging\"\n\n\
                   [[types.deploy-token.fields]]\nname = \"token\"\nkind = \"secret\"\nprimary = true\n";
    let fixture = pinned_run_fixture(&["--env", "production"], |root| {
        let store = root.join("store");
        std::fs::write(root.join(".xv.toml"), project).unwrap();
        create_vault(&store, "staging", "production");
        set_secret_in_env(&store, "staging", "STALE", "staging-value");
        make_due_in_env(&store, "staging", "STALE");
    });

    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("\"environment\": \"production\""),
        "{recorded}"
    );

    let production = || secret_value_in_env(&fixture.store, "STALE", Some("production"));
    let staging = || secret_value_in_env(&fixture.store, "STALE", Some("staging"));
    let before = production();
    assert_eq!(before, "pinned-value");
    assert_eq!(staging(), "staging-value");

    let out = fixture.run_with_ambient_env("staging");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_ne!(
        production(),
        before,
        "the recorded production vault must rotate: {stderr}"
    );
    assert_eq!(
        staging(),
        "staging-value",
        "the inherited XV_ENV selected the staging vault: {stderr}"
    );
    drop(fixture.tmp);
}

/// An inherited `XV_ENV` may not *invent* a selection for a manifest that
/// recorded none.
///
/// The project file here defines no environments at all, so installation
/// records `environment: null`. An ambient `XV_ENV` naming anything at all
/// would then fail closed the moment something in the rotation path resolves a
/// project profile — the sweep has to remove the variable, not merely ignore
/// it.
#[test]
fn an_ambient_xv_env_cannot_select_where_the_manifest_recorded_none() {
    let project =
        "[[types.deploy-token.fields]]\nname = \"token\"\nkind = \"secret\"\nprimary = true\n";
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::write(root.join(".xv.toml"), project).unwrap();
    });

    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("\"environment\": null"),
        "the fixture must record no environment: {recorded}"
    );

    let before = fixture.value();
    let out = fixture.run_with_ambient_env("staging");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("no environments") && !stderr.contains("not defined"),
        "the sweep fell back to ambient env resolution: {stderr}"
    );
    assert_ne!(
        fixture.value(),
        before,
        "the pinned secret must rotate: {stderr}"
    );
    drop(fixture.tmp);
}

#[test]
fn a_changed_participating_context_refuses_the_run() {
    let context = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "default", "backend": "local", "alias": "pinned", "default": true }
    ]
  }
}
"#;
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::create_dir_all(root.join(".xv")).unwrap();
        std::fs::write(root.join(".xv").join("context"), context).unwrap();
    });
    let before = fixture.value();
    std::fs::write(
        fixture.root.join(".xv").join("context"),
        format!("{context}\n"),
    )
    .unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("context_digest changed"), "{stderr}");
    assert_eq!(fixture.value(), before);
}

/// A context-pinned schedule must survive its own success.
///
/// `execute_secret_rotate` used to bump the ambient context's usage counters
/// on every rotation. When the context's `current` vault is the very vault the
/// schedule targets, that rewrote the exact file `context_digest` pins, so the
/// first successful firing made every later firing refuse with
/// `context_digest changed`. Two firings, both green, and the file untouched.
#[test]
fn a_pinned_sweep_does_not_invalidate_its_own_context() {
    // `current` names the target vault — the only shape in which
    // `update_usage` rewrites the file — and the workspace block is what makes
    // the context participate in resolution, so its digest is recorded.
    let context = r#"{
  "current": {
    "vault_name": "default",
    "resource_group": null,
    "subscription_id": null,
    "storage_container": null,
    "last_used": "2026-01-01T00:00:00Z",
    "usage_count": 1
  },
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "default", "backend": "local", "alias": "pinned", "default": true }
    ]
  }
}
"#;
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::create_dir_all(root.join(".xv")).unwrap();
        std::fs::write(root.join(".xv").join("context"), context).unwrap();
    });
    let context_file = fixture.root.join(".xv").join("context");
    let recorded = std::fs::read(&context_file).unwrap();
    assert_eq!(
        recorded,
        context.as_bytes(),
        "the fixture's install must not have rewritten the context either"
    );
    // The manifest really did pin this file; otherwise the test would pass
    // for the wrong reason.
    let manifest_body = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        manifest_body.contains("context_digest"),
        "the context did not participate in the recorded target: {manifest_body}"
    );

    let mut values = Vec::new();
    for firing in 1..=2 {
        // Re-arm before each firing: a successful rotation refreshes
        // `xv:rotated_at`, so without this the second run would find nothing
        // due and prove nothing about a run that actually rotates.
        make_due(&fixture.store, "STALE");
        let out = fixture.run();
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "firing {firing} did not succeed: {combined}"
        );
        assert!(
            !combined.contains("context_digest"),
            "firing {firing} complained about the context: {combined}"
        );
        values.push(fixture.value());
        assert_eq!(
            std::fs::read(&context_file).unwrap(),
            recorded,
            "firing {firing} rewrote the pinned context file"
        );
    }

    // Both firings really rotated, so the second one was not a no-op that
    // happened to leave the context alone.
    assert_ne!(values[0], "pinned-value");
    assert_ne!(values[1], values[0]);
}

#[test]
fn a_missing_working_directory_refuses_the_run() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    // Derived from the recorded value so the result is absolute and normalized
    // in whatever spelling this platform records, and so it certainly does not
    // exist.
    edit_manifest(&fixture.manifest, |json| {
        let recorded = json["execution"]["working_directory"]
            .as_str()
            .expect("the manifest records a working directory")
            .to_string();
        json["execution"]["working_directory"] =
            serde_json::Value::String(format!("{recorded}-gone"));
    });

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("working_directory"), "{stderr}");
    assert_eq!(fixture.value(), before);
}

#[test]
fn a_binary_path_the_running_process_does_not_match_refuses_the_run() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    let body = std::fs::read_to_string(&fixture.manifest).unwrap();
    let replacement = json_path(&fixture.root.join("xv-gone"));
    let edited = body
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("\"binary_path\"") {
                format!("    \"binary_path\": \"{replacement}\",")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(edited, body, "manifest shape changed: {body}");
    std::fs::write(&fixture.manifest, edited).unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("binary_path"), "{stderr}");
    assert_eq!(fixture.value(), before);
}

#[test]
fn a_changed_backend_identity_refuses_the_run() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    // Repoint the pinned local backend at the decoy's store: the account the
    // manifest pinned is no longer what the config selects.
    let body = std::fs::read_to_string(fixture.config_path()).unwrap();
    let moved = body.replacen(
        &format!("store_path = \"{}\"", json_path(&fixture.store)),
        &format!("store_path = \"{}\"", json_path(&fixture.decoy_store)),
        1,
    );
    assert_ne!(moved, body, "fixture config shape changed: {body}");
    std::fs::write(fixture.config_path(), moved).unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");

    // Put the config back so the pinned store is readable again.
    std::fs::write(fixture.config_path(), &body).unwrap();

    // Both reasons, in manifest-field order.
    let config_at = stderr.find("config_digest changed").expect(&stderr);
    let identity_at = stderr
        .find("backend_identity changed for local; review the account/provider and reinstall")
        .expect(&stderr);
    assert!(config_at < identity_at, "{stderr}");
    assert_eq!(fixture.value(), before);
}

/// Edit the published manifest through `serde_json`, not by substituting a
/// path string the test happens to hold.
///
/// The recorded spelling of a path is *xv's*, not the fixture's: the fixture
/// canonicalizes its tempdir, which on Windows yields a verbatim
/// `\\?\C:\...` path (and resolves an 8.3 short name such as `RUNNER~1` to
/// its long form), while the manifest records the canonical path with the
/// verbatim prefix stripped. A `body.replace(<fixture path>)` therefore matches
/// nothing and silently leaves the manifest unedited — which is a passing
/// `replace` and a failing assertion, on Windows only.
fn edit_manifest(path: &std::path::Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let body = std::fs::read_to_string(path).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("manifest is not JSON: {e}\n{body}"));
    edit(&mut json);
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&json).unwrap()),
    )
    .unwrap();
}

/// A string field of the published manifest, by JSON pointer — the spelling xv
/// recorded, which is what it prints back in a drift refusal.
fn recorded(manifest: &std::path::Path, pointer: &str) -> String {
    let body = std::fs::read_to_string(manifest).unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    json.pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("manifest has no string at {pointer}:\n{body}"))
        .to_string()
}

/// A path as xv spells it back in its own output.
///
/// The fixtures canonicalize their tempdir so the recorded target matches what
/// xv records, and on Windows `std::fs::canonicalize` returns a *verbatim*
/// path (`\\?\C:\...`, or `\\?\UNC\server\share\...`). xv strips that
/// prefix from every path it records or prints, so an expectation built from
/// the fixture's own spelling compares two different renderings of the same
/// file and fails on Windows only. The crate's `strip_verbatim_prefix` is
/// `pub(crate)`, so integration tests need their own copy of the rule.
#[cfg(windows)]
fn display_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match text.strip_prefix(r"\\?\") {
        // Only a plain drive path survives without the prefix, matching
        // `crate::utils::helpers::strip_verbatim_prefix`.
        Some(rest)
            if rest.len() >= 2
                && rest.as_bytes()[0].is_ascii_alphabetic()
                && rest.as_bytes()[1] == b':' =>
        {
            rest.to_string()
        }
        _ => text,
    }
}

/// No other platform has verbatim prefixes; this is the identity.
#[cfg(not(windows))]
fn display_path(path: &std::path::Path) -> String {
    path.display().to_string()
}

/// A path as it appears inside the JSON/TOML fixtures (Windows separators are
/// escaped in both).
fn json_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

// ---------------------------------------------------------------------------
// Ownership: `xv schedule status` and `xv schedule uninstall`
//
// These write unit files into a temp `HOME` and a manifest into an
// `XV_STATE_HOME` sandbox, which is all `status` and `uninstall` read to decide
// ownership. **No test registers a real job**: the scheduler is only ever
// *queried*, and `uninstall`'s deregistration is a no-op against a job that was
// never created.
// ---------------------------------------------------------------------------

use crosstache::schedule::{
    render, unit_paths_for, Platform, RotationSchedule, ScheduleCommand, ScheduleInterval,
    UnitPaths,
};

/// The platform this host schedules on, or `None` when it has no scheduler we
/// manage (a container without systemd) — where these tests have nothing to
/// say and `xv` correctly refuses before looking at anything.
fn host_platform() -> Option<Platform> {
    Platform::detect().ok()
}

/// A schedule shaped like the one an install would render, for `home`.
fn a_schedule(home: &std::path::Path, command: ScheduleCommand) -> RotationSchedule {
    RotationSchedule {
        interval: ScheduleInterval::Daily {
            hour: 3,
            minute: 30,
        },
        command,
        binary: home.join("bin").join("xv"),
        log_path: home.join(".local/state/xv/rotate.log"),
        home: home.to_path_buf(),
        state_home: None,
    }
}

/// Write the units an *older* `xv` installed: the same renderer, the
/// pre-manifest `rotate --due --force` command.
fn seed_legacy_units(
    platform: Platform,
    home: &std::path::Path,
    vault: &str,
) -> Vec<std::path::PathBuf> {
    let paths = UnitPaths::for_platform(platform, home);
    std::fs::create_dir_all(&paths.dir).unwrap();
    let schedule = a_schedule(
        home,
        ScheduleCommand::LegacyRotateDue {
            vault: Some(vault.to_string()),
        },
    );
    render(platform, &schedule, &paths)
        .into_iter()
        .map(|unit| {
            std::fs::write(&unit.path, &unit.contents).unwrap();
            unit.path
        })
        .collect()
}

/// Write the units a current `xv` installs, pointing at `manifest`.
fn seed_pinned_units(
    platform: Platform,
    home: &std::path::Path,
    manifest: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let paths = UnitPaths::for_platform(platform, home);
    std::fs::create_dir_all(&paths.dir).unwrap();
    let schedule = a_schedule(
        home,
        ScheduleCommand::ManifestRun {
            manifest: manifest.to_path_buf(),
            working_directory: home.to_path_buf(),
        },
    );
    render(platform, &schedule, &paths)
        .into_iter()
        .map(|unit| {
            std::fs::write(&unit.path, &unit.contents).unwrap();
            unit.path
        })
        .collect()
}

/// Write the units a current `xv` installs *for this exact manifest*.
///
/// `seed_pinned_units` renders a schedule of its own invention; that is fine
/// when the manifest is missing, but a seeded unit whose executable, cadence
/// or log path disagrees with a manifest that *is* there is unit drift — which
/// `status` now reports, correctly, as a refusal. A fixture that wants a
/// healthy managed schedule has to render the unit the installer would have
/// rendered, so every field is read back out of the published manifest.
fn seed_units_for_manifest(
    platform: Platform,
    home: &std::path::Path,
    manifest: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).unwrap()).unwrap();
    let number = |value: &serde_json::Value| u32::try_from(value.as_u64().unwrap()).unwrap();
    let cadence = &body["cadence"];
    let (hour, minute) = (number(&cadence["hour"]), number(&cadence["minute"]));
    let interval = match cadence["kind"].as_str().unwrap() {
        "hourly" => ScheduleInterval::Hourly { minute },
        "daily" => ScheduleInterval::Daily { hour, minute },
        "weekly" => ScheduleInterval::Weekly {
            weekday: 0,
            hour,
            minute,
        },
        other => panic!("unexpected cadence kind {other}"),
    };
    let path_of = |value: &serde_json::Value| std::path::PathBuf::from(value.as_str().unwrap());

    let paths = UnitPaths::for_platform(platform, home);
    std::fs::create_dir_all(&paths.dir).unwrap();
    let schedule = RotationSchedule {
        interval,
        command: ScheduleCommand::ManifestRun {
            manifest: manifest.to_path_buf(),
            working_directory: path_of(&body["execution"]["working_directory"]),
        },
        binary: path_of(&body["execution"]["binary_path"]),
        log_path: path_of(&body["execution"]["log_path"]),
        home: home.to_path_buf(),
        state_home: None,
    };
    render(platform, &schedule, &paths)
        .into_iter()
        .map(|unit| {
            std::fs::write(&unit.path, &unit.contents).unwrap();
            unit.path
        })
        .collect()
}

/// The reinstall hint has to be a command the user can paste, which means the
/// name they installed with — the recorded workspace alias — not a placeholder
/// and not the real vault behind it. `--vault default` here would send them at
/// a different target than `--vault pinned`.
#[test]
fn a_reinstall_hint_names_the_alias_the_schedule_was_installed_with() {
    skip_if_release!();
    let context = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "default", "backend": "local", "alias": "pinned", "default": true }
    ]
  }
}
"#;
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::create_dir_all(root.join(".xv")).unwrap();
        std::fs::write(root.join(".xv").join("context"), context).unwrap();
    });
    let manifest_body = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        manifest_body.contains("\"workspace_alias\": \"pinned\""),
        "the fixture did not record the alias: {manifest_body}"
    );

    // Drift the pinned config so status renders the "accept the new target"
    // hint at all.
    let conf = fixture.config_path();
    let body = std::fs::read_to_string(&conf).unwrap();
    std::fs::write(&conf, format!("{body}\n# a later edit\n")).unwrap();

    let out = schedule_status(&fixture.root, &fixture.state);
    assert!(out.contains("Drift:     refused"), "{out}");
    assert!(
        out.contains("xv schedule install --vault pinned"),
        "the hint did not name the recorded alias: {out}"
    );
    assert!(
        !out.contains("<alias-or-vault>"),
        "the hint left the placeholder in although the alias is known: {out}"
    );
}

fn schedule_status(root: &std::path::Path, state: &std::path::Path) -> String {
    let out = scheduler_output(
        xv_cmd_in(root)
            .env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", state)
            .args(["schedule", "status"]),
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn status_labels_a_legacy_unit_and_refuses_to_vouch_for_its_target() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        // Task Scheduler holds the command itself; there is no unit file to
        // seed, and this test may not register a real task.
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let state = root.join("state");
    seed_legacy_units(platform, &root, "payments-production");

    let out = schedule_status(&root, &state);

    assert!(out.contains("Ownership: legacy-unpinned"), "{out}");
    assert!(
        out.contains("rotate --due --force --vault payments-production"),
        "the actual installed command must be reported: {out}"
    );
    assert!(
        out.contains("Target:    unverified"),
        "a legacy unit records no target, and status may not invent one: {out}"
    );
    assert!(
        !out.contains("Drift:"),
        "there is no recorded target to compare against: {out}"
    );
}

#[test]
fn status_labels_a_manifest_with_no_unit_as_orphaned() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});

    let out = schedule_status(&fixture.root, &fixture.state);

    assert!(out.contains("Ownership: orphaned-manifest"), "{out}");
    assert!(out.contains("Target:    "), "{out}");
    assert!(out.contains("Drift:     valid"), "{out}");
    assert!(
        out.contains("uninstall"),
        "the repair hints are missing: {out}"
    );
}

#[test]
fn status_reports_a_managed_schedule_and_the_drift_it_would_refuse_on() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);

    let clean = schedule_status(&fixture.root, &fixture.state);
    assert!(clean.contains("Ownership: managed"), "{clean}");
    assert!(clean.contains("Drift:     valid"), "{clean}");

    // Change the pinned configuration file. Status must say the next run would
    // refuse — without contacting the provider to find out.
    let conf = fixture.config_path();
    let body = std::fs::read_to_string(&conf).unwrap();
    std::fs::write(&conf, format!("{body}\n# a later edit\n")).unwrap();

    let drifted = schedule_status(&fixture.root, &fixture.state);
    assert!(drifted.contains("Ownership: managed"), "{drifted}");
    assert!(drifted.contains("Drift:     refused"), "{drifted}");
    assert!(
        drifted.contains("unsafe to run"),
        "a schedule that will refuse tonight may not be announced as healthy: {drifted}"
    );
    assert!(drifted.contains("config_digest"), "{drifted}");
}

#[test]
fn status_reports_a_foreign_file_at_an_owned_path_without_touching_it() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let state = root.join("state");
    let victim = seed_legacy_units(platform, &root, "v")[0].clone();
    let mine = "# my own job, not xv's\n";
    std::fs::write(&victim, mine).unwrap();

    let out = schedule_status(&root, &state);

    assert!(out.contains("Ownership: foreign"), "{out}");
    assert!(out.contains(&victim.display().to_string()), "{out}");
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        mine,
        "status must never modify a file it did not write"
    );
}

#[test]
fn uninstall_removes_the_manifest_and_keeps_every_other_file() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    let fixture = pinned_run_fixture(&[], |_| {});
    let dir = fixture.manifest.parent().unwrap().to_path_buf();

    // Everything the design says uninstall retains.
    let last_run = dir.join("last-run.json");
    let run_lock = dir.join("run.lock");
    let recovery = dir.join("recovery").join("20260909T000000Z-manifest.json");
    let unrelated = dir.join("notes.txt");
    std::fs::create_dir_all(recovery.parent().unwrap()).unwrap();
    for (path, body) in [
        (&last_run, "{\"state\":\"success\"}"),
        (&run_lock, ""),
        (&recovery, "{\"prior\":true}"),
        (&unrelated, "mine"),
    ] {
        std::fs::write(path, body).unwrap();
    }

    // A file xv did not write, at a path it owns.
    let foreign = if platform == Platform::Schtasks {
        None
    } else {
        let paths = UnitPaths::for_platform(platform, &fixture.root);
        std::fs::create_dir_all(&paths.dir).unwrap();
        let path = seed_legacy_units(platform, &fixture.root, "v")[0].clone();
        std::fs::write(&path, "# my own job\n").unwrap();
        Some(path)
    };

    let log = fixture.root.join("scheduler-calls.log");
    let mut cmd = xv_cmd_in(&fixture.root);
    fake_scheduler(&mut cmd, &log);
    let out = scheduler_output(
        cmd.env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", &fixture.state)
            .args(["schedule", "uninstall"]),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{combined}");

    // A foreign file at an owned path retains the *registration* too:
    // classification happens before any scheduler command, and a single
    // foreign artifact suppresses deregistration entirely. Schtasks has no
    // unit file to make foreign, so it still deregisters. The fake proves the
    // command and its arguments without letting them reach the real one.
    let calls = scheduler_calls(&log);
    let deregistered = calls
        .iter()
        .any(|call| call.contains(expected_deregistration(platform)));
    if foreign.is_some() {
        assert!(
            !deregistered,
            "uninstall tore down the registration of a unit xv did not write: {calls:?}"
        );
    } else {
        assert!(deregistered, "uninstall did not deregister: {calls:?}");
    }

    assert!(
        !fixture.manifest.exists(),
        "the manifest is owned and must be removed: {combined}"
    );
    for path in [&last_run, &run_lock, &recovery, &unrelated] {
        assert!(
            path.exists(),
            "uninstall took {}: {combined}",
            path.display()
        );
    }
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "mine");
    assert_eq!(
        std::fs::read_to_string(&recovery).unwrap(),
        "{\"prior\":true}"
    );
    // The lock inode survives: deleting it would stop it excluding anything.
    assert!(dir.join("install.lock").exists(), "{combined}");
    assert!(
        dir.exists(),
        "the state directory still holds retained files"
    );

    if let Some(foreign) = foreign {
        assert_eq!(
            std::fs::read_to_string(&foreign).unwrap(),
            "# my own job\n",
            "uninstall touched a file xv did not write: {combined}"
        );
        assert!(combined.contains("Retained"), "{combined}");
    }
}

#[test]
fn uninstall_removes_a_legacy_unit() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let state = root.join("state");
    let units = seed_legacy_units(platform, &root, "payments-production");

    let log = root.join("scheduler-calls.log");
    let mut cmd = xv_cmd_in(&root);
    fake_scheduler(&mut cmd, &log);
    let out = scheduler_output(
        cmd.env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", &state)
            .args(["schedule", "uninstall"]),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{combined}");
    for unit in units {
        assert!(!unit.exists(), "{} survived: {combined}", unit.display());
    }
    let calls = scheduler_calls(&log);
    assert!(
        calls
            .iter()
            .any(|call| call.contains(expected_deregistration(platform))),
        "uninstall did not deregister the legacy job: {calls:?}"
    );
}

#[test]
fn status_names_the_missing_manifest_of_a_pinned_unit() {
    skip_if_release!();
    // A pinned unit whose manifest is gone is `legacy-unpinned` — its target
    // cannot be proven — but it is *not* the pre-manifest command, so status
    // may not say the unit recorded no target. It recorded one, at a path it
    // can name.
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let state = root.join("state");
    let manifest = state
        .join("xv")
        .join("schedules")
        .join("rotation-default")
        .join("manifest.json");
    seed_pinned_units(platform, &root, &manifest);
    assert!(!manifest.exists(), "the manifest must be missing");

    let out = schedule_status(&root, &state);

    assert!(out.contains("Ownership: legacy-unpinned"), "{out}");
    assert!(
        out.contains(&format!(
            "Target:    unverified (the recorded manifest {} is missing)",
            manifest.display()
        )),
        "{out}"
    );
    assert!(
        !out.contains("does not record backend or account identity"),
        "this unit did record a target; status must not say otherwise: {out}"
    );
}

// ---------------------------------------------------------------------------
// `last-run.json`: what the runner records, and the lock that serializes runs
// ---------------------------------------------------------------------------

/// Every canary the goldens require to be absent from an outcome.
const REDACTION_CANARIES: [&str; 5] = [
    "AKIAIOSFODNN7EXAMPLE",
    "aws-session-token-canary",
    "azure-client-secret-canary",
    "AGE-SECRET-KEY-1CANARY",
    "super-secret-value-canary",
];

fn last_run_path(state: &std::path::Path) -> std::path::PathBuf {
    state
        .join("xv")
        .join("schedules")
        .join("rotation-default")
        .join("last-run.json")
}

/// `last-run.json`, with the run that should have written it.
///
/// The child's exit code and stderr are part of the panic deliberately: a
/// missing record means the runner exited before (or instead of) writing one,
/// and *why* it exited is the only thing that distinguishes a bug in the
/// runner from a fixture that never got the run it thought it did. Without
/// them this reads as a bare `os error 2` from a path the test only half
/// controls.
fn read_outcome(state: &std::path::Path, out: &std::process::Output) -> serde_json::Value {
    let body = std::fs::read_to_string(last_run_path(state)).unwrap_or_else(|e| {
        panic!(
            "last-run.json must exist: {e}\nthe run exited {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        )
    });
    for canary in REDACTION_CANARIES {
        assert!(!body.contains(canary), "canary '{canary}' leaked: {body}");
    }
    assert!(!body.contains("STALE"), "a secret name leaked: {body}");
    serde_json::from_str(&body).expect("last-run.json is valid JSON")
}

fn manifest_digest_of(manifest: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(manifest).unwrap());
    format!("sha256:{:x}", hasher.finalize())
}

#[test]
fn a_successful_pinned_run_records_a_success_outcome() {
    let fixture = pinned_run_fixture(&[], |_| {});
    // Seed the canaries where a careless implementation would pick them up:
    // as the rotated secret's own value, and as a nearby secret's name.
    set_secret(&fixture.store, "STALE", "super-secret-value-canary");
    make_due(&fixture.store, "STALE");
    set_secret(
        &fixture.store,
        "AKIAIOSFODNN7EXAMPLE",
        "azure-client-secret-canary",
    );

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");

    let outcome = read_outcome(&fixture.state, &out);
    assert_eq!(outcome["schema_version"], 1);
    assert_eq!(outcome["schedule_id"], "rotation-default");
    assert_eq!(outcome["state"], "success");
    assert_eq!(outcome["exit_code"], 0);
    assert_eq!(
        outcome["manifest_digest"].as_str().unwrap(),
        manifest_digest_of(&fixture.manifest),
        "the outcome must bind to the exact manifest bytes it parsed"
    );
    assert!(outcome["started_at"].as_str().unwrap().ends_with('Z'));
    assert!(outcome["finished_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(outcome["summary"]["due"], 1);
    assert_eq!(outcome["summary"]["rotated"], 1);
    assert_eq!(outcome["summary"]["failed"], 0);
    assert!(outcome["summary"]["policy_managed"].as_u64().unwrap() >= 1);
    assert!(outcome["diagnostic"].is_null(), "{outcome}");
    drop(fixture.tmp);
}

#[test]
fn a_drift_refusal_records_the_golden_refusal_outcome() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let body = std::fs::read_to_string(fixture.config_path()).unwrap();
    std::fs::write(fixture.config_path(), format!("{body}\n# edited\n")).unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");

    let outcome = read_outcome(&fixture.state, &out);
    assert_eq!(outcome["state"], "refused_drift");
    assert_eq!(outcome["exit_code"], 3);
    assert!(outcome["summary"].is_null(), "{outcome}");
    assert_eq!(outcome["diagnostic"]["code"], "target_drift");
    assert_eq!(
        outcome["diagnostic"]["message"],
        "config_digest changed; review the recorded target and reinstall"
    );
    drop(fixture.tmp);
}

/// A second runner that cannot take `run.lock` logs one line, exits zero, and
/// leaves the active run's record — and the vault — untouched.
///
/// The lock is held by the *test process* rather than by a first `xv`: a real
/// two-runner race would need the winner to stall inside its sweep, which has
/// no deterministic hook. Holding the same inode from here exercises exactly
/// the code path a losing runner takes.
#[test]
fn a_contending_runner_skips_without_touching_the_outcome() {
    use fs2::FileExt;

    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();

    let lock_path = fixture
        .state
        .join("xv")
        .join("schedules")
        .join("rotation-default")
        .join("run.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.try_lock_exclusive()
        .expect("the test takes the lock first");

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert_eq!(
        out.status.code(),
        Some(0),
        "a skipped run exits zero: {stderr}"
    );
    assert!(
        stderr.contains("schedule rotation skipped: another run is already_running"),
        "{stderr}"
    );
    assert!(
        !last_run_path(&fixture.state).exists(),
        "a contender must not write an outcome"
    );
    assert_eq!(
        fixture.value(),
        before,
        "a contender must not rotate anything"
    );

    fs2::FileExt::unlock(&held).unwrap();
    drop(fixture.tmp);
}

/// A refusal discovered **after** the `running` record is written still ends
/// terminal.
///
/// Vault verification happens inside the sweep, well past the point where
/// `last-run.json` already says `running`. If that path returned without
/// replacing the record, status would report a phantom interrupted run
/// forever — so this is the case that proves the no-early-return region does
/// its job end to end, not just on the source text.
#[test]
fn a_refusal_found_during_the_sweep_still_ends_terminal() {
    let fixture = pinned_run_fixture(&[], |_| {});
    // The pinned store disappears after the manifest was written, so drift
    // validation (which reads files, not vaults) still passes and the refusal
    // comes from the sweep's own read-only probe.
    std::fs::remove_dir_all(&fixture.store).unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");

    let outcome = read_outcome(&fixture.state, &out);
    assert_ne!(
        outcome["state"], "running",
        "the running record was never replaced: {outcome}"
    );
    assert_eq!(outcome["state"], "refused_drift");
    assert_eq!(outcome["exit_code"], 3);
    assert!(outcome["finished_at"].is_string(), "{outcome}");
    assert_eq!(outcome["diagnostic"]["code"], "target_drift");
    drop(fixture.tmp);
}

// ---------------------------------------------------------------------------
// `xv schedule status`: the seven-dimension block
//
// The layout itself is pinned by the golden unit tests in
// `src/schedule/status_render.rs`, which render fixed reports. These tests
// prove the wiring: that a real `xv schedule status` process collects the
// dimensions it renders, and exits with the code the state deserves.
// ---------------------------------------------------------------------------

/// `xv schedule status` with an explicit fake-scheduler scenario. Returns the
/// exit code and the combined output.
fn schedule_status_with(
    root: &std::path::Path,
    state: &std::path::Path,
    runner: &str,
) -> (Option<i32>, String) {
    let out = scheduler_output(
        xv_cmd_in(root)
            .env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", state)
            .env("XV_SCHEDULE_RUNNER", runner)
            .args(["schedule", "status"]),
    );
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Overwrite `last-run.json` with a fixed record.
fn seed_last_run(state: &std::path::Path, body: &str) {
    let path = last_run_path(state);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn a_healthy_schedule_renders_every_golden_dimension_and_exits_zero() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        // No unit file to seed, and this test may not register a real task.
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);

    // The scheduler answers "registered" and reports a next fire time, which
    // is what the golden's healthy block needs. Nothing is registered: the
    // answer is canned (`src/schedule/testing.rs`).
    let (code, out) = schedule_status_with(
        &fixture.root,
        &fixture.state,
        "fake:installed,next=2026-09-10T03:00:00Z",
    );

    assert_eq!(code, Some(0), "a healthy schedule exits zero: {out}");
    // The default log shares the manifest's state root (`XV_STATE_HOME` here).
    let log = fixture
        .state
        .join("xv")
        .join("rotate.log")
        .display()
        .to_string();
    for line in [
        format!("[ok] A {} rotation schedule is installed.", platform.name()),
        "  Ownership: managed".to_string(),
        "  Schedule:  daily at 03:00".to_string(),
        "  Target:    default -> local/default".to_string(),
        "  Backend:   local (local)".to_string(),
        format!("  Config:    {}", fixture.config_path().display()),
        "  Project:   none".to_string(),
        format!("  Cwd:       {}", fixture.root.display()),
        "  Drift:     valid".to_string(),
        format!(
            "  Binary:    {} (installed {}, current {})",
            env!("CARGO_BIN_EXE_xv"),
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        ),
        "  Last run:  never".to_string(),
        "  Next run:  2026-09-10T03:00:00Z".to_string(),
        format!("  Log:       {log} (not yet written)"),
    ] {
        assert!(out.contains(&line), "missing {line:?} in:\n{out}");
    }
    // A healthy schedule has nothing to hint about.
    assert!(!out.contains("[hint]"), "{out}");
    drop(fixture.tmp);
}

/// The same healthy fixture, with a scheduler that says it has never heard of
/// our job. `systemctl --user disable --now` and `launchctl bootout` both leave
/// the unit files exactly where install wrote them, so ownership still reads
/// `managed` — and nothing fires. The bare `fake` runner answers every query in
/// the platform's own "no such job" shape, which is exactly that state.
#[test]
fn a_managed_schedule_the_scheduler_deregistered_fails_instead_of_reporting_ok() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        // Task Scheduler keeps no artifact of ours: with no registration there
        // is no `managed` ownership to contradict, and this test may not
        // register a real task.
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);

    let (clean, _) = schedule_status_with(&fixture.root, &fixture.state, "fake:installed");
    assert_eq!(clean, Some(0), "the fixture did not start healthy");

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(
        code,
        Some(3),
        "a job the scheduler does not have will never fire: {out}"
    );
    assert!(
        out.contains(&format!(
            "[error] The {} rotation schedule is not registered.",
            platform.name()
        )),
        "{out}"
    );
    assert!(out.contains("  Ownership: managed"), "{out}");
    assert!(out.contains("  Scheduler: not registered"), "{out}");
    assert!(
        out.contains("[hint] Run 'xv schedule install --vault default' to register it again."),
        "{out}"
    );
    assert!(
        !out.contains("[ok]"),
        "status may never say a deregistered schedule is installed: {out}"
    );
    drop(fixture.tmp);
}

#[test]
fn a_drifted_schedule_exits_with_the_configuration_error_code() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);

    let (clean, _) = schedule_status_with(&fixture.root, &fixture.state, "fake:installed");
    assert_eq!(clean, Some(0), "the fixture did not start healthy");

    let conf = fixture.config_path();
    let body = std::fs::read_to_string(&conf).unwrap();
    std::fs::write(&conf, format!("{body}\n# a later edit\n")).unwrap();

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake:installed");
    assert_eq!(
        code,
        Some(3),
        "a schedule that would refuse tonight must fail the command: {out}"
    );
    assert!(
        out.contains(&format!(
            "[error] The installed {} rotation schedule is unsafe to run.",
            platform.name()
        )),
        "{out}"
    );
    assert!(out.contains("  Drift:     refused"), "{out}");
    assert!(
        out.contains(&format!(
            "  - config_digest changed; review {} and reinstall",
            conf.display()
        )),
        "{out}"
    );
    assert!(
        out.contains(
            "[hint] Review the changes, then run 'xv schedule install --vault default' to accept \
             the new target."
        ),
        "{out}"
    );
    drop(fixture.tmp);
}

#[test]
fn a_running_record_with_no_lock_held_reads_as_interrupted() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_last_run(
        &fixture.state,
        &format!(
            r#"{{"schema_version":1,"schedule_id":"rotation-default",
"manifest_digest":"{}","started_at":"2026-09-10T03:00:00Z","finished_at":null,
"state":"running","exit_code":null,"summary":null,"diagnostic":null}}"#,
            manifest_digest_of(&fixture.manifest)
        ),
    );

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(
            "  Last run:  interrupted after 2026-09-10T03:00:00Z (no runner holds the lock)"
        ),
        "{out}"
    );
    // Probing the lock may not take it, and status may not rewrite the record.
    let body = std::fs::read_to_string(last_run_path(&fixture.state)).unwrap();
    assert!(
        body.contains("\"running\""),
        "status rewrote the record: {body}"
    );
    drop(fixture.tmp);
}

#[test]
fn an_outcome_from_an_earlier_install_is_labelled_previous_install() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    // A completed run bound to a manifest that is no longer the installed one.
    seed_last_run(
        &fixture.state,
        r#"{"schema_version":1,"schedule_id":"rotation-default",
"manifest_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
"started_at":"2026-09-10T03:00:00Z","finished_at":"2026-09-10T03:00:02Z",
"state":"success","exit_code":0,
"summary":{"policy_managed":4,"due":2,"rotated":2,"failed":0},"diagnostic":null}"#,
    );

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(
            "  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; 2 due, \
             2 rotated, 0 failed (previous install)"
        ),
        "{out}"
    );

    // Rebind the same record to the manifest that is actually installed: the
    // label must disappear, which is what proves it is the digest talking and
    // not a constant.
    let body = std::fs::read_to_string(last_run_path(&fixture.state))
        .unwrap()
        .replace(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            &manifest_digest_of(&fixture.manifest),
        );
    seed_last_run(&fixture.state, &body);
    let (_, current) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert!(
        !current.contains("(previous install)"),
        "the current install's own outcome was labelled as history: {current}"
    );
    drop(fixture.tmp);
}

#[test]
fn status_run_from_another_binary_reports_no_current_version_and_no_drift() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});

    // A second copy of this same `xv`, at a path the manifest does not record.
    // Before the fix this reported `binary_path` drift and refused a schedule
    // whose own binary was perfectly fine.
    let elsewhere = fixture.root.join("copies");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let other = elsewhere.join("xv");
    std::fs::copy(env!("CARGO_BIN_EXE_xv"), &other).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let out = scheduler_output(
        std::process::Command::new(&other)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &fixture.root)
            .env("XDG_CONFIG_HOME", fixture.root.join(".config"))
            .env("XV_NO_PARENT_CONFIG", "1")
            .env("NO_COLOR", "1")
            .env("XV_SCHEDULE_RUNNER", "fake")
            .env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", &fixture.state)
            .current_dir(&fixture.root)
            .args(["schedule", "status"]),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        combined.contains(&format!(
            "  Binary:    {} (installed {}, current unknown (status run from {}))",
            env!("CARGO_BIN_EXE_xv"),
            env!("CARGO_PKG_VERSION"),
            display_path(&other)
        )),
        "{combined}"
    );
    assert!(
        !combined.contains("binary_path changed"),
        "the binary the scheduler runs is fine; only the asking binary differs: {combined}"
    );
    assert!(combined.contains("  Drift:     valid"), "{combined}");
    drop(fixture.tmp);
}

#[test]
fn no_canary_reaches_the_status_block_the_outcome_or_the_manifest() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    // A canary as a secret *value*, and another as a secret *name*.
    set_secret(&fixture.store, "STALE", "super-secret-value-canary");
    make_due(&fixture.store, "STALE");
    set_secret(
        &fixture.store,
        "AKIAIOSFODNN7EXAMPLE",
        "azure-client-secret-canary",
    );
    make_due(&fixture.store, "AKIAIOSFODNN7EXAMPLE");

    // A completed run, then a failing one: the second removes the pinned store
    // so the sweep's own read-only vault probe fails, which is the backend
    // error path whose body must never reach an artifact.
    let first = fixture.run();
    assert!(
        first.status.code() == Some(0) || first.status.code() == Some(3),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let (_, healthy) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    std::fs::remove_dir_all(&fixture.store).unwrap();
    let failed = fixture.run();
    assert_eq!(failed.status.code(), Some(3));
    let (_, broken) = schedule_status_with(&fixture.root, &fixture.state, "fake");

    let manifest = std::fs::read_to_string(&fixture.manifest).unwrap();
    let outcome = std::fs::read_to_string(last_run_path(&fixture.state)).unwrap();
    for canary in REDACTION_CANARIES {
        for (what, body) in [
            ("the healthy status block", &healthy),
            ("the failing status block", &broken),
            ("manifest.json", &manifest),
            ("last-run.json", &outcome),
        ] {
            assert!(
                !body.contains(canary),
                "canary '{canary}' leaked into {what}:\n{body}"
            );
        }
    }
    // The canary secret *name* is also a name, and names never appear either.
    assert!(
        !outcome.contains("STALE"),
        "a secret name leaked: {outcome}"
    );
    drop(fixture.tmp);
}

#[test]
fn a_managed_schedule_whose_scheduler_fails_is_an_error_and_exits_three() {
    skip_if_release!();
    // Ownership is decided from the unit bytes on disk; the scheduler probe is
    // independent and can fail on its own. A block that opened `[ok]` and then
    // exited 3 was the disagreement this closes.
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        // Task Scheduler's registration *is* the artifact, so a failed probe
        // leaves ownership unproven rather than managed; there is nothing to
        // seed and nothing to assert.
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);

    let (healthy, _) = schedule_status_with(&fixture.root, &fixture.state, "fake:installed");
    assert_eq!(healthy, Some(0), "the fixture did not start healthy");

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake:error");
    assert_eq!(
        code,
        Some(3),
        "a scheduler that would not answer must fail the command: {out}"
    );
    assert!(
        out.contains(&format!(
            "[error] The {} rotation schedule could not be confirmed.",
            platform.name()
        )),
        "{out}"
    );
    assert!(out.contains("  Ownership: managed"), "{out}");
    assert!(out.contains("  Scheduler: error ("), "{out}");
    assert!(
        !out.contains("[ok]"),
        "an `[ok]` headline may never accompany a non-zero exit: {out}"
    );
    drop(fixture.tmp);
}

#[test]
fn an_unreadable_orphaned_manifest_exits_three() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    // A manifest that parses as JSON but is not a manifest: `install` cannot
    // repair what it cannot read, so this is an error, not a warning.
    std::fs::write(&fixture.manifest, "{\"schema_version\": 99}\n").unwrap();

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(code, Some(3), "{out}");
    assert!(
        out.contains("[error] A rotation manifest exists but could not be read"),
        "{out}"
    );
    assert!(out.contains("  Target:    unreadable ("), "{out}");
    assert!(
        out.contains("[hint] Reinstall the schedule with 'xv schedule install'"),
        "{out}"
    );
    drop(fixture.tmp);
}

// ---------------------------------------------------------------------------
// Uninstall retention, retained history, and in-place upgrades
// ---------------------------------------------------------------------------

/// `xv schedule uninstall` under the fake scheduler, returning stdout+stderr.
fn schedule_uninstall(root: &std::path::Path, state: &std::path::Path) -> (Option<i32>, String) {
    let log = root.join("uninstall-calls.log");
    let mut cmd = xv_cmd_in(root);
    fake_scheduler(&mut cmd, &log);
    let out = scheduler_output(
        cmd.env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", state)
            .args(["schedule", "uninstall"]),
    );
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Write every file the design's retention table says uninstall keeps, and
/// return them.
fn seed_retained_evidence(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let recovery = dir.join("recovery").join("20260909T000000Z-manifest.json");
    std::fs::create_dir_all(recovery.parent().unwrap()).unwrap();
    let files = vec![
        dir.join("last-run.json"),
        dir.join("run.lock"),
        recovery,
        dir.join("notes-the-user-left.txt"),
    ];
    for (n, path) in files.iter().enumerate() {
        std::fs::write(path, format!("evidence {n}")).unwrap();
    }
    files
}

/// The whole owned set, removed: the manifest-run unit files a current install
/// renders *and* `manifest.json`. Everything else in the directory stays, the
/// directory itself stays (`install.lock` alone keeps it non-empty), and the
/// parent `schedules/` directory is never a candidate for removal.
#[test]
fn uninstall_removes_the_pinned_units_and_retains_every_record() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        // No unit file to seed, and this test may not register a real task.
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    let dir = fixture.manifest.parent().unwrap().to_path_buf();
    let units = seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);
    let retained = seed_retained_evidence(&dir);
    let log_path = fixture.state.join("xv").join("rotate.log");
    std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
    std::fs::write(&log_path, "a rotation happened\n").unwrap();

    let (code, out) = schedule_uninstall(&fixture.root, &fixture.state);
    assert_eq!(code, Some(0), "{out}");

    for unit in &units {
        assert!(!unit.exists(), "{} survived: {out}", unit.display());
        assert!(
            out.contains(&format!("Removed:   {}", unit.display())),
            "{out}"
        );
    }
    assert!(!fixture.manifest.exists(), "{out}");
    for (n, path) in retained.iter().enumerate() {
        assert_eq!(
            std::fs::read_to_string(path).ok(),
            Some(format!("evidence {n}")),
            "uninstall took or rewrote {}: {out}",
            path.display()
        );
    }
    assert_eq!(
        std::fs::read_to_string(&log_path).unwrap(),
        "a rotation happened\n",
        "{out}"
    );
    // The lock inode survives: removing it would stop it excluding anything.
    assert!(dir.join("install.lock").exists(), "{out}");
    // Retained files mean the directory is not empty, so it must remain — and
    // its parent is never removed in any case.
    assert!(dir.exists(), "{out}");
    assert!(dir.parent().unwrap().exists(), "{out}");
    assert!(
        out.contains("Retained:"),
        "uninstall did not say what it kept: {out}"
    );
    drop(fixture.tmp);
}

/// A foreign `manifest.json` retains the manifest, and nothing else.
///
/// The deregistration guard exists for a unit file at a path the *scheduler*
/// reads: leaving that job registered while calling its unit "retained" would
/// retain nothing. `manifest.json` is not that — it lives in xv's own private
/// state directory, and the registration it would be protecting is xv's own,
/// pointing at xv's own units. So the units go, the job is deregistered, and
/// only the foreign manifest stays.
#[test]
fn uninstall_deregisters_when_only_the_manifest_is_foreign() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    let units = seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);
    let alien = "{\"schedule_id\":\"somebody-elses\"}\n";
    std::fs::write(&fixture.manifest, alien).unwrap();

    let log = fixture.root.join("uninstall-calls.log");
    let mut cmd = xv_cmd_in(&fixture.root);
    fake_scheduler(&mut cmd, &log);
    let out = scheduler_output(
        cmd.env("XV_BACKEND", "local")
            .env("XV_STATE_HOME", &fixture.state)
            .args(["schedule", "uninstall"]),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{combined}");

    let calls = scheduler_calls(&log);
    assert!(
        calls
            .iter()
            .any(|call| call.contains(expected_deregistration(platform))),
        "a foreign manifest suppressed a deregistration it does not own: {calls:?}"
    );
    for unit in &units {
        assert!(!unit.exists(), "{} survived: {combined}", unit.display());
    }
    assert_eq!(
        std::fs::read_to_string(&fixture.manifest).unwrap(),
        alien,
        "uninstall touched a manifest xv did not write: {combined}"
    );
    assert!(
        combined.contains(&format!("Retained:  {}", fixture.manifest.display())),
        "{combined}"
    );
    assert!(
        !combined.contains("is not managed by xv"),
        "nothing blocked the deregistration, so nothing may claim it did: {combined}"
    );
    drop(fixture.tmp);
}

/// When a foreign *unit* does block deregistration, the success line has to say
/// so. "Removed the ... rotation schedule." while the job is still registered
/// and will fire tonight is the one reading the user must not be left with.
#[test]
fn uninstall_says_why_it_left_the_registration_in_place() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    let paths = UnitPaths::for_platform(platform, &fixture.root);
    std::fs::create_dir_all(&paths.dir).unwrap();
    let foreign = seed_legacy_units(platform, &fixture.root, "v")[0].clone();
    std::fs::write(&foreign, "# my own job\n").unwrap();

    let (code, out) = schedule_uninstall(&fixture.root, &fixture.state);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(&format!(
            "the scheduler registration was left in place because {} is not managed by xv",
            foreign.display()
        )),
        "the success line did not say the job is still registered: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(&foreign).unwrap(),
        "# my own job\n",
        "{out}"
    );
    drop(fixture.tmp);
}

/// Uninstall retains history, and `status` afterwards has to show it — history
/// nothing renders is retention the user cannot see. Labelled `(previous
/// install)`, because the installation that produced it is gone.
#[test]
fn uninstall_keeps_the_last_run_visible_as_history() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    let run = fixture.run();
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(read_outcome(&fixture.state, &run)["state"], "success");

    let (code, out) = schedule_uninstall(&fixture.root, &fixture.state);
    assert_eq!(code, Some(0), "{out}");
    assert!(last_run_path(&fixture.state).exists(), "{out}");

    let (status_code, status) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(status_code, Some(0), "{status}");
    assert!(
        status.contains("rotation schedule is installed."),
        "expected the absent headline: {status}"
    );
    assert!(
        status.contains("  Last run:  success;") && status.contains("(previous install)"),
        "uninstall's retained history is invisible in status: {status}"
    );
    drop(fixture.tmp);
}

/// A reinstall preserves the outcome but must not present it as this
/// installation's own: the record is bound to the manifest digest that produced
/// it, and the label goes away only when a new run completes.
#[test]
fn a_reinstall_keeps_the_outcome_as_history_until_a_new_run_completes() {
    skip_if_release!();
    if host_platform().is_none() {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    let first = fixture.run();
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_digest = manifest_digest_of(&fixture.manifest);
    assert_eq!(
        read_outcome(&fixture.state, &first)["manifest_digest"]
            .as_str()
            .unwrap(),
        first_digest
    );

    // Reinstall. The cadence changes and the recorded target does not, so the
    // manifest digest differs while the follow-up run still has somewhere to
    // go. (The manifest is republished the way installation publishes it; the
    // fake scheduler cannot satisfy an install's verification queries, and
    // that a real reinstall leaves `last-run.json`, both locks and the log
    // alone is locked down in `schedule::install`'s unit tests.)
    let reinstalled = install_manifest(&fixture.root, &fixture.state, &["--at", "04:15"]);
    assert_eq!(reinstalled, fixture.manifest);
    let second_digest = manifest_digest_of(&fixture.manifest);
    assert_ne!(
        second_digest, first_digest,
        "the fixture's reinstall did not change the manifest"
    );
    assert!(
        last_run_path(&fixture.state).exists(),
        "the reinstall discarded the run record"
    );

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains("  Last run:  success;") && out.contains("(previous install)"),
        "a retained outcome was presented as the new installation's own: {out}"
    );

    // A completed run under the new installation replaces it, and the label
    // goes with it. Nothing is due any more, so this is a zero-rotation sweep
    // — still a completed run, which is the whole point.
    let second = fixture.run();
    assert_eq!(
        second.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let outcome = read_outcome(&fixture.state, &second);
    assert_eq!(outcome["state"], "success");
    assert_eq!(
        outcome["manifest_digest"].as_str().unwrap(),
        second_digest,
        "the new run did not bind to the reinstalled manifest"
    );

    let (code, out) = schedule_status_with(&fixture.root, &fixture.state, "fake");
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("  Last run:  success;"), "{out}");
    assert!(
        !out.contains("(previous install)"),
        "the new installation's own outcome is still labelled as history: {out}"
    );
    drop(fixture.tmp);
}

/// Rewrite one `execution` string field of the published manifest.
///
/// `value` is the literal string to store. Do **not** pass [`json_path`]: that
/// escapes backslashes for substitution into JSON source text, and `serde_json`
/// escapes them again on the way out.
fn patch_manifest(manifest: &std::path::Path, field: &str, value: &str) {
    let body = std::fs::read_to_string(manifest).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&body).unwrap();
    json["execution"][field] = serde_json::Value::String(value.to_string());
    std::fs::write(
        manifest,
        format!("{}\n", serde_json::to_string_pretty(&json).unwrap()),
    )
    .unwrap();
}

/// The ordinary in-place upgrade: `xv` was replaced at the same path and now
/// reports a different version. The pinned run is *allowed*, with a warning on
/// stderr, and status recommends a reinstall so the rendered unit and the
/// recorded schema are refreshed.
///
/// The two versions are arranged by recording an older one in the manifest
/// rather than by building a second binary: the runner compares its own
/// `CARGO_PKG_VERSION` against the recorded value, which is exactly the
/// comparison an upgrade changes.
#[test]
fn an_in_place_upgrade_is_allowed_with_a_warning_and_status_recommends_a_reinstall() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    let fixture = pinned_run_fixture(&[], |_| {});
    patch_manifest(&fixture.manifest, "installed_version", "0.0.1-older");
    if platform != Platform::Schtasks {
        seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);
    }

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(
        out.status.code(),
        Some(0),
        "an in-place upgrade may not refuse the run: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "installed_version changed from 0.0.1-older to {} at the same binary path",
            env!("CARGO_PKG_VERSION")
        )),
        "the allowed run said nothing about the version change: {stderr}"
    );
    let outcome = read_outcome(&fixture.state, &out);
    assert_eq!(outcome["state"], "success");
    assert_eq!(outcome["summary"]["rotated"], 1, "{stderr}");
    assert_ne!(
        fixture.value(),
        "pinned-value",
        "the allowed run did not actually rotate: {stderr}"
    );

    if platform == Platform::Schtasks {
        drop(fixture.tmp);
        return;
    }
    let (code, status) = schedule_status_with(&fixture.root, &fixture.state, "fake:installed");
    assert_eq!(
        code,
        Some(0),
        "an in-place upgrade is a warning, not a failure: {status}"
    );
    assert!(status.contains("  Drift:     warning"), "{status}");
    assert!(
        status.contains(&format!(
            "  Binary:    {} (installed 0.0.1-older, current {})",
            env!("CARGO_BIN_EXE_xv"),
            env!("CARGO_PKG_VERSION")
        )),
        "{status}"
    );
    assert!(
        status.contains(
            "[hint] Reinstall the schedule with 'xv schedule install --vault default' to refresh \
             the rendered unit and the recorded version."
        ),
        "status did not recommend a reinstall: {status}"
    );
    drop(fixture.tmp);
}

/// The other half of the drift table's executable row: a *changed* path refuses
/// the run, and a recorded path that is *gone* refuses too. Both are
/// `binary_path`, both exit with the configuration-error code, and neither is
/// softened into the in-place-upgrade warning.
#[test]
fn a_recorded_binary_that_moved_or_vanished_is_still_a_refusal() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    let fixture = pinned_run_fixture(&[], |_| {});
    // Built from the manifest's *own* spelling of the working directory, and
    // written as a plain string. Two Windows-only traps meet here: the fixture
    // holds a canonicalized (verbatim `\\?\C:\...`) root that xv never
    // records, and `json_path` escapes backslashes for substitution into JSON
    // *source text* — handing its output to `serde_json` doubles every
    // separator again. The result, `\\\\?\\C:\\...`, parses as neither a
    // verbatim nor a UNC path, so `Path::is_absolute()` is false and the
    // manifest is rejected by schema validation before the runner ever writes
    // `last-run.json`. The test still saw exit 3 and `binary_path` on stderr
    // and looked like it was exercising the drift refusal it names.
    let gone = std::path::Path::new(&recorded(&fixture.manifest, "/execution/working_directory"))
        .join("no-such-xv");
    patch_manifest(&fixture.manifest, "binary_path", &gone.to_string_lossy());
    // Rendered from the patched manifest, so the unit and the manifest still
    // agree: the finding under test is the missing executable, not unit drift.
    if platform != Platform::Schtasks {
        seed_units_for_manifest(platform, &fixture.root, &fixture.manifest);
    }

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("binary_path"), "{stderr}");
    assert_eq!(read_outcome(&fixture.state, &out)["state"], "refused_drift");
    assert_eq!(fixture.value(), "pinned-value", "the refused run rotated");

    if platform == Platform::Schtasks {
        drop(fixture.tmp);
        return;
    }
    // `status` compares the recorded path with itself, so this is the branch
    // where "the recorded executable is missing" is the finding rather than
    // "this is not the recorded executable".
    let (code, status) = schedule_status_with(&fixture.root, &fixture.state, "fake:installed");
    assert_eq!(code, Some(3), "{status}");
    assert!(status.contains("  Drift:     refused"), "{status}");
    assert!(status.contains("binary_path"), "{status}");
    drop(fixture.tmp);
}

// ---------------------------------------------------------------------------
// The target-isolation matrix
//
// Every row of the design's isolation list (the "Verification strategy" section
// of docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md) and
// the test that proves it. Each row asserts BOTH halves: the intended mutation
// in the pinned store, and a decoy target that is byte-for-byte untouched.
//
// | isolation row | proved by |
// |---|---|
// | same real vault name on two named local backends | `a_pinned_run_rotates_the_recorded_target_and_nothing_else` — the fixture's decoy *is* `local-b/default`, holding its own due `STALE` |
// | a workspace alias mapped to a different real vault than a same-named raw vault | `an_alias_that_shadows_a_raw_vault_rotates_the_alias_target` |
// | two project `[env.*]` profiles selecting different vaults | `a_second_project_profile_cannot_redirect_the_sweep` |
// | a state root / store path containing spaces | `a_target_whose_paths_contain_spaces_rotates_only_the_pinned_vault` |
// | invocation from a changed cwd | every `run_pinned` runs from `<root>/elsewhere`, never the recorded directory; the plain case is `a_pinned_run_rotates_the_recorded_target_and_nothing_else` |
// | conflicting `XV_BACKEND`/`XV_ENV`/`XV_CONTEXT_DIR`/`XDG_CONFIG_HOME` on the child | `no_ambient_selection_input_can_redirect_the_sweep` |
// | missing config | `a_missing_config_file_refuses_the_run` (changed bytes: `changed_config_bytes_refuse_the_run`) |
// | missing project | `a_missing_project_file_refuses_the_run` (changed: `a_changed_project_file_refuses_the_run`) |
// | missing context | `a_missing_context_file_refuses_the_run` (changed: `a_changed_participating_context_refuses_the_run`) |
// | missing working directory | `a_missing_working_directory_refuses_the_run` |
// | missing executable | `a_recorded_binary_that_moved_or_vanished_is_still_a_refusal` |
// | missing backend entry | `a_removed_backend_entry_refuses_the_run` |
// ---------------------------------------------------------------------------

/// An isolated local store at a path the caller chooses — the same shape
/// `xv_isolated_local_with_opts` builds, so a fixture can put the whole target
/// somewhere with spaces in it.
fn isolated_local_at(
    parent: &std::path::Path,
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = parent.join(name);
    let store = root.join("store");
    let key = root.join("key.txt");
    std::fs::create_dir_all(root.join(".config").join("xv")).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    let config = format!(
        r#"backend = "local"
debug = false
subscription_id = ""
default_vault = "default"
default_resource_group = ""
default_location = ""
tenant_id = ""
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0

[local]
store_path = "{store}"
key_file = "{key}"
default_vault = "default"
audit = false
git = false
"#,
        store = json_path(&store),
        key = json_path(&key),
    );
    std::fs::write(root.join(".config").join("xv").join("xv.conf"), config).unwrap();
    (root, store)
}

/// Run `xv` against a *raw* vault by repointing the config's default vault at it
/// for the duration of the command, then restoring the file byte for byte.
///
/// There is no per-command `--vault`: a vault is whatever the resolution chain
/// selects, so a fixture that needs a secret in a second raw vault has to move
/// the default. Restoring the exact prior bytes matters — `config_digest` pins
/// them, and a fixture that left the file rewritten would be testing config
/// drift instead of whatever it meant to test.
fn xv_in_vault(root: &std::path::Path, vault: &str, args: &[&str]) -> String {
    let conf = root.join(".config").join("xv").join("xv.conf");
    let original = std::fs::read_to_string(&conf).unwrap();
    let repointed = original.replace(
        "default_vault = \"default\"",
        &format!("default_vault = \"{vault}\""),
    );
    assert!(
        vault == "default" || repointed != original,
        "fixture config shape changed"
    );
    std::fs::write(&conf, &repointed).unwrap();
    // An attached workspace's default entry outranks the config default, so a
    // fixture that reaches a *raw* vault has to step out of the workspace for
    // the duration of the command. Both files are restored byte for byte.
    let context = root.join(".xv").join("context");
    let attached = std::fs::read(&context).ok();
    if attached.is_some() {
        std::fs::remove_file(&context).unwrap();
    }
    let out = xv_cmd_for(&root.join("store")).args(args).output().unwrap();
    std::fs::write(&conf, &original).unwrap();
    if let Some(bytes) = attached {
        std::fs::write(&context, bytes).unwrap();
    }
    assert!(
        out.status.success(),
        "xv {args:?} in vault '{vault}' failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A due secret named `STALE` in the raw vault `vault`, valued `<vault>-value`.
fn seed_due_secret_in_vault(root: &std::path::Path, vault: &str) {
    let value = format!("{vault}-value");
    xv_in_vault(root, vault, &["set", "STALE", "--value", &value]);
    xv_in_vault(
        root,
        vault,
        &[
            "update",
            "STALE",
            "--tag",
            "xv:rotate_every=30d",
            "--tag",
            "xv:rotated_at=2020-01-01T00:00:00Z",
        ],
    );
}

fn secret_value_in_vault(root: &std::path::Path, vault: &str) -> String {
    xv_in_vault(root, vault, &["get", "STALE", "--raw"])
}

/// `xv` in the fixture root with an explicit `--env`, for a project file that
/// defines environments and no `default_env`.
fn xv_in_env(store: &std::path::Path, env: &str, args: &[&str]) -> String {
    let mut cmd = xv_cmd_for(store);
    let out = cmd.args(args).args(["--env", env]).output().unwrap();
    assert!(
        out.status.success(),
        "xv {args:?} --env {env} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The refusal a drift check owes: the configuration-error exit code and the
/// manifest field that refused, named.
fn assert_refused(out: &std::process::Output, field: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains(field),
        "the refusal does not name {field}:\n{stderr}"
    );
    assert!(stderr.to_lowercase().contains("reinstall"), "{stderr}");
    stderr
}

/// A context attaching one alias to a real vault of a different name, on the
/// active backend.
const SHADOWING_ALIAS_CONTEXT: &str = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "real-vault", "backend": "local", "alias": "shadow", "default": true }
    ]
  }
}
"#;

/// An alias whose *name* is also a real vault must rotate the vault it points
/// at, never the vault that shares its name.
///
/// The manifest records the resolved vault, so this is the case where a runner
/// that re-resolved `--vault shadow` against anything — or one that recorded the
/// alias and resolved it later — would sweep the wrong store subtree.
#[test]
fn an_alias_that_shadows_a_raw_vault_rotates_the_alias_target() {
    let fixture = pinned_run_fixture(&["--vault", "shadow"], |root| {
        seed_due_secret_in_vault(root, "shadow");
        seed_due_secret_in_vault(root, "real-vault");
        std::fs::create_dir_all(root.join(".xv")).unwrap();
        std::fs::write(root.join(".xv").join("context"), SHADOWING_ALIAS_CONTEXT).unwrap();
    });
    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("\"workspace_alias\": \"shadow\""),
        "{recorded}"
    );
    assert!(recorded.contains("\"vault\": \"real-vault\""), "{recorded}");
    let decoy_before = fixture.decoy_snapshot();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");

    assert_ne!(
        secret_value_in_vault(&fixture.root, "real-vault"),
        "real-vault-value",
        "the vault behind the alias did not rotate: {stderr}"
    );
    assert_eq!(
        secret_value_in_vault(&fixture.root, "shadow"),
        "shadow-value",
        "the same-named raw vault was swept: {stderr}"
    );
    assert_eq!(
        fixture.decoy_snapshot(),
        decoy_before,
        "the run touched the decoy backend"
    );
    drop(fixture.tmp);
}

/// Two project profiles, two vaults: the sweep replays the one installation
/// selected, even when `XV_ENV` in the scheduled process names the other.
#[test]
fn a_second_project_profile_cannot_redirect_the_sweep() {
    let project =
        "[env.production]\nvault = \"prod-vault\"\n\n[env.staging]\nvault = \"stage-vault\"\n";
    let fixture = pinned_run_fixture(&["--env", "production"], |root| {
        // The profiles name vaults that must already exist — a schedule is only
        // installed against a target this machine can read — and a project file
        // defining environments with no `default_env` fails every command that
        // does not select one, so the vaults are seeded first, by name.
        seed_due_secret_in_vault(root, "prod-vault");
        seed_due_secret_in_vault(root, "stage-vault");
        std::fs::write(root.join(".xv.toml"), project).unwrap();
    });
    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("\"environment\": \"production\""),
        "{recorded}"
    );
    assert!(recorded.contains("\"vault\": \"prod-vault\""), "{recorded}");
    let decoy_before = fixture.decoy_snapshot();

    // The scheduled process carries `XV_ENV=staging` — the other profile, with
    // the other vault.
    let out = run_pinned_fixture_with_ambient_env(&fixture, "staging");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");

    assert_ne!(
        xv_in_env(&fixture.store, "production", &["get", "STALE", "--raw"]),
        "prod-vault-value",
        "the recorded profile's vault did not rotate: {stderr}"
    );
    assert_eq!(
        xv_in_env(&fixture.store, "staging", &["get", "STALE", "--raw"]),
        "stage-vault-value",
        "the ambient profile's vault was swept: {stderr}"
    );
    assert_eq!(fixture.decoy_snapshot(), decoy_before);
    drop(fixture.tmp);
}

/// [`run_pinned`] with a specific `XV_ENV` in the scheduled process.
fn run_pinned_fixture_with_ambient_env(fixture: &PinnedRun, env: &str) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .env("XDG_CONFIG_HOME", fixture.root.join(".config"))
        .env("NO_COLOR", "1")
        .env("XV_BACKEND", "azure")
        .env("XV_ENV", env)
        .env("XV_STATE_HOME", &fixture.state)
        .current_dir(&fixture.elsewhere)
        .args([
            "schedule",
            "run",
            "--manifest",
            fixture.manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

/// Every path in the recorded target — config, store, state root, working
/// directory, log — contains a space, on every platform.
#[test]
fn a_target_whose_paths_contain_spaces_rotates_only_the_pinned_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = std::fs::canonicalize(tmp.path()).unwrap();
    let (root, store) = isolated_local_at(&parent, "a target with spaces");
    let fixture = pinned_run_fixture_in(tmp, root, store, &[], |_| {});

    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("a target with spaces"),
        "the fixture did not put a space in the recorded paths: {recorded}"
    );
    let before = fixture.value();
    let decoy_before = fixture.decoy_snapshot();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_ne!(fixture.value(), before, "{stderr}");
    assert_eq!(fixture.decoy_snapshot(), decoy_before);
    drop(fixture.tmp);
}

/// A context attaching the decoy backend as the workspace default, for the
/// hostile `XV_CONTEXT_DIR`.
const DECOY_CONTEXT: &str = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "default", "backend": "local", "alias": "decoy", "default": true }
    ]
  }
}
"#;

/// Not one ambient selection input may reach the target: a contradicting
/// backend, a contradicting environment, a context directory attaching the
/// decoy as the workspace default, and a config home whose `xv.conf` *is* the
/// decoy store. All four at once, and the sweep still lands on the pinned
/// vault.
#[test]
fn no_ambient_selection_input_can_redirect_the_sweep() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    let decoy_before = fixture.decoy_snapshot();

    // A *valid, attractive* decoy: the fixture's own config with the store path
    // swapped, so a runner that read it would rotate the decoy rather than fail.
    let hostile_config = fixture.root.join("hostile config home");
    std::fs::create_dir_all(hostile_config.join("xv")).unwrap();
    let original = std::fs::read_to_string(fixture.config_path()).unwrap();
    let swapped = original.replace(&json_path(&fixture.store), &json_path(&fixture.decoy_store));
    assert_ne!(swapped, original, "fixture config shape changed");
    std::fs::write(hostile_config.join("xv").join("xv.conf"), swapped).unwrap();
    let hostile_context = fixture.root.join("hostile context dir");
    std::fs::create_dir_all(&hostile_context).unwrap();
    std::fs::write(hostile_context.join("context"), DECOY_CONTEXT).unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &fixture.root)
        .env("XDG_CONFIG_HOME", &hostile_config)
        .env("XV_CONTEXT_DIR", &hostile_context)
        .env("XV_BACKEND", "azure")
        .env("XV_ENV", "no-such-environment")
        .env("NO_COLOR", "1")
        .env("XV_STATE_HOME", &fixture.state)
        .current_dir(&fixture.elsewhere)
        .args([
            "schedule",
            "run",
            "--manifest",
            fixture.manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_ne!(
        fixture.value(),
        before,
        "the pinned vault did not rotate: {stderr}"
    );
    assert_eq!(
        fixture.decoy_snapshot(),
        decoy_before,
        "an ambient input redirected the sweep at the decoy"
    );
    drop(fixture.tmp);
}

#[test]
fn a_missing_config_file_refuses_the_run() {
    let fixture = pinned_run_fixture(&[], |_| {});
    let before = fixture.value();
    let decoy_before = fixture.decoy_snapshot();
    let body = std::fs::read_to_string(fixture.config_path()).unwrap();
    std::fs::remove_file(fixture.config_path()).unwrap();

    let out = fixture.run();
    let stderr = assert_refused(&out, "config_path is missing or unreadable");
    assert_eq!(fixture.decoy_snapshot(), decoy_before);

    // Put it back so the pinned store is readable again.
    std::fs::write(fixture.config_path(), &body).unwrap();
    assert_eq!(
        fixture.value(),
        before,
        "a refused run must not rotate: {stderr}"
    );
}

#[test]
fn a_missing_project_file_refuses_the_run() {
    let project = "default_env = \"production\"\n\n[env.production]\nvault = \"default\"\n";
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::write(root.join(".xv.toml"), project).unwrap();
    });
    let before = fixture.value();
    let decoy_before = fixture.decoy_snapshot();
    std::fs::remove_file(fixture.root.join(".xv.toml")).unwrap();

    let out = fixture.run();
    assert_refused(&out, "project_path is missing or unreadable");
    assert_eq!(fixture.value(), before);
    assert_eq!(fixture.decoy_snapshot(), decoy_before);
}

#[test]
fn a_missing_context_file_refuses_the_run() {
    let context = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "default", "backend": "local", "alias": "pinned", "default": true }
    ]
  }
}
"#;
    let fixture = pinned_run_fixture(&[], |root| {
        std::fs::create_dir_all(root.join(".xv")).unwrap();
        std::fs::write(root.join(".xv").join("context"), context).unwrap();
    });
    let before = fixture.value();
    let decoy_before = fixture.decoy_snapshot();
    std::fs::remove_file(fixture.root.join(".xv").join("context")).unwrap();

    let out = fixture.run();
    assert_refused(&out, "context_path is missing or unreadable");
    assert_eq!(fixture.value(), before);
    assert_eq!(fixture.decoy_snapshot(), decoy_before);
}

/// A schedule pinned to a *named* backend refuses once that backend is no
/// longer configured — and rotates neither store on the way out.
#[test]
fn a_removed_backend_entry_refuses_the_run() {
    let context = r#"{
  "current": null,
  "recent": [],
  "workspace": {
    "entries": [
      { "vault": "default", "backend": "local", "alias": "pinned", "default": true },
      { "vault": "default", "backend": "local-b", "alias": "attached" }
    ]
  }
}
"#;
    let fixture = pinned_run_fixture(&["--vault", "attached"], |root| {
        std::fs::create_dir_all(root.join(".xv")).unwrap();
        std::fs::write(root.join(".xv").join("context"), context).unwrap();
    });
    let recorded = std::fs::read_to_string(&fixture.manifest).unwrap();
    assert!(
        recorded.contains("\"backend_name\": \"local-b\""),
        "{recorded}"
    );
    // Read outside the workspace: with two entries attached, both holding a
    // `STALE`, an unqualified union read is ambiguous by design.
    let before = secret_value_in_vault(&fixture.root, "default");
    let decoy_before = fixture.decoy_snapshot();

    // Drop the named backend the schedule is pinned to.
    let body = std::fs::read_to_string(fixture.config_path()).unwrap();
    let without = body
        .split("[named_backends.local-b]")
        .next()
        .expect("the fixture config declares the named backend")
        .to_string();
    assert_ne!(without, body, "fixture config shape changed");
    std::fs::write(fixture.config_path(), &without).unwrap();

    let out = fixture.run();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("backend_name 'local-b' is no longer a configured backend"),
        "the refusal does not name the backend that went missing:\n{stderr}"
    );

    std::fs::write(fixture.config_path(), &body).unwrap();
    assert_eq!(
        secret_value_in_vault(&fixture.root, "default"),
        before,
        "a refused run must not rotate"
    );
    assert_eq!(fixture.decoy_snapshot(), decoy_before);
    drop(fixture.tmp);
}

// ---------------------------------------------------------------------------
// The real install transaction, end to end
//
// Everything above either previews (`--print`) or seeds artifacts directly.
// This is the one place the *transaction* runs for real — it renders, publishes
// the manifest, writes the unit files, registers, and verifies the scheduler's
// own answer — and then walks the whole lifecycle over it. The scheduler is
// still a fake (`XV_SCHEDULE_RUNNER=fake:registered`, debug builds only): it
// answers the verification queries out of what it was actually asked to
// register, and remembers that across the test's several processes, so nothing
// is registered on the machine running the test.
//
// What that proves is not the same on every platform. On launchd and systemd the
// fake's answers are read back out of the *unit files the transaction wrote*, so
// a unit that did not name this schedule's executable and manifest would fail
// stage 6 here. Task Scheduler has no artifact of ours — the registration *is*
// the `/Create` arguments — so there the fake can only echo them back: this test
// proves the ordering and the query shapes, not that Task Scheduler would accept
// or store them. `.github/workflows/schedule-native.yml` covers that gap with a
// real (harmless, far-future) task on a Windows runner.
// ---------------------------------------------------------------------------

/// The scenario every step of the round trip uses: a scheduler that answers for
/// what it was asked to register, and reports a next fire time.
const REGISTERING_SCHEDULER: &str = "fake:registered,next=2026-09-10T03:00:00Z";

/// The invocation that *registers* the schedule on this platform.
fn expected_registration(platform: Platform) -> &'static str {
    match platform {
        Platform::Launchd => "launchctl bootstrap gui/",
        Platform::Systemd => "systemctl --user enable --now xv-rotate.timer",
        Platform::Schtasks => "schtasks /Create /TN crosstache-xv-rotate",
    }
}

/// `xv schedule <verb>` against the registering fake, sharing one invocation log
/// (and therefore one remembered registration) across the whole round trip.
fn round_trip_xv(
    root: &std::path::Path,
    state: &std::path::Path,
    log: &std::path::Path,
    args: &[&str],
) -> (Option<i32>, String) {
    let out = scheduler_output(
        xv_cmd_in(root)
            .env("XV_BACKEND", "local")
            // One state root for the manifest *and* the default log, so every
            // recorded path stays inside the fixture on all three platforms.
            .env("XV_STATE_HOME", state)
            .env("XV_SCHEDULE_RUNNER", REGISTERING_SCHEDULER)
            .env("XV_SCHEDULE_RUNNER_LOG", log)
            .args(args),
    );
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn install_status_reinstall_uninstall_round_trip() {
    skip_if_release!();
    let Some(platform) = host_platform() else {
        return;
    };
    let (_cmd, tmp, store) = xv_isolated_local_with_opts(false, false);
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    use_the_store_once(&store);
    let state = root.join("state");
    let log = root.join("scheduler-calls.log");
    let manifest = state
        .join("xv")
        .join("schedules")
        .join("rotation-default")
        .join("manifest.json");
    let units = unit_paths_for(platform, &UnitPaths::for_platform(platform, &root));
    let conf = root.join(".config").join("xv").join("xv.conf");
    let log_file = state.join("xv").join("rotate.log");

    // ---- install --------------------------------------------------------
    let (code, out) = round_trip_xv(
        &root,
        &state,
        &log,
        &["schedule", "install", "--force", "--vault", "default"],
    );
    assert_eq!(
        code,
        Some(0),
        "the real transaction did not complete: {out}"
    );
    assert!(
        out.contains(&format!(
            "Installed a {} rotation schedule: daily at 03:00.",
            platform.name()
        )),
        "{out}"
    );
    assert!(manifest.exists(), "the manifest was not published: {out}");
    for unit in &units {
        assert!(unit.exists(), "{} was not written: {out}", unit.display());
    }
    let calls = scheduler_calls(&log);
    assert!(
        calls
            .iter()
            .any(|call| call.contains(expected_registration(platform))),
        "the schedule was never registered: {calls:?}"
    );

    // ---- status: healthy, never run -------------------------------------
    let (code, status) = round_trip_xv(&root, &state, &log, &["schedule", "status"]);
    assert_eq!(code, Some(0), "a healthy schedule exits zero: {status}");
    for line in [
        format!("[ok] A {} rotation schedule is installed.", platform.name()),
        "  Ownership: managed".to_string(),
        "  Schedule:  daily at 03:00".to_string(),
        "  Target:    default -> local/default".to_string(),
        "  Backend:   local (local)".to_string(),
        format!("  Config:    {}", display_path(&conf)),
        "  Project:   none".to_string(),
        format!("  Cwd:       {}", display_path(&root)),
        "  Drift:     valid".to_string(),
        format!(
            "  Binary:    {} (installed {}, current {})",
            env!("CARGO_BIN_EXE_xv"),
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION")
        ),
        "  Last run:  never".to_string(),
        format!("  Log:       {} (not yet written)", display_path(&log_file)),
    ] {
        assert!(status.contains(&line), "missing {line:?} in:\n{status}");
    }
    if platform != Platform::Schtasks {
        // Task Scheduler reports its next run in the machine's display
        // language, so the fake refuses to invent one.
        assert!(
            status.contains("  Next run:  2026-09-10T03:00:00Z"),
            "{status}"
        );
    }
    assert!(!status.contains("[hint]"), "{status}");

    // ---- a config edit makes status refuse ------------------------------
    let body = std::fs::read_to_string(&conf).unwrap();
    std::fs::write(&conf, format!("{body}\n# a later edit\n")).unwrap();
    let (code, drifted) = round_trip_xv(&root, &state, &log, &["schedule", "status"]);
    assert_eq!(code, Some(3), "{drifted}");
    assert!(
        drifted.contains(&format!(
            "[error] The installed {} rotation schedule is unsafe to run.",
            platform.name()
        )),
        "{drifted}"
    );
    assert!(drifted.contains("  Drift:     refused"), "{drifted}");
    assert!(
        drifted.contains(&format!(
            "  - config_digest changed; review {} and reinstall",
            display_path(&conf)
        )),
        "{drifted}"
    );

    // ---- reinstall accepts the new target -------------------------------
    let before_reinstall = scheduler_calls(&log).len();
    let (code, out) = round_trip_xv(
        &root,
        &state,
        &log,
        &["schedule", "install", "--force", "--vault", "default"],
    );
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(&format!(
            "Reinstalled a {} rotation schedule",
            platform.name()
        )),
        "a second install must report itself as a reinstall: {out}"
    );
    let calls = scheduler_calls(&log);
    assert!(
        calls[before_reinstall..]
            .iter()
            .any(|call| call.contains(expected_registration(platform))),
        "the reinstall did not re-register: {:?}",
        &calls[before_reinstall..]
    );
    let (code, status) = round_trip_xv(&root, &state, &log, &["schedule", "status"]);
    assert_eq!(code, Some(0), "{status}");
    assert!(status.contains("  Drift:     valid"), "{status}");

    // ---- the pinned run, and the outcome status reports ------------------
    set_secret(&store, "STALE", "pinned-value");
    make_due(&store, "STALE");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let run = run_pinned(&elsewhere, &root, &state, &manifest);
    let run_out = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.status.code(), Some(0), "{run_out}");
    assert_ne!(
        secret_value(&store, "STALE"),
        "pinned-value",
        "the pinned run rotated nothing: {run_out}"
    );

    let (code, status) = round_trip_xv(&root, &state, &log, &["schedule", "status"]);
    assert_eq!(code, Some(0), "{status}");
    assert!(
        status.contains("  Last run:  success;") && status.contains("1 due, 1 rotated, 0 failed"),
        "{status}"
    );
    assert!(!status.contains("(previous install)"), "{status}");

    // ---- uninstall: owned artifacts gone, history retained ---------------
    let before_uninstall = scheduler_calls(&log).len();
    let (code, out) = round_trip_xv(&root, &state, &log, &["schedule", "uninstall"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(
        out.contains(&format!(
            "Removed the {} rotation schedule.",
            platform.name()
        )),
        "{out}"
    );
    assert!(!manifest.exists(), "{out}");
    for unit in &units {
        assert!(!unit.exists(), "{} survived: {out}", unit.display());
    }
    let calls = scheduler_calls(&log);
    assert!(
        calls[before_uninstall..]
            .iter()
            .any(|call| call.contains(expected_deregistration(platform))),
        "uninstall did not deregister: {:?}",
        &calls[before_uninstall..]
    );

    // ---- status afterwards: absent, with the history it retained ---------
    let (code, status) = round_trip_xv(&root, &state, &log, &["schedule", "status"]);
    assert_eq!(code, Some(0), "{status}");
    assert!(
        status.contains(&format!(
            "[info] No {} rotation schedule is installed.",
            platform.name()
        )),
        "{status}"
    );
    assert!(
        status.contains("  Last run:  success;") && status.contains("(previous install)"),
        "uninstall must keep the history visible: {status}"
    );
    drop(tmp);
}
