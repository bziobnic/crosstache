//! Azure backend adapter.
//!
//! This module provides [`AzureBackend`], which implements the
//! [`Backend`](super::Backend) trait by wrapping the existing Azure
//! implementations (`AzureSecretOperations`, `AzureVaultOperations`,
//! `BlobManager`) behind the new trait hierarchy.
//!
//! This is a *thin adapter layer* — no business logic is duplicated.

#[allow(clippy::module_inception)]
pub mod secrets;
pub mod vaults;

#[cfg(feature = "file-ops")]
pub mod files;

use std::sync::Arc;

use async_trait::async_trait;

use super::error::BackendError;
use super::{Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend, VaultBackend};
use crate::auth::provider::AzureAuthProvider;
use crate::config::settings::Config;
use crate::secret::manager::AzureSecretOperations;
use crate::vault::operations::AzureVaultOperations;

use self::secrets::AzureSecretBackend;
use self::vaults::AzureVaultBackend;

#[cfg(feature = "file-ops")]
use self::files::AzureFileBackend;
#[cfg(feature = "file-ops")]
use super::FileBackend;
#[cfg(feature = "file-ops")]
use crate::blob::manager::BlobManager;

/// Azure Key Vault backend — wraps all existing Azure implementations
/// behind the new [`Backend`] trait.
#[allow(dead_code)]
pub struct AzureBackend {
    secret_backend: AzureSecretBackend,
    vault_backend: AzureVaultBackend,
    #[cfg(feature = "file-ops")]
    file_backend: Option<AzureFileBackend>,
    auth_provider: Arc<dyn AzureAuthProvider>,
}

impl AzureBackend {
    /// Create a new `AzureBackend` from a config and auth provider.
    ///
    /// This wires up the three sub-backends using the existing Azure
    /// implementation types.
    #[allow(dead_code)]
    pub fn new(
        config: &Config,
        auth_provider: Arc<dyn AzureAuthProvider>,
    ) -> Result<Self, BackendError> {
        // Secret backend
        let secret_ops = Arc::new(AzureSecretOperations::new(auth_provider.clone()));
        let secret_backend = AzureSecretBackend::new(secret_ops);

        // Vault backend
        let vault_ops = Arc::new(
            AzureVaultOperations::new(auth_provider.clone(), config.subscription_id.clone())
                .map_err(|e| BackendError::Internal(e.to_string()))?,
        );
        let vault_backend = AzureVaultBackend::from_config(
            vault_ops as Arc<dyn crate::vault::operations::VaultOperations>,
            config,
        );

        // File backend (only when file-ops feature is enabled)
        #[cfg(feature = "file-ops")]
        let file_backend = {
            let blob_config = config.get_blob_config();
            if !blob_config.storage_account.is_empty() {
                let blob_manager = BlobManager::new(
                    auth_provider.clone(),
                    blob_config.storage_account.clone(),
                    blob_config.container_name.clone(),
                )
                .map_err(|e| BackendError::Internal(e.to_string()))?
                .with_blob_config(
                    blob_config.chunk_size_mb,
                    blob_config.max_concurrent_uploads,
                );
                Some(AzureFileBackend::new(Arc::new(blob_manager)))
            } else {
                None
            }
        };

        Ok(Self {
            secret_backend,
            vault_backend,
            #[cfg(feature = "file-ops")]
            file_backend,
            auth_provider,
        })
    }
}

#[async_trait]
impl Backend for AzureBackend {
    fn name(&self) -> &'static str {
        "azure"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Azure
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_vaults: true,
            has_file_storage: cfg!(feature = "file-ops"),
            has_rbac: true,
            has_audit: false,
            has_versioning: true,
            has_soft_delete: true,
            has_secret_rotation: false,
            has_groups: true,
            has_folders: true,
            has_notes: true,
            has_expiry: true,
            max_secret_size: Some(25 * 1024), // 25 KiB Azure limit
            max_name_length: Some(127),       // Azure Key Vault name limit
            name_charset: NameCharset::AlphanumericHyphen,
        }
    }

    fn secrets(&self) -> &dyn SecretBackend {
        &self.secret_backend
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(&self.vault_backend)
    }

    #[cfg(feature = "file-ops")]
    fn files(&self) -> Option<&dyn FileBackend> {
        self.file_backend.as_ref().map(|fb| fb as &dyn FileBackend)
    }

    async fn health_check(&self) -> Result<(), BackendError> {
        // Verify we can obtain an Azure token (cheap connectivity check).
        self.auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await
            .map_err(|e| BackendError::AuthenticationFailed(e.to_string()))?;
        Ok(())
    }
}
