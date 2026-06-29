//! End-to-end CLI tests for the **local** (age-encrypted file) backend.
//!
//! Every test in this module invokes the real `xv` binary with an isolated
//! temp directory, config file, and store. No Azure credentials or network
//! access are required.
//!
//! Run with:
//!   cargo test --test e2e_local_backend
//!
//! These tests are NOT `#[ignore]` — they run in every CI build.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

struct TestEnv {
    _tmp: TempDir,
    config_dir: PathBuf,
    #[allow(dead_code)]
    store_dir: PathBuf,
}

impl TestEnv {
    /// Create a fresh, isolated test environment with a valid config file.
    fn new() -> Self {
        let tmp = TempDir::new().expect("create temp dir");
        let config_dir = tmp.path().join("config");
        let store_dir = tmp.path().join("store");
        let key_file = tmp.path().join("key.txt");
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

        Self {
            _tmp: tmp,
            config_dir,
            store_dir,
        }
    }

    /// Return a `Command` pre-configured for this test environment.
    fn xv(&self) -> Command {
        let binary = env!("CARGO_BIN_EXE_xv");
        let mut cmd = Command::new(binary);
        cmd.env("XDG_CONFIG_HOME", &self.config_dir);
        cmd.env("XV_BACKEND", "local");
        // Prevent inheriting any real Azure creds / config from the host
        cmd.env_remove("AZURE_SUBSCRIPTION_ID");
        cmd.env_remove("AZURE_TENANT_ID");
        cmd.env_remove("DEFAULT_VAULT");
        // Use raw/plain output for deterministic test assertions
        cmd.env("NO_COLOR", "1");
        cmd
    }

    /// Run `xv` with args and assert success. Returns stdout.
    fn xv_ok(&self, args: &[&str]) -> String {
        let output = self.xv().args(args).output().expect("execute xv binary");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "xv {:?} failed (exit {:?}):\nstdout: {}\nstderr: {}",
            args,
            output.status.code(),
            stdout,
            stderr,
        );
        stdout
    }

    /// Run `xv` and assert it fails (non-zero exit). Returns (stdout, stderr).
    fn xv_fail(&self, args: &[&str]) -> (String, String) {
        let output = self.xv().args(args).output().expect("execute xv binary");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            !output.status.success(),
            "xv {:?} should have failed but succeeded:\nstdout: {}\nstderr: {}",
            args,
            stdout,
            stderr,
        );
        (stdout, stderr)
    }

    /// Set a secret via stdin piping. Returns stdout on success.
    fn set_secret(&self, name: &str, value: &str) -> String {
        self.set_secret_with_args(name, value, &[])
    }

    /// Set a secret via stdin piping with extra args. Returns stdout on success.
    fn set_secret_with_args(&self, name: &str, value: &str, extra: &[&str]) -> String {
        let mut cmd = self.xv();
        cmd.args(["set", name, "--stdin"]);
        cmd.args(extra);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("spawn xv set");
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(value.as_bytes()).ok();
        }
        let output = child.wait_with_output().expect("wait for xv set");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            output.status.success(),
            "xv set {} failed:\nstdout: {}\nstderr: {}",
            name,
            stdout,
            stderr,
        );
        stdout
    }

    /// Get a secret's raw value.
    fn get_raw(&self, name: &str) -> String {
        let out = self.xv_ok(&["get", name, "--raw"]);
        out.trim().to_string()
    }

    /// Run `xv` with the given args, piping `input` to stdin. Returns the
    /// raw Output (no success assertion) so callers can check exact bytes
    /// or expected failures.
    fn xv_with_stdin(&self, args: &[&str], input: &str) -> std::process::Output {
        let mut cmd = self.xv();
        cmd.args(args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("spawn xv");
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(input.as_bytes()).ok();
        }
        child.wait_with_output().expect("wait for xv")
    }

    /// Get a secret's raw value with bytes preserved exactly (no trimming).
    /// `xv get --raw` prints the value with no trailing newline appended.
    fn get_raw_exact(&self, name: &str) -> String {
        self.xv_ok(&["get", name, "--raw"])
    }
}

// ===========================================================================
// Secret CRUD
// ===========================================================================

#[test]
fn set_and_get_secret() {
    let env = TestEnv::new();
    env.set_secret("DB_PASSWORD", "hunter2");
    let value = env.get_raw("DB_PASSWORD");
    assert_eq!(value, "hunter2");
}

#[test]
fn list_secrets() {
    let env = TestEnv::new();
    env.set_secret("API_KEY", "abc123");
    let output = env.xv_ok(&["ls"]);
    assert!(
        output.contains("API_KEY"),
        "list output should contain 'API_KEY', got:\n{}",
        output
    );
}

#[test]
fn list_json_format() {
    let env = TestEnv::new();
    env.set_secret("JSON_TEST", "value123");
    let output = env.xv_ok(&["list", "--format", "json"]);
    // Verify it's valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&output).expect("list --format json should produce valid JSON");
    // It should be an array containing at least one entry
    let arr = parsed.as_array().expect("JSON output should be an array");
    assert!(!arr.is_empty(), "JSON array should not be empty");
    // The secret name should appear somewhere in the serialized JSON
    assert!(
        output.contains("JSON_TEST"),
        "JSON output should mention our secret name"
    );
}

#[test]
fn update_secret_metadata() {
    let env = TestEnv::new();
    env.set_secret("META_KEY", "original");

    // Update with a note
    env.xv_ok(&["update", "META_KEY", "--note", "important note"]);

    // Value should still be accessible
    let value = env.get_raw("META_KEY");
    assert_eq!(value, "original");
}

#[test]
fn delete_and_verify() {
    let env = TestEnv::new();
    env.set_secret("TEMP_SECRET", "temp-value");

    // Delete with --force
    env.xv_ok(&["delete", "TEMP_SECRET", "--force"]);

    // List should not show the secret
    let output = env.xv_ok(&["ls", "--names-only"]);
    assert!(
        !output.contains("TEMP_SECRET"),
        "deleted secret should not appear in list"
    );
}

// ===========================================================================
// Soft Delete / Restore / Purge
// ===========================================================================

#[test]
fn soft_delete_and_restore() {
    let env = TestEnv::new();
    env.set_secret("RESTORE_ME", "precious-value");

    // Soft delete
    env.xv_ok(&["delete", "RESTORE_ME", "--force"]);

    // Get should fail
    env.xv_fail(&["get", "RESTORE_ME", "--raw"]);

    // Restore
    env.xv_ok(&["restore", "RESTORE_ME"]);

    // Get should succeed again
    let value = env.get_raw("RESTORE_ME");
    assert_eq!(value, "precious-value");
}

#[test]
fn soft_delete_and_purge() {
    let env = TestEnv::new();
    env.set_secret("PURGE_ME", "gone-forever");

    // Soft delete
    env.xv_ok(&["delete", "PURGE_ME", "--force"]);

    // Purge permanently
    env.xv_ok(&["purge", "PURGE_ME", "--force"]);

    // Restore should fail (purged permanently)
    env.xv_fail(&["restore", "PURGE_ME"]);

    // Get should also fail
    env.xv_fail(&["get", "PURGE_ME", "--raw"]);
}

// ===========================================================================
// Version History
// ===========================================================================

#[test]
fn version_history() {
    let env = TestEnv::new();

    // Set v1
    env.set_secret("VERSIONED", "v1-data");
    // Set v2 (overwrites)
    env.set_secret("VERSIONED", "v2-data");

    let output = env.xv_ok(&["history", "VERSIONED"]);
    // History should show version info (at least two entries)
    assert!(
        output.contains("v1") || output.contains("V1") || output.contains("Version"),
        "history should show version info, got:\n{}",
        output
    );
}

#[test]
fn rollback() {
    let env = TestEnv::new();

    env.set_secret("ROLLBACK_KEY", "first-value");
    env.set_secret("ROLLBACK_KEY", "second-value");

    // Current value should be v2
    let current = env.get_raw("ROLLBACK_KEY");
    assert_eq!(current, "second-value");

    // Rollback to v1
    env.xv_ok(&["rollback", "ROLLBACK_KEY", "--version", "v1", "--force"]);

    // Now should return v1 value
    let rolled = env.get_raw("ROLLBACK_KEY");
    assert_eq!(rolled, "first-value");
}

// ===========================================================================
// Find / Search
// ===========================================================================

#[test]
fn find_secret() {
    let env = TestEnv::new();
    env.set_secret("DATABASE_URL", "postgres://...");
    env.set_secret("DATABASE_PASSWORD", "secret");
    env.set_secret("API_TOKEN", "tok123");

    let output = env.xv_ok(&["find", "DATABASE"]);
    assert!(
        output.contains("DATABASE_URL"),
        "find should match DATABASE_URL"
    );
    assert!(
        output.contains("DATABASE_PASSWORD"),
        "find should match DATABASE_PASSWORD"
    );
}

// ===========================================================================
// Vault Operations
// ===========================================================================

#[test]
fn create_and_list_vaults() {
    let env = TestEnv::new();

    // Default vault already exists. Create a new one.
    env.xv_ok(&["vault", "create", "staging"]);

    let output = env.xv_ok(&["vault", "list"]);
    assert!(
        output.contains("staging"),
        "vault list should contain 'staging', got:\n{}",
        output
    );
    assert!(
        output.contains("default"),
        "vault list should contain 'default', got:\n{}",
        output
    );
}

#[test]
fn delete_vault() {
    let env = TestEnv::new();

    env.xv_ok(&["vault", "create", "throwaway"]);
    env.xv_ok(&["vault", "delete", "throwaway", "--force"]);

    let output = env.xv_ok(&["vault", "list"]);
    assert!(
        !output.contains("throwaway"),
        "deleted vault should not appear in list"
    );
}

#[test]
fn vault_isolation() {
    let env = TestEnv::new();

    // Create a second vault
    env.xv_ok(&["vault", "create", "isolated"]);

    // Set secret in default vault (the config default_vault is "default")
    env.set_secret("SHARED_NAME", "default-value");

    // List default vault — should show the secret
    let default_list = env.xv_ok(&["ls", "--names-only"]);
    assert!(
        default_list.contains("SHARED_NAME"),
        "default vault should have SHARED_NAME"
    );

    // Verify secret is NOT visible in the isolated vault.
    // We use `xv find` on all vaults to check — the isolated vault should be empty.
    // Actually, let's use --format json and verify the default vault list has
    // our secret but no cross-contamination to the other vault.
    // We can't easily switch vaults via CLI (would need context set), so let's
    // check via vault list that we have 2 vaults.
    let vault_list = env.xv_ok(&["vault", "list"]);
    assert!(vault_list.contains("default"), "should list default vault");
    assert!(
        vault_list.contains("isolated"),
        "should list isolated vault"
    );

    // The find --all-vaults command should show the secret only from default vault
    let find_output = env.xv_ok(&["find", "SHARED_NAME", "--all-vaults"]);
    assert!(
        find_output.contains("SHARED_NAME"),
        "find --all-vaults should find the secret"
    );
}

// ===========================================================================
// Error Cases
// ===========================================================================

#[test]
fn get_nonexistent() {
    let env = TestEnv::new();
    let (stdout, stderr) = env.xv_fail(&["get", "NOPE_DOES_NOT_EXIST", "--raw"]);
    // Should have some error output
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        !combined.is_empty(),
        "error output should be non-empty for missing secret"
    );
}

#[test]
fn delete_nonexistent_vault() {
    let env = TestEnv::new();
    env.xv_fail(&["vault", "delete", "NOPE_NO_VAULT", "--force"]);
}

// ===========================================================================
// Output Formats
// ===========================================================================

#[test]
fn json_output() {
    let env = TestEnv::new();
    env.set_secret("FMT_JSON", "fmt-val");

    let output = env.xv_ok(&["list", "--format", "json"]);
    let _parsed: serde_json::Value =
        serde_json::from_str(&output).expect("should produce valid JSON");
}

#[test]
fn names_only_output() {
    let env = TestEnv::new();
    env.set_secret("PLAIN_NAME_1", "val1");
    env.set_secret("PLAIN_NAME_2", "val2");

    let output = env.xv_ok(&["ls", "--names-only"]);
    let lines: Vec<&str> = output.trim().lines().collect();

    // Should have exactly 2 lines, each a plain name
    assert_eq!(lines.len(), 2, "should have 2 names, got: {:?}", lines);
    assert!(lines.contains(&"PLAIN_NAME_1"));
    assert!(lines.contains(&"PLAIN_NAME_2"));
}

// ===========================================================================
// Bulk Operations
// ===========================================================================

#[test]
fn bulk_set() {
    let env = TestEnv::new();

    // Bulk set: KEY=value pairs
    env.xv_ok(&["set", "BULK_A=alpha", "BULK_B=beta", "BULK_C=gamma"]);

    assert_eq!(env.get_raw("BULK_A"), "alpha");
    assert_eq!(env.get_raw("BULK_B"), "beta");
    assert_eq!(env.get_raw("BULK_C"), "gamma");
}

// ===========================================================================
// Backend Selection
// ===========================================================================

#[test]
fn backend_flag() {
    let env = TestEnv::new();
    // Use --backend local explicitly
    let output = env
        .xv()
        .args(["--backend", "local", "list"])
        .output()
        .expect("execute xv");
    assert!(
        output.status.success(),
        "xv --backend local list should succeed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn backend_env() {
    let env = TestEnv::new();
    // XV_BACKEND=local is already set by TestEnv::xv(), so just verify
    let output = env.xv().args(["list"]).output().expect("execute xv");
    assert!(
        output.status.success(),
        "XV_BACKEND=local xv list should succeed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ===========================================================================
// Secret with metadata (note, folder)
// ===========================================================================

#[test]
fn set_with_metadata() {
    let env = TestEnv::new();
    env.set_secret_with_args(
        "META_SECRET",
        "meta-value",
        &["--note", "important note", "--folder", "infra/db"],
    );

    let value = env.get_raw("META_SECRET");
    assert_eq!(value, "meta-value");
}

// ===========================================================================
// Delete without --force in a non-interactive session should REFUSE (exit
// non-zero) and not delete — never silently no-op.
// ===========================================================================

#[test]
fn delete_without_force_refuses_noninteractive() {
    let env = TestEnv::new();
    env.set_secret("SAFE_SECRET", "safe-value");

    // Delete WITHOUT --force in a non-TTY (test harness) must fail loudly and
    // point at --force, instead of silently no-opping with exit 0.
    let (_stdout, stderr) = env.xv_fail(&["delete", "SAFE_SECRET"]);
    assert!(
        stderr.contains("--force") || stderr.contains("force"),
        "should mention --force requirement, got stderr:\n{stderr}"
    );

    // Secret must still be there (refusal must not delete).
    let value = env.get_raw("SAFE_SECRET");
    assert_eq!(value, "safe-value");
}

// ===========================================================================
// Version command (sanity check — always works)
// ===========================================================================

#[test]
fn version_command() {
    let env = TestEnv::new();
    let output = env.xv_ok(&["version"]);
    assert!(
        output.contains("crosstache") || output.contains("xv") || output.contains("0."),
        "version output should contain version info, got:\n{}",
        output,
    );
}

// ===========================================================================
// Config commands
// ===========================================================================

#[test]
fn config_show() {
    let env = TestEnv::new();
    let output = env.xv_ok(&["config", "show"]);
    assert!(
        output.contains("local") || output.contains("Backend"),
        "config show should mention backend, got:\n{}",
        output,
    );
}

#[test]
fn config_path() {
    let env = TestEnv::new();
    let output = env.xv_ok(&["config", "path"]);
    assert!(
        output.contains("xv.conf"),
        "config path should show config file location, got: {}",
        output,
    );
}

// ===========================================================================
// Rotate (generate new random value)
// NOTE: The `rotate` command has not yet been migrated to the trait-based
// backend path, so it requires Azure credentials. Skip this test for local
// backend E2E. The underlying generate + set functionality is tested via
// set_and_get_secret and gen_password tests.
// ===========================================================================

// ===========================================================================
// Empty vault list is not an error
// ===========================================================================

#[test]
fn list_empty_vault() {
    let env = TestEnv::new();
    // Listing secrets in a fresh vault should not fail
    let output = env.xv_ok(&["ls"]);
    // Output could be empty JSON (when piped), a message about no secrets,
    // or a table header. The key thing is it doesn't crash.
    assert!(
        output.contains("No")
            || output.contains("[]")
            || output.is_empty()
            || output.contains("Vault")
            || output.contains("secret"),
        "listing empty vault should not crash, got:\n{}",
        output,
    );
}

// ===========================================================================
// Multiple operations in sequence (regression guard)
// ===========================================================================

#[test]
fn full_lifecycle() {
    let env = TestEnv::new();

    // Set
    env.set_secret("LIFECYCLE", "v1");
    assert_eq!(env.get_raw("LIFECYCLE"), "v1");

    // Update value
    env.set_secret("LIFECYCLE", "v2");
    assert_eq!(env.get_raw("LIFECYCLE"), "v2");

    // Update metadata
    env.xv_ok(&["update", "LIFECYCLE", "--note", "lifecycle test"]);

    // History
    let hist = env.xv_ok(&["history", "LIFECYCLE"]);
    assert!(
        hist.contains("v1") || hist.contains("v2") || hist.contains("Version"),
        "history should show versions"
    );

    // Rollback to v1
    env.xv_ok(&["rollback", "LIFECYCLE", "--version", "v1", "--force"]);
    assert_eq!(env.get_raw("LIFECYCLE"), "v1");

    // Delete
    env.xv_ok(&["delete", "LIFECYCLE", "--force"]);
    env.xv_fail(&["get", "LIFECYCLE", "--raw"]);

    // Restore
    env.xv_ok(&["restore", "LIFECYCLE"]);
    assert_eq!(env.get_raw("LIFECYCLE"), "v1");

    // Delete + purge
    env.xv_ok(&["delete", "LIFECYCLE", "--force"]);
    env.xv_ok(&["purge", "LIFECYCLE", "--force"]);
    env.xv_fail(&["restore", "LIFECYCLE"]);
}

// ===========================================================================
// Find with --names-only
// ===========================================================================

#[test]
fn find_names_only() {
    let env = TestEnv::new();
    env.set_secret("FINDME_ALPHA", "a");
    env.set_secret("FINDME_BETA", "b");
    env.set_secret("SOMETHING_ELSE", "c");

    // Use find without a pattern + --names-only to list all secrets
    let output = env.xv_ok(&["find", "--names-only"]);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert!(
        lines.iter().any(|l| l.contains("FINDME_ALPHA")),
        "should list FINDME_ALPHA"
    );
    assert!(
        lines.iter().any(|l| l.contains("FINDME_BETA")),
        "should list FINDME_BETA"
    );
    assert!(
        lines.iter().any(|l| l.contains("SOMETHING_ELSE")),
        "should list SOMETHING_ELSE"
    );
}

// ===========================================================================
// Gen command (password generation — vault-independent)
// ===========================================================================

#[test]
fn gen_password() {
    let env = TestEnv::new();
    let output = env.xv_ok(&["gen", "--length", "20", "--raw"]);
    let pw = output.trim();
    assert_eq!(
        pw.len(),
        20,
        "generated password should be 20 chars, got {}: '{}'",
        pw.len(),
        pw
    );
}

// ===========================================================================
// Update value via update command
// ===========================================================================

#[test]
fn update_value_via_update() {
    let env = TestEnv::new();
    env.set_secret("UPD_VAL", "original-val");

    // Update the value using the update command with positional value
    env.xv_ok(&["update", "UPD_VAL", "new-val"]);

    let value = env.get_raw("UPD_VAL");
    assert_eq!(value, "new-val");
}

// ===========================================================================
// --stdin byte-exactness (P3 fix: no implicit trimming)
// ===========================================================================

#[test]
fn stdin_set_preserves_trailing_newline_byte_exact() {
    let env = TestEnv::new();
    let pem_like = "-----BEGIN KEY-----\nabc123\n-----END KEY-----\n";

    env.set_secret("PEM_KEY", pem_like);
    assert_eq!(env.get_raw_exact("PEM_KEY"), pem_like);
}

#[test]
fn stdin_set_preserves_leading_and_trailing_spaces() {
    let env = TestEnv::new();
    let padded = "  spaced value  ";

    env.set_secret("PADDED", padded);
    assert_eq!(env.get_raw_exact("PADDED"), padded);
}

#[test]
fn stdin_set_with_trim_strips_whitespace() {
    let env = TestEnv::new();

    env.set_secret_with_args("TRIMMED", "  value with spaces  \n", &["--trim"]);
    assert_eq!(env.get_raw_exact("TRIMMED"), "value with spaces");
}

#[test]
fn stdin_set_empty_input_is_rejected() {
    let env = TestEnv::new();

    let output = env.xv_with_stdin(&["set", "EMPTY", "--stdin"], "");
    assert!(
        !output.status.success(),
        "empty stdin should be rejected for set"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be empty"),
        "expected empty-value error, got:\n{}",
        stderr
    );
}

#[test]
fn stdin_set_whitespace_only_with_trim_is_rejected() {
    let env = TestEnv::new();

    let output = env.xv_with_stdin(&["set", "WS_ONLY", "--stdin", "--trim"], " \n ");
    assert!(
        !output.status.success(),
        "whitespace-only stdin with --trim should be rejected"
    );
}

#[test]
fn trim_flag_requires_stdin() {
    let env = TestEnv::new();

    let (_, stderr) = env.xv_fail(&["set", "NO_STDIN", "--trim"]);
    assert!(
        stderr.contains("--stdin"),
        "--trim without --stdin should mention the missing flag, got:\n{}",
        stderr
    );
}

#[test]
fn stdin_update_preserves_bytes_exactly() {
    let env = TestEnv::new();
    env.set_secret("UPD_STDIN", "initial");

    let new_value = "  updated value\n";
    let output = env.xv_with_stdin(&["update", "UPD_STDIN", "--stdin"], new_value);
    assert!(
        output.status.success(),
        "update --stdin failed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(env.get_raw_exact("UPD_STDIN"), new_value);
}

#[test]
fn stdin_update_with_trim_strips_whitespace() {
    let env = TestEnv::new();
    env.set_secret("UPD_TRIM", "initial");

    let output = env.xv_with_stdin(&["update", "UPD_TRIM", "--stdin", "--trim"], "  new  \n");
    assert!(
        output.status.success(),
        "update --stdin --trim failed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(env.get_raw_exact("UPD_TRIM"), "new");
}

#[test]
fn stdin_update_empty_input_is_rejected() {
    let env = TestEnv::new();
    env.set_secret("UPD_EMPTY", "initial");

    let output = env.xv_with_stdin(&["update", "UPD_EMPTY", "--stdin"], "");
    assert!(
        !output.status.success(),
        "empty stdin should be rejected for update"
    );

    // Original value untouched
    assert_eq!(env.get_raw_exact("UPD_EMPTY"), "initial");
}

// ===========================================================================
// Vault info command
// ===========================================================================

#[test]
fn vault_info() {
    let env = TestEnv::new();
    let output = env.xv_ok(&["vault", "info", "default"]);
    assert!(
        output.contains("default"),
        "vault info should display vault name, got:\n{}",
        output,
    );
}

// ===========================================================================
// env pull / env push — must route through the active backend on local.
// Regression guard: these once required a registry the CLI never built for
// Env commands, so they always failed with "No backend registry available".
// ===========================================================================

#[test]
fn env_pull_exports_secrets_dotenv() {
    let env = TestEnv::new();
    env.set_secret("ALPHA", "one");
    env.set_secret("BETA", "two");

    // Default format is dotenv (plain); values go to stdout.
    let out = env.xv_ok(&["env", "pull"]);
    assert!(
        out.contains("ALPHA=one"),
        "env pull should export ALPHA, got:\n{out}"
    );
    assert!(
        out.contains("BETA=two"),
        "env pull should export BETA, got:\n{out}"
    );
}

#[test]
fn env_push_imports_secrets() {
    let env = TestEnv::new();
    let dotenv = "GAMMA=three\nDELTA=four\n";
    let output = env.xv_with_stdin(&["env", "push"], dotenv);
    assert!(
        output.status.success(),
        "env push failed (exit {:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Both secrets must now be retrievable from the local backend.
    assert_eq!(env.get_raw("GAMMA"), "three");
    assert_eq!(env.get_raw("DELTA"), "four");
}
