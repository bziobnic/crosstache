//! Integration tests asserting the `xv` binary exits with the documented
//! exit code per error family. These tests build and run the binary.

use std::process::Command;

fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

#[test]
fn invalid_argument_exits_2() {
    let out = xv().args(["--this-flag-does-not-exist"]).output().unwrap();
    assert!(!out.status.success());
    // clap parse failures use exit 2 on its own; we rely on that being our
    // family code as well, which the new exit_code() preserves.
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn unknown_subcommand_exits_2() {
    let out = xv()
        .args(["this-subcommand-does-not-exist"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

// Note: this test depends on a configured xv environment with a known
// vault. We mark it ignored by default; CI runs it via XV_TEST_VAULT.
#[test]
#[ignore = "requires XV_TEST_VAULT and credentials"]
fn secret_not_found_includes_suggestion_when_close_match_exists() {
    let vault = std::env::var("XV_TEST_VAULT").expect("XV_TEST_VAULT must be set");
    // Assumes a secret named "DB_PASSWORD" exists in XV_TEST_VAULT.
    let out = xv()
        .args(["get", "DB_PASSWURD", "--vault", &vault, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert_eq!(body["error"]["code"], "xv-secret-not-found");
    assert_eq!(body["error"]["suggestion"], "DB_PASSWORD");
}

#[test]
#[ignore = "requires a working config that triggers VaultNotFound predictably"]
fn json_format_emits_error_envelope() {
    // Triggers a vault-not-found by passing a vault name that cannot exist.
    let out = xv()
        .args([
            "vault",
            "info",
            "definitely-does-not-exist-zzzzzzzz",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(11));
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be JSON envelope");
    assert_eq!(body["error"]["code"], "xv-vault-not-found");
    assert_eq!(body["error"]["exit_code"], 11);
    assert!(body["error"]["message"].is_string());
}

#[test]
fn auto_format_does_not_emit_json_error_envelope_on_stdout() {
    let out = xv()
        .args(["gen", "--length", "5", "--raw"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "default Auto must not print JSON (or any) error on stdout: {stdout:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("between") || stderr.contains('6'),
        "human-readable error should be on stderr: {stderr:?}"
    );
}

#[test]
fn explicit_json_format_emits_error_envelope_on_stdout() {
    let out = xv()
        .args(["gen", "--length", "5", "--raw", "--format", "json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be JSON envelope");
    assert_eq!(body["error"]["code"], "xv-invalid-argument");
    assert_eq!(body["error"]["exit_code"], 2);
}

#[test]
fn plain_format_writes_error_to_stderr() {
    let out = xv()
        .args(["this-subcommand-does-not-exist"])
        .output()
        .unwrap();
    assert!(
        !out.stderr.is_empty(),
        "stderr should contain clap parse error"
    );
}

#[test]
fn find_unknown_in_field_exits_2() {
    let out = xv()
        .args(["find", "anything", "--in", "bogus_field"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
#[ignore = "requires XV_TEST_VAULT and credentials"]
fn find_json_envelope_is_array_of_records() {
    let vault = std::env::var("XV_TEST_VAULT").expect("XV_TEST_VAULT must be set");
    let out = xv()
        .args(["find", "db", "--vault", &vault, "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ok exit when vault reachable");
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert!(body.is_array(), "envelope is a top-level array");
    if let Some(first) = body.as_array().and_then(|a| a.first()) {
        assert!(first.get("name").is_some());
        assert!(first.get("score").is_some());
    }
}

#[test]
#[ignore = "requires XV_TEST_VAULT and credentials"]
fn ls_names_only_no_headers_no_ansi() {
    let vault = std::env::var("XV_TEST_VAULT").expect("XV_TEST_VAULT must be set");
    let out = xv()
        .args(["ls", "--names-only", "--vault", &vault])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // No ANSI escapes
    assert!(!stdout.contains('\x1b'), "names-only must be ANSI-free");
    // No "Name" header
    assert!(!stdout.lines().any(|l| l.trim() == "Name"));
}
