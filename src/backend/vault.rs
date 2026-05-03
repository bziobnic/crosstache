//! Vault backend trait.
//!
//! [`VaultBackend`] defines the contract for vault/namespace management.
//! Only backends that advertise `has_vaults` need to implement this.

use async_trait::async_trait;

use crate::vault::models::{
    AccessLevel, VaultCreateRequest, VaultProperties, VaultRole, VaultSummary, VaultUpdateRequest,
};

use super::error::BackendError;

/// Trait for vault/namespace management operations.
///
/// Required methods cover basic CRUD. Optional methods (RBAC, soft-delete
/// recovery) default to [`BackendError::Unsupported`].
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — consumed by future backends.
#[async_trait]
pub trait VaultBackend: Send + Sync {
    // -----------------------------------------------------------------------
    // Required
    // -----------------------------------------------------------------------

    /// Create a new vault/namespace.
    async fn create_vault(
        &self,
        request: VaultCreateRequest,
    ) -> Result<VaultProperties, BackendError>;

    /// Get vault details by name.
    async fn get_vault(&self, name: &str) -> Result<VaultProperties, BackendError>;

    /// List all accessible vaults.
    async fn list_vaults(&self) -> Result<Vec<VaultSummary>, BackendError>;

    /// Delete a vault (soft-delete if the backend supports it).
    async fn delete_vault(&self, name: &str) -> Result<(), BackendError>;

    // -----------------------------------------------------------------------
    // Optional
    // -----------------------------------------------------------------------

    /// Update vault properties.
    async fn update_vault(
        &self,
        _name: &str,
        _request: VaultUpdateRequest,
    ) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("update vault".into()))
    }

    /// Restore a soft-deleted vault.
    async fn restore_vault(&self, _name: &str) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("restore vault".into()))
    }

    /// Permanently purge a deleted vault.
    async fn purge_vault(&self, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("purge vault".into()))
    }

    // -----------------------------------------------------------------------
    // RBAC (optional — only if has_rbac)
    // -----------------------------------------------------------------------

    /// Grant access to a vault for a principal.
    async fn grant_access(
        &self,
        _vault: &str,
        _principal: &str,
        _level: AccessLevel,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("RBAC".into()))
    }

    /// Revoke access from a principal.
    async fn revoke_access(&self, _vault: &str, _principal: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("RBAC".into()))
    }

    /// List access assignments on a vault.
    async fn list_access(&self, _vault: &str) -> Result<Vec<VaultRole>, BackendError> {
        Err(BackendError::Unsupported("RBAC".into()))
    }
}
