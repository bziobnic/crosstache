//! Integration tests for recursive upload with directory structure preservation
//!
//! These tests require:
//! - Azure CLI authentication (az login)
//! - Storage account configured in ~/.config/xv/xv.conf
//! - Internet connection to Azure
//!
//! Run with: cargo test --test azure_recursive_upload_tests -- --nocapture --test-threads=1

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Helper to create a test directory structure
fn create_test_structure(base_dir: &Path) -> std::io::Result<()> {
    // Create directory structure
    fs::create_dir_all(base_dir.join("api/v1"))?;
    fs::create_dir_all(base_dir.join("docs"))?;
    fs::create_dir_all(base_dir.join("src"))?;

    // Create test files
    fs::write(base_dir.join("api/v1/users.md"), "# Users API v1")?;
    fs::write(base_dir.join("api/v1/auth.md"), "# Auth API v1")?;
    fs::write(base_dir.join("docs/guide.md"), "# Quick Start Guide")?;
    fs::write(base_dir.join("src/main.rs"), "fn main() {}")?;

    Ok(())
}

/// Helper to run xv command
fn run_xv_command(args: &[&str]) -> std::process::Output {
    let xv_binary = env!("CARGO_BIN_EXE_xv");
    Command::new(xv_binary)
        .args(args)
        .output()
        .expect("Failed to execute xv command")
}

/// Helper to list blobs in Azure using Azure CLI
fn list_azure_blobs(prefix: &str) -> Vec<String> {
    let output = Command::new("az")
        .args([
            "storage",
            "blob",
            "list",
            "--account-name",
            "stscottzionic07181334",
            "--container-name",
            "crosstache-files",
            "--auth-mode",
            "login",
            "--prefix",
            prefix,
            "--query",
            "[].name",
            "-o",
            "tsv",
        ])
        .output()
        .expect("Failed to execute az command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Helper to delete test blobs
fn cleanup_test_blobs(prefix: &str) {
    let blobs = list_azure_blobs(prefix);
    for blob in blobs {
        let _ = Command::new("az")
            .args([
                "storage",
                "blob",
                "delete",
                "--account-name",
                "stscottzionic07181334",
                "--container-name",
                "crosstache-files",
                "--auth-mode",
                "login",
                "--name",
                &blob,
            ])
            .output();
    }
}

#[test]
#[ignore] // Run with --ignored flag when you have Azure credentials
fn test_recursive_upload_preserves_structure() {
    // Setup
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_dir = temp_dir.path().join("test-structure");
    fs::create_dir(&test_dir).expect("Failed to create test directory");
    create_test_structure(&test_dir).expect("Failed to create test structure");

    // Cleanup any existing test blobs
    cleanup_test_blobs("test-structure/");

    // Execute: Upload with structure preservation (default)
    let output = run_xv_command(&["file", "upload", test_dir.to_str().unwrap(), "--recursive"]);

    // Verify command succeeded
    assert!(
        output.status.success(),
        "Upload command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: Check Azure for expected blob structure
    let blobs = list_azure_blobs("test-structure/");

    println!("Uploaded blobs: {:?}", blobs);

    assert!(
        blobs.contains(&"test-structure/api/v1/users.md".to_string()),
        "Expected test-structure/api/v1/users.md in blobs"
    );
    assert!(
        blobs.contains(&"test-structure/api/v1/auth.md".to_string()),
        "Expected test-structure/api/v1/auth.md in blobs"
    );
    assert!(
        blobs.contains(&"test-structure/docs/guide.md".to_string()),
        "Expected test-structure/docs/guide.md in blobs"
    );
    assert!(
        blobs.contains(&"test-structure/src/main.rs".to_string()),
        "Expected test-structure/src/main.rs in blobs"
    );

    // Verify structure is preserved (not flattened)
    assert_eq!(blobs.len(), 4, "Expected exactly 4 blobs");

    // Cleanup
    cleanup_test_blobs("test-structure/");
}

#[test]
#[ignore] // Run with --ignored flag when you have Azure credentials
fn test_recursive_upload_with_flatten() {
    // Setup
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_dir = temp_dir.path().join("flatten-test");
    fs::create_dir(&test_dir).expect("Failed to create test directory");
    create_test_structure(&test_dir).expect("Failed to create test structure");

    // Cleanup any existing test blobs
    cleanup_test_blobs("users.md");
    cleanup_test_blobs("auth.md");
    cleanup_test_blobs("guide.md");
    cleanup_test_blobs("main.rs");

    // Execute: Upload with --flatten flag
    let output = run_xv_command(&[
        "file",
        "upload",
        test_dir.to_str().unwrap(),
        "--recursive",
        "--flatten",
    ]);

    // Verify command succeeded
    assert!(
        output.status.success(),
        "Upload command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: Check Azure for flattened structure (no paths)
    let all_blobs = list_azure_blobs("");

    println!("All blobs after flatten: {:?}", all_blobs);

    assert!(
        all_blobs.contains(&"users.md".to_string()),
        "Expected users.md (flattened) in blobs"
    );
    assert!(
        all_blobs.contains(&"auth.md".to_string()),
        "Expected auth.md (flattened) in blobs"
    );
    assert!(
        all_blobs.contains(&"guide.md".to_string()),
        "Expected guide.md (flattened) in blobs"
    );
    assert!(
        all_blobs.contains(&"main.rs".to_string()),
        "Expected main.rs (flattened) in blobs"
    );

    // Verify no directory structure (files should be at root)
    assert!(
        !all_blobs.iter().any(|b| b.contains("/")),
        "Flattened blobs should not contain directory separators"
    );

    // Cleanup
    cleanup_test_blobs("users.md");
    cleanup_test_blobs("auth.md");
    cleanup_test_blobs("guide.md");
    cleanup_test_blobs("main.rs");
}

#[test]
#[ignore] // Run with --ignored flag when you have Azure credentials
fn test_recursive_upload_with_prefix() {
    // Setup
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_dir = temp_dir.path().join("prefix-test");
    fs::create_dir(&test_dir).expect("Failed to create test directory");
    create_test_structure(&test_dir).expect("Failed to create test structure");

    let prefix = "integration-test/2024-01-15";

    // Cleanup any existing test blobs
    cleanup_test_blobs(&format!("{}/", prefix));

    // Execute: Upload with --prefix flag
    let output = run_xv_command(&[
        "file",
        "upload",
        test_dir.to_str().unwrap(),
        "--recursive",
        "--prefix",
        prefix,
    ]);

    // Verify command succeeded
    assert!(
        output.status.success(),
        "Upload command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: Check Azure for prefixed structure
    let blobs = list_azure_blobs(&format!("{}/", prefix));

    println!("Prefixed blobs: {:?}", blobs);

    assert!(
        blobs.contains(&format!("{}/api/v1/users.md", prefix)),
        "Expected prefixed api/v1/users.md in blobs"
    );
    assert!(
        blobs.contains(&format!("{}/api/v1/auth.md", prefix)),
        "Expected prefixed api/v1/auth.md in blobs"
    );
    assert!(
        blobs.contains(&format!("{}/docs/guide.md", prefix)),
        "Expected prefixed docs/guide.md in blobs"
    );
    assert!(
        blobs.contains(&format!("{}/src/main.rs", prefix)),
        "Expected prefixed src/main.rs in blobs"
    );

    // Verify all blobs have the prefix
    assert!(
        blobs.iter().all(|b| b.starts_with(prefix)),
        "All blobs should start with the prefix"
    );

    // Cleanup
    cleanup_test_blobs(&format!("{}/", prefix));
}

#[test]
#[ignore] // Run with --ignored flag when you have Azure credentials
fn test_hidden_files_are_skipped() {
    // Setup
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_dir = temp_dir.path().join("hidden-test");
    fs::create_dir(&test_dir).expect("Failed to create test directory");

    // Create normal files
    fs::write(test_dir.join("visible.txt"), "visible content").unwrap();

    // Create hidden files (starting with .)
    fs::write(test_dir.join(".hidden"), "hidden content").unwrap();
    fs::write(test_dir.join(".env"), "SECRET=123").unwrap();
    fs::create_dir(test_dir.join(".git")).unwrap();
    fs::write(test_dir.join(".git/config"), "git config").unwrap();

    // Cleanup any existing test blobs
    cleanup_test_blobs("hidden-test/");

    // Execute: Upload recursively
    let output = run_xv_command(&["file", "upload", test_dir.to_str().unwrap(), "--recursive"]);

    // Verify command succeeded
    assert!(
        output.status.success(),
        "Upload command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: Check Azure - hidden files should NOT be uploaded
    let blobs = list_azure_blobs("hidden-test/");

    println!("Uploaded blobs (hidden test): {:?}", blobs);

    assert!(
        blobs.contains(&"hidden-test/visible.txt".to_string()),
        "Expected visible.txt to be uploaded"
    );

    // Verify hidden files were NOT uploaded
    assert!(
        !blobs.iter().any(|b| b.contains(".hidden")),
        "Hidden files starting with . should not be uploaded"
    );
    assert!(
        !blobs.iter().any(|b| b.contains(".env")),
        ".env files should not be uploaded"
    );
    assert!(
        !blobs.iter().any(|b| b.contains(".git")),
        ".git directory should not be uploaded"
    );

    // Cleanup
    cleanup_test_blobs("hidden-test/");
}
