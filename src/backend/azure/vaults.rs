//! Azure vault backend adapter.
//!
//! Wraps the existing [`AzureVaultOperations`] (which implements
//! [`VaultOperations`]) behind the new [`VaultBackend`] trait.

#[allow(unused_imports)]
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::error::BackendError;
use crate::backend::vault::VaultBackend;
use crate::config::settings::Config;
use crate::vault::models::{
    AccessLevel, VaultCreateRequest, VaultProperties, VaultRole, VaultSummary, VaultUpdateRequest,
};
use crate::vault::operations::VaultOperations;

use super::map_error;

/// Adapter that implements [`VaultBackend`] by delegating to an existing
/// [`VaultOperations`] implementation (i.e. `AzureVaultOperations`).
///
/// Because many old `VaultOperations` methods require a `resource_group`
/// parameter that the new `VaultBackend` trait omits, the adapter stores
/// a default resource group (from config) and uses it for every call.
#[allow(dead_code)]
pub struct AzureVaultBackend {
    inner: Arc<dyn VaultOperations>,
    /// Default resource group to use when the new trait API doesn't supply one.
    default_resource_group: String,
    /// Default subscription ID (needed by list_vaults).
    subscription_id: String,
    /// Default location (used for restore/purge).
    default_location: String,
}

impl AzureVaultBackend {
    /// Create a new adapter around an existing `VaultOperations` implementor.
    #[allow(dead_code)]
    pub fn new(
        inner: Arc<dyn VaultOperations>,
        default_resource_group: String,
        subscription_id: String,
        default_location: String,
    ) -> Self {
        Self {
            inner,
            default_resource_group,
            subscription_id,
            default_location,
        }
    }

    /// Convenience constructor from config.
    #[allow(dead_code)]
    pub fn from_config(inner: Arc<dyn VaultOperations>, config: &Config) -> Self {
        Self {
            inner,
            default_resource_group: config.default_resource_group.clone(),
            subscription_id: config.subscription_id.clone(),
            default_location: config.default_location.clone(),
        }
    }
}

#[async_trait]
impl VaultBackend for AzureVaultBackend {
    async fn create_vault(
        &self,
        request: VaultCreateRequest,
    ) -> Result<VaultProperties, BackendError> {
        self.inner.create_vault(&request).await.map_err(map_error)
    }

    async fn get_vault(&self, name: &str) -> Result<VaultProperties, BackendError> {
        // The old trait requires a resource_group; use the default.
        self.inner
            .get_vault(name, &self.default_resource_group)
            .await
            .map_err(map_error)
    }

    async fn list_vaults(&self) -> Result<Vec<VaultSummary>, BackendError> {
        self.inner
            .list_vaults(Some(&self.subscription_id), None)
            .await
            .map_err(map_error)
    }

    async fn delete_vault(&self, name: &str) -> Result<(), BackendError> {
        self.inner
            .delete_vault(name, &self.default_resource_group)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // Optional operations
    // ------------------------------------------------------------------

    async fn update_vault(
        &self,
        name: &str,
        request: VaultUpdateRequest,
    ) -> Result<VaultProperties, BackendError> {
        self.inner
            .update_vault(name, &self.default_resource_group, &request)
            .await
            .map_err(map_error)
    }

    async fn restore_vault(&self, name: &str) -> Result<VaultProperties, BackendError> {
        self.inner
            .restore_vault(name, &self.default_location)
            .await
            .map_err(map_error)
    }

    async fn purge_vault(&self, name: &str) -> Result<(), BackendError> {
        self.inner
            .purge_vault(name, &self.default_location)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // RBAC
    // ------------------------------------------------------------------

    async fn grant_access(
        &self,
        vault: &str,
        principal: &str,
        level: AccessLevel,
    ) -> Result<(), BackendError> {
        self.inner
            .grant_access(vault, &self.default_resource_group, principal, level)
            .await
            .map_err(map_error)
    }

    async fn revoke_access(&self, vault: &str, principal: &str) -> Result<(), BackendError> {
        self.inner
            .revoke_access(vault, &self.default_resource_group, principal)
            .await
            .map_err(map_error)
    }

    async fn list_access(&self, vault: &str) -> Result<Vec<VaultRole>, BackendError> {
        self.inner
            .list_access(vault, &self.default_resource_group)
            .await
            .map_err(map_error)
    }
}
