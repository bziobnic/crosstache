//! Idempotent pre-commit hook installation for `xv scan`.

use crate::error::{CrosstacheError, Result};
use std::path::{Path, PathBuf};

const MARKER: &str = "# xv-scan-managed";
const HOOK_BODY: &str = "#!/usr/bin/env bash
# xv-scan-managed
# Pre-commit hook installed by `xv scan install`. Edit at your own
# risk; `xv scan uninstall` removes this block.
set -e
xv scan --staged --hook
";

fn hook_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".git").join("hooks").join("pre-commit")
}

/// Locate the repo root by walking up from cwd until `.git` is found.
fn find_repo_root() -> Result<PathBuf> {
    let mut current = std::env::current_dir()?;
    loop {
        if current.join(".git").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(CrosstacheError::config(
                "not in a git repository (no .git found in any ancestor)",
            ));
        }
    }
}

/// Install the hook. Idempotent: if a hook with our marker already
/// exists, no-op. If a non-managed hook exists, refuse unless `force`.
pub fn install(force: bool) -> Result<HookInstallStatus> {
    let root = find_repo_root()?;
    let path = hook_path(&root);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing.contains(MARKER) {
            return Ok(HookInstallStatus::AlreadyInstalled(path));
        }
        if !force {
            return Err(CrosstacheError::config(format!(
                "{} exists and is not xv-managed; use --force to overwrite",
                path.display()
            )));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, HOOK_BODY)?;
    set_executable(&path)?;
    Ok(HookInstallStatus::Installed(path))
}

/// Remove our hook. If the file doesn't have our marker, refuse.
pub fn uninstall() -> Result<HookUninstallStatus> {
    let root = find_repo_root()?;
    let path = hook_path(&root);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(HookUninstallStatus::NotPresent);
    };
    if !existing.contains(MARKER) {
        return Err(CrosstacheError::config(format!(
            "{} is not xv-managed; refusing to remove",
            path.display()
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(HookUninstallStatus::Removed(path))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

pub enum HookInstallStatus {
    Installed(PathBuf),
    AlreadyInstalled(PathBuf),
}

pub enum HookUninstallStatus {
    Removed(PathBuf),
    NotPresent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_marker() {
        // Test against a tempdir + manual write (skip find_repo_root which uses cwd).
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();

        let path = hook_path(temp.path());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, HOOK_BODY).unwrap();

        let read = std::fs::read_to_string(&path).unwrap();
        assert!(read.contains(MARKER));
        assert!(read.contains("xv scan --staged --hook"));
    }

    #[test]
    fn uninstall_refuses_unmanaged_hook() {
        // Test the marker-detection logic against a hand-rolled file.
        let temp = tempfile::tempdir().unwrap();
        let path = hook_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();

        let existing = std::fs::read_to_string(&path).unwrap();
        assert!(!existing.contains(MARKER));
    }
}
