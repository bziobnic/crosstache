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
///
/// `excludes` carries the same default + `[scan].exclude` globs the filesystem
/// walker and `scan_head` apply (see `walker::build_exclude_set`), so
/// `--staged` (and therefore the installed pre-commit hook) doesn't scan
/// `target/`, `node_modules/`, or user-excluded paths that `scan .` skips.
pub fn scan_staged(engine: &MatchEngine, excludes: &globset::GlobSet) -> Result<Vec<Finding>> {
    let files: Vec<String> = list_staged_files()?
        .into_iter()
        .filter(|f| !excludes.is_match(f))
        .collect();
    Ok(scan_git_paths(&files, read_staged_file, engine))
}

/// Scan every file tracked at `HEAD` (`xv scan --all`). Content comes from the
/// committed tree via `git show HEAD:PATH`, so this reflects what is already
/// committed rather than the working tree or index.
///
/// `excludes` carries the same default + `[scan].exclude` globs the filesystem
/// walker applies (see `walker::build_exclude_set`), so `--all` does not scan
/// `target/`, `node_modules/`, or user-excluded paths that `scan .` skips.
pub fn scan_head(engine: &MatchEngine, excludes: &globset::GlobSet) -> Result<Vec<Finding>> {
    let files: Vec<String> = list_head_files()?
        .into_iter()
        .filter(|f| !excludes.is_match(f))
        .collect();
    Ok(scan_git_paths(&files, read_head_file, engine))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::engine::{MatchEngine, SecretRef, DEFAULT_MIN_VALUE_LENGTH};
    use crate::scan::patterns::builtin_patterns;
    use crate::scan::walker::build_exclude_set;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// `list_staged_files`/`read_staged_file` shell out to `git` using the
    /// process's current working directory (mirroring the installed
    /// pre-commit hook, which always runs with cwd = repo root). Tests that
    /// exercise `scan_staged` against a real git fixture must therefore
    /// change the process-global cwd; this lock serializes them against each
    /// other. No other test in this binary changes cwd (see installer.rs's
    /// tests, which deliberately avoid cwd-dependent code paths).
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git command must spawn");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.invalid"]);
        git(dir, &["config", "user.name", "test"]);
    }

    /// Issue #309 Finding 7: `scan --staged` (and therefore the installed
    /// pre-commit hook) must honor `[scan].exclude`, exactly like `scan .`
    /// and `scan --all` already do.
    #[test]
    fn scan_staged_honors_excludes() {
        let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempdir().unwrap();
        let dir = temp.path();
        init_repo(dir);

        // A staged file whose content matches the built-in AWS key pattern.
        std::fs::write(dir.join("leak.env"), "AWS_KEY=AKIAIOSFODNN7EXAMPLE\n").unwrap();
        git(dir, &["add", "leak.env"]);

        let secrets: Vec<SecretRef> = vec![];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(&secrets, &patterns, DEFAULT_MIN_VALUE_LENGTH);

        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();

        // Sanity check: without excludes, the staged leak is found.
        let no_excludes = build_exclude_set(&[]).unwrap();
        let unfiltered = scan_staged(&engine, &no_excludes).unwrap();

        // With [scan].exclude = ["*.env"], the same staged file is skipped.
        let with_excludes = build_exclude_set(&["*.env".to_string()]).unwrap();
        let filtered = scan_staged(&engine, &with_excludes).unwrap();

        std::env::set_current_dir(&original_cwd).unwrap();

        assert!(
            !unfiltered.is_empty(),
            "sanity check: staged leak.env should produce a finding without excludes"
        );
        assert!(
            filtered.is_empty(),
            "excluded staged file must not produce findings via scan_staged"
        );
    }
}
