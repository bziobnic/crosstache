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

/// Scan all staged files. Each file's content comes from `git show :PATH`
/// (the index, not the working tree) so the scan reflects exactly what
/// will be committed.
pub fn scan_staged(engine: &MatchEngine) -> Result<Vec<Finding>> {
    let files = list_staged_files()?;
    let mut findings: Vec<Finding> = Vec::new();
    for f in &files {
        // Skip binary-looking paths heuristically by extension; the
        // index doesn't expose the raw bytes here.
        let lower = f.to_lowercase();
        const BIN_EXT: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".pdf", ".zip", ".gz", ".tar"];
        if BIN_EXT.iter().any(|e| lower.ends_with(e)) {
            continue;
        }
        let content = match read_staged_file(f) {
            Ok(c) => c,
            Err(_) => continue, // file might be deleted in this commit
        };
        findings.extend(engine.scan_text(Path::new(f), &content));
    }
    Ok(findings)
}
