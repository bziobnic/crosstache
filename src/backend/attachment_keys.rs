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

pub(crate) struct RawAttachmentKeyStore<'a>(&'a dyn SecretBackend);

impl<'a> RawAttachmentKeyStore<'a> {
    pub(crate) fn new(secrets: &'a dyn SecretBackend) -> Self {
        Self(secrets)
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
        self.0.get_secret(vault, name, include_value).await
    }
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        validate_name(name)?;
        self.0
            .get_secret_version(vault, name, version, include_value)
            .await
    }
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        validate_name(&request.name)?;
        self.0.set_secret(vault, request).await
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
}
