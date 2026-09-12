//! Secret value-bearing composites. See module docs in `mod.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;
use zeroize::Zeroizing;

/// Secret properties and metadata
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretProperties {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub value: Option<Zeroizing<String>>,
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
