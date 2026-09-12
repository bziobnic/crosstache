//! Secret creation and update requests. See module docs in `mod.rs`.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use super::metadata::FieldUpdate;
use super::value::SecretValue;

/// Secret creation/update request
#[derive(Debug, Clone)]
pub struct SecretRequest {
    pub name: String,
    pub value: SecretValue,
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
#[derive(Debug, Clone)]
pub struct SecretUpdateRequest {
    pub name: String,
    /// Internal compare-and-swap token. This is never accepted from serialized
    /// CLI/API input; only conditional backend entry points populate it.
    pub expected_revision: Option<String>,
    pub value: Option<SecretValue>,
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
