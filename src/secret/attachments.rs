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
pub const ATTACHMENT_KEY_SECRET: &str = "xv-attachment-key";
/// File-metadata key marking client-side-encrypted content. Underscore, not
/// hyphen: Azure Blob metadata keys must be valid C# identifiers, and a
/// hyphenated key fails the whole upload with 400 InvalidMetadata.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub const ENC_METADATA_KEY: &str = "xv_encrypted";
/// File-metadata value for age encryption.
#[allow(dead_code)] // Consumed by attachment CLI/encryption tasks (Tasks 2-4)
pub const ENC_METADATA_VALUE: &str = "age";

/// True if `name` is safe to use as a single path component in the
/// `attachments/<name>/...` blob namespace: non-empty, no `/` or `\`, and
/// not `.`/`..`. Shared by attachment-file-name validation and the
/// resolved-secret-name guard — a secret name containing `/` would
/// otherwise let its `attachments/<name>/` prefix overlap a different
/// secret's attachment blobs (cross-secret cascade on delete).
pub fn is_valid_path_component(name: &str) -> bool {
    !(name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == "..")
}

/// Blob-name prefix for a secret's attachments.
pub fn attachment_prefix(secret_name: &str) -> String {
    format!("attachments/{secret_name}/")
}

/// Full blob name for one attachment of a secret.
pub fn attachment_blob_name(secret_name: &str, attachment: &str) -> String {
    format!("{}{attachment}", attachment_prefix(secret_name))
}

/// True if a blob is a client-side-encrypted attachment: reserved-namespace
/// name convention (anything under `attachments/`) OR explicit
/// `xv_encrypted: age` metadata flag. `metadata` may be empty (e.g. for a
/// local file that hasn't been uploaded yet) — the name check alone still
/// catches the reserved namespace.
///
/// `xv file sync` speaks plaintext only: it would decrypt ciphertext to disk
/// on download, or clobber ciphertext with an unflagged plaintext re-upload.
/// Callers use this to keep sync out of the attachment namespace entirely.
// ponytail: sync just skips these rather than transferring ciphertext or
// decrypting; teach it real encrypted-blob sync (or a `--decrypt` opt-in)
// if someone needs `xv file sync` to round-trip attachments/.
pub fn is_encrypted_attachment(
    name: &str,
    metadata: &std::collections::HashMap<String, String>,
) -> bool {
    name.starts_with("attachments/")
        || metadata.get(ENC_METADATA_KEY).map(String::as_str) == Some(ENC_METADATA_VALUE)
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

#[cfg(feature = "file-ops")]
use crate::backend::file::FileBackend;
#[cfg(feature = "file-ops")]
use crate::backend::local::crypto;
#[cfg(feature = "file-ops")]
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
#[cfg(feature = "file-ops")]
use crate::utils::progress::ProgressReporter;

/// Age-encrypt `request.content` with the vault's attachment key (created on
/// first use) and upload the ciphertext, flagged `xv_encrypted: age`.
#[cfg(feature = "file-ops")]
pub async fn upload_encrypted(
    secrets: &dyn SecretBackend,
    files: &dyn FileBackend,
    vault: &str,
    mut request: FileUploadRequest,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<FileInfo> {
    let identity = get_or_create_identity(secrets, vault).await?;
    let recipient = identity.to_public();
    request.content = crypto::encrypt_bytes(&request.content, &[recipient])?;
    request
        .metadata
        .insert(ENC_METADATA_KEY.to_string(), ENC_METADATA_VALUE.to_string());
    files
        .upload_file(vault, request, reporter)
        .await
        .map_err(CrosstacheError::from)
}

/// Download a file, transparently decrypting it when it carries the
/// `xv_encrypted: age` metadata flag. Unflagged files (including user-supplied
/// `.age` files encrypted with foreign keys) pass through untouched.
#[cfg(feature = "file-ops")]
pub async fn download_decrypted(
    secrets: &dyn SecretBackend,
    files: &dyn FileBackend,
    vault: &str,
    name: &str,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<Vec<u8>> {
    let data = files
        .download_file(vault, name, reporter)
        .await
        .map_err(CrosstacheError::from)?;
    // Cheap local sniff first: only consult metadata for age-shaped content.
    if !crypto::is_age_encrypted(&data) {
        return Ok(data);
    }
    let info = files
        .get_file_info(vault, name)
        .await
        .map_err(CrosstacheError::from)?;
    if info.metadata.get(ENC_METADATA_KEY).map(String::as_str) != Some(ENC_METADATA_VALUE) {
        return Ok(data); // foreign age file — not ours to decrypt
    }
    let identity = get_identity(secrets, vault).await?;
    let plaintext = crypto::decrypt_bytes(&data, &identity).map_err(|e| {
        CrosstacheError::invalid_argument(format!(
            "failed to decrypt '{name}' in vault '{vault}': wrong or rotated attachment key ({e})"
        ))
    })?;
    Ok(plaintext.to_vec())
}

/// List all attachments of `secret_name` (full blob names).
#[cfg(feature = "file-ops")]
pub async fn list_attachments(
    files: &dyn FileBackend,
    vault: &str,
    secret_name: &str,
) -> Result<Vec<FileInfo>> {
    files
        .list_files(
            vault,
            FileListRequest {
                prefix: Some(attachment_prefix(secret_name)),
                groups: None,
                limit: None,
                delimiter: None,
            },
        )
        .await
        .map_err(CrosstacheError::from)
}

/// Delete every attachment of `secret_name`. Returns the number deleted.
#[cfg(feature = "file-ops")]
pub async fn delete_attachments(
    files: &dyn FileBackend,
    vault: &str,
    secret_name: &str,
) -> Result<usize> {
    let attachments = list_attachments(files, vault, secret_name).await?;
    for a in &attachments {
        files
            .delete_file(vault, &a.name)
            .await
            .map_err(CrosstacheError::from)?;
    }
    Ok(attachments.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[cfg(feature = "file-ops")]
    use crate::backend::file::FileBackend;
    #[cfg(feature = "file-ops")]
    use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
    use crate::secret::manager::{
        SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };
    #[cfg(feature = "file-ops")]
    use crate::utils::progress::ProgressReporter;

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

    /// In-memory FileBackend storing (content, metadata) per name.
    #[cfg(feature = "file-ops")]
    #[allow(clippy::type_complexity)]
    pub(super) struct StubFiles {
        pub files: Mutex<HashMap<String, (Vec<u8>, HashMap<String, String>)>>,
    }

    #[cfg(feature = "file-ops")]
    impl StubFiles {
        pub fn new() -> Self {
            Self {
                files: Mutex::new(HashMap::new()),
            }
        }
    }

    #[cfg(feature = "file-ops")]
    fn file_info(name: &str, size: u64, metadata: HashMap<String, String>) -> FileInfo {
        FileInfo {
            name: name.to_string(),
            size,
            content_type: "application/octet-stream".to_string(),
            last_modified: chrono::Utc::now(),
            etag: String::new(),
            groups: Vec::new(),
            metadata,
            tags: HashMap::new(),
        }
    }

    #[cfg(feature = "file-ops")]
    #[async_trait]
    impl FileBackend for StubFiles {
        async fn upload_file(
            &self,
            _vault: &str,
            request: FileUploadRequest,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<FileInfo, BackendError> {
            let info = file_info(
                &request.name,
                request.content.len() as u64,
                request.metadata.clone(),
            );
            self.files
                .lock()
                .unwrap()
                .insert(request.name, (request.content, request.metadata));
            Ok(info)
        }

        async fn download_file(
            &self,
            _vault: &str,
            name: &str,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<Vec<u8>, BackendError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .map(|(c, _)| c.clone())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn list_files(
            &self,
            _vault: &str,
            request: FileListRequest,
        ) -> std::result::Result<Vec<FileInfo>, BackendError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| {
                    request
                        .prefix
                        .as_ref()
                        .is_none_or(|p| name.starts_with(p.as_str()))
                })
                .map(|(name, (c, m))| file_info(name, c.len() as u64, m.clone()))
                .collect())
        }

        async fn delete_file(
            &self,
            _vault: &str,
            name: &str,
        ) -> std::result::Result<(), BackendError> {
            self.files
                .lock()
                .unwrap()
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }

        async fn get_file_info(
            &self,
            _vault: &str,
            name: &str,
        ) -> std::result::Result<FileInfo, BackendError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .map(|(c, m)| file_info(name, c.len() as u64, m.clone()))
                .ok_or_else(|| BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                })
        }
    }

    #[cfg(feature = "file-ops")]
    fn upload_req(name: &str, content: &[u8]) -> FileUploadRequest {
        FileUploadRequest {
            name: name.to_string(),
            content: content.to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        }
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn encrypted_round_trip() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        let plaintext = b"-----BEGIN CERT-----\x00\xffbinary ok";

        upload_encrypted(
            &secrets,
            &files,
            "v",
            upload_req("attachments/db/cert.pem", plaintext),
            None,
        )
        .await
        .unwrap();

        // Stored blob is ciphertext, flagged, and not the plaintext.
        {
            let store = files.files.lock().unwrap();
            let (stored, meta) = store.get("attachments/db/cert.pem").unwrap();
            assert!(crate::backend::local::crypto::is_age_encrypted(stored));
            assert_ne!(stored.as_slice(), plaintext);
            assert_eq!(
                meta.get(ENC_METADATA_KEY).map(String::as_str),
                Some(ENC_METADATA_VALUE)
            );
        }

        let roundtrip = download_decrypted(&secrets, &files, "v", "attachments/db/cert.pem", None)
            .await
            .unwrap();
        assert_eq!(roundtrip.as_slice(), plaintext);
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_passes_through_unencrypted_files() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        files
            .upload_file("v", upload_req("plain.txt", b"hello"), None)
            .await
            .unwrap();
        let content = download_decrypted(&secrets, &files, "v", "plain.txt", None)
            .await
            .unwrap();
        assert_eq!(content, b"hello");
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_flagged_file_without_key_names_the_problem() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        upload_encrypted(
            &secrets,
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();
        // Simulate key deletion.
        secrets
            .secrets
            .lock()
            .unwrap()
            .remove(ATTACHMENT_KEY_SECRET);
        let err = download_decrypted(&secrets, &files, "v", "attachments/s/f", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("attachment key not found in vault 'v'"),
            "{err}"
        );
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_with_wrong_key_is_actionable() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        upload_encrypted(
            &secrets,
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();
        // Replace the key with a different (valid) identity.
        let other = age::x25519::Identity::generate();
        secrets.secrets.lock().unwrap().insert(
            ATTACHMENT_KEY_SECRET.to_string(),
            other.to_string().expose_secret().to_string(),
        );
        let err = download_decrypted(&secrets, &files, "v", "attachments/s/f", None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("wrong or rotated attachment key"),
            "{err}"
        );
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn list_and_delete_scope_to_one_secrets_prefix() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        for name in [
            "attachments/db/cert.pem",
            "attachments/db/key.pem",
            "attachments/other/f.txt",
        ] {
            upload_encrypted(&secrets, &files, "v", upload_req(name, b"x"), None)
                .await
                .unwrap();
        }
        files
            .upload_file("v", upload_req("normal.txt", b"y"), None)
            .await
            .unwrap();

        let listed = list_attachments(&files, "v", "db").await.unwrap();
        let mut names: Vec<_> = listed.iter().map(|f| f.name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["attachments/db/cert.pem", "attachments/db/key.pem"]
        );

        let deleted = delete_attachments(&files, "v", "db").await.unwrap();
        assert_eq!(deleted, 2);
        assert!(list_attachments(&files, "v", "db")
            .await
            .unwrap()
            .is_empty());
        // Other secret's attachment and normal files untouched.
        assert!(files
            .files
            .lock()
            .unwrap()
            .contains_key("attachments/other/f.txt"));
        assert!(files.files.lock().unwrap().contains_key("normal.txt"));
    }

    #[test]
    fn is_encrypted_attachment_matches_on_name_prefix_or_flag() {
        let empty = HashMap::new();
        assert!(is_encrypted_attachment("attachments/db/cert.pem", &empty));

        let mut flagged = HashMap::new();
        flagged.insert(ENC_METADATA_KEY.to_string(), ENC_METADATA_VALUE.to_string());
        assert!(is_encrypted_attachment("docs/readme.md", &flagged));

        assert!(!is_encrypted_attachment("docs/readme.md", &empty));

        let mut other = HashMap::new();
        other.insert(ENC_METADATA_KEY.to_string(), "not-age".to_string());
        assert!(!is_encrypted_attachment("docs/readme.md", &other));
    }

    #[test]
    fn is_valid_path_component_rejects_separators_and_dots() {
        assert!(!is_valid_path_component("a/b"));
        assert!(!is_valid_path_component("a\\b"));
        assert!(!is_valid_path_component(""));
        assert!(!is_valid_path_component("."));
        assert!(!is_valid_path_component(".."));
        assert!(is_valid_path_component("db-cert"));
        assert!(is_valid_path_component("normal_name.txt"));
    }

    #[test]
    fn enc_metadata_key_is_a_valid_azure_metadata_identifier() {
        // Azure Blob metadata keys travel as `x-ms-meta-<key>` headers and
        // must be valid C# identifiers; a hyphen makes every upload fail
        // with 400 InvalidMetadata.
        let mut chars = ENC_METADATA_KEY.chars();
        let first = chars.next().expect("key must be non-empty");
        assert!(first.is_ascii_alphabetic() || first == '_');
        assert!(
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "ENC_METADATA_KEY '{ENC_METADATA_KEY}' contains characters Azure rejects in metadata keys"
        );
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
