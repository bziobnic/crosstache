//! Scanner finding shape — what the engine emits per match.
//!
//! The cardinal invariant: a `Finding` NEVER contains the matched
//! value. Output formatters can serialize this struct with full
//! confidence — there's nothing sensitive in it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One match. Carries metadata only — never the matched bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    /// File path (relative to the scan root when possible, otherwise absolute).
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub col: usize,
    /// Name of the secret whose value matched, if known. `None` for
    /// regex-pattern matches that aren't tied to a specific secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,
    /// Vault that supplied the matched value, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    /// What kind of match fired.
    pub kind: FindingKind,
    /// How serious the match is (color/exit-policy hint).
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FindingKind {
    /// Verbatim secret value found in the file.
    #[default]
    SecretValue,
    /// Built-in regex pattern (AWS key, GitHub token, etc.).
    Pattern,
    /// High-entropy string above the threshold.
    HighEntropy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// User secret value match. Always blocks.
    #[default]
    Critical,
    /// Built-in pattern match.
    High,
    /// High-entropy heuristic. Often a false positive.
    Medium,
    /// Reserved.
    Low,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_serializes_to_expected_keys() {
        let f = Finding {
            file: PathBuf::from("src/config.js"),
            line: 42,
            col: 10,
            secret_name: Some("DB_PASSWORD".to_string()),
            vault: Some("dev-kv".to_string()),
            kind: FindingKind::SecretValue,
            severity: Severity::Critical,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["file"], "src/config.js");
        assert_eq!(json["line"], 42);
        assert_eq!(json["col"], 10);
        assert_eq!(json["secret_name"], "DB_PASSWORD");
        assert_eq!(json["vault"], "dev-kv");
        assert_eq!(json["kind"], "secret-value");
        assert_eq!(json["severity"], "critical");
    }

    #[test]
    fn finding_has_no_value_field() {
        // Structural guard: the on-disk shape must not carry a 'value'-ish key.
        let f = Finding::default();
        let json = serde_json::to_value(&f).unwrap();
        let banned = ["value", "secret", "password", "token", "raw", "match"];
        if let Some(obj) = json.as_object() {
            for key in obj.keys() {
                for b in banned {
                    assert!(
                        !key.to_lowercase().contains(b)
                            // 'secret_name' is allowed — it's the name, not the value
                            || key == "secret_name",
                        "Finding has a banned key: {key:?} contains {b:?}"
                    );
                }
            }
        } else {
            panic!("Finding must serialize as an object");
        }
    }
}
