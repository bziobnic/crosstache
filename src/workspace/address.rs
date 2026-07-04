//! Colon-address parsing: `alias:path` qualifies a secret name with a
//! workspace vault alias; `path` alone (no colon, or a colon-prefix that
//! isn't a charset-valid alias) is resolved against the workspace's default
//! entry (writes) or searched across attached vaults (reads).
//!
//! Grammar (spec §Addressing): `alias ":" path`, where `path` is today's
//! `folder/name` grammar, unchanged. `/` stays folders-only; colon
//! introduces the vault qualifier and only the FIRST colon is significant,
//! so a path may itself contain further colons (e.g. `work:a:b` → alias
//! `work`, path `a:b`).

use super::{is_valid_alias_charset, WorkspaceEntry};

/// A parsed `alias:path` (or bare `path`) secret address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAddress {
    /// `Some(alias)` when the input had a charset-valid alias prefix
    /// before the first `:`. `None` means the whole input is `path`.
    pub alias: Option<String>,
    pub path: String,
}

/// Parse `raw` into a [`SecretAddress`].
///
/// Splits on the FIRST `:`. The left-hand side is only treated as an alias
/// when it is non-empty and charset-valid (`[a-zA-Z0-9-]`) — otherwise the
/// entire original string becomes `path` unchanged (protects literal names
/// containing `:`, e.g. on the local backend's unrestricted charset).
pub fn parse_address(raw: &str) -> SecretAddress {
    if let Some(idx) = raw.find(':') {
        let left = &raw[..idx];
        let right = &raw[idx + 1..];
        if is_valid_alias_charset(left) {
            return SecretAddress {
                alias: Some(left.to_string()),
                path: right.to_string(),
            };
        }
    }
    SecretAddress {
        alias: None,
        path: raw.to_string(),
    }
}

/// The outcome of resolving a [`SecretAddress`] against a workspace.
///
/// The exact-match probe itself (checking whether the FULL raw string is a
/// literal secret name in scope, which must win before alias
/// interpretation) requires backend access and lives in the CLI layer
/// (`resolve_secret_target`, Task 4) — this type only carries the parse
/// result through to that resolution step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// Resolved to a specific workspace entry + path.
    Entry(WorkspaceEntry, String),
    /// No workspace attached; `path` is the raw name to resolve as today.
    NoWorkspace(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_name() {
        let a = parse_address("DB_PASSWORD");
        assert_eq!(a.alias, None);
        assert_eq!(a.path, "DB_PASSWORD");
    }

    #[test]
    fn parse_alias_and_folder_path() {
        let a = parse_address("work:app/db/pass");
        assert_eq!(a.alias.as_deref(), Some("work"));
        assert_eq!(a.path, "app/db/pass");
    }

    #[test]
    fn parse_colon_in_path_when_prefix_not_charset_valid() {
        // "not valid!" contains '!' -> not a charset-valid alias -> the
        // whole original string (including the colon) is the path.
        let a = parse_address("not valid!:rest");
        assert_eq!(a.alias, None);
        assert_eq!(a.path, "not valid!:rest");
    }

    #[test]
    fn parse_preserves_multi_colon_path() {
        let a = parse_address("work:a:b");
        assert_eq!(a.alias.as_deref(), Some("work"));
        assert_eq!(a.path, "a:b");
    }

    #[test]
    fn parse_empty_alias_is_path() {
        let a = parse_address(":foo");
        assert_eq!(a.alias, None);
        assert_eq!(a.path, ":foo");
    }
}
