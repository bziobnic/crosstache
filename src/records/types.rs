//! Record type definitions, built-in types, and validation.
//!
//! A `RecordType` describes the fields a typed secret ("record") carries:
//! which are listable metadata and which are encrypted secret material,
//! which are required, and which single field is the `primary` value
//! returned by plain `xv get`.
//!
//! Consumed by the `xv type`/`xv set --type`/`xv get --field` CLI wiring
//! added later in Phase A (record-types plan Tasks 4/6/7); until that
//! wiring lands, this module's public API is unused from the `xv` binary
//! target, hence the crate-wide `#[allow(dead_code)]` below.
#![allow(dead_code)]

use crate::error::{CrosstacheError, Result};
use crate::utils::output;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

// ---------------------------------------------------------------------------
// Config-file parsing and resolution
// ---------------------------------------------------------------------------

/// On-disk shape of a `[types.<name>]` block, shared by both the global
/// `xv.conf` and per-project `.xv.toml` config layers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordTypeConfig {
    pub fields: Vec<FieldDefConfig>,
}

/// On-disk shape of one field within a `[types.<name>]` block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FieldDefConfig {
    pub name: String,
    /// `"metadata"` (default) | `"secret"`.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub primary: bool,
}

impl RecordTypeConfig {
    /// Converts this config-file shape into a `RecordType`, tagging it
    /// with `source` and `name`. Does not validate — callers must call
    /// `RecordType::validate()`.
    fn into_record_type(self, name: &str, source: TypeSource) -> Result<RecordType> {
        let mut fields = Vec::with_capacity(self.fields.len());
        for f in self.fields {
            let kind = match f.kind.as_deref() {
                None | Some("metadata") => FieldKind::Metadata,
                Some("secret") => FieldKind::Secret,
                Some(other) => {
                    return Err(CrosstacheError::config(format!(
                        "type '{name}': field '{}' has invalid kind '{other}' (expected 'metadata' or 'secret')",
                        f.name
                    )));
                }
            };
            fields.push(FieldDef {
                name: f.name,
                kind,
                required: f.required,
                primary: f.primary,
            });
        }
        Ok(RecordType {
            name: name.to_string(),
            fields,
            source,
        })
    }
}

/// Resolves the effective set of record types from built-ins, the global
/// config, and the project config, with precedence project > global >
/// builtin (matched by name). Shadowing a built-in type emits a warning
/// but is allowed. Every resolved type is validated.
pub fn resolve_types(
    global: &HashMap<String, RecordTypeConfig>,
    project: &HashMap<String, RecordTypeConfig>,
) -> Result<Vec<RecordType>> {
    let mut resolved: HashMap<String, RecordType> = HashMap::new();

    for t in builtin_types() {
        resolved.insert(t.name.clone(), t);
    }

    for (name, cfg) in global {
        let is_shadow_builtin = resolved
            .get(name)
            .map(|t| t.source == TypeSource::Builtin)
            .unwrap_or(false);
        if is_shadow_builtin {
            output::warn(&format!("type '{name}' shadows a built-in type"));
        }
        let record_type = cfg.clone().into_record_type(name, TypeSource::Global)?;
        record_type.validate()?;
        resolved.insert(name.clone(), record_type);
    }

    for (name, cfg) in project {
        let is_shadow_builtin = resolved
            .get(name)
            .map(|t| t.source == TypeSource::Builtin)
            .unwrap_or(false);
        if is_shadow_builtin {
            output::warn(&format!("type '{name}' shadows a built-in type"));
        }
        let record_type = cfg.clone().into_record_type(name, TypeSource::Project)?;
        record_type.validate()?;
        resolved.insert(name.clone(), record_type);
    }

    let mut result: Vec<RecordType> = resolved.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// Looks up a resolved type by name.
pub fn find_type<'a>(types: &'a [RecordType], name: &str) -> Option<&'a RecordType> {
    types.iter().find(|t| t.name == name)
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

    #[test]
    fn parse_types_block_from_project_toml() {
        let toml = r#"
            [types.smtp]
            fields = [
              { name = "host" },
              { name = "port" },
              { name = "username", required = true },
              { name = "password", kind = "secret", primary = true },
            ]
        "#;
        #[derive(Deserialize)]
        struct Wrapper {
            types: HashMap<String, RecordTypeConfig>,
        }
        let parsed: Wrapper = toml::from_str(toml).unwrap();
        let smtp = parsed.types.get("smtp").unwrap();
        assert_eq!(smtp.fields.len(), 4);
        let host = smtp.fields.iter().find(|f| f.name == "host").unwrap();
        assert_eq!(host.kind, None); // omitted -> None, resolved to Metadata later
        let password = smtp.fields.iter().find(|f| f.name == "password").unwrap();
        assert_eq!(password.kind.as_deref(), Some("secret"));
        assert!(password.primary);
    }

    fn smtp_config() -> RecordTypeConfig {
        RecordTypeConfig {
            fields: vec![
                FieldDefConfig {
                    name: "host".to_string(),
                    kind: None,
                    required: false,
                    primary: false,
                },
                FieldDefConfig {
                    name: "password".to_string(),
                    kind: Some("secret".to_string()),
                    required: true,
                    primary: true,
                },
            ],
        }
    }

    #[test]
    fn resolve_project_shadows_global() {
        let mut global = HashMap::new();
        global.insert("smtp".to_string(), smtp_config());

        let mut project_cfg = smtp_config();
        project_cfg.fields[0].name = "hostname".to_string(); // distinguishing tweak
        let mut project = HashMap::new();
        project.insert("smtp".to_string(), project_cfg);

        let resolved = resolve_types(&global, &project).unwrap();
        let smtp = find_type(&resolved, "smtp").unwrap();
        assert_eq!(smtp.source, TypeSource::Project);
        assert!(smtp.field("hostname").is_some());
    }

    #[test]
    fn resolve_custom_shadows_builtin_with_warning() {
        let mut global = HashMap::new();
        global.insert("login".to_string(), smtp_config());
        let project = HashMap::new();

        let resolved = resolve_types(&global, &project).unwrap();
        let login = find_type(&resolved, "login").unwrap();
        assert_eq!(login.source, TypeSource::Global);
        // Warning goes to stderr; asserting resolution result only per plan note.
    }

    #[test]
    fn resolve_rejects_invalid_custom_type() {
        let mut global = HashMap::new();
        global.insert(
            "bad".to_string(),
            RecordTypeConfig {
                fields: vec![
                    FieldDefConfig {
                        name: "a".to_string(),
                        kind: Some("secret".to_string()),
                        required: true,
                        primary: true,
                    },
                    FieldDefConfig {
                        name: "b".to_string(),
                        kind: Some("secret".to_string()),
                        required: true,
                        primary: true,
                    },
                ],
            },
        );
        let project = HashMap::new();

        let err = resolve_types(&global, &project).unwrap_err();
        assert!(err.to_string().contains("bad"));
    }
}
