# `xv upgrade` — Self-Update Command Design Spec

## Overview

Add a self-update command (`xv upgrade`) that checks GitHub Releases for new versions and replaces the running binary in-place with SHA256 checksum verification.

## Command Interface

```
xv upgrade [--force] [--check]
```

| Flag | Behavior |
|------|----------|
| (none) | Check for update, prompt to confirm, download and install |
| `--check` | Only report whether an update is available (no download); exit code 0 if up-to-date, 1 if update available (scriptable) |
| `--force` | Skip confirmation prompt, install immediately |

Stable releases only — pre-releases are excluded.

## Execution Flow

1. Read current version from `built_info::PKG_VERSION`
2. HTTP GET `https://api.github.com/repos/{GITHUB_REPO}/releases/latest`
   - `GITHUB_REPO` is a module constant: `"bziobnic/crosstache"`
   - Uses `reqwest` (already a dependency) with a `User-Agent: xv/{version}` header
   - If `GITHUB_TOKEN` env var is set, include as `Authorization: Bearer {token}` (raises rate limit from 60→5000/hr)
   - GitHub API returns latest non-pre-release by default
3. Parse response: extract `tag_name` (strip leading `v`), `assets` array
4. Compare versions using `semver` crate
   - If current >= latest: print "Already up to date (v{version})" and exit 0
5. If `--check`: print "Update available: v{current} → v{latest}" and exit 1
6. Prompt: "Update xv from v{current} to v{latest}? [y/N]" (skip if `--force`)
   - User declines → exit cleanly
7. Detect platform → select asset (see Platform Detection below)
8. Validate expected download size from asset metadata (`size` field); reject if > 50MB
9. Download asset archive + matching `.sha256` file from release assets
   - Show progress via `indicatif` progress bar (already a dependency)
   - Use `response.chunk()` loop for streaming (no extra reqwest feature needed)
   - Use a longer timeout (5 minutes) for the download request
10. Verify SHA256 checksum:
    - Compute SHA256 of downloaded archive
    - Compare against content of `.sha256` file
    - Mismatch → abort with error, delete temp files
11. Extract binary from archive:
    - `.tar.gz` → `flate2` + `tar` crates
    - `.zip` → `zip` crate (Windows only, behind `#[cfg(target_os = "windows")]`)
    - Extract to a temp file in the same directory as the current binary (same filesystem for atomic rename)
    - If temp file creation fails with permission error → surface error immediately
12. On Windows: delete any existing `.old` file from a previous upgrade before proceeding
13. Replace current binary:
    - Get path via `std::env::current_exe()`
    - Warn if binary is inside `~/.cargo/bin/` ("Installed via cargo — future `cargo install` may overwrite this upgrade")
    - On Unix: rename temp file over current binary, preserve permissions
    - On Windows: rename current → `.old`, rename new → current
14. Print: "Successfully upgraded xv from v{current} to v{latest}"

## Platform Detection

```rust
fn get_asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("xv-macos-apple-silicon.tar.gz"),
        ("macos", "x86_64")  => Ok("xv-macos-intel.tar.gz"),
        ("linux", "x86_64")  => Ok("xv-linux-x64.tar.gz"),
        ("windows", "x86_64") => Ok("xv-windows-x64.zip"),
        (os, arch) => Err(format!("Unsupported platform: {os}/{arch}"))
    }
}
```

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Network error / DNS failure | "Failed to check for updates: {error}. Try again or download manually from {url}" |
| GitHub rate limit (403) | "GitHub API rate limit reached. Try again in a few minutes, or set GITHUB_TOKEN." |
| Checksum mismatch | "Checksum verification failed — aborting. The download may be corrupted." |
| Download size exceeds 50MB | "Release asset unexpectedly large ({size}MB). Aborting for safety." |
| Permission denied (temp file or replace) | "Permission denied. Try running with elevated privileges (sudo on Unix)." |
| Unsupported platform | "Unsupported platform {os}/{arch}. Download manually from {url}" |
| No matching asset in release | "No binary found for your platform in release v{version}." |
| Binary in `~/.cargo/bin/` | Warning (non-fatal): "Installed via cargo — future `cargo install` may overwrite this upgrade" |

## Error Type

Add a new `CrosstacheError::Upgrade(String)` variant for upgrade-specific errors (checksum mismatch, unsupported platform, asset not found). Network errors reuse existing `CrosstacheError::Network`.

## Module Structure

- **New file:** `src/cli/upgrade_ops.rs` — all upgrade logic
  - `pub(crate) async fn execute_upgrade_command(check: bool, force: bool) -> Result<()>` — entry point
  - Private helpers: `fetch_latest_release()`, `get_asset_name()`, `download_asset()`, `verify_checksum()`, `extract_binary()`, `replace_binary()`
  - `const GITHUB_REPO: &str = "bziobnic/crosstache";` — single source of truth for repo path
- **Modified:** `src/cli/commands.rs` — add `Commands::Upgrade` variant with `--check` and `--force` flags
- **Modified:** `src/cli/mod.rs` — add `pub(crate) mod upgrade_ops;`
- **Modified:** `src/error.rs` — add `Upgrade(String)` variant to `CrosstacheError`

## Dependencies

| Crate | Version | Purpose | Status |
|-------|---------|---------|--------|
| `reqwest` | 0.12 | HTTP requests | Already present (no new features needed) |
| `sha2` | 0.10 | Checksum verification | Already present |
| `indicatif` | 0.17 | Progress bar | Already present |
| `semver` | 1 | Version comparison | **New** |
| `flate2` | 1 | Gzip decompression | **New** |
| `tar` | 0.4 | Tar archive extraction | **New** |
| `zip` | 2 | Zip extraction (Windows only, `#[cfg]` gated) | **New** |

## Testing Strategy

- Unit tests for `get_asset_name()` platform detection
- Unit tests for version comparison logic (current < latest, current == latest, current > latest)
- Unit test for SHA256 checksum verification against a known hash
- Unit test for `.sha256` file parsing (handles trailing whitespace, filename suffixes)
- Integration test (ignored by default): full upgrade check against live GitHub API

## Out of Scope

- Pre-release channel support
- Automatic update checks on other commands (no background check-on-launch)
- Homebrew/package manager integration
- Rollback to previous version
- Targeting a specific version (`--version X.Y.Z`)
- GPG/minisign signature verification (noted as future improvement for release authenticity)
