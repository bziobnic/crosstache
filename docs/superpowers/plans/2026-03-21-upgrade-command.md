# `xv upgrade` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a self-update command that checks GitHub Releases for new versions, verifies SHA256 checksums, and replaces the running binary in-place.

**Architecture:** Single new module `upgrade_ops.rs` containing all upgrade logic. The command queries the GitHub Releases API for the latest stable version, downloads the platform-appropriate archive, verifies its checksum, extracts the binary, and replaces the current executable. Uses `semver` for version comparison, `reqwest` for downloads, `sha2` for checksums, and `flate2`/`tar`/`zip` for extraction.

**Tech Stack:** Rust, reqwest, semver, sha2, flate2, tar, zip, indicatif, dialoguer

**Spec:** `docs/superpowers/specs/2026-03-21-upgrade-command-design.md`

---

### Task 1: Add Dependencies

**Files:**
- Modify: `Cargo.toml:70-79` (additional utilities section)

- [ ] **Step 1: Add new crate dependencies**

In `Cargo.toml`, add after the `libc` line (line 79) in the `# Additional utilities` section:

```toml
semver = "1"
flate2 = "1"
tar = "0.4"
```

And add a new section for Windows-only dependencies:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
zip = "2"
```

- [ ] **Step 2: Verify dependencies resolve**

Run: `cargo check`
Expected: compiles with no new errors (warnings from pre-existing issues are fine)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add semver, flate2, tar, zip dependencies for upgrade command"
```

---

### Task 2: Add `CrosstacheError::Upgrade` Variant

**Files:**
- Modify: `src/error.rs:73` (add variant before `Unknown`)
- Modify: `src/main.rs:170` (add user-friendly error handling)

- [ ] **Step 1: Add the Upgrade variant to CrosstacheError**

In `src/error.rs`, add before the `Unknown` variant (line 73):

```rust
    #[error("Upgrade error: {0}")]
    Upgrade(String),
```

Add the constructor in the `impl CrosstacheError` block, before the `unknown` method (around line 140):

```rust
    pub fn upgrade<S: Into<String>>(msg: S) -> Self {
        Self::Upgrade(msg.into())
    }
```

- [ ] **Step 2: Add user-friendly error handling in main.rs**

In `src/main.rs`, in the `print_user_friendly_error` function, add before the `_ =>` catch-all (around line 170):

```rust
        Upgrade(msg) => {
            output::error("Upgrade Error");
            eprintln!("{msg}");
        }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check`
Expected: compiles successfully

- [ ] **Step 4: Commit**

```bash
git add src/error.rs src/main.rs
git commit -m "feat: add UpgradeError variant to CrosstacheError"
```

---

### Task 3: Add `Commands::Upgrade` to CLI and Wire Dispatcher

**Files:**
- Modify: `src/cli/commands.rs` (add Upgrade variant to Commands enum + dispatcher arm)
- Modify: `src/cli/mod.rs` (add module declaration)
- Modify: `src/main.rs:48-57` (skip config validation for Upgrade)
- Create: `src/cli/upgrade_ops.rs` (stub entry point)

- [ ] **Step 1: Add `Upgrade` variant to the `Commands` enum**

In `src/cli/commands.rs`, add after the `Whoami` variant (around line 547):

```rust
    /// Check for and install new versions
    Upgrade {
        /// Only check if an update is available (exit code 0 = up-to-date, 1 = update available)
        #[arg(long)]
        check: bool,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
```

- [ ] **Step 2: Add dispatcher arm in `Cli::execute()`**

In `src/cli/commands.rs`, in the `match self.command` block, add after the `Commands::Whoami` arm (around line 1108):

```rust
            // Upgrade does not need Azure config — only talks to GitHub API
            Commands::Upgrade { check, force } => {
                crate::cli::upgrade_ops::execute_upgrade_command(check, force).await
            }
```

- [ ] **Step 3: Skip config validation for Upgrade command**

In `src/main.rs`, update the config loading match (around line 48-57) to include `Upgrade`:

```rust
    let config = match &cli.command {
        crate::cli::Commands::Config { .. }
        | crate::cli::Commands::Init
        | crate::cli::Commands::Upgrade { .. } => {
            load_config_without_validation().await?
        }
        _ => {
            config::load_config().await?
        }
    };
```

- [ ] **Step 4: Create stub `upgrade_ops.rs`**

Create `src/cli/upgrade_ops.rs`:

```rust
//! Self-update command: check for and install new versions of xv.

use crate::error::Result;

/// Check for and optionally install a new version of xv.
pub(crate) async fn execute_upgrade_command(_check: bool, _force: bool) -> Result<()> {
    todo!("upgrade command not yet implemented")
}
```

- [ ] **Step 5: Wire module in `mod.rs`**

In `src/cli/mod.rs`, add after the `pub(crate) mod system_ops;` line:

```rust
pub(crate) mod upgrade_ops;
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo check`
Expected: compiles successfully (stub with `todo!()` is fine for compilation)

- [ ] **Step 7: Verify the command shows in help**

Run: `cargo run -- --help`
Expected: `upgrade` command appears in the commands list

- [ ] **Step 8: Commit**

```bash
git add src/cli/commands.rs src/cli/mod.rs src/cli/upgrade_ops.rs src/main.rs
git commit -m "feat: add upgrade command skeleton with CLI wiring"
```

---

### Task 4: Implement Version Check (fetch_latest_release + version comparison)

**Files:**
- Modify: `src/cli/upgrade_ops.rs`

This task implements all upgrade logic: querying GitHub API, version comparison, download with progress, checksum verification, extraction, and binary replacement. Tests are included in the same file.

- [ ] **Step 1: Implement the full upgrade module**

Replace the entire contents of `src/cli/upgrade_ops.rs` with:

```rust
//! Self-update command: check for and install new versions of xv.

use crate::cli::commands::built_info;
use crate::error::{CrosstacheError, Result};
use crate::utils::output;

const GITHUB_REPO: &str = "bziobnic/crosstache";
const MAX_ASSET_SIZE: u64 = 50 * 1024 * 1024; // 50 MB

/// GitHub release metadata (subset of API response).
#[derive(Debug, serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GitHubAsset>,
}

/// A single release asset.
#[derive(Debug, serde::Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// Check for and optionally install a new version of xv.
pub(crate) async fn execute_upgrade_command(check: bool, force: bool) -> Result<()> {
    let current = parse_tag_version(built_info::PKG_VERSION)
        .map_err(|e| CrosstacheError::upgrade(format!("Failed to parse current version: {e}")))?;

    output::info(&format!("Current version: v{current}"));
    output::info("Checking for updates...");

    let release = fetch_latest_release().await?;
    let latest = parse_tag_version(&release.tag_name)
        .map_err(|e| CrosstacheError::upgrade(format!("Failed to parse release version: {e}")))?;

    if latest <= current {
        output::success(&format!("Already up to date (v{current})"));
        return Ok(());
    }

    // Update available
    if check {
        output::info(&format!("Update available: v{current} → v{latest}"));
        output::hint(&format!("Run 'xv upgrade' to install, or download from {}", release.html_url));
        // Exit with code 1 for scriptability (e.g., `xv upgrade --check && echo "up to date"`)
        // We call exit directly to avoid main.rs printing error-formatted output.
        std::process::exit(1);
    }

    output::info(&format!("Update available: v{current} → v{latest}"));

    // Prompt for confirmation unless --force
    if !force {
        use dialoguer::Confirm;
        let confirmed = Confirm::new()
            .with_prompt(format!("Update xv from v{current} to v{latest}?"))
            .default(false)
            .interact()
            .map_err(|e| CrosstacheError::upgrade(format!("Prompt failed: {e}")))?;

        if !confirmed {
            output::info("Upgrade cancelled.");
            return Ok(());
        }
    }

    // Determine platform asset
    let asset_name = get_asset_name()?;
    let checksum_name = format!("{asset_name}.sha256");

    // Find assets in release
    let archive_asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| {
            CrosstacheError::upgrade(format!(
                "No binary found for your platform in release v{latest}. Download manually from {}",
                release.html_url
            ))
        })?;

    let checksum_asset = release
        .assets
        .iter()
        .find(|a| a.name == checksum_name)
        .ok_or_else(|| {
            CrosstacheError::upgrade(format!(
                "No checksum file found for {asset_name} in release v{latest}"
            ))
        })?;

    // Validate size
    if archive_asset.size > MAX_ASSET_SIZE {
        return Err(CrosstacheError::upgrade(format!(
            "Release asset unexpectedly large ({}MB). Aborting for safety.",
            archive_asset.size / (1024 * 1024)
        )));
    }

    // Download archive and checksum
    output::info(&format!("Downloading {asset_name}..."));
    let archive_bytes = download_asset(&archive_asset.browser_download_url, archive_asset.size).await?;
    let checksum_bytes = download_asset(&checksum_asset.browser_download_url, checksum_asset.size).await?;

    // Verify checksum
    let checksum_text = String::from_utf8_lossy(&checksum_bytes);
    let expected_hash = parse_checksum_line(&checksum_text);
    verify_checksum(&archive_bytes, expected_hash)?;
    output::success("Checksum verified");

    // Extract binary
    output::info("Extracting binary...");
    let binary_bytes = extract_binary(&archive_bytes, asset_name)?;

    // Replace current binary
    replace_binary(&binary_bytes)?;

    output::success(&format!("Successfully upgraded xv from v{current} to v{latest}"));
    Ok(())
}

/// Fetch the latest stable release metadata from GitHub.
async fn fetch_latest_release() -> Result<GitHubRelease> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let version = built_info::PKG_VERSION;

    let client = reqwest::Client::new();
    let mut request = client
        .get(&url)
        .header("User-Agent", format!("xv/{version}"))
        .header("Accept", "application/vnd.github+json");

    // Use GITHUB_TOKEN if available (raises rate limit from 60→5000/hr)
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = request.send().await.map_err(|e| {
        CrosstacheError::upgrade(format!(
            "Failed to check for updates: {e}. Try again or download manually from https://github.com/{GITHUB_REPO}/releases"
        ))
    })?;

    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(CrosstacheError::upgrade(
            "GitHub API rate limit reached. Try again in a few minutes, or set GITHUB_TOKEN.".to_string(),
        ));
    }

    if !response.status().is_success() {
        return Err(CrosstacheError::upgrade(format!(
            "GitHub API returned status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        )));
    }

    response.json::<GitHubRelease>().await.map_err(|e| {
        CrosstacheError::upgrade(format!("Failed to parse GitHub release response: {e}"))
    })
}

/// Determine the correct release asset name for this platform.
fn get_asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("xv-macos-apple-silicon.tar.gz"),
        ("macos", "x86_64") => Ok("xv-macos-intel.tar.gz"),
        ("linux", "x86_64") => Ok("xv-linux-x64.tar.gz"),
        ("windows", "x86_64") => Ok("xv-windows-x64.zip"),
        (os, arch) => Err(CrosstacheError::upgrade(format!(
            "Unsupported platform: {os}/{arch}. Download manually from https://github.com/{GITHUB_REPO}/releases"
        ))),
    }
}

/// Download a release asset with a progress bar.
async fn download_asset(url: &str, expected_size: u64) -> Result<Vec<u8>> {
    use indicatif::{ProgressBar, ProgressStyle};

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300)) // 5 minute timeout
        .build()
        .map_err(|e| CrosstacheError::upgrade(format!("Failed to create HTTP client: {e}")))?;

    let response = client
        .get(url)
        .header("User-Agent", format!("xv/{}", built_info::PKG_VERSION))
        .send()
        .await
        .map_err(|e| CrosstacheError::upgrade(format!("Download failed: {e}")))?;

    if !response.status().is_success() {
        return Err(CrosstacheError::upgrade(format!(
            "Download failed with status {}",
            response.status()
        )));
    }

    let total_size = response.content_length().unwrap_or(expected_size);

    // Only show progress bar for downloads > 1KB (skip for tiny checksum files)
    let show_progress = total_size > 1024;
    let pb = if show_progress {
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut bytes = Vec::with_capacity(total_size as usize);
    let mut stream = response;

    while let Some(chunk) = stream.chunk().await.map_err(|e| {
        CrosstacheError::upgrade(format!("Download interrupted: {e}"))
    })? {
        bytes.extend_from_slice(&chunk);
        if let Some(ref pb) = pb {
            pb.set_position(bytes.len() as u64);
        }
    }

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }
    Ok(bytes)
}

/// Parse the first hash from a `.sha256` file line.
/// Handles formats: "hash  filename", "hash *filename", or just "hash".
fn parse_checksum_line(line: &str) -> &str {
    line.trim().split_whitespace().next().unwrap_or("").trim()
}

/// Verify the SHA256 checksum of downloaded data.
fn verify_checksum(data: &[u8], expected_hex: &str) -> Result<()> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(data);
    let computed = hex::encode(hasher.finalize());

    if computed != expected_hex.to_lowercase() {
        return Err(CrosstacheError::upgrade(
            "Checksum verification failed — aborting. The download may be corrupted.".to_string(),
        ));
    }
    Ok(())
}

/// Parse a version string, stripping an optional `v` prefix.
fn parse_tag_version(tag: &str) -> std::result::Result<semver::Version, semver::Error> {
    let version_str = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(version_str)
}

/// Extract the `xv` binary from the downloaded archive.
fn extract_binary(archive_bytes: &[u8], asset_name: &str) -> Result<Vec<u8>> {
    if asset_name.ends_with(".tar.gz") {
        extract_from_tar_gz(archive_bytes)
    } else if asset_name.ends_with(".zip") {
        extract_from_zip(archive_bytes)
    } else {
        Err(CrosstacheError::upgrade(format!(
            "Unknown archive format: {asset_name}"
        )))
    }
}

/// Extract the xv binary from a .tar.gz archive.
fn extract_from_tar_gz(data: &[u8]) -> Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use std::io::Read;
    use tar::Archive;

    let decoder = GzDecoder::new(data);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().map_err(|e| {
        CrosstacheError::upgrade(format!("Failed to read archive: {e}"))
    })? {
        let mut entry = entry.map_err(|e| {
            CrosstacheError::upgrade(format!("Failed to read archive entry: {e}"))
        })?;

        let path = entry.path().map_err(|e| {
            CrosstacheError::upgrade(format!("Failed to read entry path: {e}"))
        })?;

        // Look for the xv binary (might be at root or in a subdirectory)
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == "xv" || file_name == "xv.exe" {
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).map_err(|e| {
                CrosstacheError::upgrade(format!("Failed to extract binary: {e}"))
            })?;
            return Ok(contents);
        }
    }

    Err(CrosstacheError::upgrade(
        "Could not find xv binary in archive".to_string(),
    ))
}

/// Extract the xv binary from a .zip archive (Windows).
#[cfg(target_os = "windows")]
fn extract_from_zip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::{Cursor, Read};

    let reader = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| {
        CrosstacheError::upgrade(format!("Failed to read zip archive: {e}"))
    })?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            CrosstacheError::upgrade(format!("Failed to read zip entry: {e}"))
        })?;

        let name = file.name().to_string();
        if name.ends_with("xv.exe") || name.ends_with("/xv.exe") {
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).map_err(|e| {
                CrosstacheError::upgrade(format!("Failed to extract binary: {e}"))
            })?;
            return Ok(contents);
        }
    }

    Err(CrosstacheError::upgrade(
        "Could not find xv.exe in zip archive".to_string(),
    ))
}

/// Stub for non-Windows platforms (zip archives only used on Windows).
#[cfg(not(target_os = "windows"))]
fn extract_from_zip(_data: &[u8]) -> Result<Vec<u8>> {
    Err(CrosstacheError::upgrade(
        "Zip extraction is only supported on Windows".to_string(),
    ))
}

/// Replace the current binary with the new one.
fn replace_binary(new_binary: &[u8]) -> Result<()> {
    use std::fs;
    use std::io::Write;

    let current_exe = std::env::current_exe().map_err(|e| {
        CrosstacheError::upgrade(format!("Failed to determine current binary path: {e}"))
    })?;

    // Resolve symlinks to get the actual binary path
    let current_exe = current_exe.canonicalize().unwrap_or(current_exe);

    // Warn if installed via cargo
    if let Some(path_str) = current_exe.to_str() {
        if path_str.contains(".cargo/bin") {
            output::warn(
                "Installed via cargo — future `cargo install` may overwrite this upgrade",
            );
        }
    }

    let parent_dir = current_exe.parent().ok_or_else(|| {
        CrosstacheError::upgrade("Cannot determine binary directory".to_string())
    })?;

    // Write new binary to temp file in the same directory (same filesystem for atomic rename)
    let temp_path = parent_dir.join(".xv-upgrade-tmp");

    let mut temp_file = fs::File::create(&temp_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            CrosstacheError::upgrade(
                "Permission denied. Try running with elevated privileges (sudo on Unix).".to_string(),
            )
        } else {
            CrosstacheError::upgrade(format!("Failed to create temp file: {e}"))
        }
    })?;

    temp_file.write_all(new_binary).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        CrosstacheError::upgrade(format!("Failed to write new binary: {e}"))
    })?;

    // Set executable permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&temp_path, permissions).map_err(|e| {
            CrosstacheError::upgrade(format!("Failed to set permissions: {e}"))
        })?;
    }

    // Replace the binary
    #[cfg(unix)]
    {
        fs::rename(&temp_path, &current_exe).map_err(|e| {
            let _ = fs::remove_file(&temp_path);
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                CrosstacheError::upgrade(
                    "Permission denied. Try running with elevated privileges (sudo on Unix)."
                        .to_string(),
                )
            } else {
                CrosstacheError::upgrade(format!("Failed to replace binary: {e}"))
            }
        })?;
    }

    #[cfg(windows)]
    {
        let old_path = current_exe.with_extension("exe.old");
        // Clean up any leftover .old file from a previous upgrade
        let _ = fs::remove_file(&old_path);
        // Rename current -> .old (Windows locks running executables)
        fs::rename(&current_exe, &old_path).map_err(|e| {
            CrosstacheError::upgrade(format!("Failed to rename current binary: {e}"))
        })?;
        // Rename temp -> current
        fs::rename(&temp_path, &current_exe).map_err(|e| {
            // Try to restore the old binary on failure
            let _ = fs::rename(&old_path, &current_exe);
            CrosstacheError::upgrade(format!("Failed to install new binary: {e}"))
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_asset_name_returns_valid_name() {
        let name = get_asset_name();
        assert!(name.is_ok(), "Should detect current platform");
        let name = name.unwrap();
        assert!(
            name.ends_with(".tar.gz") || name.ends_with(".zip"),
            "Asset should be tar.gz or zip, got: {name}"
        );
    }

    #[test]
    fn test_parse_version_strips_v_prefix() {
        let version = parse_tag_version("v1.2.3").unwrap();
        assert_eq!(version, semver::Version::new(1, 2, 3));
    }

    #[test]
    fn test_parse_version_without_prefix() {
        let version = parse_tag_version("1.2.3").unwrap();
        assert_eq!(version, semver::Version::new(1, 2, 3));
    }

    #[test]
    fn test_parse_version_invalid() {
        assert!(parse_tag_version("not-a-version").is_err());
    }

    #[test]
    fn test_needs_update_when_behind() {
        let current = semver::Version::new(0, 4, 0);
        let latest = semver::Version::new(0, 5, 0);
        assert!(latest > current);
    }

    #[test]
    fn test_no_update_when_current() {
        let current = semver::Version::new(0, 5, 0);
        let latest = semver::Version::new(0, 5, 0);
        assert!(latest <= current);
    }

    #[test]
    fn test_no_update_when_ahead() {
        let current = semver::Version::new(0, 6, 0);
        let latest = semver::Version::new(0, 5, 0);
        assert!(latest <= current);
    }

    #[test]
    fn test_parse_checksum_line_with_filename() {
        let line = "abc123def456  xv-macos-apple-silicon.tar.gz";
        let hash = parse_checksum_line(line);
        assert_eq!(hash, "abc123def456");
    }

    #[test]
    fn test_parse_checksum_line_hash_only() {
        let line = "abc123def456";
        let hash = parse_checksum_line(line);
        assert_eq!(hash, "abc123def456");
    }

    #[test]
    fn test_parse_checksum_line_trailing_whitespace() {
        let line = "abc123def456  \n";
        let hash = parse_checksum_line(line);
        assert_eq!(hash, "abc123def456");
    }

    #[test]
    fn test_verify_checksum_valid() {
        use sha2::{Digest, Sha256};
        let data = b"hello world";
        let hash = hex::encode(Sha256::digest(data));
        assert!(verify_checksum(data, &hash).is_ok());
    }

    #[test]
    fn test_verify_checksum_invalid() {
        assert!(verify_checksum(b"hello world", "0000000000000000000000000000000000000000000000000000000000000000").is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib upgrade_ops`
Expected: all 12 tests pass

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: all tests pass (107+ unit tests)

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets`
Expected: no new warnings

- [ ] **Step 5: Commit**

```bash
git add src/cli/upgrade_ops.rs
git commit -m "feat: implement upgrade command with version check, download, and binary replacement"
```

---

### Task 5: Integration Test

**Files:**
- Modify: `src/cli/upgrade_ops.rs` (add integration test at end of test module)

- [ ] **Step 1: Add an ignored integration test for live API check**

Add to the `tests` module in `src/cli/upgrade_ops.rs`:

```rust
    #[tokio::test]
    #[ignore] // Requires internet access
    async fn test_fetch_latest_release_from_github() {
        let release = fetch_latest_release().await.unwrap();
        assert!(!release.tag_name.is_empty(), "Should have a tag name");
        assert!(!release.assets.is_empty(), "Should have release assets");
        // Verify we can parse the version
        let version = parse_tag_version(&release.tag_name).unwrap();
        assert!(version.major > 0 || version.minor > 0, "Should be a non-zero version");
    }
```

- [ ] **Step 2: Run the integration test to verify it works**

Run: `cargo test --lib upgrade_ops::tests::test_fetch_latest_release_from_github -- --ignored --nocapture`
Expected: PASS (fetches real release data from GitHub)

- [ ] **Step 3: Run full test suite to ensure nothing is broken**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add src/cli/upgrade_ops.rs
git commit -m "test: add integration test for GitHub release API"
```

---

### Task 6: Manual Smoke Test and Final Cleanup

- [ ] **Step 1: Test `--check` flag**

Run: `cargo run -- upgrade --check`
Expected: either "Already up to date" (exit 0) or "Update available" (exit 1)

- [ ] **Step 2: Test `--help` flag**

Run: `cargo run -- upgrade --help`
Expected: shows help for upgrade command with `--check` and `--force` flags

- [ ] **Step 3: Test full upgrade flow (dry run)**

Run: `cargo run -- upgrade`
Expected: shows current version, checks for update, prompts for confirmation. Answer 'n' to cancel.

- [ ] **Step 4: Run full test suite one final time**

Run: `cargo test && cargo clippy --all-targets`
Expected: all tests pass, no new warnings

- [ ] **Step 5: Final commit if any cleanup was needed**

```bash
git add -A
git commit -m "chore: upgrade command cleanup and polish"
```
