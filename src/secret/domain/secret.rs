//! Secret value-bearing composites. See module docs in `mod.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;

use crate::secret::domain::SecretValue;

/// Value-free view of a secret: everything in [`SecretProperties`] except the
/// plaintext. This is what listings, web metadata responses, caches, and
/// logs may carry.
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretMetadata {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub version: String,
    /// Human-readable sequential version number (1 = oldest). None when not in a version list context.
    #[tabled(
        rename = "Version",
        display_with = "crate::secret::domain::metadata::display_version_number"
    )]
    pub version_number: Option<u32>,
    /// Raw Unix timestamp for sorting (not displayed)
    #[tabled(skip)]
    pub created_timestamp: i64,
    #[tabled(rename = "Created")]
    pub created_on: String,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(rename = "Enabled")]
    pub enabled: bool,
    #[tabled(skip)]
    pub expires_on: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub not_before: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub tags: HashMap<String, String>,
    #[tabled(rename = "Content Type")]
    pub content_type: String,
    #[tabled(skip)]
    pub recovery_level: Option<String>,
}

/// A secret with, optionally, its plaintext. Never serializable.
#[derive(Debug, Clone, Tabled)]
pub struct SecretProperties {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub value: Option<SecretValue>,
    #[tabled(skip)]
    pub version: String,
    /// Human-readable sequential version number (1 = oldest). None when not in a version list context.
    #[tabled(
        rename = "Version",
        display_with = "crate::secret::domain::metadata::display_version_number"
    )]
    pub version_number: Option<u32>,
    /// Raw Unix timestamp for sorting (not displayed)
    #[tabled(skip)]
    pub created_timestamp: i64,
    #[tabled(rename = "Created")]
    pub created_on: String,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(rename = "Enabled")]
    pub enabled: bool,
    #[tabled(skip)]
    pub expires_on: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub not_before: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub tags: HashMap<String, String>,
    #[tabled(rename = "Content Type")]
    pub content_type: String,
    #[tabled(skip)]
    pub recovery_level: Option<String>,
}

impl SecretProperties {
    /// Drop the plaintext, keeping every other field. The only way a
    /// `SecretProperties` becomes serializable.
    pub fn into_metadata(self) -> SecretMetadata {
        SecretMetadata {
            name: self.name,
            original_name: self.original_name,
            version: self.version,
            version_number: self.version_number,
            created_timestamp: self.created_timestamp,
            created_on: self.created_on,
            updated_on: self.updated_on,
            enabled: self.enabled,
            expires_on: self.expires_on,
            not_before: self.not_before,
            tags: self.tags,
            content_type: self.content_type,
            recovery_level: self.recovery_level,
        }
    }

    /// Borrowing variant of [`SecretProperties::into_metadata`], for callers
    /// that keep the properties (and their value) after taking the metadata.
    #[allow(dead_code)] // Public API of the library; the `xv` binary uses `into_metadata`.
    pub fn metadata(&self) -> SecretMetadata {
        self.clone().into_metadata()
    }
}

/// A secret value/metadata snapshot paired with an opaque, non-reusable
/// provider revision for generation/drift comparison. It is a compare-and-swap
/// token only when a separately advertised conditional operation guarantees that
/// contract. Callers must not infer ordering or expose provider internals.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct SecretSnapshot {
    pub properties: SecretProperties,
    pub revision: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::domain::{SecretRequest, SecretUpdateRequest, SecretValue};
    use std::collections::HashMap;

    const CANARY: &str = "super-secret-value-canary";

    fn props() -> SecretProperties {
        SecretProperties {
            name: "n".into(),
            original_name: "n".into(),
            value: Some(SecretValue::new(CANARY)),
            version: "v1".into(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: "2026-01-01".into(),
            updated_on: "2026-01-01".into(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::from([("k".to_string(), "v".to_string())]),
            content_type: "text/plain".into(),
            recovery_level: None,
        }
    }

    #[test]
    fn debug_of_every_value_bearing_type_is_redacted() {
        let p = props();
        assert!(!format!("{p:?}").contains(CANARY));
        assert!(format!("{p:?}").contains("[REDACTED]"));
        let snap = SecretSnapshot {
            properties: p.clone(),
            revision: "r".into(),
        };
        assert!(!format!("{snap:?}").contains(CANARY));
        let req = SecretRequest {
            name: "n".into(),
            value: SecretValue::new(CANARY),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        };
        assert!(!format!("{req:?}").contains(CANARY));
        let upd = SecretUpdateRequest {
            name: "n".into(),
            expected_revision: None,
            value: Some(SecretValue::new(CANARY)),
            content_type: None,
            enabled: None,
            expires_on: Default::default(),
            not_before: Default::default(),
            tags: None,
            groups: None,
            note: Default::default(),
            folder: Default::default(),
            replace_tags: false,
            replace_groups: false,
        };
        assert!(!format!("{upd:?}").contains(CANARY));
    }

    #[test]
    fn metadata_serializes_every_field_but_the_value() {
        let m = props().into_metadata();
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains(CANARY));
        assert!(!json.contains("\"value\""));
        assert!(json.contains("\"name\":\"n\""));
        assert!(json.contains("\"tags\":{\"k\":\"v\"}"));
        assert!(json.contains("\"content_type\":\"text/plain\""));
    }
}
