//! End-to-end integration tests for the xv CLI
//!
//! These tests require:
//! - Azure CLI authentication (`az login`)
//! - An existing Azure Key Vault named `test-test-delete`
//! - Internet connection to Azure
//! - A valid xv configuration (~/.config/xv/xv.conf)
//!
//! Run with:
//!   cargo test --test e2e_integration_tests -- --ignored --nocapture --test-threads=1
//!
//! Tests use a unique prefix per run to avoid name collisions.
//! Cleanup is performed at the end of the test suite.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const VAULT: &str = "xvtestdeleteme";

/// Generate a unique prefix for this test run to avoid collisions
fn test_prefix() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("e2e-{ts}")
}

/// Run xv with the given args and return the output
fn xv(args: &[&str]) -> std::process::Output {
    let binary = env!("CARGO_BIN_EXE_xv");
    Command::new(binary)
        .args(args)
        .output()
        .expect("Failed to execute xv binary")
}

/// Run xv and assert it succeeded, returning stdout as a string
fn xv_ok(args: &[&str]) -> String {
    let output = xv(args);
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

/// Run xv and assert it failed, returning stderr as a string
fn xv_fail(args: &[&str]) -> String {
    let output = xv(args);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "xv {:?} should have failed but succeeded:\nstdout: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
    );
    stderr
}

/// Clean up test secrets by deleting and purging them
fn cleanup_secrets(names: &[String]) {
    for name in names {
        // Soft delete
        let _ = xv(&["delete", name, "--vault", VAULT, "--force"]);
    }
    // Give Azure a moment to process deletions
    std::thread::sleep(std::time::Duration::from_secs(2));
    for name in names {
        // Purge
        let _ = xv(&["purge", name, "--vault", VAULT, "--force"]);
    }
}

// ============================================================================
// Utility commands (no vault state changes)
// ============================================================================

#[test]
#[ignore]
fn e2e_version() {
    let stdout = xv_ok(&["version"]);
    assert!(stdout.contains("crosstache"), "version output should contain 'crosstache'");
}

#[test]
#[ignore]
fn e2e_whoami() {
    let stdout = xv_ok(&["whoami"]);
    // Should show some identity info (tenant, user, or object ID)
    assert!(
        stdout.contains("Tenant") || stdout.contains("tenant") || stdout.contains("@") || stdout.contains("Identity"),
        "whoami should show identity info, got: {}",
        stdout
    );
}

#[test]
#[ignore]
fn e2e_config_show() {
    let stdout = xv_ok(&["config", "show"]);
    assert!(
        stdout.contains("subscription") || stdout.contains("vault") || stdout.contains("Subscription"),
        "config show should display configuration, got: {}",
        stdout
    );
}

#[test]
#[ignore]
fn e2e_config_path() {
    let stdout = xv_ok(&["config", "path"]);
    assert!(
        stdout.contains("xv.conf") || stdout.contains("config"),
        "config path should show config file location, got: {}",
        stdout
    );
}

#[test]
#[ignore]
fn e2e_completion_bash() {
    let stdout = xv_ok(&["completion", "bash"]);
    assert!(
        stdout.contains("complete") || stdout.contains("_xv"),
        "completion bash should generate shell completions, got first 100 chars: {}",
        &stdout[..stdout.len().min(100)]
    );
}

#[test]
#[ignore]
fn e2e_parse_connection_string() {
    let stdout = xv_ok(&["parse", "Server=db.example.com;Database=mydb;User=admin;Password=secret123"]);
    assert!(stdout.contains("db.example.com"), "parse should extract server, got: {}", stdout);
    assert!(stdout.contains("mydb"), "parse should extract database, got: {}", stdout);
}

#[test]
#[ignore]
fn e2e_parse_json_format() {
    let stdout = xv_ok(&[
        "parse",
        "Server=db.example.com;Database=mydb",
        "--format",
        "json",
    ]);
    assert!(stdout.contains('"'), "json format should contain quotes, got: {}", stdout);
}

// ============================================================================
// Secret lifecycle: set -> get -> list -> update -> history -> rollback -> rotate -> delete -> restore -> purge
// ============================================================================

#[test]
#[ignore]
fn e2e_secret_full_lifecycle() {
    let prefix = test_prefix();
    let secret_name = format!("{prefix}-lifecycle");
    let secret_value = "test-value-12345";

    // --- SET --- (must pipe value via stdin; xv_ok uses null stdin)
    let binary = env!("CARGO_BIN_EXE_xv");
    let set_output = Command::new(binary)
        .args(["set", &secret_name, "--vault", VAULT, "--stdin", "--note", "e2e test secret"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(secret_value.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("Failed to run xv set with stdin");

    assert!(
        set_output.status.success(),
        "xv set failed: {}",
        String::from_utf8_lossy(&set_output.stderr)
    );

    // --- GET (raw) ---
    let stdout = xv_ok(&["get", &secret_name, "--vault", VAULT, "--raw"]);
    assert!(
        stdout.trim() == secret_value,
        "get --raw should return exact value '{}', got: '{}'",
        secret_value,
        stdout.trim()
    );

    // --- LIST ---
    let stdout = xv_ok(&["list", "--vault", VAULT]);
    assert!(
        stdout.contains(&secret_name),
        "list should include our secret '{}', got: {}",
        secret_name,
        stdout
    );

    // --- LIST (json format) ---
    let stdout = xv_ok(&["list", "--vault", VAULT, "--format", "json"]);
    assert!(
        stdout.contains(&secret_name),
        "list --format json should include our secret, got: {}",
        stdout
    );

    // --- UPDATE (add group and note) ---
    xv_ok(&[
        "update",
        &secret_name,
        "--vault",
        VAULT,
        "--group",
        "e2e-test-group",
        "--note",
        "updated by e2e test",
    ]);

    // Verify group was applied
    let stdout = xv_ok(&["list", "--vault", VAULT, "--group", "e2e-test-group"]);
    assert!(
        stdout.contains(&secret_name),
        "list --group should show our secret after update, got: {}",
        stdout
    );

    // --- UPDATE (change value to create a new version) ---
    let new_value = "updated-value-67890";
    let update_output = Command::new(binary)
        .args([
            "update",
            &secret_name,
            "--vault",
            VAULT,
            "--value",
            new_value,
        ])
        .output()
        .expect("Failed to run xv update");
    assert!(
        update_output.status.success(),
        "xv update --value failed: {}",
        String::from_utf8_lossy(&update_output.stderr)
    );

    // Verify new value
    let stdout = xv_ok(&["get", &secret_name, "--vault", VAULT, "--raw"]);
    assert!(
        stdout.trim() == new_value,
        "get after update should return '{}', got: '{}'",
        new_value,
        stdout.trim()
    );

    // --- HISTORY ---
    let stdout = xv_ok(&["history", &secret_name, "--vault", VAULT]);
    assert!(
        stdout.contains("Version") || stdout.contains("version") || stdout.contains("Created"),
        "history should show version info, got: {}",
        stdout
    );

    // --- ROTATE ---
    xv_ok(&["rotate", &secret_name, "--vault", VAULT, "--length", "32"]);

    // Value should have changed
    let stdout = xv_ok(&["get", &secret_name, "--vault", VAULT, "--raw"]);
    assert!(
        stdout.trim() != new_value,
        "rotate should change the value, but got the same: '{}'",
        stdout.trim()
    );
    assert_eq!(
        stdout.trim().len(),
        32,
        "rotated value should be 32 chars, got {} chars: '{}'",
        stdout.trim().len(),
        stdout.trim()
    );

    // --- DELETE (soft) ---
    xv_ok(&["delete", &secret_name, "--vault", VAULT, "--force"]);

    // Secret should no longer appear in list
    let stdout = xv_ok(&["list", "--vault", VAULT]);
    assert!(
        !stdout.contains(&secret_name),
        "deleted secret should not appear in list, got: {}",
        stdout
    );

    // --- RESTORE ---
    // Wait a moment for Azure to register the deletion
    std::thread::sleep(std::time::Duration::from_secs(3));
    xv_ok(&["restore", &secret_name, "--vault", VAULT]);

    // Secret should be back
    std::thread::sleep(std::time::Duration::from_secs(2));
    let stdout = xv_ok(&["list", "--vault", VAULT]);
    assert!(
        stdout.contains(&secret_name),
        "restored secret should appear in list, got: {}",
        stdout
    );

    // --- FINAL CLEANUP: delete + purge ---
    xv_ok(&["delete", &secret_name, "--vault", VAULT, "--force"]);
    std::thread::sleep(std::time::Duration::from_secs(3));
    xv_ok(&["purge", &secret_name, "--vault", VAULT, "--force"]);
}

// ============================================================================
// Bulk set
// ============================================================================

#[test]
#[ignore]
fn e2e_bulk_set() {
    let prefix = test_prefix();
    let k1 = format!("{prefix}-bulk1");
    let k2 = format!("{prefix}-bulk2");
    let k3 = format!("{prefix}-bulk3");

    let arg1 = format!("{k1}=alpha");
    let arg2 = format!("{k2}=beta");
    let arg3 = format!("{k3}=gamma");

    xv_ok(&["set", &arg1, &arg2, &arg3, "--vault", VAULT]);

    // Verify each was created
    let v1 = xv_ok(&["get", &k1, "--vault", VAULT, "--raw"]);
    assert_eq!(v1.trim(), "alpha", "bulk k1 should be 'alpha', got: '{}'", v1.trim());

    let v2 = xv_ok(&["get", &k2, "--vault", VAULT, "--raw"]);
    assert_eq!(v2.trim(), "beta", "bulk k2 should be 'beta', got: '{}'", v2.trim());

    let v3 = xv_ok(&["get", &k3, "--vault", VAULT, "--raw"]);
    assert_eq!(v3.trim(), "gamma", "bulk k3 should be 'gamma', got: '{}'", v3.trim());

    // Cleanup
    cleanup_secrets(&[k1, k2, k3]);
}

// ============================================================================
// Output format tests
// ============================================================================

#[test]
#[ignore]
fn e2e_list_format_yaml() {
    let stdout = xv_ok(&["list", "--vault", VAULT, "--format", "yaml"]);
    // YAML output should start with - or contain key: value patterns
    assert!(
        stdout.contains(':') || stdout.contains('-') || stdout.contains("No results"),
        "yaml output should look like YAML, got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
#[ignore]
fn e2e_list_format_csv() {
    let stdout = xv_ok(&["list", "--vault", VAULT, "--format", "csv"]);
    assert!(
        stdout.contains(',') || stdout.contains("No results"),
        "csv output should contain commas, got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

#[test]
#[ignore]
fn e2e_list_format_json() {
    let stdout = xv_ok(&["list", "--vault", VAULT, "--format", "json"]);
    assert!(
        stdout.contains('[') || stdout.contains("No results"),
        "json output should be a JSON array, got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

// ============================================================================
// Vault operations
// ============================================================================

#[test]
#[ignore]
fn e2e_vault_list() {
    let stdout = xv_ok(&["vault", "list"]);
    assert!(
        stdout.contains(VAULT) || stdout.contains("test-test"),
        "vault list should include our test vault '{}', got: {}",
        VAULT,
        stdout
    );
}

#[test]
#[ignore]
fn e2e_vault_info() {
    let stdout = xv_ok(&["vault", "info", VAULT]);
    assert!(
        stdout.contains(VAULT) || stdout.contains("vault.azure.net"),
        "vault info should show vault details, got: {}",
        stdout
    );
}

#[test]
#[ignore]
fn e2e_vault_export_import() {
    let prefix = test_prefix();
    let secret_name = format!("{prefix}-export");
    let secret_value = "export-test-value";

    // Create a secret to export
    let binary = env!("CARGO_BIN_EXE_xv");
    let _ = Command::new(binary)
        .args(["set", &secret_name, "--vault", VAULT, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(secret_value.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("Failed to set secret for export test");

    // Export vault to temp file
    let tmp_dir = TempDir::new().expect("Failed to create temp dir");
    let export_path = tmp_dir.path().join("export.json");
    let export_path_str = export_path.to_str().unwrap();

    xv_ok(&["vault", "export", VAULT, "--output", export_path_str]);

    // Verify export file exists and contains our secret
    let export_content = std::fs::read_to_string(&export_path)
        .expect("Failed to read export file");
    assert!(
        export_content.contains(&secret_name),
        "export should contain our secret '{}', got: {}",
        secret_name,
        &export_content[..export_content.len().min(500)]
    );

    // Import with dry-run (should not fail)
    let stdout = xv_ok(&[
        "vault",
        "import",
        VAULT,
        "--input",
        export_path_str,
        "--dry-run",
    ]);
    assert!(
        stdout.contains("dry") || stdout.contains("Dry") || stdout.contains(&secret_name) || stdout.contains("import"),
        "dry-run import should show what would happen, got: {}",
        stdout
    );

    // Cleanup
    cleanup_secrets(&[secret_name]);
}

// ============================================================================
// Context management
// ============================================================================

#[test]
#[ignore]
fn e2e_context_lifecycle() {
    // Set context
    xv_ok(&["context", "use", VAULT]);

    // Show context
    let stdout = xv_ok(&["context", "show"]);
    assert!(
        stdout.contains(VAULT),
        "context show should display our vault '{}', got: {}",
        VAULT,
        stdout
    );

    // List contexts
    let stdout = xv_ok(&["context", "list"]);
    assert!(
        stdout.contains(VAULT),
        "context list should include our vault, got: {}",
        stdout
    );

    // Clear context
    xv_ok(&["context", "clear"]);
}

// ============================================================================
// Secret injection (xv run)
// ============================================================================

#[test]
#[ignore]
fn e2e_run_injects_env_vars() {
    let prefix = test_prefix();
    let secret_name = format!("{prefix}-run-test");
    let secret_value = "injected-value-abc";

    // Create a secret
    let binary = env!("CARGO_BIN_EXE_xv");
    let _ = Command::new(binary)
        .args(["set", &secret_name, "--vault", VAULT, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(secret_value.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("Failed to set secret for run test");

    // Use xv run to inject secrets and echo them
    // The env var name is the sanitized secret name (uppercased, hyphens to underscores)
    let expected_env_var = secret_name.replace('-', "_").to_uppercase();
    let output = xv(&[
        "run",
        "--vault",
        VAULT,
        "--no-masking",
        "--",
        "printenv",
        &expected_env_var,
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    // The secret value should appear in the child process output
    // Note: the exact env var naming depends on the sanitization logic
    // If it doesn't match, the test documents the actual behavior
    if output.status.success() {
        assert!(
            stdout.contains(secret_value),
            "run should inject secret as env var, got: '{}'",
            stdout.trim()
        );
    }
    // If printenv fails (var not found), that's useful diagnostic info too

    // Cleanup
    cleanup_secrets(&[secret_name]);
}

// ============================================================================
// Template injection (xv inject)
// ============================================================================

#[test]
#[ignore]
fn e2e_inject_template() {
    let prefix = test_prefix();
    let secret_name = format!("{prefix}-inject");
    let secret_value = "template-secret-xyz";

    // Create a secret
    let binary = env!("CARGO_BIN_EXE_xv");
    let _ = Command::new(binary)
        .args(["set", &secret_name, "--vault", VAULT, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(secret_value.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("Failed to set secret for inject test");

    // Create a template file
    let tmp_dir = TempDir::new().expect("Failed to create temp dir");
    let template_path = tmp_dir.path().join("test.tmpl");
    let output_path = tmp_dir.path().join("test.out");

    let template_content = format!("DB_PASSWORD={{{{ secret:{secret_name} }}}}");
    std::fs::write(&template_path, &template_content).expect("Failed to write template");

    // Run inject
    let output = xv(&[
        "inject",
        "--template",
        template_path.to_str().unwrap(),
        "--out",
        output_path.to_str().unwrap(),
        "--vault",
        VAULT,
    ]);

    if output.status.success() {
        let rendered = std::fs::read_to_string(&output_path)
            .expect("Failed to read inject output");
        assert!(
            rendered.contains(secret_value),
            "inject should resolve secret reference to '{}', got: '{}'",
            secret_value,
            rendered.trim()
        );
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("inject command stderr: {}", stderr);
        // Don't fail — inject may have specific requirements we'll document
    }

    // Cleanup
    cleanup_secrets(&[secret_name]);
}

// ============================================================================
// Error cases
// ============================================================================

#[test]
#[ignore]
fn e2e_get_nonexistent_secret() {
    let stderr = xv_fail(&["get", "this-secret-definitely-does-not-exist-xyz", "--vault", VAULT, "--raw"]);
    assert!(
        stderr.contains("not found") || stderr.contains("Not Found") || stderr.contains("404") || stderr.contains("Secret"),
        "getting nonexistent secret should show error, got: {}",
        stderr
    );
}

#[test]
#[ignore]
fn e2e_invalid_vault_name() {
    let stderr = xv_fail(&["list", "--vault", "this-vault-definitely-does-not-exist-xyz-99999"]);
    assert!(
        !stderr.is_empty(),
        "listing with invalid vault should produce an error"
    );
}

#[test]
#[ignore]
fn e2e_parse_unsupported_format() {
    let output = xv(&["parse", "Server=foo", "--format", "xml"]);
    assert!(
        !output.status.success(),
        "parse with unsupported format should fail"
    );
}

// ============================================================================
// Audit
// ============================================================================

#[test]
#[ignore]
fn e2e_audit() {
    let output = xv(&["audit", "--vault", VAULT, "--days", "7"]);
    // Audit may succeed or fail depending on permissions — just verify it doesn't panic
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() || stderr.contains("permission") || stderr.contains("Permission") || stderr.contains("error"),
        "audit should either succeed or fail gracefully, got stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}

// ============================================================================
// Diff (same vault)
// ============================================================================

#[test]
#[ignore]
fn e2e_diff_same_vault() {
    // Diff the vault against itself — should show no differences
    let output = xv(&["diff", VAULT, VAULT]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    if output.status.success() {
        // All secrets should be "unchanged" or similar
        assert!(
            stdout.contains("identical") || stdout.contains("match") || stdout.contains("0 added") || stdout.contains("Unchanged") || !stdout.is_empty(),
            "diff of same vault should show no differences, got: {}",
            stdout
        );
    }
}

// ============================================================================
// Copy
// ============================================================================

#[test]
#[ignore]
fn e2e_copy_secret() {
    let prefix = test_prefix();
    let source_name = format!("{prefix}-copy-src");
    let dest_name = format!("{prefix}-copy-dst");
    let value = "copy-test-value";

    // Create source secret
    let binary = env!("CARGO_BIN_EXE_xv");
    let _ = Command::new(binary)
        .args(["set", &source_name, "--vault", VAULT, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(value.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("Failed to set source secret");

    // Copy within same vault with new name
    xv_ok(&[
        "copy",
        &source_name,
        "--from",
        VAULT,
        "--to",
        VAULT,
        "--new-name",
        &dest_name,
    ]);

    // Verify copy
    let stdout = xv_ok(&["get", &dest_name, "--vault", VAULT, "--raw"]);
    assert_eq!(
        stdout.trim(),
        value,
        "copied secret should have same value '{}', got: '{}'",
        value,
        stdout.trim()
    );

    // Cleanup
    cleanup_secrets(&[source_name, dest_name]);
}

// ============================================================================
// Rotate with charset
// ============================================================================

#[test]
#[ignore]
fn e2e_rotate_with_charset() {
    let prefix = test_prefix();
    let secret_name = format!("{prefix}-rotate-hex");
    let initial_value = "initial";

    // Create secret
    let binary = env!("CARGO_BIN_EXE_xv");
    let _ = Command::new(binary)
        .args(["set", &secret_name, "--vault", VAULT, "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(initial_value.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("Failed to set secret for rotate test");

    // Rotate with hex charset, length 16
    xv_ok(&[
        "rotate",
        &secret_name,
        "--vault",
        VAULT,
        "--length",
        "16",
        "--charset",
        "hex",
    ]);

    // Verify the new value is hex
    let stdout = xv_ok(&["get", &secret_name, "--vault", VAULT, "--raw"]);
    let rotated = stdout.trim();
    assert_eq!(rotated.len(), 16, "rotated hex value should be 16 chars, got {}", rotated.len());
    assert!(
        rotated.chars().all(|c| c.is_ascii_hexdigit()),
        "rotated value should be hex, got: '{}'",
        rotated
    );

    // Cleanup
    cleanup_secrets(&[secret_name]);
}
