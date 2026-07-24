//! Secret backend trait.
//!
//! [`SecretBackend`] defines the contract for secret CRUD operations.
//! Every backend must implement the 6 required methods; the 8 optional
//! methods have default implementations that return [`BackendError::Unsupported`].

use async_trait::async_trait;
use std::collections::HashMap;

use crate::secret::manager::{
    DeletedSecretSummary, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
};

use super::error::BackendError;

/// A secret value/metadata snapshot paired with an opaque, non-reusable
/// provider revision. The revision is a compare-and-swap token only: callers
/// must not infer ordering or expose provider internals from it.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct SecretSnapshot {
    pub properties: SecretProperties,
    pub revision: String,
}

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

    /// Whether the backend can atomically compare an opaque source revision
    /// and commit a complete secret update.
    fn supports_conditional_update(&self) -> bool {
        false
    }

    /// Whether the backend can atomically validate an opaque source revision
    /// without writing a new version. Conditional conversion requires this in
    /// addition to conditional update support because a conversion may be a
    /// no-op.
    fn supports_revision_validation(&self) -> bool {
        false
    }

    /// Read one complete secret generation and its opaque revision.
    async fn get_secret_snapshot(
        &self,
        _vault: &str,
        _name: &str,
        _include_value: bool,
    ) -> Result<SecretSnapshot, BackendError> {
        Err(BackendError::Unsupported(
            "conditional secret snapshots".into(),
        ))
    }

    /// Commit an update only while `expected_revision` still names the active
    /// generation. The comparison and update must share one provider commit
    /// point.
    async fn update_secret_if_revision(
        &self,
        _vault: &str,
        _name: &str,
        _expected_revision: &str,
        _request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported(
            "conditional secret update".into(),
        ))
    }

    /// Atomically validate that `expected_revision` still names the active
    /// generation without creating a new secret version. This is the commit
    /// point for conversions whose prepared result is otherwise a no-op.
    async fn validate_secret_revision(
        &self,
        _vault: &str,
        _name: &str,
        _expected_revision: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported(
            "conditional secret revision validation".into(),
        ))
    }

    /// Create a secret only if the destination is still absent at the
    /// provider's commit point. Implementations must never update or replace
    /// an existing secret. Backends without a conditional-create primitive
    /// must leave this unsupported rather than emulate it with a racy
    /// exists-then-set sequence.
    async fn create_secret_if_absent(
        &self,
        _vault: &str,
        _request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported(
            "atomic create-if-absent required for rename".into(),
        ))
    }

    /// Whether this backend can atomically guard the source revision, guard
    /// destination absence, create the complete destination, and remove the
    /// source as one recoverable transaction.
    fn supports_atomic_rename(&self) -> bool {
        false
    }

    /// Rename only if the source revision is still current. Implementations
    /// must preserve the source and destination when either CAS guard fails.
    async fn rename_secret_if_revision(
        &self,
        _vault: &str,
        _name: &str,
        _new_name: &str,
        _expected_revision: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("atomic secret rename".into()))
    }

    /// Rename a secret. Backends must override this only when the entire
    /// source/destination mutation is atomic and recoverable. The default
    /// refuses the old read-create-delete emulation because it can copy a
    /// stale source or strand two live names.
    async fn rename_secret(
        &self,
        _vault: &str,
        _name: &str,
        _new_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("atomic secret rename".into()))
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

/// Extracts the denormalized `groups`/`note`/`folder` tags out of `tags`
/// (in place) and returns them, and strips the bookkeeping tags
/// (`original_name`, `created_by`) that a fresh write regenerates.
///
/// Every backend's `get_secret` folds groups/note/folder into plain tag
/// keys on `SecretProperties.tags` for display convenience — Azure stores
/// them as literal tags natively (so this "just works" there), while AWS's
/// `props_from_describe` explicitly lifts `xv:groups`/the description
/// field/`xv:folder` into the same plain `"groups"`/`"note"`/`"folder"`
/// keys. Any caller that reads a secret's properties and later writes a
/// tags map back (`SecretRequest.tags` / `SecretUpdateRequest.tags`) MUST
/// run it through this first: handing the denormalized map back verbatim
/// would create extra plain `groups`/`note`/`folder` *user* tags on AWS
/// (duplicating the real `xv:groups`/description/`xv:folder`) or persist
/// them into the wrong storage location on the local backend
/// (`SecretMeta.tags` instead of the dedicated `.groups`/`.note`/`.folder`
/// fields) — the exact bug class the #315 copy/move review caught, and
/// later reproduced by the record-types update/conversion paths (Bugbot
/// review, record-types plan). This is the single source of truth for
/// that key list; do not hand-roll it elsewhere.
pub(crate) fn split_denormalized_tags(
    tags: &mut HashMap<String, String>,
) -> (Option<Vec<String>>, Option<String>, Option<String>) {
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
    tags.remove(super::TAG_ORIGINAL_NAME);
    tags.remove(super::TAG_CREATED_BY);
    (groups, note, folder)
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
    let (groups, note, folder) = split_denormalized_tags(&mut tags);

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
    }

    impl StubBackend {
        fn new() -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
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

        async fn create_secret_if_absent(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            let mut secrets = self.secrets.lock().unwrap();
            if secrets.contains_key(&request.name) {
                return Err(BackendError::Conflict(format!(
                    "secret '{}' already exists",
                    request.name
                )));
            }
            let props = props_from_request(&request, false);
            secrets.insert(request.name.clone(), request);
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
    async fn default_rename_refuses_before_reading_or_mutating() {
        let backend = StubBackend::new();
        backend
            .set_secret("v", seeded_request("old-name"))
            .await
            .unwrap();

        let error = backend
            .rename_secret("v", "old-name", "new-name")
            .await
            .unwrap_err();

        assert!(matches!(error, BackendError::Unsupported(_)), "{error:?}");
        assert!(backend.get_secret("v", "old-name", true).await.is_ok());
        assert!(matches!(
            backend.get_secret("v", "new-name", false).await,
            Err(BackendError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn default_revision_guarded_rename_is_explicitly_unsupported() {
        let backend = StubBackend::new();
        let error = backend
            .rename_secret_if_revision("v", "old-name", "new-name", "revision")
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::Unsupported(_)), "{error:?}");
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
