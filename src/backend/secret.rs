//! Secret backend trait.
//!
//! [`SecretBackend`] defines the contract for secret CRUD operations.
//! Every backend must implement the 6 required methods; the 8 optional
//! methods have default implementations that return [`BackendError::Unsupported`].

use async_trait::async_trait;

use crate::secret::manager::{
    DeletedSecretSummary, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
};

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

    /// Rename a secret: read value + metadata, create it under `new_name`
    /// (tags, groups, note, folder, content type, expiry ride along), then
    /// delete the old name via the backend's normal delete (soft delete /
    /// recovery window / trash). Version history does not carry over.
    ///
    /// If the new secret is created but deleting the original fails, this
    /// returns [`BackendError::RenameIncomplete`] and deliberately does NOT
    /// roll back the new secret — no secret material may be lost.
    async fn rename_secret(
        &self,
        vault: &str,
        name: &str,
        new_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        if new_name == name {
            return Err(BackendError::InvalidArgument(format!(
                "secret is already named '{name}'"
            )));
        }
        if self.secret_exists(vault, new_name).await? {
            return Err(BackendError::Conflict(format!(
                "secret '{new_name}' already exists in vault '{vault}' — delete it first or pick another name"
            )));
        }

        let current = self.get_secret(vault, name, true).await?;
        let request = rename_request_from_properties(new_name, &current)?;
        let created = self.set_secret(vault, request).await?;

        if let Err(cause) = self.delete_secret(vault, name).await {
            return Err(BackendError::RenameIncomplete {
                source: name.to_string(),
                destination: new_name.to_string(),
                vault: vault.to_string(),
                cause: Box::new(cause),
            });
        }
        Ok(created)
    }

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
    async fn list_deleted_secrets(
        &self,
        _vault: &str,
    ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
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

/// Build the create-under-the-new-name request for a rename from the source
/// secret's properties. Groups/note/folder live under canonical tag keys in
/// `SecretProperties.tags` on every backend; lift them into the first-class
/// `SecretRequest` fields so each backend re-encodes them natively, and strip
/// the bookkeeping tags (`original_name`, `created_by`) that `set_secret`
/// regenerates for the new name.
pub(crate) fn rename_request_from_properties(
    new_name: &str,
    current: &SecretProperties,
) -> Result<SecretRequest, BackendError> {
    let value = current.value.clone().ok_or_else(|| {
        BackendError::Internal(format!(
            "backend returned no value for '{}'; rename aborted before creating anything",
            current.name
        ))
    })?;

    let mut tags = current.tags.clone();
    let groups = tags
        .remove("groups")
        .map(|g| {
            g.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|g| !g.is_empty());
    let note = tags.remove("note");
    let folder = tags.remove("folder");
    tags.remove("original_name");
    tags.remove("created_by");

    Ok(SecretRequest {
        name: new_name.to_string(),
        value,
        content_type: (!current.content_type.is_empty()).then(|| current.content_type.clone()),
        enabled: Some(current.enabled),
        expires_on: current.expires_on,
        not_before: current.not_before,
        tags: if tags.is_empty() { None } else { Some(tags) },
        groups,
        note,
        folder,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::manager::SecretRequest;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use zeroize::Zeroizing;

    /// In-memory SecretBackend: enough behavior to exercise the provided
    /// `rename_secret` (set/get/delete/exists); everything else Unsupported.
    struct StubBackend {
        secrets: Mutex<HashMap<String, SecretRequest>>,
        fail_delete: bool,
    }

    impl StubBackend {
        fn new(fail_delete: bool) -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
                fail_delete,
            }
        }
    }

    /// Mirror how real backends surface metadata: groups/note/folder appear
    /// under canonical tag keys in `SecretProperties.tags`.
    fn props_from_request(req: &SecretRequest, include_value: bool) -> SecretProperties {
        let mut tags = req.tags.clone().unwrap_or_default();
        if let Some(groups) = req.groups.as_ref().filter(|g| !g.is_empty()) {
            tags.insert("groups".to_string(), groups.join(","));
        }
        if let Some(note) = req.note.as_ref() {
            tags.insert("note".to_string(), note.clone());
        }
        if let Some(folder) = req.folder.as_ref() {
            tags.insert("folder".to_string(), folder.clone());
        }
        tags.insert("original_name".to_string(), req.name.clone());
        tags.insert("created_by".to_string(), "crosstache".to_string());
        SecretProperties {
            name: req.name.clone(),
            original_name: req.name.clone(),
            value: include_value.then(|| req.value.clone()),
            version: "v1".to_string(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: req.enabled.unwrap_or(true),
            expires_on: req.expires_on,
            not_before: req.not_before,
            tags,
            content_type: req.content_type.clone().unwrap_or_default(),
            recovery_level: None,
        }
    }

    #[async_trait]
    impl SecretBackend for StubBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            let props = props_from_request(&request, false);
            self.secrets
                .lock()
                .unwrap()
                .insert(request.name.clone(), request);
            Ok(props)
        }

        async fn get_secret(
            &self,
            _vault: &str,
            name: &str,
            include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            self.secrets
                .lock()
                .unwrap()
                .get(name)
                .map(|r| props_from_request(r, include_value))
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("versions".into()))
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            Ok(vec![])
        }

        async fn delete_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            if self.fail_delete {
                return Err(BackendError::Network("simulated outage".into()));
            }
            self.secrets
                .lock()
                .unwrap()
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("update".into()))
        }
    }

    fn seeded_request(name: &str) -> SecretRequest {
        let mut tags = HashMap::new();
        tags.insert("custom".to_string(), "kept".to_string());
        SecretRequest {
            name: name.to_string(),
            value: Zeroizing::new("the-value".to_string()),
            content_type: Some("text/plain".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(tags),
            groups: Some(vec!["team-a".to_string(), "team-b".to_string()]),
            note: Some("ride along".to_string()),
            folder: Some("proj/db".to_string()),
        }
    }

    #[tokio::test]
    async fn rename_moves_value_and_metadata() {
        let backend = StubBackend::new(false);
        backend
            .set_secret("v", seeded_request("old-name"))
            .await
            .unwrap();

        let created = backend
            .rename_secret("v", "old-name", "new-name")
            .await
            .unwrap();
        assert_eq!(created.name, "new-name");

        let got = backend.get_secret("v", "new-name", true).await.unwrap();
        assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("the-value"));
        assert_eq!(
            got.tags.get("groups").map(String::as_str),
            Some("team-a,team-b")
        );
        assert_eq!(got.tags.get("note").map(String::as_str), Some("ride along"));
        assert_eq!(got.tags.get("folder").map(String::as_str), Some("proj/db"));
        assert_eq!(got.tags.get("custom").map(String::as_str), Some("kept"));
        // original_name is regenerated for the new name, not copied.
        assert_eq!(
            got.tags.get("original_name").map(String::as_str),
            Some("new-name")
        );
        assert_eq!(got.content_type, "text/plain");

        assert!(matches!(
            backend.get_secret("v", "old-name", false).await,
            Err(BackendError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn rename_to_existing_name_is_a_conflict_and_mutates_nothing() {
        let backend = StubBackend::new(false);
        backend.set_secret("v", seeded_request("a")).await.unwrap();
        backend.set_secret("v", seeded_request("b")).await.unwrap();

        let err = backend.rename_secret("v", "a", "b").await.unwrap_err();
        assert!(matches!(err, BackendError::Conflict(_)), "{err:?}");
        // Both still present and untouched.
        assert!(backend.get_secret("v", "a", true).await.is_ok());
        assert!(backend.get_secret("v", "b", true).await.is_ok());
    }

    #[tokio::test]
    async fn rename_to_same_name_is_invalid_argument() {
        let backend = StubBackend::new(false);
        backend.set_secret("v", seeded_request("a")).await.unwrap();
        let err = backend.rename_secret("v", "a", "a").await.unwrap_err();
        assert!(matches!(err, BackendError::InvalidArgument(_)), "{err:?}");
    }

    #[tokio::test]
    async fn rename_of_missing_secret_is_not_found() {
        let backend = StubBackend::new(false);
        let err = backend
            .rename_secret("v", "ghost", "new")
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::NotFound { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn rename_partial_failure_reports_rename_incomplete_with_both_copies() {
        let backend = StubBackend::new(true); // delete always fails
        backend
            .set_secret("v", seeded_request("old-name"))
            .await
            .unwrap();

        let err = backend
            .rename_secret("v", "old-name", "new-name")
            .await
            .unwrap_err();
        match err {
            BackendError::RenameIncomplete {
                source,
                destination,
                vault,
                cause,
            } => {
                assert_eq!(source, "old-name");
                assert_eq!(destination, "new-name");
                assert_eq!(vault, "v");
                assert!(matches!(*cause, BackendError::Network(_)), "{cause:?}");
            }
            other => panic!("wrong error: {other:?}"),
        }
        // Both copies survive — the new secret is never rolled back.
        assert!(backend.get_secret("v", "old-name", true).await.is_ok());
        assert!(backend.get_secret("v", "new-name", true).await.is_ok());
    }

    #[test]
    fn rename_request_rebuilds_first_class_fields_from_tags() {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), "a, b".to_string());
        tags.insert("note".to_string(), "n".to_string());
        tags.insert("folder".to_string(), "f/g".to_string());
        tags.insert("original_name".to_string(), "old".to_string());
        tags.insert("created_by".to_string(), "crosstache".to_string());
        tags.insert("custom".to_string(), "kept".to_string());
        let props = SecretProperties {
            name: "old".to_string(),
            original_name: "old".to_string(),
            value: Some(Zeroizing::new("v".to_string())),
            version: "v3".to_string(),
            version_number: Some(3),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: false,
            expires_on: None,
            not_before: None,
            tags,
            content_type: "text/plain".to_string(),
            recovery_level: None,
        };

        let req = rename_request_from_properties("new", &props).unwrap();
        assert_eq!(req.name, "new");
        assert_eq!(req.value.as_str(), "v");
        assert_eq!(req.groups, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(req.note.as_deref(), Some("n"));
        assert_eq!(req.folder.as_deref(), Some("f/g"));
        assert_eq!(req.content_type.as_deref(), Some("text/plain"));
        assert_eq!(req.enabled, Some(false));
        let t = req.tags.expect("user tags kept");
        assert_eq!(t.get("custom").map(String::as_str), Some("kept"));
        assert!(!t.contains_key("original_name") && !t.contains_key("created_by"));
        assert!(!t.contains_key("groups") && !t.contains_key("note") && !t.contains_key("folder"));
    }

    #[test]
    fn rename_request_aborts_without_a_value() {
        let props = SecretProperties {
            name: "old".to_string(),
            original_name: "old".to_string(),
            value: None,
            version: "v1".to_string(),
            version_number: None,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        };
        let err = rename_request_from_properties("new", &props).unwrap_err();
        assert!(matches!(err, BackendError::Internal(_)), "{err:?}");
    }
}
