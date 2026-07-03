//! JSON envelope codec for record secret-kind fields, plus the reserved
//! tag/content-type constants that mark a secret as a record.
//!
//! Consumed by the `xv set --type` / `xv get --field`/`--record` CLI
//! wiring added later in Phase A (record-types plan Tasks 6/7); until
//! that wiring lands, this module's public API is unused from the `xv`
//! binary target, hence the crate-wide `#[allow(dead_code)]` below.
#![allow(dead_code)]

use crate::error::{CrosstacheError, Result};
use std::collections::BTreeMap;

/// Content type marker that decides record-ness. Never inferred by JSON
/// sniffing — only an exact content-type match makes a secret a record.
pub const RECORD_CONTENT_TYPE: &str = "application/vnd.xv.record";

/// Reserved tag holding the record's type name.
pub const TYPE_TAG: &str = "xv-type";

/// Prefix for metadata-field tags, e.g. `f.username`.
pub const FIELD_TAG_PREFIX: &str = "f.";

/// Encodes secret-kind fields as a deterministic JSON object (sorted keys,
/// via `BTreeMap`'s iteration order).
pub fn encode_envelope(fields: &BTreeMap<String, String>) -> Result<String> {
    serde_json::to_string(fields)
        .map_err(|e| CrosstacheError::config(format!("failed to encode record envelope: {e}")))
}

/// Parses a record envelope. Strict: the value must be a JSON object whose
/// values are all strings.
pub fn parse_envelope(value: &str) -> Result<BTreeMap<String, String>> {
    let parsed: serde_json::Value = serde_json::from_str(value).map_err(|e| {
        CrosstacheError::config(format!(
            "record envelope is not a JSON object of strings: {e}"
        ))
    })?;

    let obj = parsed.as_object().ok_or_else(|| {
        CrosstacheError::config("record envelope is not a JSON object of strings".to_string())
    })?;

    let mut fields = BTreeMap::new();
    for (key, val) in obj {
        let s = val.as_str().ok_or_else(|| {
            CrosstacheError::config(format!(
                "record envelope is not a JSON object of strings: field '{key}' is not a string"
            ))
        })?;
        fields.insert(key.clone(), s.to_string());
    }

    Ok(fields)
}

/// Returns true iff `content_type` exactly matches [`RECORD_CONTENT_TYPE`].
pub fn is_record(content_type: &str) -> bool {
    content_type == RECORD_CONTENT_TYPE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_fields() {
        let mut fields = BTreeMap::new();
        fields.insert("password".to_string(), "hunter2".to_string());
        fields.insert(
            "connection-string".to_string(),
            "postgres://...".to_string(),
        );

        let encoded = encode_envelope(&fields).unwrap();
        let decoded = parse_envelope(&encoded).unwrap();
        assert_eq!(decoded, fields);
    }

    #[test]
    fn parse_rejects_non_object() {
        assert!(parse_envelope("[1,2]").is_err());
        assert!(parse_envelope("\"str\"").is_err());
    }

    #[test]
    fn parse_rejects_non_string_values() {
        assert!(parse_envelope(r#"{"a":1}"#).is_err());
    }

    #[test]
    fn is_record_matches_exactly() {
        assert!(is_record("application/vnd.xv.record"));
        assert!(!is_record("application/json"));
        assert!(!is_record(""));
        assert!(!is_record("text/plain"));
    }

    #[test]
    fn encode_is_deterministic() {
        let mut a = BTreeMap::new();
        a.insert("b".to_string(), "2".to_string());
        a.insert("a".to_string(), "1".to_string());

        let mut b = BTreeMap::new();
        b.insert("a".to_string(), "1".to_string());
        b.insert("b".to_string(), "2".to_string());

        assert_eq!(encode_envelope(&a).unwrap(), encode_envelope(&b).unwrap());
    }
}
