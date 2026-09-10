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
