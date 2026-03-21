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
    // Setup: Use uniquely named files to avoid collisions with other blobs
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_dir = temp_dir.path().join("flatten-test");
    fs::create_dir(&test_dir).expect("Failed to create test directory");
    fs::create_dir_all(test_dir.join("sub/deep")).expect("Failed to create dirs");

    let test_files = [
        ("sub/deep/flatten-test-alpha.txt", "alpha"),
        ("sub/flatten-test-beta.txt", "beta"),
        ("flatten-test-gamma.txt", "gamma"),
    ];
    for (path, content) in &test_files {
        fs::write(test_dir.join(path), content).unwrap();
    }

    let expected_flat_names: Vec<String> = test_files
        .iter()
        .map(|(p, _)| {
            Path::new(p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();

    // Cleanup any existing test blobs
    for name in &expected_flat_names {
        cleanup_test_blobs(name);
    }

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

    // Verify: Check that each file was uploaded at root (no directory structure)
    let all_blobs = list_azure_blobs("");
    println!("All blobs after flatten: {:?}", all_blobs);

    for name in &expected_flat_names {
        assert!(
            all_blobs.contains(name),
            "Expected flattened blob '{}' in container",
            name
        );
    }

    // Verify flattened files have no directory separators
    for name in &expected_flat_names {
        assert!(
            !name.contains('/'),
            "Flattened blob '{}' should not contain directory separators",
            name
        );
    }

    // Cleanup
    for name in &expected_flat_names {
        cleanup_test_blobs(name);
    }
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
    // The upload preserves the source directory name, so blobs are at prefix/prefix-test/...
    let blobs = list_azure_blobs(&format!("{}/", prefix));

    println!("Prefixed blobs: {:?}", blobs);

    assert!(
        blobs.contains(&format!("{}/prefix-test/api/v1/users.md", prefix)),
        "Expected prefixed prefix-test/api/v1/users.md in blobs"
    );
    assert!(
        blobs.contains(&format!("{}/prefix-test/api/v1/auth.md", prefix)),
        "Expected prefixed prefix-test/api/v1/auth.md in blobs"
    );
    assert!(
        blobs.contains(&format!("{}/prefix-test/docs/guide.md", prefix)),
        "Expected prefixed prefix-test/docs/guide.md in blobs"
    );
    assert!(
        blobs.contains(&format!("{}/prefix-test/src/main.rs", prefix)),
        "Expected prefixed prefix-test/src/main.rs in blobs"
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
