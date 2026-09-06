//! Read-only planning. A recovered plan is evidence, never deletion authority.
//! Execution must recompare live secret values/metadata and every source and
//! destination generation before cleanup; this schema intentionally grants no
//! authority through progress flags or plaintext-value hashes.
use crate::backend::{error::BackendError, local::crypto, Backend, SecretBackend};
use crate::error::{CrosstacheError, Result};
use crate::secret::{attachment_key as key, attachment_rewrap as rewrap};
use age::secrecy::ExposeSecret;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    io::Read,
    path::Path,
};
use zeroize::Zeroizing;

// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
pub const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MANIFEST_FILES: usize = 10_000;
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
const ENVELOPE_MAGIC: &[u8] = b"XV-TRANSFER-MANIFEST-1\0";
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
const MAC_BYTES: usize = 32;
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
const MAX_PLAINTEXT_BYTES: usize = 7 * 1024 * 1024;
fn invalid() -> CrosstacheError {
    CrosstacheError::invalid_argument(
        "Invalid, unauthenticated, or mismatched attachment transfer manifest",
    )
}
fn conflict() -> CrosstacheError {
    CrosstacheError::conflict("Attachment transfer source changed or destination already exists")
}
fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 1024
        && s != "."
        && s != ".."
        && s.trim() == s
        && !s.chars().any(|c| c.is_control() || c == '/' || c == '\\')
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferEndpoint {
    /// Caller-supplied logical backend identity. Physical alias checks are deferred
    /// until execution exists; callers must retain this identity on resume.
    pub identity: String,
    pub vault: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferOperation {
    Copy,
    Move,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferIntent {
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    pub source_name: String,
    pub destination_name: String,
    pub operation: TransferOperation,
    pub destination_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_folder: Option<String>,
}
impl TransferIntent {
    pub fn validate(&self) -> Result<()> {
        if let Some(folder) = &self.destination_folder {
            if folder != "/" {
                crate::utils::helpers::validate_folder_path(folder)?;
                if folder.split('/').any(|part| !component(part)) {
                    return Err(invalid());
                }
            }
        }
        for endpoint in [&self.source, &self.destination] {
            if endpoint.identity.trim().is_empty()
                || endpoint.identity.len() > 4096
                || endpoint.identity.chars().any(char::is_control)
                || !component(&endpoint.vault)
            {
                return Err(invalid());
            }
        }
        if !component(&self.source_name)
            || !component(&self.destination_name)
            || key::generic_mutation_blocked(&self.source_name)
            || key::generic_mutation_blocked(&self.destination_name)
            || (self.source == self.destination && self.source_name == self.destination_name)
            || (self.source == self.destination && self.destination_key_id.is_some())
            || self
                .destination_key_id
                .as_ref()
                .is_some_and(|id| key::AttachmentKeyId::parse(id).is_none())
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferKeyBinding {
    pub key_id: String,
    pub slot: String,
    pub provider_version: String,
}
impl From<&key::AttachmentKeyRef> for TransferKeyBinding {
    fn from(r: &key::AttachmentKeyRef) -> Self {
        Self {
            key_id: r.key_id.as_str().into(),
            slot: match r.slot {
                key::KeySlot::Legacy => "legacy",
                key::KeySlot::Retained => "retained",
            }
            .into(),
            provider_version: r.provider_version.as_str().into(),
        }
    }
}
impl TransferKeyBinding {
    pub(crate) fn validate(&self) -> Result<()> {
        if key::AttachmentKeyId::parse(&self.key_id).is_none()
            || !matches!(self.slot.as_str(), "legacy" | "retained")
            || self.provider_version.is_empty()
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferFile {
    pub source_name: String,
    pub destination_name: String,
    pub size: u64,
    pub content_type: String,
    pub last_modified: chrono::DateTime<chrono::Utc>,
    pub etag: String,
    pub groups: Vec<String>,
    pub metadata: HashMap<String, String>,
    pub tags: HashMap<String, String>,
    pub ciphertext_sha256: String,
    pub source_key: TransferKeyBinding,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferPlan {
    pub schema_version: u32,
    pub intent: TransferIntent,
    pub source_version: String,
    pub files: Vec<TransferFile>,
    pub destination_key: Option<TransferKeyBinding>,
    /// Always false in schema 1: no progress or cleanup authority is supported.
    pub execution_supported: bool,
}
#[derive(Debug, Serialize)]
pub struct TransferPreview<'a> {
    pub intent: &'a TransferIntent,
    pub attachment_count: usize,
    pub ciphertext_bytes: u64,
    pub source_keys: Vec<&'a TransferKeyBinding>,
    pub destination_key: &'a Option<TransferKeyBinding>,
    pub execution_supported: bool,
    pub limitation: &'static str,
}
impl TransferPlan {
    pub fn validate(&self) -> Result<()> {
        self.intent.validate()?;
        if self.schema_version != 1
            || self.execution_supported
            || self.source_version.is_empty()
            || self.files.len() > MAX_MANIFEST_FILES
        {
            return Err(invalid());
        }
        let prefix = format!("attachments/{}/", self.intent.source_name);
        let dest = format!("attachments/{}/", self.intent.destination_name);
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for f in &self.files {
            let suffix = f.source_name.strip_prefix(&prefix).ok_or_else(invalid)?;
            if suffix.split('/').any(|s| !component(s))
                || f.destination_name != format!("{dest}{suffix}")
                || !seen.insert(&f.source_name)
                || f.ciphertext_sha256.len() != 64
                || !f
                    .ciphertext_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid());
            }
            f.source_key.validate()?;
            total = total.checked_add(f.size).ok_or_else(invalid)?;
        }
        if let Some(k) = &self.destination_key {
            k.validate()?;
        }
        if (self.files.is_empty() || self.intent.source == self.intent.destination)
            && self.destination_key.is_some()
        {
            return Err(invalid());
        }
        if !self.files.is_empty() && self.intent.source != self.intent.destination {
            let k = self.destination_key.as_ref().ok_or_else(invalid)?;
            if self.intent.destination_key_id.as_deref() != Some(k.key_id.as_str())
                || k.slot != "retained"
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// Excludes attachment metadata, tags, and secret values.
    pub fn preview(&self) -> TransferPreview<'_> {
        TransferPreview { intent: &self.intent, attachment_count: self.files.len(), ciphertext_bytes: self.files.iter().fold(0u64, |n,f| n.saturating_add(f.size)), source_keys: self.files.iter().map(|f| &f.source_key).collect(), destination_key: &self.destination_key, execution_supported: false, limitation: "Read-only preview; execution, physical storage alias verification, and recovery cleanup are not enabled." }
    }
}
async fn absent(backend: &dyn Backend, endpoint: &TransferEndpoint, name: &str) -> Result<()> {
    match backend
        .guarded_secrets()
        .get_secret(&endpoint.vault, name, false)
        .await
    {
        Err(BackendError::NotFound { .. }) => {}
        Err(e) => return Err(e.into()),
        Ok(_) => return Err(conflict()),
    }
    if !backend
        .attachment_names(&endpoint.vault, name)
        .await?
        .is_empty()
    {
        return Err(conflict());
    }
    Ok(())
}
/// Transfer execution is deliberately one-file, in-memory, not streaming.
/// Ciphertext, plaintext and output may coexist, plus provider buffers.
pub const MAX_SOURCE_CIPHERTEXT_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_OUTPUT_CIPHERTEXT_BYTES: u64 = MAX_SOURCE_CIPHERTEXT_BYTES + 64 * 1024;
pub(crate) async fn snapshot(
    files: &dyn crate::backend::FileBackend,
    vault: &str,
    name: &str,
) -> Result<rewrap::Snapshot> {
    snapshot_with_limit(files, vault, name, MAX_SOURCE_CIPHERTEXT_BYTES).await
}
pub(crate) async fn destination_snapshot(
    files: &dyn crate::backend::FileBackend,
    vault: &str,
    name: &str,
) -> Result<rewrap::Snapshot> {
    snapshot_with_limit(files, vault, name, MAX_OUTPUT_CIPHERTEXT_BYTES).await
}
async fn snapshot_with_limit(
    files: &dyn crate::backend::FileBackend,
    vault: &str,
    name: &str,
    limit: u64,
) -> Result<rewrap::Snapshot> {
    let before = files.get_file_restore_info(vault, name).await?;
    if before.size > limit {
        return Err(BackendError::Unsupported(
            "Attachment exceeds the 256 MiB transfer memory limit".into(),
        )
        .into());
    }
    let current = rewrap::snapshot(files, vault, name).await?;
    if !rewrap::same_info(&before, &current.info) || current.info.size > limit {
        return Err(conflict());
    }
    Ok(current)
}
/// Authenticates every current attachment, takes full metadata and ciphertext
/// fingerprints, and rechecks inventory and source version. Performs only reads.
pub async fn plan(
    source: &dyn Backend,
    destination: &dyn Backend,
    intent: TransferIntent,
) -> Result<TransferPlan> {
    intent.validate()?;
    let caps = destination.capabilities();
    if !caps.name_charset.is_valid(&intent.destination_name)
        || caps
            .max_name_length
            .is_some_and(|max| intent.destination_name.len() > max)
    {
        return Err(invalid());
    }
    absent(destination, &intent.destination, &intent.destination_name).await?;
    let source_secret = source
        .guarded_secrets()
        .get_secret(&intent.source.vault, &intent.source_name, false)
        .await?;
    let names = source
        .attachment_names(&intent.source.vault, &intent.source_name)
        .await?;
    if names.len() > MAX_MANIFEST_FILES || source_secret.version.is_empty() {
        return Err(invalid());
    }
    let mut result = TransferPlan {
        schema_version: 1,
        intent,
        source_version: source_secret.version.clone(),
        files: vec![],
        destination_key: None,
        execution_supported: false,
    };
    let i = &result.intent;
    if !names.is_empty() {
        let files = source
            .files()
            .ok_or_else(|| BackendError::Unsupported("strict attachment snapshots".into()))?;
        let destination_files = destination
            .files()
            .ok_or_else(|| BackendError::Unsupported("destination attachment storage".into()))?;
        let keys = source.attachment_keys();
        let pointer = keys
            .get_secret(&i.source.vault, key::ACTIVE_POINTER_SECRET, true)
            .await?;
        let active = match pointer.value.as_deref().and_then(|v| key::parse_pointer_value(v)) {
            Some(key::PointerKind::V2 { active, .. }) => active,
            _ => return Err(BackendError::Unsupported("transfer preview requires a healthy V2 attachment key ring; upgrade the key ring first".into()).into()),
        };
        let ring = rewrap::Ring::load(keys.as_ref(), &i.source.vault, &active).await?;
        let dest_keys = destination.attachment_keys();
        let mut destination_ring = None;
        if i.source != i.destination {
            let expected = i
                .destination_key_id
                .as_deref()
                .and_then(key::AttachmentKeyId::parse)
                .ok_or_else(|| {
                    BackendError::InvalidArgument(
                        "cross-vault preview requires an explicit destination active key ID".into(),
                    )
                })?;
            let dest_ring = rewrap::Ring::load(dest_keys.as_ref(), &i.destination.vault, &expected)
                .await
                .map_err(|error| CrosstacheError::invalid_argument(format!(
                    "Destination needs an existing healthy attachment key ring matching --to-key-id: {error}. Use attachment-key initialize --apply --offline for an empty ring, or attachment-key upgrade/recover for an existing unhealthy ring."
                )))?;
            result.destination_key = Some((&dest_ring.target).into());
            destination_ring = Some(dest_ring);
        }
        let prefix = format!("attachments/{}/", i.source_name);
        let mut evidence_bytes = serde_json::to_vec(&result).map_err(|_| invalid())?.len();
        for name in &names {
            files.validate_file_name(name)?;
            let suffix = name.strip_prefix(&prefix).ok_or_else(invalid)?;
            destination_files
                .validate_file_name(&format!("attachments/{}/{suffix}", i.destination_name))?;
            if files
                .get_file_restore_info(&i.source.vault, name)
                .await?
                .size
                > MAX_SOURCE_CIPHERTEXT_BYTES
            {
                return Err(BackendError::Unsupported(
                    "Attachment exceeds the 256 MiB transfer memory limit".into(),
                )
                .into());
            }
            let snap = snapshot(files, &i.source.vault, name).await?;
            let reference = rewrap::source_ref(&ring, &snap)?;
            let _plaintext =
                rewrap::authenticate(keys.as_ref(), &i.source.vault, &reference, &snap).await?;
            let evidence = TransferFile {
                source_name: name.clone(),
                destination_name: format!("attachments/{}/{suffix}", i.destination_name),
                size: snap.info.size,
                content_type: snap.info.content_type,
                last_modified: snap.info.last_modified,
                etag: snap.info.etag,
                groups: snap.info.groups,
                metadata: snap.info.metadata,
                tags: snap.info.tags,
                ciphertext_sha256: hex::encode(Sha256::digest(&snap.data.content)),
                source_key: (&reference).into(),
            };
            evidence_bytes = evidence_bytes
                .checked_add(serde_json::to_vec(&evidence).map_err(|_| invalid())?.len() + 1)
                .ok_or_else(invalid)?;
            if evidence_bytes > MAX_PLAINTEXT_BYTES {
                return Err(BackendError::Unsupported(
                    "Transfer metadata exceeds the recovery journal budget".into(),
                )
                .into());
            }
            result.files.push(evidence);
        }
        for saved in &result.files {
            let current = snapshot(files, &i.source.vault, &saved.source_name).await?;
            if current.info.size != saved.size
                || current.info.content_type != saved.content_type
                || current.info.last_modified != saved.last_modified
                || current.info.etag != saved.etag
                || current.info.groups != saved.groups
                || current.info.metadata != saved.metadata
                || current.info.tags != saved.tags
                || hex::encode(Sha256::digest(&current.data.content)) != saved.ciphertext_sha256
            {
                return Err(conflict());
            }
        }
        ring.recheck(keys.as_ref(), &i.source.vault).await?;
        if let Some(ring) = destination_ring {
            ring.recheck(dest_keys.as_ref(), &i.destination.vault)
                .await?;
        }
    }
    let current_secret = source
        .guarded_secrets()
        .get_secret(&i.source.vault, &i.source_name, false)
        .await?;
    if current_secret.name != source_secret.name
        || current_secret.original_name != source_secret.original_name
        || current_secret.enabled != source_secret.enabled
        || current_secret.expires_on != source_secret.expires_on
        || current_secret.not_before != source_secret.not_before
        || current_secret.tags != source_secret.tags
        || current_secret.content_type != source_secret.content_type
        || current_secret.updated_on != source_secret.updated_on
    {
        return Err(conflict());
    }
    if names
        != source
            .attachment_names(&i.source.vault, &i.source_name)
            .await?
        || current_secret.version != result.source_version
    {
        return Err(conflict());
    }
    absent(destination, &i.destination, &i.destination_name).await?;
    result.validate()?;
    Ok(result)
}
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
fn envelope_mac(recovery: &age::x25519::Identity) -> Hmac<Sha256> {
    let private = recovery.to_string();
    let mut derivation = Sha256::new();
    derivation.update(b"crosstache/transfer-manifest/mac-key/v1\0");
    derivation.update(private.expose_secret().as_bytes());
    let key = Zeroizing::new(<[u8; 32]>::from(derivation.finalize()));
    let mut mac = Hmac::<Sha256>::new_from_slice(&*key).expect("SHA256 HMAC accepts 32-byte key");
    mac.update(ENVELOPE_MAGIC);
    mac
}
/// Only encrypted bytes leave this codec; the recovery identity is caller owned.
/// The private-derived MAC additionally prevents forgery by public recipients.
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
pub fn encode(plan: &TransferPlan, recovery: &age::x25519::Identity) -> Result<Vec<u8>> {
    plan.validate()?;
    let plaintext = Zeroizing::new(serde_json::to_vec(plan).map_err(|_| invalid())?);
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(invalid());
    }
    let encrypted = crypto::encrypt_bytes(&plaintext, &[recovery.to_public()])?;
    let mut mac = envelope_mac(recovery);
    mac.update(&encrypted);
    let mut envelope = Vec::with_capacity(ENVELOPE_MAGIC.len() + MAC_BYTES + encrypted.len());
    envelope.extend_from_slice(ENVELOPE_MAGIC);
    envelope.extend_from_slice(&mac.finalize().into_bytes());
    envelope.extend_from_slice(&encrypted);
    if envelope.len() > MAX_MANIFEST_BYTES {
        return Err(invalid());
    }
    Ok(envelope)
}
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
pub fn decode(
    ciphertext: &[u8],
    recovery: &age::x25519::Identity,
    expected: &TransferIntent,
) -> Result<TransferPlan> {
    expected.validate()?;
    if ciphertext.len() > MAX_MANIFEST_BYTES {
        return Err(invalid());
    }
    let rest = ciphertext
        .strip_prefix(ENVELOPE_MAGIC)
        .ok_or_else(invalid)?;
    if rest.len() <= MAC_BYTES {
        return Err(invalid());
    }
    let (tag, ciphertext) = rest.split_at(MAC_BYTES);
    let mut mac = envelope_mac(recovery);
    mac.update(ciphertext);
    mac.verify_slice(tag).map_err(|_| invalid())?;
    let decryptor = match age::Decryptor::new_buffered(ciphertext).map_err(|_| invalid())? {
        age::Decryptor::Recipients(d) => d,
        _ => return Err(invalid()),
    };
    let reader = decryptor
        .decrypt(std::iter::once(recovery as &dyn age::Identity))
        .map_err(|_| invalid())?;
    let mut plaintext = Zeroizing::new(Vec::new());
    reader
        .take((MAX_PLAINTEXT_BYTES + 1) as u64)
        .read_to_end(&mut plaintext)
        .map_err(|_| invalid())?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(invalid());
    }
    let plan: TransferPlan = serde_json::from_slice(&plaintext).map_err(|_| invalid())?;
    plan.validate()?;
    if &plan.intent != expected {
        return Err(invalid());
    }
    Ok(plan)
}
/// Atomic, private persistence using the existing symlink-resistant helper.
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
pub fn persist(path: &Path, plan: &TransferPlan, recovery: &age::x25519::Identity) -> Result<()> {
    crate::utils::helpers::atomic_write_file_no_follow(path, &encode(plan, recovery)?, true)
}
/// A bounded stream reader: callers retain control over secure file opening.
// Recovery codec is exercised by tests; production recovery is enabled in PR2.
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_manifest(
    reader: impl Read,
    recovery: &age::x25519::Identity,
    expected: &TransferIntent,
) -> Result<TransferPlan> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    decode(&bytes, recovery, expected)
}
#[cfg(test)]
#[path = "attachment_transfer_tests.rs"]
mod tests;
