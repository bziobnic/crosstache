//! Azure secret backend adapter.
//!
//! Wraps the existing [`AzureSecretOperations`] (which implements
//! [`SecretOperations`]) behind the new [`SecretBackend`] trait.

#[allow(unused_imports)]
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::secret::manager::{
    DeletedSecretSummary, SecretOperations, SecretProperties, SecretRequest, SecretSummary,
    SecretUpdateRequest,
};

use super::map_error;

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

        // Determine whether we need to read the current secret.
        // We need it when:
        //  - value is None (to avoid overwriting with empty string)
        //  - replace_tags is false and new tags are provided (to merge)
        //  - replace_groups is false and new groups are provided (to merge)
        //  - any tri-state metadata field is Unchanged (the underlying Azure
        //    update is a PUT, so unchanged fields must be carried forward)
        let needs_current = request.value.is_none()
            || (!request.replace_tags && request.tags.is_some())
            || (!request.replace_groups && request.groups.is_some())
            || request.expires_on.is_unchanged()
            || request.not_before.is_unchanged()
            || request.note.is_unchanged()
            || request.folder.is_unchanged();

        let current = if needs_current {
            Some(
                self.inner
                    .get_secret(vault, name, true)
                    .await
                    .map_err(map_error)?,
            )
        } else {
            None
        };

        // Resolve the value: use the provided value, or fall back to the current one.
        let value = match request.value {
            Some(v) => v,
            None => current
                .as_ref()
                .and_then(|c| c.value.clone())
                .unwrap_or_else(|| zeroize::Zeroizing::new(String::new())),
        };

        // Resolve tags: honor replace_tags semantics.
        let tags = match request.tags {
            Some(new_tags) if !request.replace_tags => {
                // Merge: start with existing tags, then overlay new ones.
                let mut merged = current.as_ref().map(|c| c.tags.clone()).unwrap_or_default();
                merged.extend(new_tags);
                Some(merged)
            }
            other => other,
        };

        // Resolve groups: honor replace_groups semantics.
        let groups = match request.groups {
            Some(new_groups) if !request.replace_groups => {
                // Merge: start with existing groups (stored in tags as comma-separated),
                // then add any new groups that aren't already present.
                let mut existing: Vec<String> = current
                    .as_ref()
                    .and_then(|c| c.tags.get("groups"))
                    .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default();
                for g in new_groups {
                    if !existing.contains(&g) {
                        existing.push(g);
                    }
                }
                Some(existing)
            }
            other => other,
        };

        let compat_request = SecretRequest {
            name: request.new_name.unwrap_or_else(|| request.name.clone()),
            value,
            content_type: request.content_type,
            enabled: request.enabled,
            expires_on: request
                .expires_on
                .apply(current.as_ref().and_then(|c| c.expires_on)),
            not_before: request
                .not_before
                .apply(current.as_ref().and_then(|c| c.not_before)),
            tags,
            groups,
            note: request
                .note
                .apply(current.as_ref().and_then(|c| c.tags.get("note").cloned())),
            folder: request
                .folder
                .apply(current.as_ref().and_then(|c| c.tags.get("folder").cloned())),
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

    async fn list_deleted_secrets(
        &self,
        vault: &str,
    ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
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
