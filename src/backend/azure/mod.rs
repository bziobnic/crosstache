//! Azure backend adapter.
//!
//! This module provides [`AzureBackend`], which implements the
//! [`Backend`](super::Backend) trait by wrapping the existing Azure
//! implementations (`AzureSecretOperations`, `AzureVaultOperations`,
//! `BlobManager`) behind the new trait hierarchy.
//!
//! This is a *thin adapter layer* — no business logic is duplicated.

pub mod audit;
pub mod auth;
pub mod detect;
#[allow(clippy::module_inception)]
pub mod secrets;
pub mod types;
pub mod vaults;

#[cfg(feature = "file-ops")]
pub mod files;

use std::sync::Arc;

use async_trait::async_trait;

use super::error::BackendError;
use super::{
    AuditBackend, Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend,
    VaultBackend,
};
use crate::auth::provider::AzureAuthProvider;
use crate::config::settings::Config;
use crate::error::CrosstacheError;
use crate::secret::manager::AzureSecretOperations;
use crate::vault::operations::AzureVaultOperations;

/// Map [`CrosstacheError`] → [`BackendError`].
///
/// This is a best-effort mapping; variants without a direct BackendError
/// equivalent are mapped to `BackendError::Internal`.
///
/// Shared by all Azure sub-backends (secrets, vaults, files).
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — called by future trait impls.
pub fn map_error(err: CrosstacheError) -> BackendError {
    match err {
        CrosstacheError::SecretNotFound { name, suggestion } => {
            BackendError::NotFound { name, suggestion }
        }
        CrosstacheError::VaultNotFound { name, suggestion } => {
            BackendError::VaultNotFound { name, suggestion }
        }
        CrosstacheError::AuthenticationError(msg) => BackendError::AuthenticationFailed(msg),
        CrosstacheError::PermissionDenied(msg) => BackendError::PermissionDenied(msg),
        CrosstacheError::Conflict(msg) => BackendError::Conflict(msg),
        CrosstacheError::RateLimited(_msg) => BackendError::RateLimited {
            retry_after_secs: None,
        },
        CrosstacheError::NetworkError(msg) => BackendError::Network(msg),
        CrosstacheError::DnsResolutionError {
            vault_name,
            details,
        } => BackendError::Network(format!(
            "DNS resolution failed for '{vault_name}': {details}"
        )),
        CrosstacheError::ConnectionTimeout(msg) => BackendError::Network(msg),
        CrosstacheError::ConnectionRefused(msg) => BackendError::Network(msg),
        CrosstacheError::SslError(msg) => BackendError::Network(msg),
        CrosstacheError::InvalidArgument(msg) => BackendError::InvalidArgument(msg),
        CrosstacheError::InvalidUrl(msg) => BackendError::InvalidArgument(msg),
        other => BackendError::Internal(other.to_string()),
    }
}

use self::audit::AzureAuditBackend;
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
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — fields read via trait impls.
pub struct AzureBackend {
    secret_backend: AzureSecretBackend,
    vault_backend: AzureVaultBackend,
    audit_backend: AzureAuditBackend,
    #[cfg(feature = "file-ops")]
    file_backend: Option<AzureFileBackend>,
    auth_provider: Arc<dyn AzureAuthProvider>,
}

impl AzureBackend {
    /// Create a new `AzureBackend` from a config and auth provider.
    ///
    /// This wires up the three sub-backends using the existing Azure
    /// implementation types.
    pub fn new(
        config: &Config,
        auth_provider: Arc<dyn AzureAuthProvider>,
    ) -> Result<Self, BackendError> {
        if !config.default_vault.is_empty() {
            types::AzureVaultName::try_from(config.default_vault.as_str()).map_err(map_error)?;
        }

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

        let audit_backend = AzureAuditBackend::new(
            auth_provider.clone(),
            config.subscription_id.clone(),
            config.default_resource_group.clone(),
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
            audit_backend,
            #[cfg(feature = "file-ops")]
            file_backend,
            auth_provider,
        })
    }

    /// Return the auth provider used by this backend.
    ///
    /// Used by the CLI layer during migration: handlers that still rely on
    /// Azure-specific managers (`SecretManager`, `VaultManager`) can extract
    /// the already-created auth provider instead of constructing a new one.
    #[allow(dead_code)] // Used during migration — will be removed once all handlers use backend traits.
    pub fn auth_provider(&self) -> &Arc<dyn AzureAuthProvider> {
        &self.auth_provider
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
            has_file_storage: {
                #[cfg(feature = "file-ops")]
                {
                    self.file_backend.is_some()
                }
                #[cfg(not(feature = "file-ops"))]
                {
                    false
                }
            },
            has_rbac: true,
            has_audit: true,
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
            max_tags: Some(15),
            max_tag_value_len: Some(256),
        }
    }

    fn secrets(&self) -> &dyn SecretBackend {
        &self.secret_backend
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(&self.vault_backend)
    }

    fn audit(&self) -> Option<&dyn AuditBackend> {
        Some(&self.audit_backend)
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

#[cfg(test)]
mod tests {
    use super::*;
    use azure_core::auth::{AccessToken, TokenCredential};
    use azure_identity::AzureCliCredential;

    struct StubAzureAuthProvider;

    #[async_trait]
    impl AzureAuthProvider for StubAzureAuthProvider {
        async fn get_token(&self, _scopes: &[&str]) -> crate::error::Result<AccessToken> {
            unreachable!("capability checks must not request Azure tokens")
        }

        async fn get_tenant_id(&self) -> crate::error::Result<String> {
            unreachable!("capability checks must not request Azure tenant IDs")
        }

        async fn get_object_id(&self) -> crate::error::Result<String> {
            unreachable!("capability checks must not request Azure object IDs")
        }

        fn get_token_credential(&self) -> Arc<dyn TokenCredential> {
            Arc::new(AzureCliCredential::new())
        }

        async fn resolve_user_to_object_id(&self, _user: &str) -> crate::error::Result<String> {
            unreachable!("capability checks must not resolve Azure users")
        }
    }

    #[test]
    fn azure_backend_declares_and_exposes_audit_backend() {
        let backend = AzureBackend::new(&Config::default(), Arc::new(StubAzureAuthProvider))
            .expect("default Azure backend should construct for capability inspection");

        assert!(backend.capabilities().has_audit);
        assert!(backend.audit().is_some());
    }
}
