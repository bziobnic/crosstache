//! Backend-agnostic error type.
//!
//! [`BackendError`] is the error type returned by all backend trait methods.
//! It maps cleanly to [`CrosstacheError`] at the boundary via [`From`].

use crate::error::CrosstacheError;

/// Errors that can originate from any backend implementation.
///
/// Each variant captures a backend-agnostic failure mode. The
/// [`From<BackendError> for CrosstacheError`] impl converts these into the
/// application-level error type used by CLI and TUI layers.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// A secret was not found.
    #[error("secret not found: {name}")]
    NotFound {
        name: String,
        suggestion: Option<String>,
    },

    /// A vault/namespace was not found.
    #[error("vault not found: {name}")]
    VaultNotFound {
        name: String,
        suggestion: Option<String>,
    },

    /// Authentication with the backend failed.
    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),

    /// The caller lacks permission for the requested operation.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// The operation is not supported by this backend.
    ///
    /// The `String` payload describes the unsupported feature
    /// (e.g. `"version history"`, `"RBAC"`).
    #[error("operation not supported: {0}")]
    Unsupported(String),

    /// A conflict occurred (e.g. a secret already exists in create-only mode).
    #[error("conflict: {0}")]
    Conflict(String),

    /// The backend rate-limited the request.
    #[error("rate limited — retry after {retry_after_secs:?}s")]
    RateLimited { retry_after_secs: Option<u64> },

    /// A network-level error (timeout, DNS, TLS, etc.).
    #[error("network error: {0}")]
    Network(String),

    /// An internal error inside the backend implementation.
    #[error("backend internal error: {0}")]
    Internal(String),

    /// Catch-all for errors that don't fit other variants.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<BackendError> for CrosstacheError {
    fn from(err: BackendError) -> Self {
        match err {
            BackendError::NotFound { name, suggestion } => {
                CrosstacheError::SecretNotFound { name, suggestion }
            }
            BackendError::VaultNotFound { name, suggestion } => {
                CrosstacheError::VaultNotFound { name, suggestion }
            }
            BackendError::AuthenticationFailed(msg) => CrosstacheError::AuthenticationError(msg),
            BackendError::PermissionDenied(msg) => CrosstacheError::PermissionDenied(msg),
            BackendError::Unsupported(feature) => {
                CrosstacheError::InvalidArgument(format!("operation not supported: {feature}"))
            }
            BackendError::Conflict(msg) => {
                CrosstacheError::AzureApiError(format!("conflict: {msg}"))
            }
            BackendError::RateLimited { retry_after_secs } => {
                let detail = match retry_after_secs {
                    Some(secs) => format!("rate limited — retry after {secs}s"),
                    None => "rate limited".to_string(),
                };
                CrosstacheError::AzureApiError(detail)
            }
            BackendError::Network(msg) => CrosstacheError::NetworkError(msg),
            BackendError::Internal(msg) => CrosstacheError::Unknown(msg),
            BackendError::Other(err) => CrosstacheError::Unknown(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_converts_to_secret_not_found() {
        let be = BackendError::NotFound {
            name: "my-key".into(),
            suggestion: Some("my-key-v2".into()),
        };
        let ce: CrosstacheError = be.into();
        assert!(matches!(
            ce,
            CrosstacheError::SecretNotFound {
                ref name,
                ref suggestion,
            } if name == "my-key" && suggestion.as_deref() == Some("my-key-v2")
        ));
    }

    #[test]
    fn vault_not_found_converts() {
        let be = BackendError::VaultNotFound {
            name: "prod".into(),
            suggestion: None,
        };
        let ce: CrosstacheError = be.into();
        assert!(matches!(ce, CrosstacheError::VaultNotFound { .. }));
    }

    #[test]
    fn unsupported_converts_to_invalid_argument() {
        let be = BackendError::Unsupported("versioning".into());
        let ce: CrosstacheError = be.into();
        assert!(matches!(ce, CrosstacheError::InvalidArgument(_)));
    }

    #[test]
    fn network_converts() {
        let be = BackendError::Network("timeout".into());
        let ce: CrosstacheError = be.into();
        assert!(matches!(ce, CrosstacheError::NetworkError(_)));
    }
}
