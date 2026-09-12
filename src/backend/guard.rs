//! Guarded generic secret facade (design §8 / task §E).
//!
//! [`GuardedSecretBackend`] wraps any [`SecretBackend`] and structurally
//! enforces the attachment-key custody boundary on *generic* secret operations,
//! before any provider I/O:
//!
//! - every mutation of the exact active pointer or a strict-format retained
//!   record (`xv-attachment-key-ak1-<64 hex>`) is refused with zero provider
//!   calls (invariant I3);
//! - `restore_from_backup` is disabled entirely, because its destination name
//!   cannot be verified before the provider mutation happens (design Decision
//!   F) — it never reaches the inner backend;
//! - `list_secrets` hides the active pointer and *marked* key-custody records,
//!   while leaving unmarked strict-format user collisions visible (§E).
//!
//! The custody path (`crate::secret::attachments`) must NOT go through this
//! facade: it legitimately reads and writes the reserved records through
//! `AttachmentKeyStore`. Registry construction installs the owned guard on
//! every generic handle; custody delegation preserves any agent policy layer.
//!
//! Read-hiding of the reserved records (denying `get_secret` on the pointer and
//! marked records) is intentionally deferred: it needs a metadata pre-fetch to
//! classify marked-ness and is a disclosure concern, not the data-loss concern
//! this decorator closes first. `list_secrets` hiding is applied here because it
//! is free (the summaries already carry `content_type`).

use super::{AuditBackend, Backend, BackendCapabilities, BackendKind, VaultBackend};
use async_trait::async_trait;
use std::sync::Arc;

use crate::secret::attachment_key::{
    generic_mutation_blocked_canonical, hidden_from_generic_listing_canonical,
};
use crate::secret::domain::{
    DeletedSecretSummary, SecretProperties, SecretRequest, SecretSnapshot, SecretSummary,
    SecretUpdateRequest,
};

use super::error::BackendError;
use super::secret::SecretBackend;

/// A generic-facade wrapper around a raw [`SecretBackend`] that enforces the
/// reserved attachment-key custody boundary structurally.
pub struct GuardedSecretBackend<'a> {
    inner: SecretSource<'a>,
}

enum SecretSource<'a> {
    Borrowed(&'a dyn SecretBackend),
    Owned(Arc<dyn Backend>),
}

impl SecretSource<'_> {
    fn secrets(&self) -> &dyn SecretBackend {
        match self {
            Self::Borrowed(secrets) => *secrets,
            Self::Owned(backend) => backend.secrets(),
        }
    }
}

/// Registry-owned boundary. The raw backend is private; generic callers only
/// obtain the guarded secret facade, including through cloned/lazy handles.
pub(crate) struct GuardedBackend {
    inner: Arc<dyn Backend>,
    secrets: GuardedSecretBackend<'static>,
}

impl GuardedBackend {
    pub(crate) fn wrap(inner: Arc<dyn Backend>) -> Arc<dyn Backend> {
        Arc::new(Self {
            secrets: GuardedSecretBackend {
                inner: SecretSource::Owned(inner.clone()),
            },
            inner,
        })
    }
}

#[async_trait]
impl Backend for GuardedBackend {
    async fn validate_transfer_recovery_path(
        &self,
        vault: &str,
        path: &std::path::Path,
    ) -> Result<(), BackendError> {
        self.inner
            .validate_transfer_recovery_path(vault, path)
            .await
    }

    async fn transfer_location(
        &self,
        vault: &str,
    ) -> Result<crate::backend::TransferLocation, BackendError> {
        self.inner.transfer_location(vault).await
    }

    async fn transfer_secret_namespace(&self, vault: &str) -> Result<String, BackendError> {
        self.inner.transfer_secret_namespace(vault).await
    }

    async fn transfer_secret_physical_namespace(
        &self,
        vault: &str,
    ) -> Result<String, BackendError> {
        self.inner.transfer_secret_physical_namespace(vault).await
    }

    async fn transfer_file_physical_namespace(&self, vault: &str) -> Result<String, BackendError> {
        self.inner.transfer_file_physical_namespace(vault).await
    }

    async fn transfer_secret_names_collide(
        &self,
        vault: &str,
        left: &str,
        right: &str,
    ) -> Result<bool, BackendError> {
        self.inner
            .transfer_secret_names_collide(vault, left, right)
            .await
    }

    async fn prepare_transfer_destination(
        &self,
        vault: &str,
        expected: &crate::backend::TransferLocation,
    ) -> Result<crate::backend::TransferLocation, BackendError> {
        self.inner
            .prepare_transfer_destination(vault, expected)
            .await
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn kind(&self) -> BackendKind {
        self.inner.kind()
    }
    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }
    fn secrets(&self) -> &dyn SecretBackend {
        &self.secrets
    }
    fn attachment_keys(&self) -> Box<dyn super::attachment_keys::AttachmentKeyStore + '_> {
        self.inner.attachment_keys()
    }
    async fn attachment_names(&self, vault: &str, name: &str) -> Result<Vec<String>, BackendError> {
        self.inner.attachment_names(vault, name).await
    }
    fn vaults(&self) -> Option<&dyn VaultBackend> {
        self.inner.vaults()
    }
    fn audit(&self) -> Option<&dyn AuditBackend> {
        self.inner.audit()
    }
    #[cfg(feature = "file-ops")]
    fn files(&self) -> Option<&dyn super::FileBackend> {
        self.inner.files()
    }
    async fn health_check(&self) -> Result<(), BackendError> {
        self.inner.health_check().await
    }
}

impl<'a> GuardedSecretBackend<'a> {
    /// Wrap a raw secret backend in the generic guard.
    pub fn new(inner: &'a dyn SecretBackend) -> Self {
        Self {
            inner: SecretSource::Borrowed(inner),
        }
    }

    /// Refuse a generic mutation of a protected custody resource before any
    /// provider I/O.
    fn ensure_mutable(&self, name: &str) -> Result<(), BackendError> {
        if generic_mutation_blocked_canonical(name) {
            return Err(BackendError::PermissionDenied(format!(
                "'{name}' is a protected attachment key custody resource and cannot be created, \
                 modified, renamed, or deleted through generic secret operations"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl SecretBackend for GuardedSecretBackend<'_> {
    // -- Mutations: guarded before delegating -------------------------------

    async fn validate_transfer_metadata(
        &self,
        vault: &str,
        request: &SecretRequest,
    ) -> Result<(), BackendError> {
        self.ensure_mutable(&request.name)?;
        self.inner
            .secrets()
            .validate_transfer_metadata(vault, request)
            .await
    }

    async fn validate_transfer_delete(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.ensure_mutable(name)?;
        self.inner
            .secrets()
            .validate_transfer_delete(vault, name)
            .await
    }

    fn supports_atomic_create(&self) -> bool {
        self.inner.secrets().supports_atomic_create()
    }
    fn supports_conditional_delete(&self) -> bool {
        self.inner.secrets().supports_conditional_delete()
    }
    async fn delete_secret_if_revision(
        &self,
        vault: &str,
        name: &str,
        expected_revision: &str,
    ) -> Result<(), BackendError> {
        self.ensure_mutable(name)?;
        self.inner
            .secrets()
            .delete_secret_if_revision(vault, name, expected_revision)
            .await
    }

    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(&request.name)?;
        self.inner.secrets().set_secret(vault, request).await
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.ensure_mutable(name)?;
        self.inner.secrets().delete_secret(vault, name).await
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(name)?;
        self.inner
            .secrets()
            .update_secret(vault, name, request)
            .await
    }

    async fn create_secret_if_absent(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(&request.name)?;
        self.inner
            .secrets()
            .create_secret_if_absent(vault, request)
            .await
    }

    async fn update_secret_if_revision(
        &self,
        vault: &str,
        name: &str,
        expected_revision: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(name)?;
        self.inner
            .secrets()
            .update_secret_if_revision(vault, name, expected_revision, request)
            .await
    }

    async fn rename_secret(
        &self,
        vault: &str,
        name: &str,
        new_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(name)?;
        self.ensure_mutable(new_name)?;
        self.inner
            .secrets()
            .rename_secret(vault, name, new_name)
            .await
    }

    async fn rename_secret_if_revision(
        &self,
        vault: &str,
        name: &str,
        new_name: &str,
        expected_revision: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(name)?;
        self.ensure_mutable(new_name)?;
        self.inner
            .secrets()
            .rename_secret_if_revision(vault, name, new_name, expected_revision)
            .await
    }

    async fn rollback(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(name)?;
        self.inner.secrets().rollback(vault, name, version).await
    }

    async fn restore_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.ensure_mutable(name)?;
        self.inner.secrets().restore_secret(vault, name).await
    }

    async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.ensure_mutable(name)?;
        self.inner.secrets().purge_secret(vault, name).await
    }

    async fn native_rotate(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.ensure_mutable(name)?;
        self.inner.secrets().native_rotate(vault, name).await
    }

    /// Disabled entirely: an opaque backup blob names its own destination, so
    /// the reserved-resource guard cannot vet the target before the provider
    /// mutates it. Never reaches the inner backend (design Decision F).
    async fn restore_from_backup(
        &self,
        _vault: &str,
        _backup: &[u8],
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported(
            "generic restore-from-backup is disabled: the destination cannot be verified before \
             the provider mutation"
                .into(),
        ))
    }

    // -- Listing: hide pointer + marked records -----------------------------

    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        let mut out = self
            .inner
            .secrets()
            .list_secrets(vault, group_filter)
            .await?;
        out.retain(|s| !hidden_from_generic_listing_canonical(&s.name, &s.content_type));
        Ok(out)
    }

    // -- Reads and metadata: delegated (read-hiding deferred) ---------------

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .secrets()
            .get_secret(vault, name, include_value)
            .await
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .secrets()
            .get_secret_version(vault, name, version, include_value)
            .await
    }

    async fn get_secret_snapshot(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretSnapshot, BackendError> {
        self.inner
            .secrets()
            .get_secret_snapshot(vault, name, include_value)
            .await
    }

    async fn get_transfer_snapshot(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretSnapshot, BackendError> {
        self.inner
            .secrets()
            .get_transfer_snapshot(vault, name, include_value)
            .await
    }

    async fn validate_secret_revision(
        &self,
        vault: &str,
        name: &str,
        expected_revision: &str,
    ) -> Result<SecretProperties, BackendError> {
        self.inner
            .secrets()
            .validate_secret_revision(vault, name, expected_revision)
            .await
    }

    async fn list_versions(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        self.inner.secrets().list_versions(vault, name).await
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        self.inner.secrets().secret_exists(vault, name).await
    }

    async fn list_deleted_secrets(
        &self,
        vault: &str,
    ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
        self.inner.secrets().list_deleted_secrets(vault).await
    }

    async fn backup_secret(&self, vault: &str, name: &str) -> Result<Vec<u8>, BackendError> {
        self.inner.secrets().backup_secret(vault, name).await
    }

    // -- Capability passthrough --------------------------------------------

    fn supports_conditional_update(&self) -> bool {
        self.inner.secrets().supports_conditional_update()
    }

    fn supports_revision_validation(&self) -> bool {
        self.inner.secrets().supports_revision_validation()
    }

    fn supports_atomic_rename(&self) -> bool {
        self.inner.secrets().supports_atomic_rename()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use zeroize::Zeroizing;

    const STRICT: &str =
        "xv-attachment-key-ak1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Records every inner call so tests can assert a guarded refusal reaches
    /// the provider zero times.
    struct SpyBackend {
        calls: Mutex<Vec<String>>,
        summaries: Vec<SecretSummary>,
    }

    impl SpyBackend {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                summaries: Vec::new(),
            }
        }
        fn record(&self, call: &str) {
            self.calls.lock().unwrap().push(call.to_string());
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn props(name: &str) -> SecretProperties {
        SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: None,
            version: "v1".to_string(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    fn summary(name: &str, content_type: &str) -> SecretSummary {
        SecretSummary {
            name: name.to_string(),
            original_name: name.to_string(),
            note: None,
            folder: None,
            groups: None,
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            content_type: content_type.to_string(),
            tags: HashMap::new(),
        }
    }

    #[async_trait]
    impl SecretBackend for SpyBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> Result<SecretProperties, BackendError> {
            self.record(&format!("set_secret:{}", request.name));
            Ok(props(&request.name))
        }
        async fn get_secret(
            &self,
            _vault: &str,
            name: &str,
            _include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            self.record(&format!("get_secret:{name}"));
            Ok(props(name))
        }
        async fn get_secret_version(
            &self,
            _vault: &str,
            name: &str,
            _version: &str,
            _include_value: bool,
        ) -> Result<SecretProperties, BackendError> {
            self.record(&format!("get_secret_version:{name}"));
            Ok(props(name))
        }
        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>, BackendError> {
            self.record("list_secrets");
            Ok(self.summaries.clone())
        }
        async fn delete_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
            self.record(&format!("delete_secret:{name}"));
            Ok(())
        }
        async fn update_secret(
            &self,
            _vault: &str,
            name: &str,
            _request: SecretUpdateRequest,
        ) -> Result<SecretProperties, BackendError> {
            self.record(&format!("update_secret:{name}"));
            Ok(props(name))
        }
        async fn restore_from_backup(
            &self,
            _vault: &str,
            _backup: &[u8],
        ) -> Result<SecretProperties, BackendError> {
            // A "working" inner restore: the guard must never reach this.
            self.record("restore_from_backup");
            Ok(props("restored"))
        }
    }

    #[async_trait]
    impl Backend for SpyBackend {
        fn name(&self) -> &'static str {
            "spy"
        }

        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        fn secrets(&self) -> &dyn SecretBackend {
            self
        }

        async fn attachment_names(
            &self,
            vault: &str,
            name: &str,
        ) -> Result<Vec<String>, BackendError> {
            self.record(&format!("attachment_names:{vault}:{name}"));
            Ok(vec![format!("attachments/{name}/proof.txt")])
        }

        async fn health_check(&self) -> Result<(), BackendError> {
            Ok(())
        }
    }

    fn req(name: &str) -> SecretRequest {
        SecretRequest {
            name: name.to_string(),
            value: Zeroizing::new("v".to_string()),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        }
    }

    #[tokio::test]
    async fn blocks_reserved_mutations_with_zero_provider_calls() {
        let spy = SpyBackend::new();
        let guard = GuardedSecretBackend::new(&spy);

        for name in ["xv-attachment-key", STRICT] {
            assert!(matches!(
                guard.set_secret("v", req(name)).await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                guard.delete_secret("v", name).await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                guard.rename_secret("v", name, "elsewhere").await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                guard
                    .delete_secret_if_revision("v", name, "generation")
                    .await,
                Err(BackendError::PermissionDenied(_))
            ));
            // Renaming a normal secret ONTO a reserved name is also blocked.
            assert!(matches!(
                guard.rename_secret("v", "normal", name).await,
                Err(BackendError::PermissionDenied(_))
            ));
        }
        assert!(
            spy.calls().is_empty(),
            "no reserved mutation may reach the provider: {:?}",
            spy.calls()
        );
    }

    #[tokio::test]
    async fn blocks_aliased_pointer_spellings() {
        let spy = SpyBackend::new();
        let guard = GuardedSecretBackend::new(&spy);
        // Underscore, repeated-hyphen, and uppercase aliases all canonicalize
        // onto the reserved pointer and must be refused (design §8).
        for alias in [
            "xv_attachment_key",
            "xv--attachment--key",
            "XV-ATTACHMENT-KEY",
        ] {
            assert!(
                matches!(
                    guard.set_secret("v", req(alias)).await,
                    Err(BackendError::PermissionDenied(_))
                ),
                "alias {alias} must be blocked"
            );
        }
        assert!(spy.calls().is_empty(), "{:?}", spy.calls());
    }

    #[tokio::test]
    async fn passes_through_ordinary_mutations() {
        let spy = SpyBackend::new();
        let guard = GuardedSecretBackend::new(&spy);
        guard.set_secret("v", req("normal")).await.unwrap();
        guard
            .delete_secret("v", "xv-attachment-key-notes")
            .await
            .unwrap();
        assert_eq!(
            spy.calls(),
            vec![
                "set_secret:normal".to_string(),
                "delete_secret:xv-attachment-key-notes".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn restore_from_backup_is_disabled_with_zero_provider_calls() {
        let spy = SpyBackend::new();
        let guard = GuardedSecretBackend::new(&spy);
        assert!(matches!(
            guard.restore_from_backup("v", b"blob").await,
            Err(BackendError::Unsupported(_))
        ));
        assert!(spy.calls().is_empty(), "{:?}", spy.calls());
    }

    #[tokio::test]
    async fn list_hides_pointer_and_marked_records_only() {
        let mut spy = SpyBackend::new();
        spy.summaries = vec![
            summary("normal", ""),
            summary("xv-attachment-key", ""),
            summary(STRICT, KEY_RECORD_CONTENT_TYPE),
            summary(
                "xv-attachment-key-ak1-deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "",
            ),
            summary("xv-attachment-key-notes", ""),
        ];
        let guard = GuardedSecretBackend::new(&spy);
        let out = guard.list_secrets("v", None).await.unwrap();
        let names: Vec<_> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "normal",
                "xv-attachment-key-ak1-deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                "xv-attachment-key-notes",
            ]
        );
    }

    #[tokio::test]
    async fn reads_are_delegated_unchanged() {
        let spy = SpyBackend::new();
        let guard = GuardedSecretBackend::new(&spy);
        // Reads (including of reserved names) currently delegate — read-hiding
        // is deferred; this locks the current behavior.
        guard
            .get_secret("v", "xv-attachment-key", false)
            .await
            .unwrap();
        guard.get_secret("v", "normal", false).await.unwrap();
        assert_eq!(
            spy.calls(),
            vec![
                "get_secret:xv-attachment-key".to_string(),
                "get_secret:normal".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn owned_guard_forwards_attachment_visibility_to_inner_backend() {
        let spy = Arc::new(SpyBackend::new());
        let guarded = GuardedBackend::wrap(spy.clone());

        assert_eq!(
            guarded.attachment_names("prod", "db").await.unwrap(),
            vec!["attachments/db/proof.txt"]
        );
        assert_eq!(spy.calls(), vec!["attachment_names:prod:db"]);
    }
}
