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
        // Fake Azure creds: config validation requires these to be
        // non-empty before any command runs. The fake UUIDs satisfy
        // validation but never reach Azure — tested code paths exit
        // on filesystem ops or local errors, not Azure round-trips.
        .env(
            "AZURE_SUBSCRIPTION_ID",
            "00000000-0000-0000-0000-000000000000",
        )
        .env("AZURE_TENANT_ID", "00000000-0000-0000-0000-000000000000")
        // (We don't inherit other AZURE_* vars — env_clear() handles that —
        // so accidentally hitting a real subscription is impossible.)
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

/// Build an isolated `xv` command backed by the **local** (age-encrypted
/// file) backend, with a valid `xv.conf` written up front — so commands
/// that need a working backend (not just filesystem/config-parse checks)
/// can run fully offline, deterministically, with no Azure credentials or
/// network access. Mirrors the harness in `e2e_local_backend.rs::TestEnv`.
///
/// Returns `(command, tempdir)`; hold the tempdir alive for the test
/// duration (it cleans up on drop). The `--backend local` selection happens
/// via `XV_BACKEND=local` (read directly by config loading, unlike a CLI
/// `--backend` flag which is resolved too late to affect the early
/// `Config::validate()` pass) plus `backend = "local"` in `xv.conf` as a
/// belt-and-suspenders match.
pub fn xv_isolated_local() -> (Command, TempDir) {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_dir = temp.path().join(".config");
    let store_dir = temp.path().join("store");
    let key_file = temp.path().join("key.txt");
    let xv_dir = config_dir.join("xv");
    std::fs::create_dir_all(&xv_dir).expect("create config dir");
    std::fs::create_dir_all(&store_dir).expect("create store dir");

    let config_content = format!(
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
        store = store_dir.display(),
        key = key_file.display(),
    );
    std::fs::write(xv_dir.join("xv.conf"), config_content).expect("write config");

    let mut cmd = xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", &config_dir)
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(temp.path());
    (cmd, temp)
}

/// Build an isolated `xv` command backed by the **local** backend (same
/// global `xv.conf` as `xv_isolated_local()`) whose cwd is a project
/// subdirectory containing `xv_toml` written out as `.xv.toml` — for tests
/// that need env-profile (`.xv.toml`) resolution with a fully hermetic
/// environment (`env_clear()` + explicit allowlist), per the #317 lesson:
/// selective `env_remove()` leaks host vars like `DEBUG`, `CACHE_TTL`,
/// `AZURE_CREDENTIAL_PRIORITY`, `BLOB_*` into the child.
///
/// Returns `(command, tempdir)`; hold the tempdir alive for the test
/// duration. Note: `XV_NO_PARENT_CONFIG=1` is still safe here — a `.xv.toml`
/// directly in cwd is always found regardless of that flag (it only blocks
/// *ancestor* walk-up), so profile resolution is still bounded to this one
/// project dir.
pub fn xv_isolated_local_with_profile(xv_toml: &str) -> (Command, TempDir) {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_dir = temp.path().join(".config");
    let store_dir = temp.path().join("store");
    let key_file = temp.path().join("key.txt");
    let xv_dir = config_dir.join("xv");
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(&xv_dir).expect("create config dir");
    std::fs::create_dir_all(&store_dir).expect("create store dir");
    std::fs::create_dir_all(&project_dir).expect("create project dir");

    let config_content = format!(
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
        store = store_dir.display(),
        key = key_file.display(),
    );
    std::fs::write(xv_dir.join("xv.conf"), config_content).expect("write config");
    std::fs::write(project_dir.join(".xv.toml"), xv_toml).expect("write .xv.toml");

    let mut cmd = xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", &config_dir)
        .env("XV_NO_PARENT_CONFIG", "1")
        .env("XV_BACKEND", "local")
        .env("NO_COLOR", "1")
        .current_dir(&project_dir);
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
    assert!(
        body.get("error").is_some(),
        "envelope must have 'error' key: {body}"
    );
    let err = &body["error"];
    assert!(err.get("code").is_some(), "envelope.error must have 'code'");
    assert!(
        err.get("message").is_some(),
        "envelope.error must have 'message'"
    );
    assert!(
        err.get("exit_code").is_some(),
        "envelope.error must have 'exit_code'"
    );
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
