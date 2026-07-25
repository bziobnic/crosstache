//! CLI-level coverage for `xv git` and `xv audit --verify`.
//!
//! Complements `local_audit_git_tests.rs` (which drives the backend directly)
//! by exercising the actual argument parsing, capability gates, output, and exit
//! codes a user hits.

mod common;

use common::xv_isolated_local_with_opts;

/// Run a git command inside the store and return stdout.
fn git_in(store: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(store)
        .args(args)
        .output()
        .expect("run git");
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ---------------------------------------------------------------------------
// xv git
// ---------------------------------------------------------------------------

#[test]
fn git_log_lists_a_commit_per_write() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, true);
    assert!(cmd
        .args(["set", "A", "--value", "1"])
        .status()
        .unwrap()
        .success());
    assert!(xv_cmd_for(&store)
        .args(["set", "B", "--value", "2"])
        .status()
        .unwrap()
        .success());

    let out = xv_cmd_for(&store).args(["git", "log"]).output().unwrap();
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("set A"), "{stdout}");
    assert!(stdout.contains("set B"), "{stdout}");

    // Cross-check against git itself, not just our own rendering.
    let subjects = git_in(&store, &["log", "--pretty=%s"]);
    assert_eq!(
        subjects.lines().collect::<Vec<_>>(),
        vec!["set B", "set A"],
        "real git history should show one commit per write, newest first"
    );
}

/// Rebuild a command against an existing isolated store dir.
///
/// `xv_isolated_local_with_opts` mints a fresh tempdir per call, so multi-step
/// CLI flows need to re-point at the same store; this reuses the tempdir that
/// owns `store`.
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

#[test]
fn git_log_filters_by_secret_and_honors_json_format() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, true);
    cmd.args(["set", "ALPHA", "--value", "1"]).status().unwrap();
    xv_cmd_for(&store)
        .args(["set", "BETA", "--value", "2"])
        .status()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["git", "log", "ALPHA"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("set ALPHA"), "{stdout}");
    assert!(
        !stdout.contains("set BETA"),
        "filtered log leaked BETA: {stdout}"
    );

    let out = xv_cmd_for(&store)
        .args(["git", "log", "--format", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("git log --format json must emit JSON ({e}): {stdout}"));
    assert!(parsed.is_array(), "{parsed}");
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}

#[test]
fn git_status_and_diff_report_the_store() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, true);
    cmd.args(["set", "A", "--value", "1"]).status().unwrap();

    let out = xv_cmd_for(&store).args(["git", "status"]).output().unwrap();
    assert!(out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("clean"),
        "auto-commit leaves a clean tree: {combined}"
    );

    let out = xv_cmd_for(&store).args(["git", "diff"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".age"),
        "diff should name the ciphertext file: {stdout}"
    );
    assert!(
        !stdout.contains("value") || !stdout.contains("+1"),
        "diff must not include file contents: {stdout}"
    );
}

#[test]
fn git_init_works_before_the_flag_is_enabled() {
    // Chicken-and-egg guard: `xv git init` must not require [local].git, since
    // enabling versioning is the reason you'd run it.
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(false, false);
    let out = cmd.args(["git", "init"]).output().unwrap();
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(store.join(".git").exists(), "repository should exist");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("git = true"),
        "must tell the user auto-commit is still off: {combined}"
    );

    // The managed .gitignore protects key material from the very first commit.
    let ignore = std::fs::read_to_string(store.join(".gitignore")).unwrap();
    assert!(ignore.contains("key.txt"), "{ignore}");
}

#[test]
fn git_commands_are_rejected_on_non_local_backends() {
    // Needs a config that passes Azure's own validation (subscription_id), so
    // the request reaches the `xv git` capability gate rather than tripping
    // config validation first.
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join(".config");
    let xv_dir = config_dir.join("xv");
    std::fs::create_dir_all(&xv_dir).unwrap();
    std::fs::write(
        xv_dir.join("xv.conf"),
        r#"backend = "azure"
debug = false
subscription_id = "00000000-0000-0000-0000-000000000000"
default_vault = "example-kv"
default_resource_group = "example-rg"
default_location = "eastus"
tenant_id = "00000000-0000-0000-0000-000000000000"
output_json = false
no_color = true
cache_enabled = false
cache_ttl_secs = 0
clipboard_timeout = 0
"#,
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", &config_dir)
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "azure")
        .env("NO_COLOR", "1")
        .current_dir(tmp.path())
        .args(["git", "log"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "must refuse on a cloud backend");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("xv history"),
        "error should point at the backend-native alternative: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// xv audit (local)
// ---------------------------------------------------------------------------

#[test]
fn audit_lists_local_events() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(true, false);
    cmd.args(["set", "DB_PASSWORD", "--value", "hunter2"])
        .status()
        .unwrap();
    xv_cmd_for(&store)
        .args(["get", "DB_PASSWORD", "--raw"])
        .output()
        .unwrap();

    let out = xv_cmd_for(&store)
        .args(["audit", "--vault", "default", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("json rows");
    let ops: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["operation"].as_str().unwrap())
        .collect();
    assert!(ops.contains(&"PutSecretValue"), "{ops:?}");
    assert!(ops.contains(&"GetSecretValue"), "{ops:?}");
}

#[test]
fn audit_verify_reports_an_intact_chain() {
    let (mut cmd, _tmp, store) = xv_isolated_local_with_opts(true, false);
    cmd.args(["set", "A", "--value", "1"]).status().unwrap();

    let out = xv_cmd_for(&store)
        .args(["audit", "--verify"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("intact"), "{combined}");
    // The honesty caveat must travel with the success message.
    assert!(
        combined.contains("cannot prove completeness"),
        "verify output must not overstate the guarantee: {combined}"
    );
}

#[test]
fn audit_verify_fails_nonzero_on_a_tampered_chain() {
    let (cmd, _tmp, store) = xv_isolated_local_with_opts(true, false);
    for name in ["A", "B", "C"] {
        xv_cmd_for(&store)
            .args(["set", name, "--value", "1"])
            .status()
            .unwrap();
    }
    drop(cmd);

    let log = store.join("vaults/default/.audit/log.jsonl");
    let body = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert!(lines.len() >= 3, "expected records, got {}", lines.len());
    // Excise the middle record.
    std::fs::write(&log, format!("{}\n{}\n", lines[0], lines[lines.len() - 1])).unwrap();

    let out = xv_cmd_for(&store)
        .args(["audit", "--verify"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a broken chain must exit non-zero so CI can gate on it"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.to_lowercase().contains("broken"), "{stderr}");
}

#[test]
fn audit_verify_requires_the_audit_flag() {
    let (mut cmd, _tmp, _store) = xv_isolated_local_with_opts(false, false);
    let out = cmd.args(["audit", "--verify"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("audit = true"),
        "should name the config key to enable: {stderr}"
    );
}
