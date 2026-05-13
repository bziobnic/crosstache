//! Name encoding/decoding for AWS Secrets Manager.
//!
//! AWS allows `[a-zA-Z0-9/_+=.@-]` and 512-char names. Our prefix-based
//! virtual vault scheme produces names like `myproj-kv/db-password`.
//!
//! The reserved namespace `.xv-*` is used for vault markers and other
//! xv-internal bookkeeping. User-supplied names starting with `.xv-` are
//! rejected at the `set_secret` boundary.

use crate::backend::error::BackendError;

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
    full_name.ends_with(&format!("/{MARKER_BASENAME}"))
        || full_name == MARKER_BASENAME
}

/// Validate a user-facing secret name. Rejects empty names and names
/// starting with `.xv-` (reserved namespace).
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_name_joins_prefix() {
        assert_eq!(aws_name("myproj-kv", "db-password"), "myproj-kv/db-password");
    }

    #[test]
    fn strip_prefix_extracts_secret_name() {
        assert_eq!(strip_prefix("myproj-kv", "myproj-kv/db-password"), Some("db-password".to_string()));
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
        assert!(matches!(validate_secret_name(".xv-vault"), Err(_)));
        assert!(matches!(validate_secret_name(".xv-anything"), Err(_)));
    }

    #[test]
    fn validate_secret_name_rejects_empty() {
        assert!(matches!(validate_secret_name(""), Err(_)));
    }

    #[test]
    fn validate_secret_name_accepts_normal_names() {
        assert!(validate_secret_name("db-password").is_ok());
        assert!(validate_secret_name("api/v1/key").is_ok());
        assert!(validate_secret_name("v1.2.3-rc.1").is_ok());
    }
}
