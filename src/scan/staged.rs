//! Pull staged file contents from `git diff --cached` for pre-commit scanning.

use crate::error::{CrosstacheError, Result};
use crate::scan::engine::MatchEngine;
use crate::scan::finding::Finding;
use std::path::Path;
use std::process::Command;

/// Run `git diff --cached --name-only -z` to enumerate staged files.
fn list_staged_files() -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["diff", "--cached", "--name-only", "-z"])
        .output()
        .map_err(|e| CrosstacheError::config(format!("failed to run git: {e}")))?;
    if !out.status.success() {
        return Err(CrosstacheError::config(format!(
            "git diff --cached failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Read the staged content of a file (post-staging, pre-commit).
fn read_staged_file(path: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["show", &format!(":{path}")])
        .output()
        .map_err(|e| CrosstacheError::config(format!("failed to run git show: {e}")))?;
    if !out.status.success() {
        return Err(CrosstacheError::config(format!(
            "git show :{path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Enumerate every file tracked at `HEAD` via `git ls-tree -r --name-only -z HEAD`.
fn list_head_files() -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", "-z", "HEAD"])
        .output()
        .map_err(|e| CrosstacheError::config(format!("failed to run git: {e}")))?;
    if !out.status.success() {
        return Err(CrosstacheError::config(format!(
            "git ls-tree HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Read a file's content at `HEAD` (the last commit, not the index or worktree).
fn read_head_file(path: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["show", &format!("HEAD:{path}")])
        .output()
        .map_err(|e| CrosstacheError::config(format!("failed to run git show: {e}")))?;
    if !out.status.success() {
        return Err(CrosstacheError::config(format!(
            "git show HEAD:{path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Heuristically skip binary-looking paths by extension; git object reads
/// don't expose raw bytes for a content-sniff here.
fn looks_binary(path: &str) -> bool {
    const BIN_EXT: &[&str] = &[
        ".png", ".jpg", ".jpeg", ".gif", ".pdf", ".zip", ".gz", ".tar",
    ];
    let lower = path.to_lowercase();
    BIN_EXT.iter().any(|e| lower.ends_with(e))
}

/// Scan a set of git-tracked paths, reading each one's content via `reader`
/// (the index for staged scans, `HEAD` for full-tree scans).
fn scan_git_paths<F>(files: &[String], reader: F, engine: &MatchEngine) -> Vec<Finding>
where
    F: Fn(&str) -> Result<String>,
{
    let mut findings: Vec<Finding> = Vec::new();
    for f in files {
        if looks_binary(f) {
            continue;
        }
        let content = match reader(f) {
            Ok(c) => c,
            Err(_) => continue, // file might be deleted in this revision
        };
        findings.extend(engine.scan_text(Path::new(f), &content));
    }
    findings
}

/// Scan all staged files. Each file's content comes from `git show :PATH`
/// (the index, not the working tree) so the scan reflects exactly what
/// will be committed.
pub fn scan_staged(engine: &MatchEngine) -> Result<Vec<Finding>> {
    let files = list_staged_files()?;
    Ok(scan_git_paths(&files, |p| read_staged_file(p), engine))
}

/// Scan every file tracked at `HEAD` (`xv scan --all`). Content comes from the
/// committed tree via `git show HEAD:PATH`, so this reflects what is already
/// committed rather than the working tree or index.
pub fn scan_head(engine: &MatchEngine) -> Result<Vec<Finding>> {
    let files = list_head_files()?;
    Ok(scan_git_paths(&files, |p| read_head_file(p), engine))
}
