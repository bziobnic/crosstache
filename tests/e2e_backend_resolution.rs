//! End-to-end regression tests for issue #305: `.xv.toml` env-profile
//! backend resolution ordering.
//!
//! Two bugs shared a root cause in `main::run()`:
//!   1. The global config was validated (`Config::validate`) BEFORE the
//!      `.xv.toml` env-profile backend was folded into `config.backend`, so
//!      a profile selecting `local`/`aws` could be rejected against
//!      incomplete global Azure config that was never going to be used.
//!   2. `XV_BACKEND` populates `cli.backend` via clap's `env = "XV_BACKEND"`
//!      attribute, indistinguishable from a real `--backend` flag. The
//!      profile lookup was gated on `cli.backend.is_none()`, so setting
//!      `XV_BACKEND` silently skipped the profile lookup entirely and won
//!      the CLI slot in `resolve_effective_backend`, outranking the
//!      profile — even though documented precedence places the profile
//!      above `XV_BACKEND` / global config.
//!
//! This harness is intentionally separate from `tests/e2e_local_backend.rs`:
//! that suite's `TestEnv` hard-sets `XV_BACKEND=local` on every invocation,
//! which would mask both bugs above.
//!
//! Run with:
//!   cargo test --test e2e_backend_resolution

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// A test environment with a global `xv.conf` (azure backend, no/empty
/// credentials, valid `[local]` block) and an isolated project directory
/// that may contain a `.xv.toml` env profile.
struct BackendResolutionEnv {
    _tmp: TempDir,
    config_dir: PathBuf,
    project_dir: PathBuf,
}

impl BackendResolutionEnv {
    /// Global config: no `backend` key (defaults fall through to "azure"),
    /// empty Azure credentials (would fail `validate()` if ever reached),
    /// plus a valid `[local]` block so the local backend can actually run.
    fn new() -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let config_dir = tmp.path().join("config");
        let project_dir = tmp.path().join("project");
        let store_dir = tmp.path().join("store");
        let key_file = tmp.path().join("key.txt");
        let xv_dir = config_dir.join("xv");

        std::fs::create_dir_all(&xv_dir).expect("create config dir");
        std::fs::create_dir_all(&project_dir).expect("create project dir");
        std::fs::create_dir_all(&store_dir).expect("create store dir");

        let config_content = format!(
            r#"debug = false
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

        Self {
            _tmp: tmp,
            config_dir,
            project_dir,
        }
    }

    /// Write a `.xv.toml` in the project dir with a single `dev` env
    /// profile (the active/default env) whose `backend` field is `local`.
    fn write_local_profile(&self) {
        let xv_toml = r#"default_env = "dev"

[env.dev]
backend = "local"
"#;
        std::fs::write(self.project_dir.join(".xv.toml"), xv_toml).expect("write .xv.toml");
    }

    /// Base command with cwd set to the project dir, isolated config, and
    /// Azure env vars scrubbed. Deliberately does NOT set `XV_BACKEND` —
    /// callers add it explicitly when a test needs it.
    fn xv(&self) -> Command {
        let binary = env!("CARGO_BIN_EXE_xv");
        let mut cmd = Command::new(binary);
        cmd.current_dir(&self.project_dir);
        cmd.env("XDG_CONFIG_HOME", &self.config_dir);
        cmd.env_remove("XV_BACKEND");
        cmd.env_remove("AZURE_SUBSCRIPTION_ID");
        cmd.env_remove("AZURE_TENANT_ID");
        cmd.env_remove("DEFAULT_VAULT");
        cmd.env_remove("XV_ENV");
        cmd.env("NO_COLOR", "1");
        cmd
    }

    fn run(&self, cmd: &mut Command, args: &[&str]) -> std::process::Output {
        cmd.args(args).output().expect("execute xv binary")
    }
}

// ---------------------------------------------------------------------------
// Test A (bug 1): profile backend must be folded in BEFORE validation.
// ---------------------------------------------------------------------------
//
// Global config has no `backend` key and empty Azure credentials, so
// `validate()` would fail if the effective backend were still "azure" at
// validation time. The project's `.xv.toml` selects `local`. No
// `XV_BACKEND` is set. Before the fix, `load_config()` validated the
// global config (still "azure") first and failed with "Subscription ID is
// required" — the profile was resolved too late to matter.
#[test]
fn profile_backend_resolved_before_validation() {
    let env = BackendResolutionEnv::new();
    env.write_local_profile();

    let output = env.run(&mut env.xv(), &["list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected `xv list` to succeed using the .xv.toml local profile, but it failed \
(exit {:?})\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code(),
    );
    assert!(
        !stderr.contains("Subscription ID is required"),
        "must not hit Azure validation when the profile selects local: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test B (bug 2): XV_BACKEND must not outrank the .xv.toml profile.
// ---------------------------------------------------------------------------
//
// Same setup as Test A, plus `XV_BACKEND=azure` in the environment. The
// profile's `local` backend must still win. Before the fix, clap folded
// XV_BACKEND into `cli.backend`, which (a) suppressed the profile lookup
// entirely (`cli.backend.is_none()` was false) and (b) won the CLI slot in
// `resolve_effective_backend`, so the command failed azure validation.
#[test]
fn xv_backend_env_does_not_outrank_profile() {
    let env = BackendResolutionEnv::new();
    env.write_local_profile();

    let mut cmd = env.xv();
    cmd.env("XV_BACKEND", "azure");
    let output = env.run(&mut cmd, &["list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected the .xv.toml profile's `local` backend to win over XV_BACKEND=azure, \
but the command failed (exit {:?})\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code(),
    );
    assert!(
        !stderr.contains("Subscription ID is required"),
        "XV_BACKEND must not outrank the .xv.toml profile: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test C: a real --backend flag still outranks the profile.
// ---------------------------------------------------------------------------
//
// Same setup, but this time `--backend azure` is passed as an actual CLI
// argument. The explicit flag must still win over the profile, proving the
// fix didn't also break the top of the precedence chain. Global config has
// empty Azure credentials, so this is expected to fail validation.
#[test]
fn explicit_backend_flag_still_wins_over_profile() {
    let env = BackendResolutionEnv::new();
    env.write_local_profile();

    let output = env.run(&mut env.xv(), &["--backend", "azure", "list"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected `--backend azure` to outrank the local profile and fail azure validation, \
but the command succeeded"
    );
    assert!(
        stderr.contains("Subscription ID"),
        "expected an azure-validation failure mentioning \"Subscription ID\", got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test D: no .xv.toml — XV_BACKEND still wins over config file / default.
// ---------------------------------------------------------------------------
//
// No project profile exists. `XV_BACKEND=local` must still take effect via
// the config-file layer (`load_from_env` copies it into `config.backend`),
// proving the fix didn't regress the case where XV_BACKEND is the only
// override in play.
#[test]
fn xv_backend_env_wins_when_no_profile_present() {
    let env = BackendResolutionEnv::new();
    // Deliberately no .xv.toml written.

    let mut cmd = env.xv();
    cmd.env("XV_BACKEND", "local");
    let output = env.run(&mut cmd, &["list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected XV_BACKEND=local to take effect with no .xv.toml present, but the command \
failed (exit {:?})\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code(),
    );
    assert!(
        !stderr.contains("Subscription ID is required"),
        "XV_BACKEND=local should have selected the local backend: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test E: a literal `--backend` token after `--` (passthrough child-command
// args) must not be mistaken for a real `--backend` flag on `xv` itself.
// ---------------------------------------------------------------------------
//
// `xv run -- echo --backend prod` carries a `--backend` token that belongs
// to the child command, not to `xv`. Before the fix, the `cli_backend_was_arg`
// scan over `std::env::args_os()` did not stop at the `--` separator, so it
// matched this passthrough token, set `cli_backend_was_arg = true`, and
// suppressed the `.xv.toml` profile lookup — with `cli.backend` still `None`
// (clap correctly left the real `--backend` flag unset), the effective
// backend silently fell through to the global/azure layer instead of the
// profile's `local` backend.
#[test]
fn backend_token_after_separator_is_not_mistaken_for_cli_flag() {
    let env = BackendResolutionEnv::new();
    env.write_local_profile();

    let output = env.run(&mut env.xv(), &["run", "--", "echo", "--backend", "prod"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected the .xv.toml local profile to still win when a passthrough child-command arg \
contains a literal `--backend` token, but the command failed (exit {:?})\nstdout: {stdout}\n\
stderr: {stderr}",
        output.status.code(),
    );
    assert!(
        !stderr.contains("Subscription ID is required"),
        "a passthrough `--backend` token after `--` must not suppress the .xv.toml profile: {stderr}"
    );
}
