//! Secret creation and update requests. See module docs in `mod.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroize::Zeroizing;

use super::metadata::FieldUpdate;

/// Secret creation/update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequest {
    pub name: String,
    pub value: Zeroizing<String>,
    pub content_type: Option<String>,
    pub enabled: Option<bool>,
    pub expires_on: Option<DateTime<Utc>>,
    pub not_before: Option<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
    pub groups: Option<Vec<String>>,
    pub note: Option<String>,
    pub folder: Option<String>,
}

/// Secret update request for advanced operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretUpdateRequest {
    pub name: String,
    /// Internal compare-and-swap token. This is never accepted from serialized
    /// CLI/API input; only conditional backend entry points populate it.
    #[serde(skip)]
    pub expected_revision: Option<String>,
    pub value: Option<Zeroizing<String>>,
    pub content_type: Option<String>,
    pub enabled: Option<bool>,
    pub expires_on: FieldUpdate<DateTime<Utc>>,
    pub not_before: FieldUpdate<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
    pub groups: Option<Vec<String>>,
    pub note: FieldUpdate<String>,
    pub folder: FieldUpdate<String>,
    pub replace_tags: bool,
    pub replace_groups: bool,
}
