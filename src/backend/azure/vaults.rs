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

    async fn get_vault(
        &self,
        name: &str,
        resource_group: Option<&str>,
    ) -> Result<VaultProperties, BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        self.inner
            .get_vault(name, resource_group)
            .await
            .map_err(map_error)
    }

    async fn list_vaults(
        &self,
        resource_group: Option<&str>,
    ) -> Result<Vec<VaultSummary>, BackendError> {
        self.inner
            .list_vaults(Some(&self.subscription_id), resource_group)
            .await
            .map_err(map_error)
    }

    async fn delete_vault(
        &self,
        name: &str,
        resource_group: Option<&str>,
    ) -> Result<(), BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        self.inner
            .delete_vault(name, resource_group)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // Optional operations
    // ------------------------------------------------------------------

    async fn update_vault(
        &self,
        name: &str,
        resource_group: Option<&str>,
        request: VaultUpdateRequest,
    ) -> Result<VaultProperties, BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        self.inner
            .update_vault(name, resource_group, &request)
            .await
            .map_err(map_error)
    }

    async fn restore_vault(
        &self,
        name: &str,
        location: Option<&str>,
    ) -> Result<VaultProperties, BackendError> {
        let location = location.unwrap_or(&self.default_location);
        self.inner
            .restore_vault(name, location)
            .await
            .map_err(map_error)
    }

    async fn purge_vault(&self, name: &str, location: Option<&str>) -> Result<(), BackendError> {
        let location = location.unwrap_or(&self.default_location);
        self.inner
            .purge_vault(name, location)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // RBAC
    // ------------------------------------------------------------------

    async fn grant_access(
        &self,
        vault: &str,
        resource_group: Option<&str>,
        principal: &str,
        level: AccessLevel,
    ) -> Result<(), BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        self.inner
            .grant_access(vault, resource_group, principal, level)
            .await
            .map_err(map_error)
    }

    async fn revoke_access(
        &self,
        vault: &str,
        resource_group: Option<&str>,
        principal: &str,
    ) -> Result<(), BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        self.inner
            .revoke_access(vault, resource_group, principal)
            .await
            .map_err(map_error)
    }

    async fn list_access(
        &self,
        vault: &str,
        resource_group: Option<&str>,
    ) -> Result<Vec<VaultRole>, BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        self.inner
            .list_access(vault, resource_group)
            .await
            .map_err(map_error)
    }

    async fn vault_uses_rbac(
        &self,
        vault: &str,
        resource_group: Option<&str>,
    ) -> Result<bool, BackendError> {
        let resource_group = resource_group.unwrap_or(&self.default_resource_group);
        let props = self
            .inner
            .get_vault(vault, resource_group)
            .await
            .map_err(map_error)?;
        Ok(props.enable_rbac_authorization == Some(true))
    }

    // ------------------------------------------------------------------
    // Secret-scoped RBAC
    // ------------------------------------------------------------------

    async fn grant_secret_access(
        &self,
        vault: &str,
        secret: &str,
        principal: &str,
        level: AccessLevel,
    ) -> Result<(), BackendError> {
        self.inner
            .grant_secret_access(
                vault,
                &self.default_resource_group,
                secret,
                principal,
                level,
            )
            .await
            .map_err(map_error)
    }

    async fn revoke_secret_access(
        &self,
        vault: &str,
        secret: &str,
        principal: &str,
    ) -> Result<(), BackendError> {
        self.inner
            .revoke_secret_access(vault, &self.default_resource_group, secret, principal)
            .await
            .map_err(map_error)
    }

    async fn list_secret_access(
        &self,
        vault: &str,
        secret: &str,
    ) -> Result<Vec<VaultRole>, BackendError> {
        self.inner
            .list_secret_access(vault, &self.default_resource_group, secret)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // Principal resolution (Graph-backed; kept inside the Azure layer)
    // ------------------------------------------------------------------

    async fn resolve_principal(&self, user: &str) -> Result<String, BackendError> {
        self.inner
            .resolve_user_to_object_id(user)
            .await
            .map_err(map_error)
    }

    async fn resolve_principal_ids(
        &self,
        principal_ids: &[String],
    ) -> std::collections::HashMap<String, (String, String)> {
        self.inner.resolve_principal_ids(principal_ids).await
    }
}
