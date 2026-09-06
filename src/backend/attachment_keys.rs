//! Narrow custody access for attachment encryption. This is deliberately not a
//! `SecretBackend`: it cannot list, delete, rename, restore, or access arbitrary
//! secrets, and it never exposes the underlying provider handle.

use super::{BackendError, SecretBackend};
use crate::secret::attachment_key::{classify_reserved_name, ReservedClass};
use crate::secret::manager::{SecretProperties, SecretRequest};
use async_trait::async_trait;

#[cfg_attr(not(feature = "file-ops"), allow(dead_code))] // Encryption consumers are feature-gated.
#[async_trait]
pub trait AttachmentKeyStore: Send + Sync {
    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError>;
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError>;
    /// Commit a retained identity without replacing existing Local/AWS records.
    /// Versioned providers must return the exact new immutable version.
    async fn commit_retained_key(
        &self,
        _vault: &str,
        _request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported(
            "retained attachment key commit".into(),
        ))
    }
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError>;
}

/// Only exact canonical custody names are accepted. Alias spellings are useful
/// for generic refusal, but never for a privileged internal access path.
pub(crate) fn validate_name(name: &str) -> Result<(), BackendError> {
    if matches!(classify_reserved_name(name), ReservedClass::Ordinary) {
        return Err(BackendError::PermissionDenied(
            "attachment key access requires a canonical custody record name".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_retained_request(request: &SecretRequest) -> Result<(), BackendError> {
    if !matches!(
        classify_reserved_name(&request.name),
        ReservedClass::StrictRetainedRecord
    ) || !crate::secret::attachment_key::is_marked_key_record(
        request.content_type.as_deref().unwrap_or_default(),
    ) {
        return Err(BackendError::PermissionDenied(
            "retained commit requires a marked canonical key record".into(),
        ));
    }
    Ok(())
}

pub(crate) struct RawAttachmentKeyStore<'a> {
    secrets: &'a dyn SecretBackend,
    versioned_set: bool,
}

impl<'a> RawAttachmentKeyStore<'a> {
    pub(crate) fn new(secrets: &'a dyn SecretBackend) -> Self {
        Self {
            secrets,
            versioned_set: false,
        }
    }

    /// Azure Key Vault Set creates an immutable provider version rather than
    /// offering conditional name creation. Exact-version verification follows.
    pub(crate) fn versioned(secrets: &'a dyn SecretBackend) -> Self {
        Self {
            secrets,
            versioned_set: true,
        }
    }
}

#[async_trait]
impl AttachmentKeyStore for RawAttachmentKeyStore<'_> {
    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        validate_name(name)?;
        self.secrets.get_secret(vault, name, include_value).await
    }
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        validate_name(name)?;
        self.secrets
            .get_secret_version(vault, name, version, include_value)
            .await
    }
    async fn commit_retained_key(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        validate_retained_request(&request)?;
        if self.versioned_set {
            self.secrets.set_secret(vault, request).await
        } else {
            self.secrets.create_secret_if_absent(vault, request).await
        }
    }
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        if !matches!(
            classify_reserved_name(&request.name),
            ReservedClass::ActivePointer
        ) {
            return Err(BackendError::PermissionDenied(
                "only the active attachment pointer may be upserted".into(),
            ));
        }
        self.secrets.set_secret(vault, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{local::LocalBackend, Backend};
    use crate::config::settings::LocalConfig;
    use zeroize::Zeroizing;

    fn request(name: &str, value: &str) -> SecretRequest {
        SecretRequest {
            name: name.into(),
            value: Zeroizing::new(value.into()),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        }
    }

    #[tokio::test]
    async fn custody_cannot_read_or_overwrite_ordinary_or_aliased_names() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(tmp.path().join("store").display().to_string()),
            key_file: Some(tmp.path().join("identity").display().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        }))
        .unwrap();
        let keys = raw.attachment_keys();
        for name in [
            "ordinary",
            "xv-attachment-key-notes",
            "xv_attachment_key",
            "XV-ATTACHMENT-KEY",
        ] {
            let original = raw
                .secrets()
                .set_secret("default", request(name, "unchanged"))
                .await
                .unwrap();
            assert!(matches!(
                keys.get_secret("default", name, true).await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                keys.get_secret_version("default", name, &original.version, true)
                    .await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                keys.commit_retained_key("default", request(name, "overwritten"))
                    .await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                keys.set_secret("default", request(name, "overwritten"))
                    .await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert_eq!(
                raw.secrets()
                    .get_secret("default", name, true)
                    .await
                    .unwrap()
                    .value
                    .unwrap()
                    .as_str(),
                "unchanged"
            );
        }
    }
    #[tokio::test]
    async fn retained_commit_is_create_only_and_cannot_use_pointer_upsert() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(tmp.path().join("store").display().to_string()),
            key_file: Some(tmp.path().join("identity").display().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        }))
        .unwrap();
        let keys = raw.attachment_keys();
        let identity = age::x25519::Identity::generate();
        let key_id = crate::secret::attachment_key::AttachmentKeyId::derive(
            &identity.to_public().to_string(),
        );
        let name = crate::secret::attachment_key::retained_record_name(&key_id);
        use age::secrecy::ExposeSecret;
        let mut req = request(&name, identity.to_string().expose_secret());
        req.content_type = Some(crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE.into());
        let mut unmarked = req.clone();
        unmarked.content_type = None;
        assert!(matches!(
            keys.commit_retained_key("default", unmarked).await,
            Err(BackendError::PermissionDenied(_))
        ));
        let committed = keys
            .commit_retained_key("default", req.clone())
            .await
            .unwrap();
        assert!(!committed.version.is_empty());
        assert!(matches!(
            keys.commit_retained_key("default", req.clone()).await,
            Err(BackendError::Conflict(_))
        ));
        assert!(matches!(
            keys.set_secret("default", req).await,
            Err(BackendError::PermissionDenied(_))
        ));
        assert_eq!(
            raw.secrets()
                .list_versions("default", &name)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(all(test, feature = "aws"))]
#[path = "attachment_key_aws_tests.rs"]
mod aws_tests;
