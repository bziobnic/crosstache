//! Vault backend trait.
//!
//! [`VaultBackend`] defines the contract for vault/namespace management.
//! Only backends that advertise `has_vaults` need to implement this.

use std::collections::HashMap;

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
    ///
    /// `resource_group` overrides the backend's configured default when `Some`
    /// (Azure addresses vaults by resource group); backends without the concept
    /// ignore it.
    async fn get_vault(
        &self,
        name: &str,
        resource_group: Option<&str>,
    ) -> Result<VaultProperties, BackendError>;

    /// List all accessible vaults. `resource_group` filters to that group when
    /// `Some` (Azure); backends without the concept ignore it.
    async fn list_vaults(
        &self,
        resource_group: Option<&str>,
    ) -> Result<Vec<VaultSummary>, BackendError>;

    /// Delete a vault (soft-delete if the backend supports it).
    /// `resource_group` behaves as for [`get_vault`](Self::get_vault).
    async fn delete_vault(
        &self,
        name: &str,
        resource_group: Option<&str>,
    ) -> Result<(), BackendError>;

    // -----------------------------------------------------------------------
    // Optional
    // -----------------------------------------------------------------------

    /// Update vault properties. `resource_group` behaves as for
    /// [`get_vault`](Self::get_vault).
    async fn update_vault(
        &self,
        _name: &str,
        _resource_group: Option<&str>,
        _request: VaultUpdateRequest,
    ) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("update vault".into()))
    }

    /// Restore a soft-deleted vault.
    ///
    /// `location` is the region the vault was deleted in (Azure requires it to
    /// address the soft-deleted vault); backends without regional soft-delete
    /// ignore it and fall back to their configured default when `None`.
    async fn restore_vault(
        &self,
        _name: &str,
        _location: Option<&str>,
    ) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("restore vault".into()))
    }

    /// Permanently purge a deleted vault. `location` behaves as for
    /// [`restore_vault`](Self::restore_vault).
    async fn purge_vault(&self, _name: &str, _location: Option<&str>) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("purge vault".into()))
    }

    // -----------------------------------------------------------------------
    // RBAC (optional — only if has_rbac)
    // -----------------------------------------------------------------------

    /// Grant access to a vault for a principal.
    ///
    /// `resource_group` overrides the backend's configured default when `Some`
    /// (Azure addresses vaults by resource group, and `xv vault share` exposes
    /// `--resource-group`); backends without the concept ignore it.
    async fn grant_access(
        &self,
        _vault: &str,
        _resource_group: Option<&str>,
        _principal: &str,
        _level: AccessLevel,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("RBAC".into()))
    }

    /// Revoke access from a principal. `resource_group` behaves as for
    /// [`grant_access`](Self::grant_access).
    async fn revoke_access(
        &self,
        _vault: &str,
        _resource_group: Option<&str>,
        _principal: &str,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("RBAC".into()))
    }

    /// List access assignments on a vault. `resource_group` behaves as for
    /// [`grant_access`](Self::grant_access).
    async fn list_access(
        &self,
        _vault: &str,
        _resource_group: Option<&str>,
    ) -> Result<Vec<VaultRole>, BackendError> {
        Err(BackendError::Unsupported("RBAC".into()))
    }

    /// Whether the vault uses RBAC authorization mode (as opposed to
    /// access-policy mode). `xv vault share` requires RBAC mode, so this gates
    /// the grant/revoke/list operations with a friendly pre-flight error.
    /// `resource_group` behaves as for [`grant_access`](Self::grant_access).
    /// Non-RBAC backends return [`BackendError::Unsupported`].
    async fn vault_uses_rbac(
        &self,
        _vault: &str,
        _resource_group: Option<&str>,
    ) -> Result<bool, BackendError> {
        Err(BackendError::Unsupported("RBAC mode check".into()))
    }

    // -----------------------------------------------------------------------
    // Secret-scoped RBAC (optional — only if the backend supports assigning
    // access at the individual-secret granularity, e.g. Azure Key Vault RBAC
    // role assignments scoped to a single secret)
    // -----------------------------------------------------------------------

    /// Grant a principal access to a single secret within a vault.
    async fn grant_secret_access(
        &self,
        _vault: &str,
        _secret: &str,
        _principal: &str,
        _level: AccessLevel,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("secret-level RBAC".into()))
    }

    /// Revoke a principal's access to a single secret within a vault.
    async fn revoke_secret_access(
        &self,
        _vault: &str,
        _secret: &str,
        _principal: &str,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("secret-level RBAC".into()))
    }

    /// List the access assignments scoped to a single secret.
    async fn list_secret_access(
        &self,
        _vault: &str,
        _secret: &str,
    ) -> Result<Vec<VaultRole>, BackendError> {
        Err(BackendError::Unsupported("secret-level RBAC".into()))
    }

    // -----------------------------------------------------------------------
    // Principal resolution (optional — only backends with a directory service)
    // -----------------------------------------------------------------------

    /// Resolve a user identifier (email/UPN/object id) to the backend's
    /// principal object id, for use as the `principal` argument to the
    /// grant/revoke methods. Directory-backed (Graph API on Azure); other
    /// backends default to [`BackendError::Unsupported`].
    async fn resolve_principal(&self, _user: &str) -> Result<String, BackendError> {
        Err(BackendError::Unsupported("principal resolution".into()))
    }

    /// Resolve principal object ids to `(display_name, email)` pairs, for
    /// enriching access listings. Ids that can't be resolved are simply
    /// absent from the returned map; the default implementation resolves
    /// nothing (empty map), so callers must tolerate missing entries.
    async fn resolve_principal_ids(
        &self,
        _principal_ids: &[String],
    ) -> HashMap<String, (String, String)> {
        HashMap::new()
    }
}
