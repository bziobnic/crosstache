//! Backend-prefixed addressing for `xv://` URIs and migrate endpoints.
//!
//! Provides [`BackendRef`], a parsed representation of an optional backend
//! prefix followed by a vault name and an optional secret name.
//!
//! # Formats
//!
//! | Surface | Format | Example |
//! |---------|--------|---------|
//! | `xv://` URI | `[backend:]vault/secret` | `aws:prod/db-password` |
//! | `xv migrate --from/--to` | `backend[:vault]` | `aws:prod-sm` |

use super::BackendKind;

/// A parsed reference to a secret or vault with an optional backend prefix.
///
/// Produced by [`BackendRef::parse`] from `xv://` URI path segments of the
/// form `[backend:]vault[/secret]`.
///
/// When `backend` is `None`, the caller should resolve the reference against
/// the currently-active backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRef {
    /// Optional backend override. `None` means use the currently-active backend.
    pub backend: Option<BackendKind>,
    /// Vault name.
    pub vault: String,
    /// Optional secret name. Present when the input contains a `/secret` segment.
    pub secret: Option<String>,
}

impl BackendRef {
    /// Parse `[backend:]vault[/secret]`.
    ///
    /// Used for `xv://` URI path segments. A `:` in the input introduces an
    /// optional backend prefix; a `/` after the vault name introduces an
    /// optional secret. Vault names must not contain `:` or `/` (both Azure
    /// Key Vault and the local backend enforce this).
    ///
    /// Returns an error if the backend prefix is unrecognised or the vault
    /// name is empty after stripping.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("reference cannot be empty".into());
        }

        // Split on the first ':' to detect an optional backend prefix.
        // Because vault names must not contain ':', any ':' unambiguously
        // marks the backend separator.
        let (backend, rest) = if let Some(colon) = s.find(':') {
            let left = &s[..colon];
            let right = &s[colon + 1..];
            let kind = left.parse::<BackendKind>().map_err(|_| {
                format!("unknown backend '{left}' in '{s}': valid values are azure, local, aws")
            })?;
            (Some(kind), right)
        } else {
            (None, s)
        };

        // Split on the first '/' to separate vault from optional secret.
        let (vault, secret) = match rest.find('/') {
            Some(pos) => {
                let v = rest[..pos].to_string();
                let sec = &rest[pos + 1..];
                (v, if sec.is_empty() { None } else { Some(sec.to_string()) })
            }
            None => (rest.to_string(), None),
        };

        if vault.is_empty() {
            return Err(format!("vault name cannot be empty in '{s}'"));
        }

        Ok(BackendRef { backend, vault, secret })
    }

    /// Parse a migrate endpoint string: `backend` or `backend:vault`.
    ///
    /// Unlike [`parse`](Self::parse), a bare word here is treated as a
    /// **backend kind** (not a vault name), because the migrate command always
    /// requires an explicit backend. The vault part is optional and, when
    /// present, overrides the `--vault` flag for that side of the migration.
    ///
    /// Returns `(BackendKind, Option<vault_name>)`.
    pub fn parse_migrate_endpoint(s: &str) -> Result<(BackendKind, Option<String>), String> {
        let s = s.trim();
        if let Some(colon) = s.find(':') {
            let left = &s[..colon];
            let right = &s[colon + 1..];
            let kind = left
                .parse::<BackendKind>()
                .map_err(|_| format!("unknown backend '{left}' in '{s}': valid values are azure, local, aws"))?;
            let vault = right.to_string();
            Ok((kind, if vault.is_empty() { None } else { Some(vault) }))
        } else {
            let kind = s
                .parse::<BackendKind>()
                .map_err(|_| format!("unknown backend '{s}': valid values are azure, local, aws"))?;
            Ok((kind, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── BackendRef::parse ────────────────────────────────────────────────────

    #[test]
    fn parse_unprefixed_vault_and_secret() {
        let r = BackendRef::parse("prod-kv/MY_TOKEN").unwrap();
        assert_eq!(r.backend, None);
        assert_eq!(r.vault, "prod-kv");
        assert_eq!(r.secret.as_deref(), Some("MY_TOKEN"));
    }

    #[test]
    fn parse_aws_prefixed() {
        let r = BackendRef::parse("aws:prod/db-password").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Aws));
        assert_eq!(r.vault, "prod");
        assert_eq!(r.secret.as_deref(), Some("db-password"));
    }

    #[test]
    fn parse_azure_prefixed() {
        let r = BackendRef::parse("azure:dev-kv/TOKEN").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Azure));
        assert_eq!(r.vault, "dev-kv");
        assert_eq!(r.secret.as_deref(), Some("TOKEN"));
    }

    #[test]
    fn parse_local_prefixed() {
        let r = BackendRef::parse("local:default/foo").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Local));
        assert_eq!(r.vault, "default");
        assert_eq!(r.secret.as_deref(), Some("foo"));
    }

    #[test]
    fn parse_unknown_backend_rejected() {
        let err = BackendRef::parse("gcp:my-vault/secret").unwrap_err();
        assert!(err.contains("unknown backend"), "got: {err}");
        assert!(err.contains("gcp"), "got: {err}");
    }

    #[test]
    fn parse_vault_only_no_secret() {
        let r = BackendRef::parse("my-vault").unwrap();
        assert_eq!(r.backend, None);
        assert_eq!(r.vault, "my-vault");
        assert_eq!(r.secret, None);
    }

    #[test]
    fn parse_prefixed_vault_only_no_secret() {
        let r = BackendRef::parse("aws:prod").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Aws));
        assert_eq!(r.vault, "prod");
        assert_eq!(r.secret, None);
    }

    #[test]
    fn parse_whitespace_trimmed() {
        // Outer whitespace is trimmed by parse(); inner path segments are not
        // split further. In practice, callers come from the xv:// regex
        // ([^/\s]+ groups), which excludes whitespace from capture groups.
        let r = BackendRef::parse("  aws:prod/db  ").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Aws));
        assert_eq!(r.vault, "prod");
        assert_eq!(r.secret.as_deref(), Some("db"));
    }

    #[test]
    fn parse_empty_string_rejected() {
        assert!(BackendRef::parse("").is_err());
        assert!(BackendRef::parse("   ").is_err());
    }

    #[test]
    fn parse_empty_vault_after_colon_rejected() {
        let err = BackendRef::parse("aws:/secret").unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn parse_trailing_slash_no_secret_segment() {
        let r = BackendRef::parse("aws:prod/").unwrap();
        assert_eq!(r.vault, "prod");
        assert_eq!(r.secret, None);
    }

    #[test]
    fn parse_alias_keyvault_accepted() {
        let r = BackendRef::parse("keyvault:my-kv/SECRET").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Azure));
    }

    #[test]
    fn parse_alias_age_accepted() {
        let r = BackendRef::parse("age:default/key").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Local));
    }

    #[test]
    fn parse_alias_secretsmanager_accepted() {
        let r = BackendRef::parse("secretsmanager:prod/db").unwrap();
        assert_eq!(r.backend, Some(BackendKind::Aws));
    }

    // ── BackendRef::parse_migrate_endpoint ──────────────────────────────────

    #[test]
    fn migrate_azure_backend_only() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("azure").unwrap();
        assert_eq!(kind, BackendKind::Azure);
        assert_eq!(vault, None);
    }

    #[test]
    fn migrate_local_backend_only() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("local").unwrap();
        assert_eq!(kind, BackendKind::Local);
        assert_eq!(vault, None);
    }

    #[test]
    fn migrate_aws_backend_only() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("aws").unwrap();
        assert_eq!(kind, BackendKind::Aws);
        assert_eq!(vault, None);
    }

    #[test]
    fn migrate_aws_with_vault() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("aws:prod-sm").unwrap();
        assert_eq!(kind, BackendKind::Aws);
        assert_eq!(vault.as_deref(), Some("prod-sm"));
    }

    #[test]
    fn migrate_azure_with_vault() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("azure:my-keyvault").unwrap();
        assert_eq!(kind, BackendKind::Azure);
        assert_eq!(vault.as_deref(), Some("my-keyvault"));
    }

    #[test]
    fn migrate_local_with_vault() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("local:default").unwrap();
        assert_eq!(kind, BackendKind::Local);
        assert_eq!(vault.as_deref(), Some("default"));
    }

    #[test]
    fn migrate_unknown_backend_rejected() {
        let err = BackendRef::parse_migrate_endpoint("gcp").unwrap_err();
        assert!(
            err.contains("unknown") || err.contains("gcp"),
            "got: {err}"
        );
    }

    #[test]
    fn migrate_alias_az_accepted() {
        let (kind, _) = BackendRef::parse_migrate_endpoint("az").unwrap();
        assert_eq!(kind, BackendKind::Azure);
    }

    #[test]
    fn migrate_alias_age_accepted() {
        let (kind, _) = BackendRef::parse_migrate_endpoint("age").unwrap();
        assert_eq!(kind, BackendKind::Local);
    }

    #[test]
    fn migrate_alias_asm_with_vault() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("asm:my-store").unwrap();
        assert_eq!(kind, BackendKind::Aws);
        assert_eq!(vault.as_deref(), Some("my-store"));
    }
}
