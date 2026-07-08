//! Vault management facade
//!
//! This module provides a high-level interface for vault operations,
//! combining vault operations with RBAC management and providing
//! a unified API for vault management tasks.

use std::sync::Arc;

use super::models::{VaultCreateRequest, VaultProperties};
use super::operations::{AzureVaultOperations, VaultOperations};
use crate::auth::provider::AzureAuthProvider;
use crate::error::Result;
use crate::utils::output;

/// High-level vault manager.
///
/// The CLI resolves every vault verb through the `VaultBackend` trait; the only
/// surviving `VaultManager` consumer is the interactive setup flow
/// (`config/init.rs`), which uses the Azure-specific `create_vault_with_setup`
/// (access-policy + storage setup not yet on the trait).
pub struct VaultManager {
    vault_ops: Arc<dyn VaultOperations>,
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(auth_provider: Arc<dyn AzureAuthProvider>, subscription_id: String) -> Result<Self> {
        let vault_ops = Arc::new(AzureVaultOperations::new(auth_provider, subscription_id)?);

        Ok(Self { vault_ops })
    }

    /// Create a new vault with automatic access policy setup
    pub async fn create_vault_with_setup(
        &self,
        name: &str,
        location: &str,
        resource_group: &str,
        additional_options: Option<VaultCreateRequest>,
    ) -> Result<VaultProperties> {
        output::info(&format!("Creating vault '{name}'..."));

        let mut request = additional_options.unwrap_or_default();
        request.name = name.to_string();
        request.location = location.to_string();
        request.resource_group = resource_group.to_string();

        // Set sensible defaults if not provided
        if request.sku.is_none() {
            request.sku = Some("standard".to_string());
        }
        if request.soft_delete_retention_in_days.is_none() {
            request.soft_delete_retention_in_days = Some(90);
        }
        if request.purge_protection.is_none() {
            request.purge_protection = Some(true);
        }

        let vault = self.vault_ops.create_vault(&request).await?;

        output::success(&format!(
            "Successfully created vault '{}' in {} ({})",
            vault.name, vault.location, vault.resource_group
        ));

        Ok(vault)
    }
}
