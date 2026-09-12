//! Value-free secret metadata. See module docs in `mod.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;

use crate::error::{CrosstacheError, Result};

/// Tri-state update for an optional metadata field: leave it as-is, set a
/// new value, or remove the current value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldUpdate<T> {
    #[default]
    Unchanged,
    Set(T),
    Clear,
}

impl<T> FieldUpdate<T> {
    /// Build from a CLI-style `(value, clear)` flag pair. Supplying both a
    /// value and the clear flag for the same field is an error.
    pub fn from_flags(value: Option<T>, clear: bool, field: &str) -> Result<Self> {
        match (value, clear) {
            (Some(_), true) => Err(CrosstacheError::invalid_argument(format!(
                "Cannot set and clear {field} in the same update"
            ))),
            (Some(v), false) => Ok(FieldUpdate::Set(v)),
            (None, true) => Ok(FieldUpdate::Clear),
            (None, false) => Ok(FieldUpdate::Unchanged),
        }
    }

    /// Resolve against the current value: `Unchanged` preserves it,
    /// `Set` replaces it, `Clear` removes it.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            FieldUpdate::Unchanged => current,
            FieldUpdate::Set(v) => Some(v),
            FieldUpdate::Clear => None,
        }
    }

    pub fn is_unchanged(&self) -> bool {
        matches!(self, FieldUpdate::Unchanged)
    }
}

/// Attribute/tag-only update for
/// [`crate::secret::manager::SecretOperations::update_secret_attributes`].
///
/// `None` fields are left unchanged by the backend. `tags`, when `Some`,
/// replaces the entire tag map (Azure `PATCH /secrets/{name}` semantics), so
/// callers must supply the full desired map including crosstache's metadata
/// tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretAttributesUpdate {
    pub enabled: Option<bool>,
    pub content_type: Option<String>,
    pub expires_on: Option<DateTime<Utc>>,
    pub not_before: Option<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
}

/// Display function for optional version number (e.g. Some(3) → "v3", None → "-")
pub(crate) fn display_version_number(v: &Option<u32>) -> String {
    match v {
        Some(n) => format!("v{n}"),
        None => "-".to_string(),
    }
}

/// Display function for optional group
pub(crate) fn display_optional_group(option: &Option<String>) -> String {
    option
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

/// Secret summary for list operations
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretSummary {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(skip)]
    pub original_name: String,
    #[tabled(rename = "Note", display_with = "display_optional_group")]
    pub note: Option<String>,
    #[tabled(rename = "Folder", display_with = "display_optional_group")]
    pub folder: Option<String>,
    #[tabled(rename = "Groups", display_with = "display_optional_group")]
    pub groups: Option<String>,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(skip)]
    pub enabled: bool,
    /// Canonical display-safe expiry metadata for list/filter consumers.
    #[tabled(skip)]
    #[serde(default)]
    pub expires_on: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub content_type: String,
    /// Full tag map, used to derive record-types metadata (`xv-type`,
    /// `f.*` fields) for `ls --type` filtering and JSON field lifting
    /// (record-types plan Task 10). `#[serde(default)]` so summaries
    /// deserialized from an older cache entry (written before this field
    /// existed) still parse.
    #[tabled(skip)]
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

/// Summary of a soft-deleted secret awaiting purge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedSecretSummary {
    pub name: String,
    pub original_name: String,
    /// When the secret was deleted (backend-formatted timestamp), when known.
    pub deleted_on: Option<String>,
    /// When the backend will permanently purge it (None = no schedule).
    pub scheduled_purge_on: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_update_from_flags() {
        assert_eq!(
            FieldUpdate::from_flags(Some("x".to_string()), false, "note").unwrap(),
            FieldUpdate::Set("x".to_string())
        );
        assert_eq!(
            FieldUpdate::from_flags(None::<String>, true, "note").unwrap(),
            FieldUpdate::Clear
        );
        assert_eq!(
            FieldUpdate::from_flags(None::<String>, false, "note").unwrap(),
            FieldUpdate::Unchanged
        );
        // Set + clear together is an error
        assert!(FieldUpdate::from_flags(Some("x".to_string()), true, "note").is_err());
    }

    #[test]
    fn test_field_update_apply() {
        assert_eq!(FieldUpdate::Unchanged.apply(Some(1)), Some(1));
        assert_eq!(FieldUpdate::<i32>::Unchanged.apply(None), None);
        assert_eq!(FieldUpdate::Set(2).apply(Some(1)), Some(2));
        assert_eq!(FieldUpdate::Set(2).apply(None), Some(2));
        assert_eq!(FieldUpdate::Clear.apply(Some(1)), None);
        assert_eq!(FieldUpdate::<i32>::Clear.apply(None), None);
    }
}
