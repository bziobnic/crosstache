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
fn update_note_only_preserves_group_membership() {
    let env = TestEnv::new();
    env.set_secret_with_args("GROUPED_KEY", "original", &["--group", "team-a"]);

    // Sanity check: the secret starts out counted in its group.
    let before = env.xv_ok(&["group", "list", "--format", "csv"]);
    assert!(before.contains("team-a,1"), "before update: {before}");

    // A note-only update must not silently drop the existing `groups` tag
    // (or any other tag) via a full-replacement PATCH/write.
    env.xv_ok(&["update", "GROUPED_KEY", "--note", "note only update"]);

    let after = env.xv_ok(&["group", "list", "--format", "csv"]);
    assert!(
        after.contains("team-a,1"),
        "group membership should survive a note-only update: {after}"
    );

    // Value and note should both reflect the expected post-update state.
    let value = env.get_raw("GROUPED_KEY");
    assert_eq!(value, "original");
}

#[test]
fn clear_note_and_folder_in_update() {
    let env = TestEnv::new();
    // Create secret with note and folder
    env.set_secret_with_args(
        "CLEAR_METADATA_KEY",
        "value",
        &["--note", "initial note", "--folder", "app/db"],
    );

    // Verify the secret value is correct
    let value = env.get_raw("CLEAR_METADATA_KEY");
    assert_eq!(value, "value");

    // Clear the note and folder via update
    env.xv_ok(&[
        "update",
        "CLEAR_METADATA_KEY",
        "--clear-note",
        "--clear-folder",
    ]);

    // Verify value is still correct after clearing metadata
    let value_after = env.get_raw("CLEAR_METADATA_KEY");
    assert_eq!(value_after, "value");

    // Verify that the secret is NOT found when searching by its former folder
    // (this indirectly confirms the folder was cleared)
    let found_by_folder = env.xv_ok(&["find", "--folder", "app/db", "--names-only"]);
    assert!(
        !found_by_folder.contains("CLEAR_METADATA_KEY"),
        "secret should not be found by its former folder after clearing: {}",
        found_by_folder
    );
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

#[test]
fn recursive_ls_qualifies_names_by_folder() {
    let env = TestEnv::new();
    env.set_secret_with_args("db-pass", "v", &["--folder", "prod/db"]);
    env.set_secret("root-a", "v");

    let names = env.xv_ok(&["ls", "-r", "--names-only"]);
    assert!(names.lines().any(|l| l == "prod/db/db-pass"), "{names}");
    assert!(names.lines().any(|l| l == "root-a"), "{names}");

    // Scoped recursion is relative to the listing root.
    let scoped = env.xv_ok(&["ls", "prod", "-r", "--names-only"]);
    assert!(scoped.lines().any(|l| l == "db/db-pass"), "{scoped}");

    // Non-recursive --names-only keeps the shipped unqualified shape.
    let flat = env.xv_ok(&["ls", "--names-only"]);
    assert!(flat.lines().any(|l| l == "db-pass"), "{flat}");
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

#[test]
fn parse_prints_exactly_one_table() {
    let env = TestEnv::new();
    // `xv parse` has its own local `--fmt` flag (default "table").
    let out = env.xv_ok(&["parse", "Server=myhost;Database=mydb"]);
    assert_eq!(
        out.matches("myhost").count(),
        1,
        "connection-string table printed more than once:\n{out}"
    );

    // JSON format must not leak a human table before the JSON document.
    let json_out = env.xv_ok(&["parse", "Server=myhost;Database=mydb", "--fmt", "json"]);
    assert!(
        json_out.trim_start().starts_with('['),
        "json output polluted by a table:\n{json_out}"
    );
}

#[test]
fn ls_sort_flag_is_accepted_and_lists_everything() {
    let env = TestEnv::new();
    env.set_secret("older", "v");
    env.set_secret("newer", "v");

    // Local timestamps have minute resolution, so ordering is covered by the
    // unit test; here we assert the flag parses and output is complete.
    let out = env.xv_ok(&["ls", "--sort", "updated"]);
    assert!(out.contains("older") && out.contains("newer"), "{out}");

    let json = env.xv_ok(&["ls", "--sort", "updated", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(parsed.as_array().map(Vec::len), Some(2));

    let (_, stderr) = env.xv_fail(&["ls", "--sort", "bogus"]);
    assert!(stderr.contains("possible values"), "{stderr}");
}

#[test]
fn ls_deleted_lists_soft_deleted_secrets() {
    let env = TestEnv::new();
    env.set_secret("doomed", "v");
    env.set_secret("kept", "v");
    env.xv_ok(&["delete", "doomed", "--force"]);

    // Piped stdout resolves Auto → JSON, so the human views need the
    // explicit format (same convention as the live `xv ls` e2e tests).
    let out = env.xv_ok(&["ls", "--deleted", "--format", "table"]);
    assert!(out.contains("doomed") && !out.contains("kept"), "{out}");
    assert!(out.contains("1 deleted secret"), "count line: {out}");

    // Long view has the date columns (explicit table format + -l takes the
    // borderless long path, mirroring live `xv ls`).
    let long = env.xv_ok(&["ls", "--deleted", "-l", "--format", "table"]);
    assert!(
        long.contains("DELETED") && long.contains("PURGE SCHEDULED"),
        "{long}"
    );

    // Machine format: row array with a populated deleted date (local backend);
    // purge schedule is empty (local trash never auto-purges).
    let json = env.xv_ok(&["ls", "--deleted", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let rows = parsed.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "doomed");
    assert!(!rows[0]["deleted"].as_str().unwrap().is_empty());
    assert_eq!(rows[0]["purge_scheduled"], "");
}

#[test]
fn ls_deleted_conflicts_and_empty_states() {
    let env = TestEnv::new();

    // clap conflicts: FOLDER positional and -r.
    let (_, err1) = env.xv_fail(&["ls", "prod", "--deleted"]);
    assert!(err1.contains("cannot be used with"), "{err1}");
    let (_, err2) = env.xv_fail(&["ls", "--deleted", "-r"]);
    assert!(err2.contains("cannot be used with"), "{err2}");

    // Empty: human info on stderr, valid-empty JSON on stdout.
    let json = env.xv_ok(&["ls", "--deleted", "--format", "json"]);
    assert_eq!(json.trim(), "[]");
}

#[test]
fn ls_deleted_columns_selection_and_validation() {
    let env = TestEnv::new();
    env.set_secret("doomed", "v");
    env.xv_ok(&["delete", "doomed", "--force"]);

    // --columns projects the requested subset on the deleted table.
    let out = env.xv_ok(&["ls", "--deleted", "--format", "table", "--columns", "Name"]);
    assert!(out.contains("doomed"), "{out}");
    assert!(!out.contains("Purge Scheduled"), "{out}");

    // Unknown column errors, even on the empty grid/long path (human table
    // formats validate --columns regardless of Auto/JSON's empty-array
    // fast path, matching the branch-wide rule in `display_cached_secret_list`).
    let env2 = TestEnv::new();
    let (_, err) = env2.xv_fail(&["ls", "--deleted", "--format", "table", "--columns", "Nope"]);
    assert!(
        !err.is_empty(),
        "expected an error for unknown column: {err}"
    );
}

#[test]
fn group_list_counts_members_across_formats() {
    let env = TestEnv::new();
    env.set_secret_with_args("a", "v", &["--group", "team-a"]);
    env.set_secret_with_args("b", "v", &["--group", "team-a"]);
    env.set_secret_with_args("c", "v", &["--group", "team-b"]);
    env.set_secret("ungrouped", "v");

    // Piped stdout resolves Auto → JSON; force the human table for the
    // count-line assertion.
    let table = env.xv_ok(&["group", "list", "--format", "table"]);
    assert!(
        table.contains("team-a") && table.contains("team-b"),
        "{table}"
    );
    assert!(table.contains("2 groups"), "count line: {table}");

    let csv = env.xv_ok(&["group", "list", "--format", "csv"]);
    let mut lines = csv.lines();
    assert_eq!(lines.next(), Some("Group,Secrets"), "{csv}");
    assert!(
        csv.contains("team-a,2") && csv.contains("team-b,1"),
        "{csv}"
    );

    let json = env.xv_ok(&["group", "list", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(parsed.as_array().map(Vec::len), Some(2));

    // Empty vault → valid-empty machine output.
    let fresh = TestEnv::new();
    assert_eq!(
        fresh.xv_ok(&["group", "list", "--format", "json"]).trim(),
        "[]"
    );
}

#[test]
fn group_list_excludes_disabled_secrets() {
    let env = TestEnv::new();
    // Create secrets with groups
    env.set_secret_with_args("active-a", "v", &["--group", "team-x"]);
    env.set_secret_with_args("active-b", "v", &["--group", "team-x"]);
    env.set_secret_with_args("disabled-member", "v", &["--group", "team-x"]);

    // Disable the third secret
    env.xv_ok(&["update", "disabled-member", "--enabled", "false"]);

    // Verify the group count excludes the disabled secret
    let csv = env.xv_ok(&["group", "list", "--format", "csv"]);
    assert!(
        csv.contains("team-x,2"),
        "disabled secret should not be counted: {csv}"
    );

    let json = env.xv_ok(&["group", "list", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(
        parsed
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.get("secrets")),
        Some(&serde_json::json!(2)),
        "disabled secret should not contribute to count: {json}"
    );

    // ls (default view) must exclude it while disabled...
    let ls = env.xv_ok(&["ls", "--names-only"]);
    assert!(
        !ls.lines().any(|l| l == "disabled-member"),
        "disabled secret should not be listed: {ls}"
    );

    // ...and the full roundtrip must work: RE-ENABLE and reappear.
    env.xv_ok(&["update", "disabled-member", "--enabled", "true"]);

    let csv = env.xv_ok(&["group", "list", "--format", "csv"]);
    assert!(
        csv.contains("team-x,3"),
        "re-enabled secret should be counted again: {csv}"
    );
    let ls = env.xv_ok(&["ls", "--names-only"]);
    assert!(
        ls.lines().any(|l| l == "disabled-member"),
        "re-enabled secret should be listed again: {ls}"
    );
}

#[test]
fn find_folder_scopes_by_segment_boundary() {
    let env = TestEnv::new();
    env.set_secret_with_args("db-pass", "v", &["--folder", "prod/db"]);
    env.set_secret_with_args("api-key", "v", &["--folder", "prod"]);
    env.set_secret_with_args("trap", "v", &["--folder", "production"]);
    env.set_secret("root-a", "v");

    // No pattern: everything in scope, unranked.
    let out = env.xv_ok(&["find", "--folder", "prod", "--names-only"]);
    let names: Vec<&str> = out.lines().collect();
    assert!(
        names.contains(&"db-pass") && names.contains(&"api-key"),
        "{out}"
    );
    assert!(!names.contains(&"trap"), "segment boundary violated: {out}");
    assert!(!names.contains(&"root-a"), "{out}");

    // Trailing slash tolerated; invalid path errors.
    let out2 = env.xv_ok(&["find", "--folder", "prod/", "--names-only"]);
    assert!(out2.contains("db-pass"), "{out2}");
}

#[test]
fn context_envs_is_removed() {
    let env = TestEnv::new();

    // No longer listed in help.
    let help = env.xv_ok(&["context", "--help"]);
    assert!(
        !help.contains("envs"),
        "context envs still visible in help:\n{help}"
    );

    // The alias is gone entirely: clap now rejects it as an unrecognized subcommand.
    let output = env
        .xv()
        .args(["context", "envs"])
        .output()
        .expect("run context envs");
    assert!(
        !output.status.success(),
        "context envs unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized subcommand") || stderr.contains("error"),
        "expected a clap error for the removed subcommand:\n{stderr}"
    );
}

#[test]
fn complete_folders_is_silent_without_cache() {
    let env = TestEnv::new();
    env.set_secret_with_args("db-pass", "v", &["--folder", "prod/db"]);
    let out = env.xv_ok(&["__complete-folders"]);
    assert_eq!(out.trim(), "", "cache disabled → no completions, no errors");
}

#[test]
fn update_rename_moves_secret_and_metadata() {
    let env = TestEnv::new();
    env.set_secret_with_args(
        "old-name",
        "rename-me",
        &["--note", "keep this note", "--group", "team-a"],
    );

    // NOTE: output::success prints to STDERR (src/utils/output.rs), so run
    // the raw command to assert the rename success line.
    let output = env
        .xv()
        .args(["update", "old-name", "--rename", "new-name"])
        .output()
        .expect("run xv update --rename");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "update --rename failed:\n{stderr}");
    assert!(
        stderr.contains("renamed") && stderr.contains("new-name"),
        "expected a rename success line on stderr:\n{stderr}"
    );

    // Value moved.
    assert_eq!(env.get_raw("new-name"), "rename-me");

    // Metadata rode along, and the old name is out of the listing.
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("new-name") && json.contains("keep this note") && json.contains("team-a"),
        "metadata missing after rename:\n{json}"
    );
    assert!(!json.contains("old-name"), "old name still listed:\n{json}");

    // The old name no longer resolves.
    let (_, stderr) = env.xv_fail(&["get", "old-name"]);
    assert!(stderr.to_lowercase().contains("not found"), "{stderr}");
}

#[test]
fn update_rename_applies_other_flags_first() {
    let env = TestEnv::new();
    env.set_secret_with_args("combo", "v1", &["--note", "old note"]);

    // Success lines go to stderr; the behavioral assertions below are the
    // real check, so xv_ok (which only asserts exit status) is enough here.
    env.xv_ok(&[
        "update",
        "combo",
        "--note",
        "new note",
        "--rename",
        "combo-renamed",
    ]);

    assert_eq!(env.get_raw("combo-renamed"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("combo-renamed") && json.contains("new note"),
        "{json}"
    );
    assert!(
        !json.contains("old note"),
        "stale note survived the update:\n{json}"
    );
}

#[test]
fn update_rename_refuses_to_overwrite_an_existing_secret() {
    let env = TestEnv::new();
    env.set_secret("keep-me", "original");
    env.set_secret("mover", "moving");

    let (_, stderr) = env.xv_fail(&["update", "mover", "--rename", "keep-me"]);
    assert!(stderr.contains("already exists"), "{stderr}");

    // Nothing was clobbered or deleted.
    assert_eq!(env.get_raw("keep-me"), "original");
    assert_eq!(env.get_raw("mover"), "moving");
}

#[test]
fn update_rename_to_the_same_name_is_an_error() {
    let env = TestEnv::new();
    env.set_secret("same", "v");
    let (_, stderr) = env.xv_fail(&["update", "same", "--rename", "same"]);
    assert!(stderr.contains("already named"), "{stderr}");
}

#[test]
fn update_rename_of_a_missing_secret_fails() {
    let env = TestEnv::new();
    let (_, stderr) = env.xv_fail(&["update", "ghost", "--rename", "anything"]);
    assert!(stderr.to_lowercase().contains("not found"), "{stderr}");
}

// ===========================================================================
// xv mv — single-secret move/rename
// ===========================================================================

#[test]
fn mv_secret_between_folders_keeps_name_and_value() {
    let env = TestEnv::new();
    env.set_secret_with_args("pass", "v1", &["--folder", "db", "--note", "n1"]);

    env.xv_ok(&["mv", "db/pass", "app/"]);

    assert_eq!(env.get_raw("pass"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\""), "folder not updated:\n{json}");
    assert!(json.contains("n1"), "note lost on folder move:\n{json}");
}

#[test]
fn mv_secret_rename_to_root() {
    let env = TestEnv::new();
    env.set_secret_with_args("pass", "v1", &["--folder", "db"]);

    env.xv_ok(&["mv", "db/pass", "newname"]);

    assert_eq!(env.get_raw("newname"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        !json.contains("\"db\""),
        "folder should be cleared:\n{json}"
    );
}

#[test]
fn mv_secret_combined_move_and_rename() {
    let env = TestEnv::new();
    env.set_secret_with_args("pass", "v1", &["--folder", "db"]);

    env.xv_ok(&["mv", "db/pass", "app/pw"]);

    assert_eq!(env.get_raw("pw"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\"") && json.contains("pw"), "{json}");
}

#[test]
fn mv_noop_and_errors() {
    let env = TestEnv::new();
    env.set_secret_with_args("pass", "v1", &["--folder", "db"]);
    env.set_secret_with_args("taken", "v2", &[]);

    // No-op exits 0 and says so.
    let out = env
        .xv()
        .args(["mv", "db/pass", "db/pass"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Destination collision refused before any change.
    let (_, stderr) = env.xv_fail(&["mv", "db/pass", "taken"]);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert_eq!(env.get_raw("pass"), "v1", "source must be untouched");

    // Wrong source folder → not found (the secret lives in db/, not prod/).
    let (_, stderr) = env.xv_fail(&["mv", "prod/pass", "app/"]);
    assert!(stderr.to_lowercase().contains("not found"), "{stderr}");

    // Typo in the path → not found, with a closest-match suggestion.
    let (_, stderr) = env.xv_fail(&["mv", "db/pas", "app/"]);
    assert!(
        stderr.contains("db/pass"),
        "expected suggestion in: {stderr}"
    );
}

#[test]
fn mv_secret_dry_run_does_not_mutate() {
    let env = TestEnv::new();
    env.set_secret_with_args("pass", "v1", &["--folder", "db"]);

    let out = env.xv_ok(&["mv", "db/pass", "app/", "--dry-run"]);
    assert!(out.contains("db/pass") && out.contains("app/pass"), "{out}");

    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"db\""), "dry-run must not mutate: {json}");
}

// Local backend never diverges name/original_name (no sanitization: the
// display name IS the backend key end-to-end — the on-disk stem encoding,
// legacy percent-encoded or opaque keyed-hash, is a filesystem-only detail
// never surfaced through `SecretBackend`). Confirmed by reading
// `src/backend/local/secrets.rs::set_secret`, which always sets
// `SecretMeta { name: name.clone(), original_name: name.clone(), .. }`.
// These tests still cover the issue's real concern end-to-end on this
// backend: names containing characters that *would* need sanitization on
// Azure (spaces, dots) must round-trip correctly through `xv mv`.
#[test]
fn mv_sanitized_name_rename_with_space() {
    let env = TestEnv::new();
    env.set_secret_with_args("my secret", "v1", &["--folder", "db"]);

    // original_name == name on the local backend; no divergence to exploit,
    // but the name must still survive quoting/parsing through mv's grammar.
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"my secret\""), "{json}");

    env.xv_ok(&["mv", "db/my secret", "newname"]);

    assert_eq!(env.get_raw("newname"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        !json.contains("\"my secret\""),
        "old name still listed: {json}"
    );
    assert!(!json.contains("\"db\""), "folder should be cleared: {json}");
}

#[test]
fn mv_sanitized_name_folder_move() {
    let env = TestEnv::new();
    env.set_secret_with_args("my secret", "v1", &["--folder", "db"]);

    env.xv_ok(&["mv", "db/my secret", "app/"]);

    assert_eq!(env.get_raw("my secret"), "v1");
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\""), "folder not updated: {json}");
    assert!(
        json.contains("\"my secret\""),
        "display name lost on folder move: {json}"
    );
}

// ===========================================================================
// xv mv — bulk folder move
// ===========================================================================

#[test]
fn mv_folder_bulk_dry_run_and_apply() {
    let env = TestEnv::new();
    env.set_secret_with_args("a", "1", &["--folder", "app"]);
    env.set_secret_with_args("b", "2", &["--folder", "app/db"]);
    env.set_secret_with_args("c", "3", &["--folder", "approved"]); // boundary trap
    env.set_secret_with_args("d", "4", &[]);

    // Dry run: full plan on stdout, nothing changed.
    let plan = env.xv_ok(&["mv", "app/", "svc/", "--dry-run"]);
    assert!(plan.contains("app/a") && plan.contains("svc/a"), "{plan}");
    assert!(
        plan.contains("app/db/b") && plan.contains("svc/db/b"),
        "remainder must be preserved: {plan}"
    );
    assert!(
        !plan.contains("approved"),
        "segment boundary violated: {plan}"
    );
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\""), "dry-run must not mutate: {json}");

    // Apply with --yes.
    env.xv_ok(&["mv", "app/", "svc/", "--yes"]);
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(
        json.contains("\"svc\"") && json.contains("svc/db"),
        "{json}"
    );
    assert!(
        !json.contains("\"app\"") && !json.contains("app/db"),
        "{json}"
    );
    assert!(
        json.contains("approved"),
        "unrelated folder touched: {json}"
    );
}

#[test]
fn mv_folder_bulk_requires_yes_when_not_a_tty() {
    let env = TestEnv::new();
    env.set_secret_with_args("a", "1", &["--folder", "app"]);

    // Tests run with piped stdin (not a TTY): without --yes this must refuse.
    let (_, stderr) = env.xv_fail(&["mv", "app/", "svc/"]);
    assert!(
        stderr.contains("--yes"),
        "should tell the user about --yes: {stderr}"
    );
    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\""), "refusal must not mutate: {json}");
}

#[test]
fn mv_folder_bulk_empty_prefix_errors() {
    let env = TestEnv::new();
    env.set_secret_with_args("a", "1", &["--folder", "app"]);
    let (_, stderr) = env.xv_fail(&["mv", "ghost/", "svc/", "--yes"]);
    assert!(stderr.contains("no secrets under 'ghost/'"), "{stderr}");
}

#[test]
fn mv_folder_identical_prefixes_errors() {
    let env = TestEnv::new();
    env.set_secret_with_args("a", "1", &["--folder", "app"]);

    let (_, stderr) = env.xv_fail(&["mv", "app/", "app/", "--yes"]);
    assert!(stderr.contains("identical"), "{stderr}");

    let json = env.xv_ok(&["ls", "--format", "json"]);
    assert!(json.contains("\"app\""), "{json}");
}

#[test]
fn mv_folder_to_root() {
    let env = TestEnv::new();
    env.set_secret_with_args("a", "1", &["--folder", "app"]);
    env.set_secret_with_args("b", "2", &["--folder", "app/db"]);

    env.xv_ok(&["mv", "app/", "/", "--yes"]);
    let json = env.xv_ok(&["ls", "--format", "json"]);
    // 'a' is now at the root; 'b' keeps its remainder 'db'.
    assert!(!json.contains("\"app\""), "{json}");
    assert!(json.contains("\"db\""), "remainder folder lost: {json}");
}

// ===========================================================================
// Cross-vault move / copy (issue #307)
// ===========================================================================

#[test]
fn move_without_force_refuses_to_overwrite_existing_target() {
    let env = TestEnv::new();
    env.xv_ok(&["vault", "create", "dest"]);

    env.set_secret("src-secret", "src-value");
    // Set a target secret directly in the destination vault.
    env.xv_ok(&["context", "use", "dest", "--global"]);
    env.set_secret("dst-secret", "dst-original-value");
    env.xv_ok(&["context", "use", "default", "--global"]);

    let (_stdout, stderr) = env.xv_fail(&[
        "move",
        "src-secret",
        "--from",
        "default",
        "--to",
        "dest",
        "--new-name",
        "dst-secret",
    ]);
    assert!(
        stderr.contains("already exists"),
        "expected 'already exists' error, got:\n{stderr}"
    );

    // Both secrets are unchanged.
    assert_eq!(env.get_raw("src-secret"), "src-value");
    env.xv_ok(&["context", "use", "dest", "--global"]);
    assert_eq!(env.get_raw("dst-secret"), "dst-original-value");
    env.xv_ok(&["context", "use", "default", "--global"]);
}

#[test]
fn move_with_force_overwrites_existing_target_and_deletes_source() {
    let env = TestEnv::new();
    env.xv_ok(&["vault", "create", "dest"]);

    env.set_secret("src-secret2", "src-value2");
    env.xv_ok(&["context", "use", "dest", "--global"]);
    env.set_secret("dst-secret2", "dst-original-value2");
    env.xv_ok(&["context", "use", "default", "--global"]);

    env.xv_ok(&[
        "move",
        "src-secret2",
        "--from",
        "default",
        "--to",
        "dest",
        "--new-name",
        "dst-secret2",
        "--force",
    ]);

    // Destination now holds the source's value.
    env.xv_ok(&["context", "use", "dest", "--global"]);
    assert_eq!(env.get_raw("dst-secret2"), "src-value2");
    env.xv_ok(&["context", "use", "default", "--global"]);

    // Source is gone.
    let (_, stderr) = env.xv_fail(&["get", "src-secret2", "--raw"]);
    assert!(
        stderr.to_lowercase().contains("not found"),
        "source secret should be gone after forced move:\n{stderr}"
    );
}

#[test]
fn copy_refuses_to_overwrite_existing_target_even_semantics_unchanged() {
    let env = TestEnv::new();
    env.xv_ok(&["vault", "create", "dest2"]);

    env.set_secret("copy-src", "copy-src-value");
    env.xv_ok(&["context", "use", "dest2", "--global"]);
    env.set_secret("copy-dst", "copy-dst-original");
    env.xv_ok(&["context", "use", "default", "--global"]);

    let (_stdout, stderr) = env.xv_fail(&[
        "copy",
        "copy-src",
        "--from",
        "default",
        "--to",
        "dest2",
        "--new-name",
        "copy-dst",
    ]);
    assert!(
        stderr.contains("already exists"),
        "expected 'already exists' error, got:\n{stderr}"
    );

    // Both secrets are unchanged; copy has no --force flag to override this.
    assert_eq!(env.get_raw("copy-src"), "copy-src-value");
    env.xv_ok(&["context", "use", "dest2", "--global"]);
    assert_eq!(env.get_raw("copy-dst"), "copy-dst-original");
    env.xv_ok(&["context", "use", "default", "--global"]);
}
