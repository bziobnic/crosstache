//! Secret file attachments — client-side age encryption over `FileBackend`.
//!
//! Attachments are age-encrypted with a per-vault x25519 identity stored as
//! the reserved secret [`ATTACHMENT_KEY_SECRET`] in the vault's own secret
//! store, so access to attachment plaintext is gated by vault (secret-store)
//! permissions, not storage-layer permissions. Ciphertext lives in ordinary
//! file storage under `attachments/<secret-name>/<filename>`; the association
//! is the naming convention. See
//! `docs/superpowers/specs/2026-07-21-secret-file-attachments-design.md`.

#[cfg(any(test, feature = "file-ops"))]
use age::secrecy::ExposeSecret;
#[cfg(any(test, feature = "file-ops"))]
use zeroize::Zeroizing;

use crate::backend::attachment_keys::AttachmentKeyStore;
use crate::backend::error::BackendError;
#[cfg(test)]
use crate::backend::secret::SecretBackend;
use crate::error::{CrosstacheError, Result};
#[cfg(feature = "file-ops")]
use crate::secret::attachment_key::{
    self, AttachmentKeyId, AttachmentKeyMaterial, AttachmentKeyRef, KeySlot, PointerKind,
    SecretVersion,
};
#[cfg(any(test, feature = "file-ops"))]
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
    secrets: &dyn AttachmentKeyStore,
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

#[cfg(feature = "file-ops")]
use crate::backend::file::FileBackend;
#[cfg(feature = "file-ops")]
use crate::backend::local::crypto;
#[cfg(feature = "file-ops")]
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
#[cfg(feature = "file-ops")]
use crate::utils::progress::ProgressReporter;

/// Bounded number of fresh-candidate attempts during V2 initialization before
/// giving up (persistent strict-name collisions with unmarked user secrets).
#[cfg(feature = "file-ops")]
const MAX_INIT_ATTEMPTS: usize = 8;

/// Resolve the key material to encrypt a new upload with, dispatching on the
/// vault's attachment-key mode (design §10.1):
///
/// - missing pointer   → initialize V2 (immutable retained record + pointer)
/// - raw V1 identity   → V1 legacy key, exact version bound from one response
/// - valid V2 pointer  → the active retained record, exact version + ID verify
/// - malformed value   → error; never replaced (invariant I7)
#[cfg(feature = "file-ops")]
async fn resolve_upload_material(
    secrets: &dyn AttachmentKeyStore,
    vault: &str,
) -> Result<AttachmentKeyMaterial> {
    match secrets.get_secret(vault, ATTACHMENT_KEY_SECRET, true).await {
        Ok(props) => {
            let version = props.version.clone();
            let value = props.value.ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "secret '{ATTACHMENT_KEY_SECRET}' in vault '{vault}' has no value"
                ))
            })?;
            match attachment_key::parse_pointer_value(&value) {
                Some(PointerKind::V1RawIdentity) => {
                    material_from_identity_value(KeySlot::Legacy, value, version, vault)
                }
                Some(PointerKind::V2 { active, .. }) => {
                    resolve_active_retained(secrets, vault, &active).await
                }
                None => Err(CrosstacheError::invalid_argument(format!(
                    "attachment key pointer in vault '{vault}' is malformed; refusing to replace it"
                ))),
            }
        }
        Err(BackendError::NotFound { .. }) => {
            let mut generate = || {
                Zeroizing::new(
                    age::x25519::Identity::generate()
                        .to_string()
                        .expose_secret()
                        .to_string(),
                )
            };
            initialize_v2(secrets, vault, &mut generate).await
        }
        Err(e) => Err(e.into()),
    }
}

/// Build key material from a stored identity value and its exact provider
/// version, tagging it with `slot`.
#[cfg(feature = "file-ops")]
fn material_from_identity_value(
    slot: KeySlot,
    value: Zeroizing<String>,
    version: String,
    vault: &str,
) -> Result<AttachmentKeyMaterial> {
    AttachmentKeyMaterial::from_identity(slot, SecretVersion::new(version), value).ok_or_else(
        || {
            CrosstacheError::invalid_argument(format!(
                "attachment key record in vault '{vault}' does not hold a valid age identity"
            ))
        },
    )
}

/// Read the current active retained record for `active_id` (value + version in
/// one response) and verify its derived key ID (design §10.4, invariant I6).
#[cfg(feature = "file-ops")]
async fn resolve_active_retained(
    secrets: &dyn AttachmentKeyStore,
    vault: &str,
    active_id: &AttachmentKeyId,
) -> Result<AttachmentKeyMaterial> {
    let name = attachment_key::retained_record_name(active_id);
    let props = secrets
        .get_secret(vault, &name, true)
        .await
        .map_err(|e| match e {
            BackendError::NotFound { .. } => CrosstacheError::invalid_argument(format!(
                "active attachment key '{}' is missing in vault '{vault}'",
                active_id.as_str()
            )),
            other => other.into(),
        })?;
    let version = props.version.clone();
    let value = props.value.ok_or_else(|| {
        CrosstacheError::invalid_argument(format!(
            "active attachment key '{}' in vault '{vault}' has no value",
            active_id.as_str()
        ))
    })?;
    let material = material_from_identity_value(KeySlot::Retained, value, version, vault)?;
    if !material.verify_id(active_id) {
        return Err(CrosstacheError::invalid_argument(format!(
            "attachment key mismatch for '{}' in vault '{vault}': the stored record does not \
             derive the active key ID",
            active_id.as_str()
        )));
    }
    Ok(material)
}

/// Publish (or re-publish) the V2 active pointer to `active_id`, then read it
/// back and require a confirmed V2 pointer response (design §10.2 steps 9–10).
#[cfg(feature = "file-ops")]
async fn publish_v2_pointer(
    secrets: &dyn AttachmentKeyStore,
    vault: &str,
    active_id: &AttachmentKeyId,
) -> Result<()> {
    let request = SecretRequest {
        name: ATTACHMENT_KEY_SECRET.to_string(),
        value: Zeroizing::new(attachment_key::format_v2_pointer(active_id, None)),
        content_type: Some("text/x-xv-attachment-key-pointer".to_string()),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: Some("crosstache attachment key ring active pointer".to_string()),
        folder: None,
    };
    secrets.set_secret(vault, request).await?;
    let props = secrets
        .get_secret(vault, ATTACHMENT_KEY_SECRET, true)
        .await?;
    let value = props.value.unwrap_or_default();
    match attachment_key::parse_pointer_value(&value) {
        Some(PointerKind::V2 { .. }) => Ok(()),
        _ => Err(CrosstacheError::invalid_argument(format!(
            "attachment key pointer publish in vault '{vault}' was not confirmed"
        ))),
    }
}

/// Initialize a V2 key ring on an empty vault (design §10.2): generate a
/// candidate, commit a marked immutable retained record on its strict
/// key-ID-derived name, re-read the exact version and verify the derived ID,
/// then publish the active pointer. An unmarked user secret occupying the
/// strict name causes the candidate to be discarded and a fresh one generated —
/// the user secret is never modified.
#[cfg(feature = "file-ops")]
async fn initialize_v2(
    secrets: &dyn AttachmentKeyStore,
    vault: &str,
    generate: &mut dyn FnMut() -> Zeroizing<String>,
) -> Result<AttachmentKeyMaterial> {
    for _ in 0..MAX_INIT_ATTEMPTS {
        let candidate = generate();
        let parsed = candidate
            .trim()
            .parse::<age::x25519::Identity>()
            .map_err(|e| {
                CrosstacheError::invalid_argument(format!(
                    "generated attachment key is invalid: {e}"
                ))
            })?;
        let key_id = AttachmentKeyId::derive(&parsed.to_public().to_string());
        let retained_name = attachment_key::retained_record_name(&key_id);

        // Inspect the exact record name WITHOUT requesting its value.
        match secrets.get_secret(vault, &retained_name, false).await {
            Ok(props) => {
                if attachment_key::is_marked_key_record(&props.content_type) {
                    // A marked record already exists (a concurrent initializer,
                    // or a crash after commit but before pointer). Adopt it
                    // idempotently: verify, then ensure the pointer is set.
                    let material = resolve_active_retained(secrets, vault, &key_id).await?;
                    publish_v2_pointer(secrets, vault, &key_id).await?;
                    return Ok(material);
                }
                // Unmarked user secret collision — discard candidate, retry.
                continue;
            }
            Err(BackendError::NotFound { .. }) => {
                // Commit the marked immutable retained record.
                let request = SecretRequest {
                    name: retained_name.clone(),
                    value: candidate.clone(),
                    content_type: Some(attachment_key::KEY_RECORD_CONTENT_TYPE.to_string()),
                    enabled: Some(true),
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: Some("crosstache attachment key custody record".to_string()),
                    folder: None,
                };
                let committed = match secrets.commit_retained_key(vault, request).await {
                    Ok(committed) => committed,
                    // Another writer won after the metadata probe. Reclassify
                    // on the next attempt; never turn a create conflict into an upsert.
                    Err(BackendError::Conflict(_)) => continue,
                    Err(error) => return Err(error.into()),
                };
                if committed.version.is_empty() {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "attachment key commit in vault '{vault}' returned no version"
                    )));
                }
                // Re-read the exact committed version and verify the derived ID.
                let reread = secrets
                    .get_secret_version(vault, &retained_name, &committed.version, true)
                    .await?;
                if reread.version != committed.version {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "attachment key commit in vault '{vault}' returned a different version on verification"
                    )));
                }
                let value = reread.value.ok_or_else(|| {
                    CrosstacheError::invalid_argument(format!(
                        "attachment key record in vault '{vault}' has no value after commit"
                    ))
                })?;
                let material = material_from_identity_value(
                    KeySlot::Retained,
                    value,
                    committed.version.clone(),
                    vault,
                )?;
                if !material.verify_id(&key_id) {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "attachment key commit in vault '{vault}' did not verify"
                    )));
                }
                publish_v2_pointer(secrets, vault, &key_id).await?;
                return Ok(material);
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(CrosstacheError::invalid_argument(format!(
        "could not initialize the attachment key ring in vault '{vault}' after \
         {MAX_INIT_ATTEMPTS} attempts (persistent strict-name collisions)"
    )))
}

/// Resolve the key material a schema-1 blob references: read the exact provider
/// version of the record named by the slot/key ID, then verify the fetched
/// identity derives the expected key ID (design §11 steps 8–10, invariant I6).
/// Never falls back to a current key or scans keys.
#[cfg(feature = "file-ops")]
async fn resolve_referenced_material(
    secrets: &dyn AttachmentKeyStore,
    vault: &str,
    key_ref: &AttachmentKeyRef,
) -> Result<AttachmentKeyMaterial> {
    let record_name = match key_ref.slot {
        KeySlot::Legacy => ATTACHMENT_KEY_SECRET.to_string(),
        KeySlot::Retained => attachment_key::retained_record_name(&key_ref.key_id),
    };
    let props = secrets
        .get_secret_version(vault, &record_name, key_ref.provider_version.as_str(), true)
        .await
        .map_err(|e| match e {
            BackendError::NotFound { .. } => CrosstacheError::invalid_argument(format!(
                "attachment key generation for '{}' is missing in vault '{vault}'; \
                 the referenced key record/version no longer exists",
                key_ref.key_id.as_str()
            )),
            other => other.into(),
        })?;
    let value = props.value.ok_or_else(|| {
        CrosstacheError::invalid_argument(format!(
            "attachment key record for '{}' in vault '{vault}' has no value",
            key_ref.key_id.as_str()
        ))
    })?;
    let material =
        AttachmentKeyMaterial::from_identity(key_ref.slot, key_ref.provider_version.clone(), value)
            .ok_or_else(|| {
                CrosstacheError::invalid_argument(format!(
                    "attachment key record for '{}' in vault '{vault}' is not a valid identity",
                    key_ref.key_id.as_str()
                ))
            })?;
    if !material.verify_id(&key_ref.key_id) {
        return Err(CrosstacheError::invalid_argument(format!(
            "attachment key mismatch for '{}' in vault '{vault}': the stored record does not \
             derive the referenced key ID",
            key_ref.key_id.as_str()
        )));
    }
    Ok(material)
}

/// Age-encrypt `request.content` with the vault's attachment key and upload the
/// ciphertext, stamping the reserved schema-1 crypto metadata bound to the
/// exact key generation (design §10.5). Caller-supplied reserved metadata keys
/// are overwritten.
#[cfg(feature = "file-ops")]
pub async fn upload_encrypted(
    secrets: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    mut request: FileUploadRequest,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<FileInfo> {
    let material = resolve_upload_material(secrets, vault).await?;
    request.content = crypto::encrypt_bytes(&request.content, &[material.recipient().clone()])?;
    attachment_key::apply_crypto_metadata(&mut request.metadata, material.reference());
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
    secrets: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    name: &str,
    reporter: Option<&dyn ProgressReporter>,
) -> Result<Vec<u8>> {
    use attachment_key::DownloadPlan;

    let snapshot = files.download_file_snapshot(vault, name, reporter).await?;
    let data = snapshot.content;
    let is_age = crypto::is_age_encrypted(&data);

    match attachment_key::classify_download(name, &snapshot.metadata, is_age) {
        DownloadPlan::Passthrough => Ok(data),
        DownloadPlan::FailClosedNonCiphertext => Err(CrosstacheError::invalid_argument(format!(
            "attachment '{name}' in vault '{vault}' is a managed attachment but its bytes are not \
             age ciphertext; refusing to return it as plaintext"
        ))),
        DownloadPlan::LegacyNoSchema => {
            // Pre-schema V1 attachment: decrypt with the current V1 key.
            let identity = get_identity(secrets, vault).await?;
            let plaintext = crypto::decrypt_bytes(&data, &identity).map_err(|e| {
                CrosstacheError::invalid_argument(format!(
                    "failed to decrypt '{name}' in vault '{vault}': wrong or rotated attachment key ({e})"
                ))
            })?;
            Ok(plaintext.to_vec())
        }
        DownloadPlan::Schema1 { key_ref } => {
            let material = resolve_referenced_material(secrets, vault, &key_ref).await?;
            let identity = material
                .expose_identity()
                .parse::<age::x25519::Identity>()
                .map_err(|e| {
                    CrosstacheError::invalid_argument(format!(
                        "attachment key for '{name}' in vault '{vault}' is unusable: {e}"
                    ))
                })?;
            let plaintext = crypto::decrypt_bytes(&data, &identity).map_err(|e| {
                CrosstacheError::invalid_argument(format!(
                    "failed to decrypt '{name}' in vault '{vault}': the pinned attachment key \
                     could not decrypt this blob ({e})"
                ))
            })?;
            Ok(plaintext.to_vec())
        }
        DownloadPlan::ReferenceInvalid => Err(CrosstacheError::invalid_argument(format!(
            "attachment '{name}' in vault '{vault}' declares the schema-1 key envelope but its \
             key reference is missing or malformed; refusing to fall back to another key"
        ))),
    }
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

    /// In-memory SecretBackend with per-name version history. Each write
    /// appends a new version whose opaque token is its 1-based index string,
    /// mirroring providers that return an addressable version per write.
    /// `set_count` asserts key reuse (no regeneration on second call).
    pub(super) struct StubSecrets {
        // name -> versions of (value, content_type), newest last.
        pub secrets: Mutex<HashMap<String, Vec<(String, String)>>>,
        pub set_count: Mutex<usize>,
        create_collision: Mutex<Option<(String, String)>>,
        read_version_override: Mutex<Option<String>>,
    }

    impl StubSecrets {
        pub fn new() -> Self {
            Self {
                secrets: Mutex::new(HashMap::new()),
                set_count: Mutex::new(0),
                create_collision: Mutex::new(None),
                read_version_override: Mutex::new(None),
            }
        }

        /// Append `value` as a new version of `name` with empty content type
        /// (test helper for seeding raw user/V1 secrets).
        pub fn put(&self, name: &str, value: &str) {
            self.secrets
                .lock()
                .unwrap()
                .entry(name.to_string())
                .or_default()
                .push((value.to_string(), String::new()));
        }

        /// Latest value of `name`, if any (test helper).
        pub fn latest(&self, name: &str) -> Option<String> {
            self.secrets
                .lock()
                .unwrap()
                .get(name)
                .and_then(|v| v.last().map(|(val, _)| val.clone()))
        }
    }

    fn props(
        name: &str,
        value: Option<&str>,
        version: &str,
        content_type: &str,
    ) -> SecretProperties {
        SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: value.map(|v| Zeroizing::new(v.to_string())),
            version: version.to_string(),
            version_number: version.parse().ok(),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: content_type.to_string(),
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
            let mut map = self.secrets.lock().unwrap();
            let versions = map.entry(request.name.clone()).or_default();
            let ct = request.content_type.clone().unwrap_or_default();
            versions.push((request.value.to_string(), ct.clone()));
            let version = versions.len().to_string();
            Ok(props(&request.name, None, &version, &ct))
        }

        async fn create_secret_if_absent(
            &self,
            _vault: &str,
            request: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            let mut count = self.set_count.lock().unwrap();
            let mut map = self.secrets.lock().unwrap();
            if let Some((name, value)) = self.create_collision.lock().unwrap().take() {
                map.insert(name, vec![(value, String::new())]);
            }
            if map.contains_key(&request.name) {
                return Err(BackendError::Conflict("exists".into()));
            }
            let ct = request.content_type.unwrap_or_default();
            map.insert(
                request.name.clone(),
                vec![(request.value.to_string(), ct.clone())],
            );
            *count += 1;
            Ok(props(&request.name, None, "1", &ct))
        }

        async fn get_secret(
            &self,
            _vault: &str,
            name: &str,
            include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            let map = self.secrets.lock().unwrap();
            match map.get(name) {
                Some(v) if !v.is_empty() => {
                    let version = v.len().to_string();
                    let (val, ct) = v.last().unwrap();
                    Ok(props(
                        name,
                        include_value.then_some(val.as_str()),
                        &version,
                        ct,
                    ))
                }
                _ => Err(BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }),
            }
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            name: &str,
            version: &str,
            include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            let map = self.secrets.lock().unwrap();
            let versions = map.get(name).ok_or_else(|| BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            })?;
            let idx: usize = version.parse().map_err(|_| BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            })?;
            if idx == 0 || idx > versions.len() {
                return Err(BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                });
            }
            let (val, ct) = &versions[idx - 1];
            Ok(props(
                name,
                include_value.then_some(val.as_str()),
                self.read_version_override
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or(version),
                ct,
            ))
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
        reject_split_reads: std::sync::atomic::AtomicBool,
        pub files: Mutex<HashMap<String, (Vec<u8>, HashMap<String, String>)>>,
    }

    #[cfg(feature = "file-ops")]
    impl StubFiles {
        pub fn new() -> Self {
            Self {
                reject_split_reads: std::sync::atomic::AtomicBool::new(false),
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
            if self
                .reject_split_reads
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(BackendError::Conflict(
                    "separate body read is no longer this generation".into(),
                ));
            }
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

        async fn download_file_snapshot(
            &self,
            _vault: &str,
            name: &str,
            _reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<crate::backend::file::FileDownloadSnapshot, BackendError> {
            let files = self.files.lock().unwrap();
            let (content, metadata) = files.get(name).ok_or_else(|| BackendError::NotFound {
                name: name.into(),
                suggestion: None,
            })?;
            Ok(crate::backend::file::FileDownloadSnapshot {
                content: content.clone(),
                metadata: metadata.clone(),
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

    /// Encrypt `content` with a specific committed key material and upload it
    /// with the matching schema-1 metadata — mirrors what one initializer does
    /// after committing its own generation (used by the concurrency proof).
    #[cfg(feature = "file-ops")]
    async fn upload_with_material(
        files: &StubFiles,
        vault: &str,
        name: &str,
        content: &[u8],
        material: &AttachmentKeyMaterial,
    ) {
        let mut request = upload_req(name, content);
        request.content =
            crate::backend::local::crypto::encrypt_bytes(content, &[material.recipient().clone()])
                .unwrap();
        attachment_key::apply_crypto_metadata(&mut request.metadata, material.reference());
        files.upload_file(vault, request, None).await.unwrap();
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn decrypt_uses_one_generation_instead_of_independent_body_and_metadata_reads() {
        let secrets = StubSecrets::new();
        let keys = crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets);
        let files = StubFiles::new();
        upload_encrypted(
            &keys,
            &files,
            "v",
            upload_req("attachments/db/cert", b"certificate"),
            None,
        )
        .await
        .unwrap();
        files
            .reject_split_reads
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let bytes = download_decrypted(&keys, &files, "v", "attachments/db/cert", None)
            .await
            .unwrap();
        assert_eq!(bytes, b"certificate");
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn encrypted_round_trip() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        let plaintext = b"-----BEGIN CERT-----\x00\xffbinary ok";

        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
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

        let roundtrip = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/db/cert.pem",
            None,
        )
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
        let content = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "plain.txt",
            None,
        )
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
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();
        // Simulate custody loss: the pinned key record no longer resolves.
        secrets.secrets.lock().unwrap().clear();
        let err = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/f",
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("is missing in vault 'v'"), "{err}");
    }

    /// The race fix (design §4.1 / invariant I1): a schema-1 blob pins the exact
    /// key version that encrypted it, so replacing the current attachment key
    /// (a new version) does NOT orphan the existing blob — it still decrypts.
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_pins_exact_version_across_key_replacement() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", b"payload"),
            None,
        )
        .await
        .unwrap();
        // Publish a brand-new (valid) identity as a later version of the key.
        let other = age::x25519::Identity::generate();
        secrets.put(ATTACHMENT_KEY_SECRET, other.to_string().expose_secret());
        // The blob still decrypts because its metadata pins the prior version.
        let out = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/f",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.as_slice(), b"payload");
    }

    /// A managed-namespace object whose bytes are not age ciphertext must fail
    /// closed, never be returned as plaintext (design §11 step 2, criterion 9).
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_managed_plaintext_fails_closed() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        // A plaintext file placed directly under the managed namespace.
        files
            .upload_file(
                "v",
                upload_req("attachments/s/leak.txt", b"cleartext"),
                None,
            )
            .await
            .unwrap();
        let err = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/leak.txt",
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("refusing to return it as plaintext"),
            "{err}"
        );
    }

    /// A schema-1 blob whose metadata key ID does not match the identity stored
    /// at the pinned version fails closed (invariant I6: verify, don't trust).
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_schema1_key_id_mismatch_fails_closed() {
        use crate::secret::attachment_key::{AttachmentKeyId, META_KEY_ID};
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        // Seed a V1 vault: the fixed `xv-attachment-key` record holds a raw
        // identity, so the legacy slot's lookup name is stable and a forged
        // key ID reaches the derive-and-verify check (not a missing record).
        let v1 = age::x25519::Identity::generate();
        secrets.put(ATTACHMENT_KEY_SECRET, v1.to_string().expose_secret());
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();
        // Rewrite the blob's key-ID metadata to a different (valid) key ID.
        {
            let mut store = files.files.lock().unwrap();
            let (_, meta) = store.get_mut("attachments/s/f").unwrap();
            let forged = AttachmentKeyId::derive(
                "age1forgedrecipientxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            );
            meta.insert(META_KEY_ID.to_string(), forged.as_str().to_string());
        }
        let err = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/f",
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("mismatch"), "{err}");
    }

    /// Error paths never leak private key material (design §22.1, invariant I9):
    /// no raw age identity or `AGE-SECRET-KEY` string appears in any error.
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn attachment_errors_never_leak_private_material() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        // V2 init produces a real private identity behind the pointer.
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();
        let private_identity = {
            // Recover the retained record's raw identity to search for it.
            let map = secrets.secrets.lock().unwrap();
            map.iter()
                .find(|(name, _)| name.starts_with("xv-attachment-key-ak1-"))
                .map(|(_, versions)| versions[0].0.clone())
                .unwrap()
        };
        assert!(private_identity.starts_with("AGE-SECRET-KEY-1"));

        // Drive the mismatch error path (forged key ID on a V1 blob).
        let secrets2 = StubSecrets::new();
        let files2 = StubFiles::new();
        let v1 = age::x25519::Identity::generate();
        secrets2.put(ATTACHMENT_KEY_SECRET, v1.to_string().expose_secret());
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets2),
            &files2,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();
        {
            use crate::secret::attachment_key::{AttachmentKeyId, META_KEY_ID};
            let mut store = files2.files.lock().unwrap();
            let (_, meta) = store.get_mut("attachments/s/f").unwrap();
            let forged =
                AttachmentKeyId::derive("age1zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
            meta.insert(META_KEY_ID.to_string(), forged.as_str().to_string());
        }
        let mismatch = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets2),
            &files2,
            "v",
            "attachments/s/f",
            None,
        )
        .await
        .unwrap_err()
        .to_string();

        // Missing-generation error path.
        secrets.secrets.lock().unwrap().clear();
        let missing = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/f",
            None,
        )
        .await
        .unwrap_err()
        .to_string();

        for err in [&mismatch, &missing] {
            assert!(!err.contains("AGE-SECRET-KEY"), "leaked key marker: {err}");
            assert!(
                !err.contains(&private_identity),
                "leaked the raw identity: {err}"
            );
            let v1_raw = v1.to_string().expose_secret().to_string();
            assert!(!err.contains(&v1_raw), "leaked the V1 identity: {err}");
        }
    }

    /// V1 upload stamps the reserved schema-1 legacy metadata bound to the exact
    /// key version (design §10.5).
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn v1_upload_stamps_schema1_legacy_metadata() {
        use crate::secret::attachment_key::{
            AttachmentKeyId, CRYPTO_SCHEMA_V1, META_CRYPTO_SCHEMA, META_KEY_ID, META_KEY_SLOT,
            META_KEY_VERSION,
        };
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        // Seed an existing V1 vault (raw identity in the fixed record).
        let v1 = age::x25519::Identity::generate();
        secrets.put(ATTACHMENT_KEY_SECRET, v1.to_string().expose_secret());
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();

        let stored_key = secrets.latest(ATTACHMENT_KEY_SECRET).unwrap();
        let recipient = stored_key
            .trim()
            .parse::<age::x25519::Identity>()
            .unwrap()
            .to_public()
            .to_string();
        let expected_id = AttachmentKeyId::derive(&recipient);

        let store = files.files.lock().unwrap();
        let (_, meta) = store.get("attachments/s/f").unwrap();
        assert_eq!(meta.get(META_CRYPTO_SCHEMA).unwrap(), CRYPTO_SCHEMA_V1);
        assert_eq!(meta.get(META_KEY_SLOT).unwrap(), "legacy");
        assert_eq!(meta.get(META_KEY_ID).unwrap(), expected_id.as_str());
        assert!(!meta.get(META_KEY_VERSION).unwrap().is_empty());
        // Sanity: the generated identity string is a real age key.
        assert!(stored_key.starts_with("AGE-SECRET-KEY-1"), "{stored_key}");
    }

    /// An empty vault initializes directly into V2 (design §14.2): a marked
    /// immutable retained record on the strict key-ID name, plus a non-secret
    /// V2 active pointer. The uploaded blob is bound to the retained slot.
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn v2_init_on_empty_vault_creates_marked_record_and_pointer() {
        use crate::secret::attachment_key::{
            is_marked_key_record, parse_pointer_value, retained_record_name, AttachmentKeyId,
            PointerKind, META_KEY_ID, META_KEY_SLOT,
        };
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", b"x"),
            None,
        )
        .await
        .unwrap();

        // The active pointer is a V2 pointer, not a raw identity.
        let pointer = secrets.latest(ATTACHMENT_KEY_SECRET).unwrap();
        let active = match parse_pointer_value(&pointer) {
            Some(PointerKind::V2 { active, legacy }) => {
                assert!(legacy.is_none(), "direct V2 init has no legacy fallback");
                active
            }
            other => panic!("expected V2 pointer, got {other:?}"),
        };

        // The retained record exists, is marked, and derives the active ID.
        let retained = retained_record_name(&active);
        let props = secrets.get_secret("v", &retained, true).await.unwrap();
        assert!(
            is_marked_key_record(&props.content_type),
            "record must be marked"
        );
        let stored_id = AttachmentKeyId::derive(
            &props
                .value
                .unwrap()
                .trim()
                .parse::<age::x25519::Identity>()
                .unwrap()
                .to_public()
                .to_string(),
        );
        assert_eq!(stored_id, active);

        // The blob is bound to the retained slot and the active key ID.
        let store = files.files.lock().unwrap();
        let (_, meta) = store.get("attachments/s/f").unwrap();
        assert_eq!(meta.get(META_KEY_SLOT).unwrap(), "retained");
        assert_eq!(meta.get(META_KEY_ID).unwrap(), active.as_str());
    }

    /// V2 round trip: encrypt then decrypt through the retained record.
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn v2_round_trip_uses_retained_slot() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();
        let plaintext = b"top secret bytes";
        upload_encrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            upload_req("attachments/s/f", plaintext),
            None,
        )
        .await
        .unwrap();
        let out = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/f",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.as_slice(), plaintext);
    }

    /// Concurrent-initializer proof (design §10.3, acceptance criterion 1): two
    /// initializers commit two different generations; each blob pins its own
    /// retained record + version, so both decrypt regardless of which pointer
    /// won. Driven deterministically via `initialize_v2` with fixed generators.
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn two_initializers_produce_two_decryptable_blobs() {
        let secrets = StubSecrets::new();
        let files = StubFiles::new();

        let id_a = age::x25519::Identity::generate();
        let id_b = age::x25519::Identity::generate();
        let raw_a = id_a.to_string().expose_secret().to_string();
        let raw_b = id_b.to_string().expose_secret().to_string();

        // A commits its generation and publishes the pointer.
        let mut gen_a = || Zeroizing::new(raw_a.clone());
        let mat_a = initialize_v2(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            "v",
            &mut gen_a,
        )
        .await
        .unwrap();
        upload_with_material(&files, "v", "attachments/s/a", b"aaa", &mat_a).await;

        // B commits a different generation and re-publishes the pointer last.
        let mut gen_b = || Zeroizing::new(raw_b.clone());
        let mat_b = initialize_v2(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            "v",
            &mut gen_b,
        )
        .await
        .unwrap();
        upload_with_material(&files, "v", "attachments/s/b", b"bbb", &mat_b).await;

        assert_ne!(
            mat_a.reference().key_id,
            mat_b.reference().key_id,
            "the two initializers must commit distinct generations"
        );

        // Both blobs decrypt through their own pinned key, though the pointer
        // now names B.
        let out_a = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/a",
            None,
        )
        .await
        .unwrap();
        let out_b = download_decrypted(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            &files,
            "v",
            "attachments/s/b",
            None,
        )
        .await
        .unwrap();
        assert_eq!(out_a.as_slice(), b"aaa");
        assert_eq!(out_b.as_slice(), b"bbb");
    }

    /// A generated candidate whose strict key-ID name is already occupied by an
    /// unmarked user secret is discarded; the user secret is never modified
    /// (design §10.2 step 4). A fixed generator forces the persistent collision.
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn v2_init_discards_candidate_on_unmarked_collision() {
        use crate::secret::attachment_key::{retained_record_name, AttachmentKeyId};
        let secrets = StubSecrets::new();

        let candidate = age::x25519::Identity::generate();
        let raw = candidate.to_string().expose_secret().to_string();
        let key_id = AttachmentKeyId::derive(&candidate.to_public().to_string());
        let collision_name = retained_record_name(&key_id);
        // Pre-seed an UNMARKED user secret occupying that exact name.
        secrets.put(&collision_name, "user's own secret value");

        // A generator that always yields the colliding candidate can never make
        // progress, so init must give up rather than touch the user secret.
        let mut gen = || Zeroizing::new(raw.clone());
        let err = match initialize_v2(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            "v",
            &mut gen,
        )
        .await
        {
            Ok(_) => panic!("init must not succeed over an unmarked collision"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("could not initialize"), "{err}");

        // The user secret is untouched: one unmarked version, original value.
        let versions = secrets.secrets.lock().unwrap();
        let seeded = versions.get(&collision_name).unwrap();
        assert_eq!(seeded.len(), 1, "user secret must not gain versions");
        assert_eq!(seeded[0].0, "user's own secret value");
        assert!(seeded[0].1.is_empty(), "user secret must stay unmarked");
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
            upload_encrypted(
                &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
                &files,
                "v",
                upload_req(name, b"x"),
                None,
            )
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
    async fn get_identity_missing_key_is_actionable() {
        let stub = StubSecrets::new();
        match get_identity(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&stub),
            "prod",
        )
        .await
        {
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
        stub.put(ATTACHMENT_KEY_SECRET, "not-a-key");
        assert!(get_identity(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&stub),
            "v"
        )
        .await
        .is_err());
    }
    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn retained_commit_collision_after_probe_preserves_user_record_and_retries() {
        use age::secrecy::ExposeSecret;
        let secrets = StubSecrets::new();
        let first = age::x25519::Identity::generate();
        let second = age::x25519::Identity::generate();
        let first_name = attachment_key::retained_record_name(&AttachmentKeyId::derive(
            &first.to_public().to_string(),
        ));
        *secrets.create_collision.lock().unwrap() = Some((first_name.clone(), "user-value".into()));
        let mut candidates = [first, second].into_iter();
        let material = initialize_v2(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            "v",
            &mut || {
                Zeroizing::new(
                    candidates
                        .next()
                        .unwrap()
                        .to_string()
                        .expose_secret()
                        .to_string(),
                )
            },
        )
        .await
        .unwrap();
        assert_eq!(secrets.latest(&first_name).as_deref(), Some("user-value"));
        assert_ne!(
            attachment_key::retained_record_name(&material.reference().key_id),
            first_name
        );
        assert!(secrets.latest(ATTACHMENT_KEY_SECRET).is_some());
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn retained_commit_rejects_wrong_version_before_publishing_pointer() {
        use age::secrecy::ExposeSecret;
        let secrets = StubSecrets::new();
        *secrets.read_version_override.lock().unwrap() = Some("wrong-version".into());
        let result = initialize_v2(
            &crate::backend::attachment_keys::RawAttachmentKeyStore::new(&secrets),
            "v",
            &mut || {
                Zeroizing::new(
                    age::x25519::Identity::generate()
                        .to_string()
                        .expose_secret()
                        .to_string(),
                )
            },
        )
        .await;
        assert!(
            result.is_err(),
            "a mismatched provider version must fail verification"
        );
        assert!(secrets.latest(ATTACHMENT_KEY_SECRET).is_none());
    }
}
