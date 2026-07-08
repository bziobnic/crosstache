//! Name encoding/decoding for AWS Secrets Manager.
//!
//! AWS allows `[a-zA-Z0-9/_+=.@-]` and 512-char names. Our prefix-based
//! virtual vault scheme produces names like `myproj-kv/db-password`.
//!
//! The reserved namespace `.xv-*` is used for vault markers and other
//! xv-internal bookkeeping. User-supplied names starting with `.xv-` are
//! rejected at the `set_secret` boundary.

use crate::backend::error::BackendError;
use crate::backend::NameCharset;

/// Human-readable description of the allowed AWS Secrets Manager charset,
/// used in validation error messages.
const ALLOWED_CHARSET_DESC: &str = "letters, digits, and / _ + = . @ -";

/// The AWS Secrets Manager name length limit.
pub const MAX_NAME_LEN: usize = 512;

/// The marker filename inside each vault prefix.
const MARKER_BASENAME: &str = ".xv-vault";

/// Returns the full AWS name for a secret in a given vault.
pub fn aws_name(vault: &str, secret_name: &str) -> String {
    format!("{vault}/{secret_name}")
}

/// Strips the vault prefix from an AWS name, returning the inner secret name.
/// Returns None if the name doesn't belong to the given vault.
pub fn strip_prefix(vault: &str, full_name: &str) -> Option<String> {
    let prefix = format!("{vault}/");
    full_name.strip_prefix(&prefix).map(|s| s.to_string())
}

/// Returns the AWS name of the vault marker secret.
pub fn marker_name(vault: &str) -> String {
    format!("{vault}/{MARKER_BASENAME}")
}

/// Returns true if `full_name` is a vault marker secret name.
pub fn is_marker(full_name: &str) -> bool {
    full_name.ends_with(&format!("/{MARKER_BASENAME}")) || full_name == MARKER_BASENAME
}

/// Validate a user-facing secret name. Rejects empty names and names in the
/// reserved `.xv-*` namespace.
pub fn validate_secret_name(name: &str) -> Result<(), BackendError> {
    if name.is_empty() {
        return Err(BackendError::InvalidArgument(
            "secret name cannot be empty".into(),
        ));
    }
    if name.starts_with(".xv-") {
        return Err(BackendError::InvalidArgument(format!(
            "secret name '{name}' is in the reserved '.xv-*' namespace"
        )));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(BackendError::InvalidArgument(format!(
            "secret name too long: {} chars (max {MAX_NAME_LEN})",
            name.len()
        )));
    }
    if !NameCharset::AwsRelaxed.is_valid(name) {
        let offender = name
            .chars()
            .find(|c| !NameCharset::AwsRelaxed.is_valid(&c.to_string()))
            .expect("is_valid returned false, so at least one char must be invalid");
        return Err(BackendError::InvalidArgument(format!(
            "secret name '{name}' contains invalid character '{offender}'; \
             AWS secret names may only contain {ALLOWED_CHARSET_DESC}"
        )));
    }
    Ok(())
}

/// Validate the full AWS secret name after the vault prefix is prepended.
pub fn validate_full_secret_name(vault: &str, name: &str) -> Result<(), BackendError> {
    validate_secret_name(name)?;
    let full_name = aws_name(vault, name);
    if full_name.len() > MAX_NAME_LEN {
        return Err(BackendError::InvalidArgument(format!(
            "AWS secret name too long: {} chars for '{vault}/{name}' (max {MAX_NAME_LEN})",
            full_name.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_name_joins_prefix() {
        assert_eq!(
            aws_name("myproj-kv", "db-password"),
            "myproj-kv/db-password"
        );
    }

    #[test]
    fn strip_prefix_extracts_secret_name() {
        assert_eq!(
            strip_prefix("myproj-kv", "myproj-kv/db-password"),
            Some("db-password".to_string())
        );
        assert_eq!(strip_prefix("myproj-kv", "other-vault/db-password"), None);
    }

    #[test]
    fn marker_name_constant() {
        assert_eq!(marker_name("myproj-kv"), "myproj-kv/.xv-vault");
    }

    #[test]
    fn is_marker_detects_marker() {
        assert!(is_marker("myproj-kv/.xv-vault"));
        assert!(!is_marker("myproj-kv/db-password"));
    }

    #[test]
    fn validate_secret_name_rejects_reserved() {
        assert!(validate_secret_name(".xv-vault").is_err());
        assert!(validate_secret_name(".xv-anything").is_err());
    }

    #[test]
    fn validate_secret_name_rejects_empty() {
        assert!(validate_secret_name("").is_err());
    }

    #[test]
    fn validate_secret_name_accepts_normal_names() {
        assert!(validate_secret_name("db-password").is_ok());
        assert!(validate_secret_name("api/v1/key").is_ok());
        assert!(validate_secret_name("v1.2.3-rc.1").is_ok());
    }

    #[test]
    fn validate_secret_name_rejects_disallowed_characters() {
        let err = validate_secret_name("has space").unwrap_err();
        assert!(
            err.to_string().contains('\''),
            "error should quote the offending character: {err}"
        );
    }

    #[test]
    fn validate_secret_name_accepts_full_charset() {
        assert!(validate_secret_name("app/db/key-1_v2.@x").is_ok());
    }

    #[test]
    fn validate_full_secret_name_counts_vault_prefix() {
        let vault = "vault-prefix";
        let max_inner = MAX_NAME_LEN - vault.len() - 1;
        assert!(validate_full_secret_name(vault, &"a".repeat(max_inner)).is_ok());
        assert!(matches!(
            validate_full_secret_name(vault, &"a".repeat(max_inner + 1)),
            Err(BackendError::InvalidArgument(_))
        ));
    }
}
