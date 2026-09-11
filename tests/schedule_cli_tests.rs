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
store_path = "{store}"
key_file = "{key}"
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

    let status = xv_cmd_in(tmp.path())
        .args(["schedule", "status"])
        .output()
        .unwrap();
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
    // The bug: a user whose profile sets XDG_STATE_HOME gets a manifest under
    // it and a unit pointing there, but launchd and systemd user units export
    // no XDG_STATE_HOME — so at fire time the runner recomputes
    // $HOME/.local/state/... and refuses the manifest it was just handed.
    let (_cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    use_the_store_once(&store);
    let root = store.parent().unwrap();
    let state = root.join("custom-state");
    std::fs::create_dir_all(&state).unwrap();

    let out = xv_cmd_for(&store)
        .env("XDG_STATE_HOME", &state)
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
    // ...so the unit must carry the variable that put it there.
    assert!(
        combined.contains(&format!("XDG_STATE_HOME={}", state.display()))
            || combined.contains(&format!(
                "<key>XDG_STATE_HOME</key>\n        <string>{}</string>",
                state.display()
            )),
        "the unit does not pin XDG_STATE_HOME:\n{combined}"
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
    let swapped = original
        .replace(
            &store.to_string_lossy().replace('\\', "\\\\"),
            &decoy_store.to_string_lossy().replace('\\', "\\\\"),
        )
        .replace(
            &root.join("key.txt").to_string_lossy().replace('\\', "\\\\"),
            &decoy_key.to_string_lossy().replace('\\', "\\\\"),
        );
    assert_ne!(swapped, original, "fixture config shape changed");
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
    assert!(
        stderr.contains(&format!(
            "config_digest changed; review {} and reinstall",
            fixture.config_path().display()
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
    let body = std::fs::read_to_string(&fixture.manifest).unwrap();
    let gone = fixture.root.join("gone");
    let edited = body.replace(
        &format!("\"working_directory\": \"{}\"", json_path(&fixture.root)),
        &format!("\"working_directory\": \"{}\"", json_path(&gone)),
    );
    assert_ne!(edited, body, "manifest shape changed: {body}");
    std::fs::write(&fixture.manifest, edited).unwrap();

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
    render, Platform, RotationSchedule, ScheduleCommand, ScheduleInterval, UnitPaths,
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

/// The reinstall hint has to be a command the user can paste, which means the
/// name they installed with — the recorded workspace alias — not a placeholder
/// and not the real vault behind it. `--vault default` here would send them at
/// a different target than `--vault pinned`.
#[test]
fn a_reinstall_hint_names_the_alias_the_schedule_was_installed_with() {
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
    let out = xv_cmd_in(root)
        .env("XV_BACKEND", "local")
        .env("XV_STATE_HOME", state)
        .args(["schedule", "status"])
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn status_labels_a_legacy_unit_and_refuses_to_vouch_for_its_target() {
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
    let Some(platform) = host_platform() else {
        return;
    };
    if platform == Platform::Schtasks {
        return;
    }
    let fixture = pinned_run_fixture(&[], |_| {});
    seed_pinned_units(platform, &fixture.root, &fixture.manifest);

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
    let out = cmd
        .env("XV_BACKEND", "local")
        .env("XV_STATE_HOME", &fixture.state)
        .args(["schedule", "uninstall"])
        .output()
        .unwrap();
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
    let out = cmd
        .env("XV_BACKEND", "local")
        .env("XV_STATE_HOME", &state)
        .args(["schedule", "uninstall"])
        .output()
        .unwrap();
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
