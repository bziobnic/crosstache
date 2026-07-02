//! Full integration test suite for the xv CLI.
//!
//! Tests exercise the binary end-to-end for help, version, config, gen,
//! format flags, and error handling. Most tests avoid Azure by using
//! isolated config or minimal env vars.

use std::process::Command;
use std::sync::Mutex;

/// Guards against concurrent test runs that could interfere with env vars.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn run_xv(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_xv");
    Command::new(bin)
        .args(args)
        .output()
        .expect("xv binary failed to run")
}

fn run_xv_with_env(
    args: &[&str],
    env_clear: &[&str],
    env_set: &[(&str, &str)],
) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_xv");
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for k in env_clear {
        cmd.env_remove(k);
    }
    for (k, v) in env_set {
        cmd.env(k, v);
    }
    cmd.output().expect("xv binary failed to run")
}

// -----------------------------------------------------------------------------
// Help and version
// -----------------------------------------------------------------------------

#[test]
fn test_help_succeeds() {
    let out = run_xv(&["--help"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("crosstache") || stdout.contains("xv"));
    assert!(stdout.contains("Commands"));
}

#[test]
fn test_version_flag_succeeds() {
    let out = run_xv(&["--version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("xv") || stdout.contains("crosstache"));
}

#[test]
fn test_version_command_succeeds() {
    let out = run_xv(&["version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("crosstache Rust CLI") || stdout.contains("Version:"));
}

#[test]
fn test_vault_help_succeeds() {
    let out = run_xv(&["vault", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("list") || stdout.contains("create"));
}

#[test]
fn test_list_help_succeeds() {
    let out = run_xv(&["list", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("group") || stdout.contains("Group"));
}

#[test]
#[cfg(feature = "file-ops")]
fn test_file_help_succeeds() {
    let out = run_xv(&["file", "--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("list") || stdout.contains("upload"));
}

#[test]
fn test_config_help_succeeds() {
    let out = run_xv(&["config", "--help"]);
    assert!(out.status.success());
}

#[test]
fn test_gen_help_succeeds() {
    let out = run_xv(&["gen", "--help"]);
    assert!(out.status.success());
}

#[test]
fn test_completion_bash_succeeds() {
    let out = run_xv(&["completion", "bash"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("complete") || stdout.contains("_xv"));
}

#[test]
fn test_context_help_succeeds() {
    let out = run_xv(&["context", "--help"]);
    assert!(out.status.success());
}

// -----------------------------------------------------------------------------
// Config command (uses load_config_no_validation, no Azure)
// -----------------------------------------------------------------------------

#[test]
fn test_config_path_succeeds() {
    let _guard = ENV_LOCK.lock().unwrap();
    let out = run_xv_with_env(
        &["config", "path"],
        &[],
        &[("XDG_CONFIG_HOME", "/tmp/xv-integration-test-config-path")],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("xv.conf") || stdout.contains(".config"));
}

#[test]
fn test_config_show_succeeds_with_isolated_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_home = tmp.path().to_string_lossy();
    let out = run_xv_with_env(
        &["config", "show"],
        &["AZURE_SUBSCRIPTION_ID", "AZURE_TENANT_ID"],
        &[("XDG_CONFIG_HOME", config_home.as_ref())],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("subscription")
            || stdout.contains("Subscription")
            || stdout.contains("default_vault")
    );
}

// -----------------------------------------------------------------------------
// Gen command (no Azure when not using --save)
// -----------------------------------------------------------------------------

#[test]
fn test_gen_raw_produces_correct_length() {
    let _guard = ENV_LOCK.lock().unwrap();
    let out = run_xv_with_env(
        &["gen", "--length", "20", "--raw"],
        &[],
        &[
            (
                "AZURE_SUBSCRIPTION_ID",
                "00000000-0000-0000-0000-000000000000",
            ),
            ("AZURE_TENANT_ID", "00000000-0000-0000-0000-000000000000"),
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim().lines().next().unwrap_or("");
    assert_eq!(
        line.len(),
        20,
        "expected 20 chars, got {} chars",
        line.len()
    );
}

#[test]
fn test_gen_numeric_charset() {
    let _guard = ENV_LOCK.lock().unwrap();
    let out = run_xv_with_env(
        &["gen", "--length", "10", "--charset", "numeric", "--raw"],
        &[],
        &[
            (
                "AZURE_SUBSCRIPTION_ID",
                "00000000-0000-0000-0000-000000000000",
            ),
            ("AZURE_TENANT_ID", "00000000-0000-0000-0000-000000000000"),
        ],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim().lines().next().unwrap_or("");
    assert_eq!(line.len(), 10);
    assert!(
        line.chars().all(|c| c.is_ascii_digit()),
        "expected numeric, got: {}",
        line
    );
}

#[test]
fn test_gen_invalid_length_fails() {
    let _guard = ENV_LOCK.lock().unwrap();
    let out = run_xv_with_env(
        &["gen", "--length", "3", "--raw"],
        &[],
        &[
            (
                "AZURE_SUBSCRIPTION_ID",
                "00000000-0000-0000-0000-000000000000",
            ),
            ("AZURE_TENANT_ID", "00000000-0000-0000-0000-000000000000"),
        ],
    );
    assert!(!out.status.success());
}

// -----------------------------------------------------------------------------
// Format flag parsing
// -----------------------------------------------------------------------------

#[test]
fn test_vault_list_format_flag_accepted() {
    let out = run_xv(&["vault", "list", "--format", "json", "--help"]);
    assert!(out.status.success());
}

#[test]
fn test_format_auto_json_yaml_csv_table() {
    for fmt in ["auto", "json", "yaml", "csv", "table"] {
        let out = run_xv(&["vault", "list", "--format", fmt, "--help"]);
        assert!(out.status.success(), "format {} should be accepted", fmt);
    }
}

// -----------------------------------------------------------------------------
// Error handling
// -----------------------------------------------------------------------------

#[test]
fn test_vault_list_fails_without_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_home = tmp.path().to_string_lossy();
    let out = run_xv_with_env(
        &["vault", "list"],
        &["AZURE_SUBSCRIPTION_ID", "AZURE_TENANT_ID", "DEFAULT_VAULT"],
        &[("XDG_CONFIG_HOME", config_home.as_ref())],
    );
    assert!(
        !out.status.success(),
        "vault list without config should fail"
    );
}

#[test]
fn test_list_fails_without_vault() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_home = tmp.path().to_string_lossy();
    let out = run_xv_with_env(
        &["list"],
        &["DEFAULT_VAULT"],
        &[
            ("XDG_CONFIG_HOME", config_home.as_ref()),
            (
                "AZURE_SUBSCRIPTION_ID",
                "00000000-0000-0000-0000-000000000000",
            ),
            ("AZURE_TENANT_ID", "00000000-0000-0000-0000-000000000000"),
        ],
    );
    assert!(!out.status.success(), "list without vault should fail");
}

#[test]
fn test_unknown_subcommand_fails() {
    let out = run_xv(&["nonexistent-subcommand"]);
    assert!(!out.status.success());
}

// -----------------------------------------------------------------------------
// Output format behavior (vault list with cached or API path)
// -----------------------------------------------------------------------------

#[test]
fn test_vault_list_json_format_flag() {
    let out = run_xv(&["vault", "list", "--format", "json", "--no-cache", "--help"]);
    assert!(
        out.status.success(),
        "vault list --format json --help should work"
    );
}

// --- Help matrix: every top-level command supports --help ---

#[test]
fn every_top_level_command_supports_help() {
    // The full list of top-level Commands as of v0.7.0-rc.2.
    // If a new command is added, append it here AND ensure --help works.
    let commands: &[&[&str]] = &[
        &["set"],
        &["get"],
        &["find"],
        &["list"],
        &["delete"],
        &["update"],
        &["restore"],
        &["purge"],
        &["history"],
        &["rollback"],
        &["rotate"],
        &["run"],
        &["inject"],
        &["whoami"],
        &["info"],
        &["audit"],
        &["share"],
        &["share", "grant"],
        &["share", "revoke"],
        &["share", "list"],
        &["env"],
        &["env", "list"],
        &["env", "use"],
        &["env", "create"],
        &["context"],
        &["context", "show"],
        &["context", "use"],
        &["context", "list"],
        &["context", "init"],
        &["vault"],
        &["vault", "create"],
        &["vault", "list"],
        &["vault", "delete"],
        &["vault", "info"],
        &["vault", "share"],
        &["config"],
        &["config", "show"],
        &["config", "path"],
        &["completion"],
        &["scan"],
        &["scan", "install"],
        &["scan", "uninstall"],
        &["init"],
        &["upgrade"],
        &["version"],
        &["gen"],
        &["copy"],
        &["move"],
        &["diff"],
        &["parse"],
    ];
    for args in commands {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_xv"));
        cmd.args(*args).arg("--help");
        let out = cmd.output().expect("spawn");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?} --help should exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.stdout.is_empty() || !out.stderr.is_empty(),
            "{args:?} --help produced empty output"
        );
    }
}

#[test]
#[cfg(not(feature = "tui"))]
fn tui_subcommand_unknown_when_feature_disabled() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .args(["tui"])
        .output()
        .expect("spawn");
    // clap returns exit 2 for unknown subcommands.
    assert_eq!(out.status.code(), Some(2));
}

// -----------------------------------------------------------------------------
// AWS migration dry-run (LocalStack-gated)
// -----------------------------------------------------------------------------

#[test]
#[cfg(feature = "aws")]
fn migrate_dry_run_against_local_to_aws_shows_summary() {
    use tempfile::TempDir;

    if std::env::var("AWS_INTEGRATION_TESTS").is_err() {
        eprintln!("skipping: AWS_INTEGRATION_TESTS not set");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let xv = env!("CARGO_BIN_EXE_xv");

    // Set up local store with one secret
    let set_out = Command::new(xv)
        .args(["--backend", "local", "set", "test-secret", "value123"])
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .output()
        .expect("set should succeed");
    assert!(
        set_out.status.success(),
        "set failed: {:?}",
        String::from_utf8_lossy(&set_out.stderr)
    );

    // Run migrate dry-run
    let out = Command::new(xv)
        .args([
            "migrate",
            "--from",
            "local",
            "--to",
            "aws",
            "--vault",
            "default",
            "--dry-run",
        ])
        .env("HOME", tmp.path())
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env(
            "AWS_ENDPOINT_URL",
            std::env::var("AWS_ENDPOINT_URL")
                .unwrap_or_else(|_| "http://localhost:4566".to_string()),
        )
        .env("AWS_REGION", "us-east-1")
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .output()
        .expect("migrate should run");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("Source:") || stderr.contains("Source:"),
        "expected 'Source:' in output; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Target:") || stderr.contains("Target:"),
        "expected 'Target:' in output; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("to migrate") || stderr.contains("to migrate"),
        "expected 'to migrate' counts; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
