//! Secret value-bearing composites. See module docs in `mod.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;

use crate::secret::domain::SecretValue;

/// Value-free view of a secret: everything in [`Secret`] except the
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

/// Whether a snapshot read should disclose the plaintext value.
///
/// An enum rather than a `bool` so the call site reads as intent and cannot be
/// flipped by an argument-order mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotValue {
    /// Read metadata and the revision only; the provider value is not fetched.
    Omit,
    /// Read the complete generation, including the plaintext value.
    Include,
}

/// A secret together with its plaintext. Never serializable.
///
/// The value is not optional: a `Secret` exists only where a value method on
/// [`crate::backend::SecretBackend`] produced one, so "was a value requested?"
/// is a type fact rather than a runtime check. Value-free reads return
/// [`SecretMetadata`] instead.
///
/// `Deref`/`DerefMut` to [`SecretMetadata`] keep `secret.name`, `secret.tags`,
/// and friends working exactly as they did on the old flat struct.
#[derive(Debug, Clone)]
pub struct Secret {
    pub metadata: SecretMetadata,
    pub value: SecretValue,
}

impl std::ops::Deref for Secret {
    type Target = SecretMetadata;

    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}

impl std::ops::DerefMut for Secret {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.metadata
    }
}

impl Secret {
    /// Drop the plaintext, keeping every other field. The only way a
    /// [`Secret`] becomes serializable.
    pub fn into_metadata(self) -> SecretMetadata {
        self.metadata
    }

    /// Borrowing variant of [`Secret::into_metadata`], for callers that keep
    /// the secret (and its value) after taking the metadata.
    #[allow(dead_code)] // Public API of the library; the `xv` binary uses `into_metadata`.
    pub fn metadata(&self) -> SecretMetadata {
        self.metadata.clone()
    }

    /// Split into the value-free half and the plaintext.
    pub fn into_parts(self) -> (SecretMetadata, SecretValue) {
        (self.metadata, self.value)
    }
}

/// A secret metadata (and optionally value) snapshot paired with an opaque,
/// non-reusable provider revision for generation/drift comparison. It is a
/// compare-and-swap token only when a separately advertised conditional
/// operation guarantees that contract. Callers must not infer ordering or
/// expose provider internals.
///
/// `value` is `Some` exactly when the read asked for
/// [`SnapshotValue::Include`].
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct SecretSnapshot {
    pub metadata: SecretMetadata,
    pub value: Option<SecretValue>,
    pub revision: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::domain::{SecretRequest, SecretUpdateRequest, SecretValue};
    use std::collections::HashMap;

    const CANARY: &str = "super-secret-value-canary";

    fn metadata() -> SecretMetadata {
        SecretMetadata {
            name: "n".into(),
            original_name: "n".into(),
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

    fn secret() -> Secret {
        Secret {
            metadata: metadata(),
            value: SecretValue::new(CANARY),
        }
    }

    #[test]
    fn into_parts_round_trips_metadata_and_value() {
        let (meta, value) = secret().into_parts();
        assert_eq!(meta.name, "n");
        assert_eq!(meta.version, "v1");
        assert_eq!(value.expose_secret(), CANARY);

        let rebuilt = Secret {
            metadata: meta,
            value,
        };
        assert_eq!(rebuilt.metadata.name, "n");
        assert_eq!(rebuilt.value.expose_secret(), CANARY);
    }

    #[test]
    fn deref_exposes_metadata_fields_without_the_value() {
        let mut s = secret();
        assert_eq!(s.name, "n");
        assert_eq!(s.content_type, "text/plain");
        assert_eq!(s.tags.get("k").map(String::as_str), Some("v"));
        s.enabled = false;
        assert!(!s.metadata.enabled);
        assert_eq!(s.metadata().name, "n");
        assert_eq!(s.into_metadata().name, "n");
    }

    #[test]
    fn snapshot_value_is_copy_and_eq() {
        let a = SnapshotValue::Include;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(SnapshotValue::Include, SnapshotValue::Omit);
    }

    #[test]
    fn debug_of_every_value_bearing_type_is_redacted() {
        let s = secret();
        assert!(!format!("{s:?}").contains(CANARY));
        assert!(format!("{s:?}").contains("[REDACTED]"));
        let snap = SecretSnapshot {
            metadata: metadata(),
            value: Some(SecretValue::new(CANARY)),
            revision: "r".into(),
        };
        assert!(!format!("{snap:?}").contains(CANARY));
        assert!(format!("{snap:?}").contains("[REDACTED]"));
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
        let m = secret().into_metadata();
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains(CANARY));
        assert!(!json.contains("\"value\""));
        assert!(json.contains("\"name\":\"n\""));
        assert!(json.contains("\"tags\":{\"k\":\"v\"}"));
        assert!(json.contains("\"content_type\":\"text/plain\""));
    }
}
