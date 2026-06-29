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

use crate::error::{CrosstacheError, Result};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;

/// Walker configuration. Empty = use defaults.
#[derive(Debug, Clone, Default)]
pub struct WalkConfig {
    /// Extra exclude globs on top of `DEFAULT_EXCLUDES`.
    pub extra_excludes: Vec<String>,
}

/// Build the scanner's exclude globset: [`DEFAULT_EXCLUDES`] plus any extra
/// globs (e.g. `[scan].exclude` from `.xv.toml`).
///
/// Shared by the filesystem walker and the git-tree scans (`scan --all`) so
/// every scan source applies the same exclusion rules. Match paths against it
/// relative to the scan root / repo root.
pub fn build_exclude_set(extra_excludes: &[String]) -> Result<globset::GlobSet> {
    let mut gs = GlobSetBuilder::new();
    for g in DEFAULT_EXCLUDES
        .iter()
        .copied()
        .chain(extra_excludes.iter().map(|s| s.as_str()))
    {
        let glob = Glob::new(g).map_err(|e| {
            CrosstacheError::config(format!("invalid scan exclude glob '{g}': {e}"))
        })?;
        gs.add(glob);
    }
    gs.build()
        .map_err(|e| CrosstacheError::config(format!("scan glob build failed: {e}")))
}

/// Walk one or more roots and return the paths to scan, with all the
/// exclusion rules applied (gitignore, xvignore, defaults, custom,
/// binary skip).
pub fn walk(roots: &[&Path], cfg: &WalkConfig) -> Result<Vec<PathBuf>> {
    let excludes = build_exclude_set(&cfg.extra_excludes)?;

    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        let walker = WalkBuilder::new(root)
            .add_custom_ignore_filename(".xvignore")
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            // Apply user exclude globs against the path relative to root.
            let rel = path.strip_prefix(root).unwrap_or(path);
            if excludes.is_match(rel) {
                continue;
            }
            // Skip binary files.
            if is_binary_file(path) {
                continue;
            }
            out.push(path.to_path_buf());
        }
    }
    Ok(out)
}

/// Quick magic-byte check: read the first 8KB and return true if any
/// NUL byte is present, OR if the prefix matches one of a few common
/// binary magic numbers (ELF, Mach-O thin/fat, PE, ZIP, PNG, JPEG, GIF).
fn is_binary_file(path: &Path) -> bool {
    let mut buf = [0u8; 8192];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let n = f.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return false;
    }
    let head = &buf[..n];
    // NUL byte heuristic.
    if head.contains(&0u8) {
        return true;
    }
    // Magic numbers.
    const MAGIC: &[&[u8]] = &[
        &[0x7F, 0x45, 0x4C, 0x46], // ELF
        &[0xFE, 0xED, 0xFA, 0xCE], // Mach-O 32 BE
        &[0xCE, 0xFA, 0xED, 0xFE], // Mach-O 32 LE
        &[0xFE, 0xED, 0xFA, 0xCF], // Mach-O 64 BE
        &[0xCF, 0xFA, 0xED, 0xFE], // Mach-O 64 LE
        &[0x4D, 0x5A],             // PE / DOS
        &[0x50, 0x4B, 0x03, 0x04], // ZIP
        &[0x89, 0x50, 0x4E, 0x47], // PNG
        &[0xFF, 0xD8, 0xFF],       // JPEG
        &[0x47, 0x49, 0x46, 0x38], // GIF
    ];
    MAGIC.iter().any(|m| head.starts_with(m))
}

/// Read `.xvignore` (gitignore syntax) from the given dir if present.
/// Returns the parsed lines verbatim; the walker hands them to the
/// `ignore` crate.
///
/// Currently unused — the `ignore::WalkBuilder::add_custom_ignore_filename`
/// path in `walk()` handles `.xvignore` discovery natively, so this manual
/// reader isn't called. Kept exposed for callers that want raw entries
/// (e.g., a future `xv scan show-excludes` debug command).
#[allow(dead_code)]
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

    #[test]
    fn build_exclude_set_matches_defaults_and_custom() {
        let set = build_exclude_set(&["secrets/**".to_string()]).unwrap();
        // Defaults
        assert!(set.is_match("target/debug/app"));
        assert!(set.is_match("node_modules/pkg/index.js"));
        assert!(set.is_match("Cargo.lock"));
        assert!(set.is_match(".git/config"));
        // Custom [scan].exclude glob
        assert!(set.is_match("secrets/prod.env"));
        // Normal source is not excluded
        assert!(!set.is_match("src/main.rs"));
    }

    #[test]
    fn walk_returns_text_files_under_root() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(temp.path().join("sub")).unwrap();
        std::fs::write(temp.path().join("sub/b.txt"), "world").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.txt".to_string()));
    }

    #[test]
    fn walk_skips_default_excludes() {
        let temp = tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        std::fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::create_dir_all(temp.path().join("target/debug")).unwrap();
        std::fs::write(temp.path().join("target/debug/build.lock"), "x").unwrap();
        std::fs::write(temp.path().join("good.txt"), "ok").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let paths: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("good.txt")));
        for p in &paths {
            assert!(!p.contains(".git/"), "must not include .git: {p}");
            assert!(!p.contains("target/"), "must not include target: {p}");
        }
    }

    #[test]
    fn walk_honors_xvignore() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join(".xvignore"), "ignored.txt\n").unwrap();
        std::fs::write(temp.path().join("ignored.txt"), "hide me").unwrap();
        std::fs::write(temp.path().join("kept.txt"), "scan me").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"kept.txt".to_string()));
        assert!(!names.contains(&"ignored.txt".to_string()));
    }

    #[test]
    fn walk_skips_binary_files() {
        let temp = tempdir().unwrap();
        // ELF magic prefix → binary
        std::fs::write(
            temp.path().join("binary.bin"),
            [0x7Fu8, b'E', b'L', b'F', 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        std::fs::write(temp.path().join("text.txt"), "hello").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"text.txt".to_string()));
        assert!(!names.contains(&"binary.bin".to_string()));
    }
}
