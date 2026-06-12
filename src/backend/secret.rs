//! Secret backend trait.
//!
//! [`SecretBackend`] defines the contract for secret CRUD operations.
//! Every backend must implement the 6 required methods; the 8 optional
//! methods have default implementations that return [`BackendError::Unsupported`].

use async_trait::async_trait;

use crate::secret::manager::{SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest};

use super::error::BackendError;

/// Trait for secret management operations.
///
/// All backends must implement the required methods. Optional methods
/// return `Err(BackendError::Unsupported(...))` by default so that
/// backends can opt-in to features incrementally.
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — consumed by future backends.
#[async_trait]
pub trait SecretBackend: Send + Sync {
    /// Create or update a secret. Returns the new version's properties.
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError>;

    /// Get a secret by name, optionally including the plaintext value.
    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError>;

    /// Get a specific version of a secret.
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError>;

    /// List all secrets in a vault, optionally filtered by group.
    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError>;

    /// Delete a secret (soft-delete if the backend supports it).
    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError>;

    /// Update secret metadata (tags, groups, enabled state, etc.).
    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError>;

    // -----------------------------------------------------------------------
    // Optional operations — defaults return Unsupported
    // -----------------------------------------------------------------------

    /// List all versions of a secret.
    async fn list_versions(
        &self,
        _vault: &str,
        _name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        Err(BackendError::Unsupported("version history".into()))
    }

    /// Rollback to a previous version.
    async fn rollback(
        &self,
        _vault: &str,
        _name: &str,
        _version: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("rollback".into()))
    }

    /// Restore a soft-deleted secret.
    async fn restore_secret(
        &self,
        _vault: &str,
        _name: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("restore".into()))
    }

    /// Permanently purge a deleted secret.
    async fn purge_secret(&self, _vault: &str, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("purge".into()))
    }

    /// Check if a secret exists (default: try `get_secret` and map the result).
    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        match self.get_secret(vault, name, false).await {
            Ok(_) => Ok(true),
            Err(BackendError::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// List deleted secrets (only meaningful when soft-delete is supported).
    async fn list_deleted_secrets(&self, _vault: &str) -> Result<Vec<SecretSummary>, BackendError> {
        Err(BackendError::Unsupported("list deleted secrets".into()))
    }

    /// Backup a secret to portable bytes.
    async fn backup_secret(&self, _vault: &str, _name: &str) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("backup".into()))
    }

    /// Restore a secret from previously-backed-up bytes.
    async fn restore_from_backup(
        &self,
        _vault: &str,
        _backup: &[u8],
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("restore from backup".into()))
    }

    /// Trigger the backend's native rotation mechanism for a secret.
    ///
    /// On AWS this calls `RotateSecret`, which invokes the rotation Lambda
    /// configured on the secret. Success means the rotation request was
    /// accepted — the rotation itself may complete asynchronously.
    async fn native_rotate(&self, _vault: &str, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("native rotation".into()))
    }
}
