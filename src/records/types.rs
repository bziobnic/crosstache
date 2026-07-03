//! Record type definitions, built-in types, and validation.
//!
//! A `RecordType` describes the fields a typed secret ("record") carries:
//! which are listable metadata and which are encrypted secret material,
//! which are required, and which single field is the `primary` value
//! returned by plain `xv get`.

use crate::error::{CrosstacheError, Result};

/// Whether a field's value lives in tags (metadata) or in the encrypted
/// secret value (secret).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Metadata,
    Secret,
}

/// A single field declared by a record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    pub name: String,
    pub kind: FieldKind,
    pub required: bool,
    pub primary: bool,
}

/// Where a resolved `RecordType` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeSource {
    Builtin,
    Global,
    Project,
}

/// A named collection of field definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordType {
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub source: TypeSource,
}

impl RecordType {
    /// Returns the single primary field of this type.
    ///
    /// Panics if `validate()` has not been called successfully first —
    /// callers must validate a `RecordType` before relying on this.
    pub fn primary(&self) -> &FieldDef {
        self.fields
            .iter()
            .find(|f| f.primary)
            .expect("RecordType::primary called on an unvalidated type with no primary field")
    }

    /// Looks up a field by name.
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Validates structural invariants:
    /// - exactly one `primary` field
    /// - the primary field is `Secret` kind and `required`
    /// - every field name is kebab-case (matches secret-name charset rules)
    pub fn validate(&self) -> Result<()> {
        let primaries: Vec<&FieldDef> = self.fields.iter().filter(|f| f.primary).collect();

        if primaries.is_empty() {
            return Err(CrosstacheError::config(format!(
                "record type '{}' has no primary field; exactly one field must be marked primary",
                self.name
            )));
        }
        if primaries.len() > 1 {
            return Err(CrosstacheError::config(format!(
                "record type '{}' has {} primary fields; exactly one field must be marked primary",
                self.name,
                primaries.len()
            )));
        }

        let primary = primaries[0];
        if primary.kind != FieldKind::Secret {
            return Err(CrosstacheError::config(format!(
                "record type '{}': primary field '{}' must be kind = secret",
                self.name, primary.name
            )));
        }
        if !primary.required {
            return Err(CrosstacheError::config(format!(
                "record type '{}': primary field '{}' must be required",
                self.name, primary.name
            )));
        }

        for field in &self.fields {
            if !is_kebab_case(&field.name) {
                return Err(CrosstacheError::config(format!(
                    "record type '{}': field name '{}' is not valid (kebab-case, lowercase alphanumeric and hyphens only)",
                    self.name, field.name
                )));
            }
        }

        Ok(())
    }
}

/// Validates that `name` is kebab-case: lowercase alphanumeric segments
/// separated by single hyphens, no leading/trailing/consecutive hyphens.
fn is_kebab_case(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn field(name: &str, kind: FieldKind, required: bool, primary: bool) -> FieldDef {
    FieldDef {
        name: name.to_string(),
        kind,
        required,
        primary,
    }
}

/// The built-in record types: `login`, `api-key`, `database`.
pub fn builtin_types() -> Vec<RecordType> {
    vec![
        RecordType {
            name: "login".to_string(),
            source: TypeSource::Builtin,
            fields: vec![
                field("username", FieldKind::Metadata, true, false),
                field("url", FieldKind::Metadata, false, false),
                field("password", FieldKind::Secret, true, true),
            ],
        },
        RecordType {
            name: "api-key".to_string(),
            source: TypeSource::Builtin,
            fields: vec![
                field("url", FieldKind::Metadata, false, false),
                field("account", FieldKind::Metadata, false, false),
                field("key", FieldKind::Secret, true, true),
            ],
        },
        RecordType {
            name: "database".to_string(),
            source: TypeSource::Builtin,
            fields: vec![
                field("host", FieldKind::Metadata, false, false),
                field("port", FieldKind::Metadata, false, false),
                field("database", FieldKind::Metadata, false, false),
                field("username", FieldKind::Metadata, false, false),
                field("password", FieldKind::Secret, true, true),
                field("connection-string", FieldKind::Secret, false, false),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_types_are_valid() {
        let types = builtin_types();
        assert_eq!(types.len(), 3);
        for t in &types {
            t.validate().unwrap_or_else(|e| panic!("{}: {e}", t.name));
        }

        let login = types.iter().find(|t| t.name == "login").unwrap();
        assert_eq!(login.primary().name, "password");

        let database = types.iter().find(|t| t.name == "database").unwrap();
        let cs = database.field("connection-string").unwrap();
        assert_eq!(cs.kind, FieldKind::Secret);
        assert!(!cs.required);
        assert!(!cs.primary);
    }

    #[test]
    fn validate_rejects_zero_primaries() {
        let t = RecordType {
            name: "bad".to_string(),
            source: TypeSource::Builtin,
            fields: vec![field("username", FieldKind::Metadata, true, false)],
        };
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_rejects_two_primaries() {
        let t = RecordType {
            name: "bad".to_string(),
            source: TypeSource::Builtin,
            fields: vec![
                field("password", FieldKind::Secret, true, true),
                field("key", FieldKind::Secret, true, true),
            ],
        };
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_secret_primary() {
        let t = RecordType {
            name: "bad".to_string(),
            source: TypeSource::Builtin,
            fields: vec![field("username", FieldKind::Metadata, true, true)],
        };
        assert!(t.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_field_name() {
        let t = RecordType {
            name: "bad".to_string(),
            source: TypeSource::Builtin,
            fields: vec![field("Bad Name", FieldKind::Secret, true, true)],
        };
        assert!(t.validate().is_err());

        let ok = RecordType {
            name: "ok".to_string(),
            source: TypeSource::Builtin,
            fields: vec![field("totp-seed", FieldKind::Secret, true, true)],
        };
        assert!(ok.validate().is_ok());
    }
}
