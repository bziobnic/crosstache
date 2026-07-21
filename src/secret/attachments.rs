//! Secret file attachments — client-side age encryption over `FileBackend`.
//!
//! Attachments are age-encrypted with a per-vault x25519 identity stored as
//! the reserved secret [`ATTACHMENT_KEY_SECRET`] in the vault's own secret
//! store, so access to attachment plaintext is gated by vault (secret-store)
//! permissions, not storage-layer permissions. Ciphertext lives in ordinary
//! file storage under `attachments/<secret-name>/<filename>`; the association
//! is the naming convention. See
//! `docs/superpowers/specs/2026-07-21-secret-file-attachments-design.md`.

use age::secrecy::ExposeSecret;
use zeroize::Zeroizing;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::SecretRequest;

/// Reserved per-vault secret holding the age identity for attachments.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub const ATTACHMENT_KEY_SECRET: &str = "xv-attachment-key";
/// File-metadata key marking client-side-encrypted content.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub const ENC_METADATA_KEY: &str = "xv-encrypted";
/// File-metadata value for age encryption.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub const ENC_METADATA_VALUE: &str = "age";

/// Blob-name prefix for a secret's attachments.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub fn attachment_prefix(secret_name: &str) -> String {
    format!("attachments/{secret_name}/")
}

/// Full blob name for one attachment of a secret.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub fn attachment_blob_name(secret_name: &str, attachment: &str) -> String {
    format!("{}{attachment}", attachment_prefix(secret_name))
}

/// Parse an age identity out of a stored secret value.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
fn parse_identity(value: &str, vault: &str) -> Result<age::x25519::Identity> {
    value.trim().parse::<age::x25519::Identity>().map_err(|e| {
        CrosstacheError::invalid_argument(format!(
            "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' does not hold a valid age identity: {e}"
        ))
    })
}

/// Fetch the vault's attachment identity. Errors (actionably) if absent.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub async fn get_identity(
    secrets: &dyn SecretBackend,
    vault: &str,
) -> Result<age::x25519::Identity> {
    match secrets.get_secret(vault, ATTACHMENT_KEY_SECRET, true).await {
        Ok(props) => {
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            parse_identity(&value, vault)
        }
        Err(BackendError::NotFound { .. }) => Err(CrosstacheError::invalid_argument(format!(
            "attachment key not found in vault '{vault}' — no attachments have been created here, or the '{ATTACHMENT_KEY_SECRET}' secret was deleted"
        ))),
        Err(e) => Err(e.into()),
    }
}

/// Fetch the vault's attachment identity, generating and storing it on first
/// use. After a create, the stored value is re-read and used, so a concurrent
/// create race converges on a single key.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub async fn get_or_create_identity(
    secrets: &dyn SecretBackend,
    vault: &str,
) -> Result<age::x25519::Identity> {
    match secrets.get_secret(vault, ATTACHMENT_KEY_SECRET, true).await {
        Ok(props) => {
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            parse_identity(&value, vault)
        }
        Err(BackendError::NotFound { .. }) => {
            let identity = age::x25519::Identity::generate();
            let request = SecretRequest {
                name: ATTACHMENT_KEY_SECRET.to_string(),
                value: Zeroizing::new(identity.to_string().expose_secret().to_string()),
                content_type: Some("application/x-age-identity".to_string()),
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: Some(
                    "crosstache attachment encryption key — deleting this makes all \
                     attachments in this vault unreadable"
                        .to_string(),
                ),
                folder: None,
            };
            secrets.set_secret(vault, request).await?;
            // Re-read: under a concurrent first-create, whichever write landed
            // last is authoritative; using the stored value converges all
            // clients on one key.
            let props = secrets
                .get_secret(vault, ATTACHMENT_KEY_SECRET, true)
                .await?;
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            parse_identity(&value, vault)
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::secret::manager::{
        SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };

    /// In-memory SecretBackend: get/set only, everything else Unsupported.
    /// `set_count` asserts key reuse (no regeneration on second call).
    pub(super) struct StubSecrets {
        pub secrets: Mutex<HashMap<String, String>>,
        pub set_count: Mutex<usize>,
    }

    impl StubSecrets {
        pub fn new() -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
                set_count: Mutex::new(0),
            }
        }
    }

    fn props(name: &str, value: Option<&str>) -> SecretProperties {
        SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: value.map(|v| Zeroizing::new(v.to_string())),
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

    #[async_trait]
    impl SecretBackend for StubSecrets {
        async fn set_secret(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            *self.set_count.lock().unwrap() += 1;
            self.secrets
                .lock()
                .unwrap()
                .insert(request.name.clone(), request.value.to_string());
            Ok(props(&request.name, None))
        }

        async fn get_secret(
            &self,
            _vault: &str,
            name: &str,
            include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.secrets
                .lock()
                .unwrap()
                .get(name)
                .map(|v| props(name, include_value.then_some(v.as_str())))
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
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("versions".into()))
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<SecretSummary>, BackendError> {
            Ok(vec![])
        }

        async fn delete_secret(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            Err(BackendError::Unsupported("delete".into()))
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("update".into()))
        }
    }

    #[test]
    fn attachment_paths() {
        assert_eq!(attachment_prefix("db-cert"), "attachments/db-cert/");
        assert_eq!(
            attachment_blob_name("db-cert", "cert.pem"),
            "attachments/db-cert/cert.pem"
        );
    }

    #[tokio::test]
    async fn get_or_create_generates_once_and_reuses() {
        let stub = StubSecrets::new();
        let id1 = get_or_create_identity(&stub, "v").await.unwrap();
        let id2 = get_or_create_identity(&stub, "v").await.unwrap();
        assert_eq!(*stub.set_count.lock().unwrap(), 1, "second call must reuse");
        assert_eq!(id1.to_public().to_string(), id2.to_public().to_string());
        // Stored value is a valid age identity string.
        let stored = stub
            .secrets
            .lock()
            .unwrap()
            .get(ATTACHMENT_KEY_SECRET)
            .unwrap()
            .clone();
        assert!(stored.starts_with("AGE-SECRET-KEY-1"), "{stored}");
    }

    #[tokio::test]
    async fn get_identity_missing_key_is_actionable() {
        let stub = StubSecrets::new();
        match get_identity(&stub, "prod").await {
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("attachment key not found in vault 'prod'"),
                    "{msg}"
                );
            }
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn get_identity_garbage_value_is_an_error() {
        let stub = StubSecrets::new();
        stub.secrets
            .lock()
            .unwrap()
            .insert(ATTACHMENT_KEY_SECRET.to_string(), "not-a-key".to_string());
        assert!(get_identity(&stub, "v").await.is_err());
    }
}
