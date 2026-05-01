//! Shared helpers for hermetic E2E tests.
//!
//! The cardinal invariant: tests that import this module never read
//! or write the user's real ~/.config/xv/, ~/.azure/, or any
//! .xv.toml outside their own tempdir. Helpers below set up the
//! isolation; tests that bypass them risk pollution.

#![allow(dead_code)] // each test file imports a subset of helpers

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Spawn the `xv` binary built by Cargo for the current target.
pub fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

/// Apply isolation env vars to a `Command`. Caller passes a tempdir;
/// this routes XDG_CONFIG_HOME, HOME, and XV_NO_PARENT_CONFIG so
/// config and walk-up resolution start clean.
pub fn isolate<'a>(cmd: &'a mut Command, tempdir: &Path) -> &'a mut Command {
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", tempdir)
        .env("XDG_CONFIG_HOME", tempdir.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        // Don't inherit AZURE_* — prevents accidentally hitting a real subscription.
        .current_dir(tempdir)
}

/// Convenience: build an isolated `xv` command in a fresh tempdir.
/// Returns (command, tempdir). Hold the tempdir alive for the test
/// duration (it cleans up on drop).
pub fn xv_isolated() -> (Command, TempDir) {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut cmd = xv();
    isolate(&mut cmd, temp.path());
    (cmd, temp)
}

/// Init a minimal git repo at `path`. Used by scan-install tests.
pub fn init_git_repo(path: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed at {}", path.display());
    // git requires user.name/email to commit; not strictly needed for
    // scan install/uninstall but cheap to set up:
    let _ = Command::new("git")
        .args(["config", "user.email", "test@example.invalid"])
        .current_dir(path)
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(path)
        .status();
}

/// Write a minimal `.xv.toml` at `dir/.xv.toml`. Returns the path.
pub fn write_xv_toml(dir: &Path, default_env: &str, envs: &[(&str, &str)]) -> std::path::PathBuf {
    use std::fmt::Write as _;
    let path = dir.join(".xv.toml");
    let mut s = String::new();
    writeln!(s, "default_env = \"{default_env}\"\n").unwrap();
    for (name, vault) in envs {
        writeln!(s, "[env.{name}]").unwrap();
        writeln!(s, "vault = \"{vault}\"").unwrap();
        writeln!(s, "resource_group = \"test-rg\"").unwrap();
        writeln!(s).unwrap();
    }
    std::fs::write(&path, s).expect("write .xv.toml");
    path
}

/// Parse a JSON error envelope from stdout. Asserts the shape:
/// `{ "error": { "code": "...", "message": "...", "exit_code": N, ... } }`.
pub fn parse_json_envelope(stdout: &[u8]) -> serde_json::Value {
    let body: serde_json::Value =
        serde_json::from_slice(stdout).expect("stdout must be valid JSON");
    assert!(body.get("error").is_some(), "envelope must have 'error' key: {body}");
    let err = &body["error"];
    assert!(err.get("code").is_some(), "envelope.error must have 'code'");
    assert!(err.get("message").is_some(), "envelope.error must have 'message'");
    assert!(err.get("exit_code").is_some(), "envelope.error must have 'exit_code'");
    body
}

/// Read stdout as a UTF-8 string for assertions.
pub fn stdout_str(out: &std::process::Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout utf-8")
}

/// Read stderr as a UTF-8 string for assertions.
pub fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr utf-8")
}
