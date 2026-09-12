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
use zeroize::Zeroizing;

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

/// Parses a record envelope into individually zeroizing sensitive values.
pub fn parse_sensitive_envelope(value: &str) -> Result<BTreeMap<String, Zeroizing<String>>> {
    serde_json::from_str(value).map_err(|_| {
        CrosstacheError::config("record envelope is not a JSON object of strings".to_string())
    })
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

    /// Disclosure canary through a REAL production failure path.
    ///
    /// `parse_envelope`/`parse_sensitive_envelope` are the functions that
    /// turn a decrypted record value into fields, so they are the first
    /// place a plaintext meets an error constructor: every malformed
    /// envelope they reject was built out of a secret value. `serde_json`'s
    /// own `Display` is happy to quote the input it choked on, so this pins
    /// that neither the wrapper message nor the underlying parser error
    /// echoes the envelope's contents.
    #[test]
    fn parse_errors_never_echo_the_envelope_contents() {
        const CANARY: &str = "disclosure-canary-7f3e";
        // Every shape these parsers reject, each carrying the canary.
        let malformed = [
            // Truncated object.
            format!(r#"{{"password":"{CANARY}""#),
            // Not an object.
            format!(r#"["{CANARY}"]"#),
            format!(r#""{CANARY}""#),
            // Object with a non-string value alongside a canary field.
            format!(r#"{{"password":"{CANARY}","port":5432}}"#),
            // Trailing garbage after a valid object.
            format!(r#"{{"password":"{CANARY}"}} trailing"#),
            // Not JSON at all.
            format!("password = {CANARY}"),
        ];
        for value in malformed {
            for rendered in [
                parse_envelope(&value)
                    .map(|_| ())
                    .map_err(|e| (format!("{e}"), format!("{e:?}"), e.code().to_string())),
                parse_sensitive_envelope(&value)
                    .map(|_| ())
                    .map_err(|e| (format!("{e}"), format!("{e:?}"), e.code().to_string())),
            ] {
                let (display, debug, code) =
                    rendered.expect_err(&format!("must be rejected: {value}"));
                assert!(
                    !display.contains(CANARY),
                    "parse error Display echoed the envelope: {display}"
                );
                assert!(
                    !debug.contains(CANARY),
                    "parse error Debug echoed the envelope: {debug}"
                );
                assert!(!code.contains(CANARY), "error code echoed the envelope");
            }
        }
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

    #[test]
    fn sensitive_parser_returns_zeroizing_values() {
        let mut fields = parse_sensitive_envelope(
            r#"{"password":"hunter2","one-time-code":"GEZDGNBVGY3TQOJQ"}"#,
        )
        .unwrap();
        let code = fields.remove("one-time-code").unwrap();
        assert_eq!(code.as_str(), "GEZDGNBVGY3TQOJQ");
        assert_eq!(fields["password"].as_str(), "hunter2");
    }

    #[test]
    fn sensitive_parser_keeps_the_strict_object_of_strings_contract() {
        for invalid in [r#"["value"]"#, r#"{"field":1}"#, r#"{"field":null}"#] {
            assert!(
                parse_sensitive_envelope(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn sensitive_parser_redacts_unexpected_scalar_values() {
        let uri = "otpauth://totp/Leak?secret=FULL-URI-SENTINEL&issuer=Leak";
        for (invalid, sentinel) in [(format!(r#""{uri}""#), uri), ("731904".into(), "731904")] {
            let message = parse_sensitive_envelope(&invalid).unwrap_err().to_string();
            assert!(
                message.contains("record envelope is not a JSON object of strings"),
                "{message}"
            );
            assert!(!message.contains(sentinel), "{message}");
        }
    }
}
