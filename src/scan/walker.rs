//! File walker for the scanner. Honors:
//! - .gitignore (via the `ignore` crate)
//! - .xvignore (line-based, .gitignore syntax, scanner-specific)
//! - [scan].exclude globs from .xv.toml
//! - Built-in defaults (.git/**, target/**, dist/**, node_modules/**)
//! - Binary-file skip (magic-byte check)

#[allow(unused_imports)]
use std::path::{Path, PathBuf};

/// Default exclude globs, applied on top of any user config.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    "target/**",
    "dist/**",
    "node_modules/**",
    "*.lock",
    "*.min.*",
];

/// Read `.xvignore` (gitignore syntax) from the given dir if present.
/// Returns the parsed lines verbatim; the walker hands them to the
/// `ignore` crate.
pub fn read_xvignore(dir: &Path) -> Vec<String> {
    let path = dir.join(".xvignore");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_xvignore_returns_empty_for_missing_file() {
        let temp = tempdir().unwrap();
        let lines = read_xvignore(temp.path());
        assert!(lines.is_empty());
    }

    #[test]
    fn read_xvignore_strips_comments_and_blank_lines() {
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path().join(".xvignore"),
            "# comment\n\ntarget/\n# another\n*.bak\n",
        )
        .unwrap();
        let lines = read_xvignore(temp.path());
        assert_eq!(lines, vec!["target/", "*.bak"]);
    }
}
