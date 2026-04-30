//! Scan engine: Aho-Corasick over secret values + regex patterns.
//! Pure; no I/O.

use crate::scan::finding::{Finding, FindingKind, Severity};
use crate::scan::patterns::BuiltinPattern;
use std::path::Path;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

/// Minimum length for a secret value to be added to the automaton.
/// Prevents single-letter / very-short secrets from generating noise.
pub const DEFAULT_MIN_VALUE_LENGTH: usize = 8;

/// Reference to a vault-side secret. Engine consumes a slice of these
/// and never exposes the values back through findings.
#[derive(Debug, Clone)]
pub struct SecretRef {
    pub name: String,
    pub vault: String,
    pub value: String,
}

/// Pre-built scan engine. Construct once per scan; reuse across files.
pub struct MatchEngine {
    /// Aho-Corasick automaton over secret values (and any literal
    /// pattern prefixes — none today, reserved for future).
    needles: Option<AhoCorasick>,
    /// Per-needle metadata so a hit can be turned back into a Finding.
    needle_meta: Vec<NeedleMeta>,
    /// Compiled built-in patterns.
    patterns: Vec<NeedlessPattern>,
}

struct NeedleMeta {
    secret_name: String,
    vault: String,
}

struct NeedlessPattern {
    name: &'static str,
    regex: regex::Regex,
    severity: Severity,
}

impl MatchEngine {
    /// Build the engine. `secrets` whose `value.len() < DEFAULT_MIN_VALUE_LENGTH`
    /// are skipped to avoid trivially-matched short strings.
    pub fn new(secrets: &[SecretRef], patterns: &[BuiltinPattern]) -> Self {
        let mut filtered: Vec<&SecretRef> = secrets
            .iter()
            .filter(|s| s.value.len() >= DEFAULT_MIN_VALUE_LENGTH)
            .collect();
        // Sort by descending value length so longest-leftmost matching
        // produces the longest match when two secrets share a prefix.
        filtered.sort_by_key(|s| std::cmp::Reverse(s.value.len()));

        let needles = if filtered.is_empty() {
            None
        } else {
            let patterns_vec: Vec<&str> =
                filtered.iter().map(|s| s.value.as_str()).collect();
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(patterns_vec)
                    .expect("Aho-Corasick build must succeed"),
            )
        };
        let needle_meta = filtered
            .iter()
            .map(|s| NeedleMeta {
                secret_name: s.name.clone(),
                vault: s.vault.clone(),
            })
            .collect();

        let compiled_patterns = patterns
            .iter()
            .map(|p| NeedlessPattern {
                name: p.name,
                regex: p.regex.clone(),
                severity: p.severity,
            })
            .collect();

        Self {
            needles,
            needle_meta,
            patterns: compiled_patterns,
        }
    }

    /// Scan a text blob. Returns findings in source-order (line then col).
    pub fn scan_text(&self, file: &Path, content: &str) -> Vec<Finding> {
        let mut findings: Vec<Finding> = Vec::new();

        // Track byte-offsets covered by user-secret matches so pattern
        // matches at the same offset can be suppressed (user-secret
        // wins per Task 4 spec).
        let mut covered: Vec<(usize, usize)> = Vec::new();

        if let Some(ac) = &self.needles {
            for m in ac.find_iter(content) {
                let pat_id = m.pattern().as_usize();
                let (line, col) = byte_offset_to_line_col(content, m.start());
                let meta = &self.needle_meta[pat_id];
                covered.push((m.start(), m.end()));
                findings.push(Finding {
                    file: file.to_path_buf(),
                    line,
                    col,
                    secret_name: Some(meta.secret_name.clone()),
                    vault: Some(meta.vault.clone()),
                    kind: FindingKind::SecretValue,
                    severity: Severity::Critical,
                });
            }
        }

        for p in &self.patterns {
            for m in p.regex.find_iter(content) {
                if covered
                    .iter()
                    .any(|&(s, e)| m.start() < e && m.end() > s)
                {
                    // Already covered by a user-secret match.
                    continue;
                }
                let (line, col) = byte_offset_to_line_col(content, m.start());
                let kind = if p.name == "high-entropy" {
                    FindingKind::HighEntropy
                } else {
                    FindingKind::Pattern
                };
                findings.push(Finding {
                    file: file.to_path_buf(),
                    line,
                    col,
                    secret_name: None,
                    vault: None,
                    kind,
                    severity: p.severity,
                });
            }
        }

        // Sort: line asc, col asc.
        findings.sort_by_key(|f| (f.line, f.col));
        findings
    }
}

/// Convert a byte offset into (1-based line, 1-based col).
fn byte_offset_to_line_col(content: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut last_newline_at: usize = 0;
    for (i, b) in content.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            last_newline_at = i + 1;
        }
    }
    let col = offset.saturating_sub(last_newline_at) + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::patterns::builtin_patterns;

    #[test]
    fn matches_user_secret_value_with_name() {
        let secrets = vec![SecretRef {
            name: "DB_PASSWORD".to_string(),
            vault: "dev-kv".to_string(),
            value: "hunter2-very-long-password".to_string(),
        }];
        let engine = MatchEngine::new(&secrets, &[]);
        let findings = engine.scan_text(
            Path::new("src/config.rs"),
            "let db_pw = \"hunter2-very-long-password\";\n",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.secret_name.as_deref(), Some("DB_PASSWORD"));
        assert_eq!(f.vault.as_deref(), Some("dev-kv"));
        assert_eq!(f.kind, FindingKind::SecretValue);
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.line, 1);
        assert!(f.col >= 14, "col is 1-based and points at the match start");
    }

    #[test]
    fn skips_short_values_below_min_length() {
        // Default min length is 8 — values shorter than that aren't
        // added to the automaton (avoids matching "abc" everywhere).
        let secrets = vec![SecretRef {
            name: "SHORT".to_string(),
            vault: "v".to_string(),
            value: "abc".to_string(),
        }];
        let engine = MatchEngine::new(&secrets, &[]);
        let findings = engine.scan_text(Path::new("x"), "abc abc abc abc");
        assert!(findings.is_empty(), "short secret values must be skipped");
    }

    #[test]
    fn matches_aws_key_pattern_when_no_secret_overlaps() {
        let secrets: Vec<SecretRef> = vec![];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(&secrets, &patterns);
        let findings = engine.scan_text(
            Path::new("creds.txt"),
            "AWS_KEY=AKIAIOSFODNN7EXAMPLE\n",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.kind, FindingKind::Pattern);
        assert_eq!(f.secret_name, None, "pattern matches have no secret_name");
    }

    #[test]
    fn user_secret_match_wins_over_pattern_at_same_offset() {
        // If a secret value happens to also match a pattern, the
        // user-secret match wins (Critical severity, secret_name
        // populated).
        let secrets = vec![SecretRef {
            name: "API_KEY".to_string(),
            vault: "v".to_string(),
            value: "AKIAIOSFODNN7EXAMPLE".to_string(),
        }];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(&secrets, &patterns);
        let findings = engine.scan_text(
            Path::new("x"),
            "key = \"AKIAIOSFODNN7EXAMPLE\";",
        );
        assert!(!findings.is_empty());
        let f = &findings[0];
        assert_eq!(f.secret_name.as_deref(), Some("API_KEY"));
        assert_eq!(f.kind, FindingKind::SecretValue);
    }

    #[test]
    fn line_and_col_calculation() {
        let secrets = vec![SecretRef {
            name: "X".to_string(),
            vault: "v".to_string(),
            value: "needle12345".to_string(),
        }];
        let engine = MatchEngine::new(&secrets, &[]);
        let content = "line1\nline2 needle12345 line2 cont\nline3";
        let findings = engine.scan_text(Path::new("x"), content);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.line, 2);
        assert_eq!(f.col, 7); // 1-based; 'l','i','n','e','2',' ' = 6 chars, then col 7
    }
}
