//! Narrow custody access for attachment encryption. This is deliberately not a
//! `SecretBackend`: it cannot delete, rename, restore, or access arbitrary
//! secrets. Enumeration exposes only marked retained-record metadata, never
//! values or the underlying provider handle.

use super::{BackendError, SecretBackend};
use crate::secret::attachment_key::{
    self as key, classify_reserved_name, AttachmentKeyRef, ReservedClass,
};
#[cfg(test)]
use crate::secret::domain::SecretValue;
use crate::secret::domain::{Secret, SecretMetadata, SecretRequest};
use async_trait::async_trait;
use serde::Serialize;

/// Metadata-only view of a retained attachment key custody record.
#[derive(Debug, Clone, Serialize)]
pub struct RetainedKeySummary {
    pub name: String,
    pub key_id: String,
    pub enabled: bool,
    pub retired: bool,
}

#[cfg_attr(not(feature = "file-ops"), allow(dead_code))] // Encryption consumers are feature-gated.
#[async_trait]
pub trait AttachmentKeyStore: Send + Sync {
    /// Check deterministic local policy for a planned custody write and its
    /// required exact value readback, without mutation. The default checks the
    /// canonical custody name only; policy wrappers authorize Set and raw Get.
    /// This does not promise that remote provider permissions or concurrent
    /// state will still permit the eventual write and verification reads.
    async fn preflight_set_secret(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
        validate_name(name)
    }

    /// Certify unfiltered custody visibility. Callers also require full provider
    /// vault permissions and a complete file listing; remote IAM is not inferred.
    async fn assert_complete_visibility(&self, _vault: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "complete attachment custody visibility".into(),
        ))
    }
    async fn preflight_retirement(
        &self,
        _vault: &str,
        _reference: &AttachmentKeyRef,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "attachment key retirement".into(),
        ))
    }
    /// Advisory metadata only. Never disables, replaces or deletes an identity.
    async fn mark_retired(
        &self,
        _vault: &str,
        _reference: &AttachmentKeyRef,
    ) -> Result<SecretMetadata, BackendError> {
        Err(BackendError::Unsupported(
            "attachment key retirement".into(),
        ))
    }

    /// List visible current retained custody records without reading values or
    /// historical versions.
    async fn list_retained_keys(
        &self,
        _vault: &str,
    ) -> Result<Vec<RetainedKeySummary>, BackendError> {
        Err(BackendError::Unsupported(
            "retained attachment key enumeration".into(),
        ))
    }
    async fn get_secret_metadata(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretMetadata, BackendError>;
    async fn get_secret(&self, vault: &str, name: &str) -> Result<Secret, BackendError>;
    /// Mirrors `SecretBackend`'s split so custody reads declare disclosure the
    /// same way. No custody caller needs a value-free version read today.
    #[allow(dead_code)]
    async fn get_secret_version_metadata(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretMetadata, BackendError>;
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<Secret, BackendError>;
    /// Commit a retained identity without replacing existing Local/AWS records.
    /// Versioned providers must return the exact new immutable version.
    async fn commit_retained_key(
        &self,
        _vault: &str,
        _request: SecretRequest,
    ) -> Result<SecretMetadata, BackendError> {
        Err(BackendError::Unsupported(
            "retained attachment key commit".into(),
        ))
    }
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretMetadata, BackendError>;
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

pub(crate) fn validate_retirement_ref(reference: &AttachmentKeyRef) -> Result<(), BackendError> {
    if reference.slot != key::KeySlot::Retained
        || reference.provider_version.as_str().is_empty()
        || !matches!(
            classify_reserved_name(&key::retained_record_name(&reference.key_id)),
            ReservedClass::StrictRetainedRecord
        )
    {
        return Err(BackendError::PermissionDenied(
            "retirement requires an exact canonical retained reference".into(),
        ));
    }
    Ok(())
}
fn retirement_conflict() -> BackendError {
    BackendError::Conflict("attachment retirement identity or metadata verification failed".into())
}
fn validate_retirement_record(
    p: &Secret,
    reference: &AttachmentKeyRef,
) -> Result<(), BackendError> {
    if p.name != key::retained_record_name(&reference.key_id)
        || p.version != reference.provider_version.as_str()
        || !p.enabled
        || !key::is_marked_key_record(&p.content_type)
    {
        return Err(retirement_conflict());
    }
    let identity = p
        .value
        .expose_secret()
        .trim()
        .parse::<age::x25519::Identity>()
        .map_err(|_| retirement_conflict())?;
    if key::AttachmentKeyId::derive(&identity.to_public().to_string()) != reference.key_id {
        return Err(retirement_conflict());
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
    async fn assert_complete_visibility(&self, _vault: &str) -> Result<(), BackendError> {
        Ok(())
    }
    async fn preflight_retirement(
        &self,
        _vault: &str,
        reference: &AttachmentKeyRef,
    ) -> Result<(), BackendError> {
        validate_retirement_ref(reference)
    }
    async fn mark_retired(
        &self,
        vault: &str,
        reference: &AttachmentKeyRef,
    ) -> Result<SecretMetadata, BackendError> {
        use crate::secret::domain::{FieldUpdate, SecretUpdateRequest};
        validate_retirement_ref(reference)?;
        let name = key::retained_record_name(&reference.key_id);
        let before = self.secrets.get_secret(vault, &name).await?;
        validate_retirement_record(&before, reference)?;
        let exact = self
            .secrets
            .get_secret_version(vault, &name, reference.provider_version.as_str())
            .await?;
        validate_retirement_record(&exact, reference)?;
        if before.value != exact.value {
            return Err(retirement_conflict());
        }
        if before.tags.get(key::KEY_RETIRED_TAG).map(String::as_str) != Some("true") {
            self.secrets
                .update_secret(
                    vault,
                    &name,
                    SecretUpdateRequest {
                        name: name.clone(),
                        expected_revision: None,
                        value: None,
                        content_type: None,
                        enabled: None,
                        expires_on: FieldUpdate::Unchanged,
                        not_before: FieldUpdate::Unchanged,
                        tags: Some(std::collections::HashMap::from([(
                            key::KEY_RETIRED_TAG.into(),
                            "true".into(),
                        )])),
                        groups: None,
                        note: FieldUpdate::Unchanged,
                        folder: FieldUpdate::Unchanged,
                        replace_tags: false,
                        replace_groups: false,
                    },
                )
                .await?;
        }
        let after = self.secrets.get_secret(vault, &name).await?;
        validate_retirement_record(&after, reference)?;
        let mut expected_tags = before.tags.clone();
        expected_tags.insert(key::KEY_RETIRED_TAG.into(), "true".into());
        if after.value != before.value
            || after.tags != expected_tags
            || after.original_name != before.original_name
            || after.content_type != before.content_type
            || after.enabled != before.enabled
            || after.expires_on != before.expires_on
            || after.not_before != before.not_before
            || after.created_on != before.created_on
            || after.created_timestamp != before.created_timestamp
            || after.recovery_level != before.recovery_level
        {
            return Err(retirement_conflict());
        }
        let exact_after = self
            .secrets
            .get_secret_version(vault, &name, reference.provider_version.as_str())
            .await?;
        validate_retirement_record(&exact_after, reference)?;
        if exact_after.value != before.value {
            return Err(retirement_conflict());
        }
        Ok(after.into_metadata())
    }

    async fn list_retained_keys(
        &self,
        vault: &str,
    ) -> Result<Vec<RetainedKeySummary>, BackendError> {
        let mut retained = self
            .secrets
            .list_secrets(vault, None)
            .await?
            .into_iter()
            .filter_map(|summary| {
                if !matches!(
                    classify_reserved_name(&summary.name),
                    ReservedClass::StrictRetainedRecord
                ) || summary.content_type
                    != crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE
                {
                    return None;
                }
                let key_id = summary
                    .name
                    .strip_prefix(crate::secret::attachment_key::RETAINED_RECORD_PREFIX)?
                    .to_owned();
                Some(RetainedKeySummary {
                    name: summary.name,
                    key_id,
                    enabled: summary.enabled,
                    retired: summary.tags.get(key::KEY_RETIRED_TAG).map(String::as_str)
                        == Some("true"),
                })
            })
            .collect::<Vec<_>>();
        retained.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(retained)
    }

    async fn get_secret_metadata(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretMetadata, BackendError> {
        validate_name(name)?;
        self.secrets.get_secret_metadata(vault, name).await
    }
    async fn get_secret(&self, vault: &str, name: &str) -> Result<Secret, BackendError> {
        validate_name(name)?;
        self.secrets.get_secret(vault, name).await
    }
    async fn get_secret_version_metadata(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretMetadata, BackendError> {
        validate_name(name)?;
        self.secrets
            .get_secret_version_metadata(vault, name, version)
            .await
    }
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<Secret, BackendError> {
        validate_name(name)?;
        self.secrets.get_secret_version(vault, name, version).await
    }
    async fn commit_retained_key(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretMetadata, BackendError> {
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
    ) -> Result<SecretMetadata, BackendError> {
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

    fn request(name: &str, value: &str) -> SecretRequest {
        SecretRequest {
            name: name.into(),
            value: SecretValue::new(value),
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
    async fn retained_enumeration_returns_only_exact_marked_records_in_name_order() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(tmp.path().join("store").display().to_string()),
            key_file: Some(tmp.path().join("identity").display().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        }))
        .unwrap();
        let key_a = format!("ak1-{}", "a".repeat(64));
        let key_b = format!("ak1-{}", "b".repeat(64));
        let name_a = format!("xv-attachment-key-{key_a}");
        let name_b = format!("xv-attachment-key-{key_b}");
        let fixtures: [(&str, Option<&str>, bool); 6] = [
            (name_b.as_str(), Some(crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE), false),
            (name_a.as_str(), Some(crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE), true),
            ("xv-attachment-key-ak1-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", None, true),
            ("xv-attachment-key-notes", Some(crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE), true),
            ("xv-attachment-key", Some(crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE), true),
            ("ordinary", None, true),
        ];
        for (name, content_type, enabled) in fixtures {
            let mut req = request(name, "must-not-be-returned");
            req.content_type = content_type.map(str::to_owned);
            req.enabled = Some(enabled);
            raw.secrets().set_secret("default", req).await.unwrap();
        }

        let retained = raw
            .attachment_keys()
            .list_retained_keys("default")
            .await
            .unwrap();

        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].name, name_a);
        assert_eq!(retained[0].key_id, key_a);
        assert!(retained[0].enabled);
        assert_eq!(retained[1].name, name_b);
        assert_eq!(retained[1].key_id, key_b);
        assert!(!retained[1].enabled);
    }

    struct ErrorListBackend;

    #[async_trait]
    impl SecretBackend for ErrorListBackend {
        async fn set_secret(
            &self,
            _: &str,
            _: SecretRequest,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }
        async fn get_secret_metadata(
            &self,
            _: &str,
            _: &str,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }

        async fn get_secret(&self, _: &str, _: &str) -> Result<Secret, BackendError> {
            unimplemented!()
        }
        async fn get_secret_version_metadata(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }

        async fn get_secret_version(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Secret, BackendError> {
            unimplemented!()
        }
        async fn list_secrets(
            &self,
            _: &str,
            _: Option<&str>,
        ) -> Result<Vec<crate::secret::domain::SecretSummary>, BackendError> {
            Err(BackendError::AuthenticationFailed(
                "provider list failed".into(),
            ))
        }
        async fn delete_secret(&self, _: &str, _: &str) -> Result<(), BackendError> {
            unimplemented!()
        }
        async fn update_secret(
            &self,
            _: &str,
            _: &str,
            _: crate::secret::domain::SecretUpdateRequest,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }
    }

    struct LegacyKeyStore;

    #[async_trait]
    impl AttachmentKeyStore for LegacyKeyStore {
        async fn get_secret_metadata(
            &self,
            _: &str,
            _: &str,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }

        async fn get_secret(&self, _: &str, _: &str) -> Result<Secret, BackendError> {
            unimplemented!()
        }
        async fn get_secret_version_metadata(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }

        async fn get_secret_version(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Secret, BackendError> {
            unimplemented!()
        }
        async fn set_secret(
            &self,
            _: &str,
            _: SecretRequest,
        ) -> Result<SecretMetadata, BackendError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn retained_enumeration_propagates_provider_list_errors() {
        let error = RawAttachmentKeyStore::new(&ErrorListBackend)
            .list_retained_keys("default")
            .await
            .unwrap_err();
        assert!(
            matches!(error, BackendError::AuthenticationFailed(message) if message == "provider list failed")
        );
    }

    #[tokio::test]
    async fn stores_without_enumeration_support_return_unsupported() {
        let error = LegacyKeyStore
            .list_retained_keys("default")
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::Unsupported(_)));
    }

    #[tokio::test]
    async fn stores_without_retirement_support_fail_closed() {
        let reference = key::AttachmentKeyRef {
            key_id: key::AttachmentKeyId::derive("test"),
            slot: key::KeySlot::Retained,
            provider_version: key::SecretVersion::new("version"),
        };
        assert!(matches!(
            LegacyKeyStore.assert_complete_visibility("default").await,
            Err(BackendError::Unsupported(_))
        ));
        assert!(matches!(
            LegacyKeyStore
                .preflight_retirement("default", &reference)
                .await,
            Err(BackendError::Unsupported(_))
        ));
        assert!(matches!(
            LegacyKeyStore.mark_retired("default", &reference).await,
            Err(BackendError::Unsupported(_))
        ));
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
                keys.get_secret("default", name).await,
                Err(BackendError::PermissionDenied(_))
            ));
            assert!(matches!(
                keys.get_secret_version("default", name, &original.version)
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
                    .get_secret("default", name)
                    .await
                    .unwrap()
                    .value
                    .expose_secret(),
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
