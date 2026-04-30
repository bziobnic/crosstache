//! Built-in regex pattern set: ~7 families covering common provider
//! secret formats. The headline use case is user-secret-value matching;
//! these patterns are a *small* safety net for cases where the value
//! isn't fetched (e.g., new vaults, local-only secrets).

use regex::Regex;
use crate::scan::finding::Severity;

/// One built-in regex pattern.
pub struct BuiltinPattern {
    /// Stable identifier; used as the kind label in CLI output.
    pub name: &'static str,
    /// Compiled regex.
    pub regex: Regex,
    /// Default severity assigned to findings of this kind.
    pub severity: Severity,
}

/// The full built-in pattern set. Built lazily on first call;
/// callers should stash the returned Vec.
pub fn builtin_patterns() -> Vec<BuiltinPattern> {
    fn r(name: &'static str, pat: &str, severity: Severity) -> BuiltinPattern {
        BuiltinPattern {
            name,
            regex: Regex::new(pat).expect("built-in regex must compile"),
            severity,
        }
    }
    vec![
        // AWS access key IDs (AKIA + 16 uppercase alphanumerics).
        r(
            "aws-access-key-id",
            r"\bAKIA[0-9A-Z]{16}\b",
            Severity::High,
        ),
        // GitHub PAT — ghp_<36 chars>; also ghs_, gho_, ghu_, ghr_.
        r(
            "github-token",
            r"\bgh[posru]_[A-Za-z0-9]{36,255}\b",
            Severity::High,
        ),
        // Stripe live or test secret key.
        r(
            "stripe-secret-key",
            r"\bsk_(live|test)_[A-Za-z0-9]{24,99}\b",
            Severity::High,
        ),
        // Slack tokens (xoxb-, xoxp-, xoxa-, xoxr-, xoxs-).
        r(
            "slack-token",
            r"\bxox[bpoars]-[A-Za-z0-9-]{10,255}\b",
            Severity::High,
        ),
        // JWTs: three base64url segments separated by '.'.
        r(
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            Severity::Medium,
        ),
        // SSH/PEM private-key headers.
        r(
            "ssh-private-key",
            r"-----BEGIN (?:OPENSSH |RSA |DSA |EC |PGP )?PRIVATE KEY-----",
            Severity::High,
        ),
        // High-entropy fallback: long base64-ish or hex-ish runs.
        // 32+ chars from base64url alphabet.
        r(
            "high-entropy",
            r"\b[A-Za-z0-9+/_-]{32,}\b",
            Severity::Medium,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn match_count(p: &BuiltinPattern, hay: &str) -> usize {
        p.regex.find_iter(hay).count()
    }

    #[test]
    fn aws_access_key_id_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "aws-access-key-id")
            .unwrap();
        assert_eq!(match_count(&p, "AKIAIOSFODNN7EXAMPLE"), 1);
        assert_eq!(match_count(&p, "akia7nope"), 0, "must be uppercase");
    }

    #[test]
    fn github_token_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "github-token")
            .unwrap();
        // ghp_ prefix tokens are 36 chars after the prefix
        let token = "ghp_1234567890abcdefghijklmnopqrstuvwxyz";
        assert_eq!(match_count(&p, token), 1);
    }

    #[test]
    fn stripe_secret_key_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "stripe-secret-key")
            .unwrap();
        let key = &format!("{}_live_{}", "sk", "x".repeat(24));
        assert_eq!(match_count(&p, key), 1);
        let test_key = &format!("{}_test_{}", "sk", "x".repeat(24));
        assert_eq!(match_count(&p, test_key), 1, "test mode also matches");
    }

    #[test]
    fn slack_token_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "slack-token")
            .unwrap();
        let bot = "xoxb-12345-67890-abcdefABCDEF1234567890";
        assert_eq!(match_count(&p, bot), 1);
    }

    #[test]
    fn jwt_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "jwt")
            .unwrap();
        // Three base64url segments separated by '.'
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(match_count(&p, jwt), 1);
    }

    #[test]
    fn ssh_private_key_header_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "ssh-private-key")
            .unwrap();
        let header = "-----BEGIN OPENSSH PRIVATE KEY-----";
        assert_eq!(match_count(&p, header), 1);
        let rsa = "-----BEGIN RSA PRIVATE KEY-----";
        assert_eq!(match_count(&p, rsa), 1);
    }

    #[test]
    fn no_pattern_matches_innocuous_text() {
        let patterns = builtin_patterns();
        let hay = "This is just normal English text with no secrets in it. \
                   The quick brown fox jumps over the lazy dog 1234567890.";
        for p in &patterns {
            if p.name == "high-entropy" {
                continue; // tested separately
            }
            assert_eq!(
                match_count(p, hay),
                0,
                "pattern {} matched innocuous text",
                p.name
            );
        }
    }

    #[test]
    fn builtin_patterns_have_distinct_names() {
        use std::collections::HashSet;
        let names: HashSet<&str> = builtin_patterns().iter().map(|p| p.name).collect();
        assert_eq!(
            names.len(),
            builtin_patterns().len(),
            "duplicate pattern names"
        );
    }
}
