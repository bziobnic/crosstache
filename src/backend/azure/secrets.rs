//! Azure secret backend adapter.
//!
//! Wraps the existing [`AzureSecretOperations`] (which implements
//! [`SecretOperations`]) behind the new [`SecretBackend`] trait.

#[allow(unused_imports)]
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::error::CrosstacheError;
use crate::secret::manager::{
    SecretOperations, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
};

/// Adapter that implements [`SecretBackend`] by delegating to an existing
/// [`SecretOperations`] implementation (i.e. `AzureSecretOperations`).
#[allow(dead_code)]
pub struct AzureSecretBackend {
    inner: Arc<dyn SecretOperations>,
}

impl AzureSecretBackend {
    /// Wrap an existing `SecretOperations` implementor.
    #[allow(dead_code)]
    pub fn new(inner: Arc<dyn SecretOperations>) -> Self {
        Self { inner }
    }
}

/// Map [`CrosstacheError`] → [`BackendError`].
///
/// This is a best-effort mapping; variants without a direct BackendError
/// equivalent are mapped to `BackendError::Internal`.
fn map_error(err: CrosstacheError) -> BackendError {
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
        other => BackendError::Internal(other.to_string()),
    }
}

#[async_trait]
impl SecretBackend for AzureSecretBackend {
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .set_secret(vault, &request)
            .await
            .map_err(map_error)
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .get_secret(vault, name, include_value)
            .await
            .map_err(map_error)
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .get_secret_version(vault, name, version, include_value)
            .await
            .map_err(map_error)
    }

    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        self.inner
            .list_secrets(vault, group_filter)
            .await
            .map_err(map_error)
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.inner
            .delete_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        // The old SecretOperations::update_secret takes &SecretRequest, but
        // the new trait takes SecretUpdateRequest. Translate by building a
        // SecretRequest from the update request fields.
        let compat_request = SecretRequest {
            name: request.new_name.unwrap_or_else(|| request.name.clone()),
            value: request
                .value
                .unwrap_or_else(|| zeroize::Zeroizing::new(String::new())),
            content_type: request.content_type,
            enabled: request.enabled,
            expires_on: request.expires_on,
            not_before: request.not_before,
            tags: request.tags,
            groups: request.groups,
            note: request.note,
            folder: request.folder,
        };
        self.inner
            .update_secret(vault, name, &compat_request)
            .await
            .map_err(map_error)
    }

    // ------------------------------------------------------------------
    // Optional operations — Azure supports all of these
    // ------------------------------------------------------------------

    async fn list_versions(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        self.inner
            .get_secret_versions(vault, name)
            .await
            .map_err(map_error)
    }

    async fn rollback(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .rollback_secret(vault, name, version)
            .await
            .map_err(map_error)
    }

    async fn restore_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .restore_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.inner
            .purge_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        self.inner
            .secret_exists(vault, name)
            .await
            .map_err(map_error)
    }

    async fn list_deleted_secrets(&self, vault: &str) -> Result<Vec<SecretSummary>, BackendError> {
        self.inner
            .list_deleted_secrets(vault)
            .await
            .map_err(map_error)
    }

    async fn backup_secret(&self, vault: &str, name: &str) -> Result<Vec<u8>, BackendError> {
        self.inner
            .backup_secret(vault, name)
            .await
            .map_err(map_error)
    }

    async fn restore_from_backup(
        &self,
        vault: &str,
        backup: &[u8],
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .restore_secret_from_backup(vault, backup)
            .await
            .map_err(map_error)
    }
}
