//! Offline, resumable re-encryption of the visible current attachment inventory.
//! Providers do not offer portable conditional replacement: writers must stop.
use crate::backend::{
    attachment_keys::AttachmentKeyStore, file::FileDownloadSnapshot, local::crypto, FileBackend,
};
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::error::{AttachmentError, CrosstacheError, Result};
use crate::secret::attachment_key::{
    self as key, AttachmentKeyId, AttachmentKeyRef, KeySlot, PointerKind, SecretVersion,
};
use crate::secret::domain::SecretProperties;
use crate::secret::domain::SecretValue;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

#[derive(Debug, Serialize)]
pub struct RewrapReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub target_key_id: String,
    pub target_version: String,
    pub files: Vec<RewrappedFile>,
}
#[derive(Debug, Serialize)]
pub struct RewrappedFile {
    pub name: String,
    pub outcome: &'static str,
}
fn conflict() -> CrosstacheError {
    CrosstacheError::conflict("Attachment maintenance verification failed or the V2 key ring/files changed; stop all writers and retry with the current active key ID.")
}

/// Strict custody resolution shared by offline maintenance. No identity fallback.
pub(crate) async fn exact_identity(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    reference: &AttachmentKeyRef,
) -> Result<age::x25519::Identity> {
    if reference.provider_version.as_str().is_empty() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    let name = match reference.slot {
        KeySlot::Legacy => key::ACTIVE_POINTER_SECRET.into(),
        KeySlot::Retained => key::retained_record_name(&reference.key_id),
    };
    let p = keys
        .get_secret_version(vault, &name, reference.provider_version.as_str(), true)
        .await?;
    if p.version != reference.provider_version.as_str() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    if !p.enabled
        || (reference.slot == KeySlot::Retained && !key::is_marked_key_record(&p.content_type))
    {
        return Err(AttachmentError::KeyInvalid.into());
    }
    let identity = p
        .value
        .ok_or(AttachmentError::KeyInvalid)?
        .expose_secret()
        .trim()
        .parse::<age::x25519::Identity>()
        .map_err(|_| AttachmentError::KeyInvalid)?;
    if AttachmentKeyId::derive(&identity.to_public().to_string()) != reference.key_id {
        return Err(AttachmentError::KeyMismatch.into());
    }
    Ok(identity)
}
async fn retained(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    id: &AttachmentKeyId,
) -> Result<AttachmentKeyRef> {
    let p = keys
        .get_secret(vault, &key::retained_record_name(id), false)
        .await?;
    if !p.enabled || !key::is_marked_key_record(&p.content_type) {
        return Err(AttachmentError::KeyInvalid.into());
    }
    let reference = AttachmentKeyRef {
        key_id: id.clone(),
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(p.version),
    };
    exact_identity(keys, vault, &reference).await?;
    Ok(reference)
}
async fn pointer(keys: &dyn AttachmentKeyStore, vault: &str) -> Result<SecretProperties> {
    let p = keys
        .get_secret(vault, key::ACTIVE_POINTER_SECRET, true)
        .await?;
    if !p.enabled || p.value.is_none() {
        return Err(AttachmentError::PointerInvalid.into());
    }
    if p.version.is_empty() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    Ok(p)
}
/// Public custody evidence only. Never contains a private key or plaintext hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedRingBinding {
    pub(crate) target: super::attachment_transfer::TransferKeyBinding,
    pub(crate) pointer_version: String,
    pub(crate) active_id: String,
    pub(crate) legacy_id: Option<String>,
}
impl SavedRingBinding {
    pub(crate) fn validate(&self) -> Result<()> {
        self.target.validate()?;
        if self.pointer_version.is_empty()
            || self.target.slot != "retained"
            || self.active_id != self.target.key_id
            || self
                .legacy_id
                .as_ref()
                .is_some_and(|id| AttachmentKeyId::parse(id).is_none())
        {
            return Err(conflict());
        }
        Ok(())
    }
    pub(crate) async fn recheck(&self, keys: &dyn AttachmentKeyStore, vault: &str) -> Result<Ring> {
        self.validate()?;
        let id = AttachmentKeyId::parse(&self.active_id).ok_or_else(conflict)?;
        let ring = Ring::load(keys, vault, &id).await?;
        if ring.saved() != *self {
            return Err(conflict());
        }
        Ok(ring)
    }
}
/// Pinned current and exact bindings, including the explicit pre-schema legacy ID.
pub(crate) struct Ring {
    pointer: SecretProperties,
    pub(crate) target: AttachmentKeyRef,
    pub(crate) legacy: Option<AttachmentKeyRef>,
}
impl Ring {
    pub(crate) fn saved(&self) -> SavedRingBinding {
        SavedRingBinding {
            target: (&self.target).into(),
            pointer_version: self.pointer.version.clone(),
            active_id: self.target.key_id.as_str().into(),
            legacy_id: self.legacy.as_ref().map(|r| r.key_id.as_str().into()),
        }
    }
    pub(crate) async fn load(
        keys: &dyn AttachmentKeyStore,
        vault: &str,
        expected: &AttachmentKeyId,
    ) -> Result<Self> {
        let pointer = pointer(keys, vault).await?;
        let (active, legacy) = match pointer
            .value
            .as_ref()
            .map(SecretValue::expose_secret)
            .and_then(key::parse_pointer_value)
        {
            Some(PointerKind::V2 { active, legacy }) if &active == expected => (active, legacy),
            _ => return Err(conflict()),
        };
        let target = retained(keys, vault, &active).await?;
        let legacy = match legacy {
            Some(id) => Some(retained(keys, vault, &id).await?),
            None => None,
        };
        let ring = Self {
            pointer,
            target,
            legacy,
        };
        ring.recheck(keys, vault).await?;
        Ok(ring)
    }
    pub(crate) async fn recheck(&self, keys: &dyn AttachmentKeyStore, vault: &str) -> Result<()> {
        for reference in std::iter::once(&self.target).chain(self.legacy.iter()) {
            if retained(keys, vault, &reference.key_id).await? != *reference {
                return Err(conflict());
            }
        }
        let p = pointer(keys, vault).await?;
        if p.version != self.pointer.version
            || p.value != self.pointer.value
            || p.enabled != self.pointer.enabled
        {
            return Err(conflict());
        }
        Ok(())
    }
}
pub(crate) fn same_info(a: &FileInfo, b: &FileInfo) -> bool {
    a.name == b.name
        && a.size == b.size
        && a.etag == b.etag
        && a.last_modified == b.last_modified
        && a.content_type == b.content_type
        && a.groups == b.groups
        && a.tags == b.tags
        && a.metadata == b.metadata
}
pub(crate) struct Snapshot {
    pub(crate) data: FileDownloadSnapshot,
    pub(crate) info: FileInfo,
}
pub(crate) async fn snapshot(files: &dyn FileBackend, vault: &str, name: &str) -> Result<Snapshot> {
    let before = files.get_file_restore_info(vault, name).await?;
    let data = files.download_file_snapshot(vault, name, None).await?;
    let info = files.get_file_restore_info(vault, name).await?;
    if info.name != name
        || !same_info(&before, &info)
        || info.metadata != data.metadata
        || info.size != data.content.len() as u64
    {
        return Err(conflict());
    }
    Ok(Snapshot { data, info })
}
/// Reserved references must never escape validation through ordinary-file passthrough.
fn managed(info: &FileInfo) -> Result<bool> {
    let reserved = info
        .metadata
        .keys()
        .any(|k| key::is_reserved_crypto_metadata_key(k));
    if reserved {
        if info
            .metadata
            .get(key::META_ENCRYPTED)
            .is_some_and(|v| v != key::ENC_VALUE_AGE)
        {
            return Err(AttachmentError::ReferenceInvalid.into());
        }
        if !key::is_in_attachments_namespace(&info.name)
            && info.metadata.get(key::META_ENCRYPTED).map(String::as_str)
                != Some(key::ENC_VALUE_AGE)
        {
            return Err(AttachmentError::ReferenceInvalid.into());
        }
        match info
            .metadata
            .get(key::META_CRYPTO_SCHEMA)
            .map(String::as_str)
        {
            None if [key::META_KEY_ID, key::META_KEY_SLOT, key::META_KEY_VERSION]
                .iter()
                .any(|k| info.metadata.contains_key(*k)) =>
            {
                return Err(AttachmentError::ReferenceInvalid.into())
            }
            Some(key::CRYPTO_SCHEMA_V1)
                if key::parse_key_ref_from_metadata(&info.metadata).is_some()
                    && info.metadata.get(key::META_ENCRYPTED).map(String::as_str)
                        == Some(key::ENC_VALUE_AGE) => {}
            Some(_) => return Err(AttachmentError::ReferenceInvalid.into()),
            None => {}
        }
    }
    Ok(reserved || key::is_in_attachments_namespace(&info.name))
}
pub(crate) struct Inventory {
    names: BTreeSet<String>,
    pub(crate) managed: BTreeMap<String, FileInfo>,
}
pub(crate) async fn inventory(files: &dyn FileBackend, vault: &str) -> Result<Inventory> {
    let listed = files
        .list_files(
            vault,
            FileListRequest {
                prefix: None,
                groups: None,
                limit: None,
                delimiter: None,
            },
        )
        .await?;
    let mut names = BTreeSet::new();
    let mut entries = BTreeMap::new();
    for listed_file in listed {
        if !names.insert(listed_file.name.clone()) {
            return Err(conflict());
        }
        files.validate_file_name(&listed_file.name)?;
        let refreshed = files.get_file_info(vault, &listed_file.name).await?;
        if refreshed.name != listed_file.name {
            return Err(conflict());
        }
        if managed(&refreshed)? {
            let full = files
                .get_file_restore_info(vault, &listed_file.name)
                .await?;
            if full.name != listed_file.name
                || full.metadata != refreshed.metadata
                || !managed(&full)?
            {
                return Err(conflict());
            }
            entries.insert(full.name.clone(), full);
        }
    }
    Ok(Inventory {
        names,
        managed: entries,
    })
}
impl Inventory {
    pub(crate) async fn recheck(&self, files: &dyn FileBackend, vault: &str) -> Result<()> {
        let actual = inventory(files, vault).await?;
        if self.names != actual.names
            || self.managed.len() != actual.managed.len()
            || self
                .managed
                .iter()
                .any(|(name, info)| !actual.managed.get(name).is_some_and(|f| same_info(info, f)))
        {
            return Err(conflict());
        }
        Ok(())
    }
}
pub(crate) fn source_ref(ring: &Ring, snap: &Snapshot) -> Result<AttachmentKeyRef> {
    if !managed(&snap.info)? {
        return Err(AttachmentError::ReferenceInvalid.into());
    }
    match key::classify_download(
        &snap.info.name,
        &snap.data.metadata,
        crypto::is_age_encrypted(&snap.data.content),
    ) {
        key::DownloadPlan::Schema1 { key_ref } => Ok(key_ref),
        key::DownloadPlan::LegacyNoSchema => ring
            .legacy
            .clone()
            .ok_or_else(|| AttachmentError::ReferenceInvalid.into()),
        _ => Err(AttachmentError::ReferenceInvalid.into()),
    }
}
pub(crate) async fn authenticate(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    reference: &AttachmentKeyRef,
    snap: &Snapshot,
) -> Result<Zeroizing<Vec<u8>>> {
    let identity = exact_identity(keys, vault, reference).await?;
    crypto::decrypt_bytes(&snap.data.content, &identity)
        .map_err(|_| AttachmentError::DecryptionFailed.into())
}
fn hash(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}
struct Verified {
    reference: AttachmentKeyRef,
    ciphertext_hash: [u8; 32],
}

/// Authenticate the complete visible inventory before replacing any file. Keeps
/// only full metadata and ciphertext hashes between files, never plaintext on disk.
/// An interrupted apply leaves verified replacements that a retry skips.
pub async fn rewrap(
    keys: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    expected: &AttachmentKeyId,
    apply: bool,
) -> Result<RewrapReport> {
    let ring = Ring::load(keys, vault, expected).await?;
    let mut inventory = inventory(files, vault).await?;
    let mut verified = BTreeMap::new();
    let mut report = RewrapReport {
        schema_version: 1,
        operation: "rewrap",
        outcome: if apply { "applied" } else { "ready" },
        target_key_id: ring.target.key_id.as_str().into(),
        target_version: ring.target.provider_version.as_str().into(),
        files: Vec::new(),
    };
    for (name, info) in &inventory.managed {
        let snap = snapshot(files, vault, name).await?;
        if !same_info(info, &snap.info) {
            return Err(conflict());
        }
        let reference = source_ref(&ring, &snap)?;
        let _plaintext = authenticate(keys, vault, &reference, &snap).await?;
        report.files.push(RewrappedFile {
            name: name.clone(),
            outcome: if reference == ring.target
                && key::parse_key_ref_from_metadata(&snap.data.metadata).as_ref()
                    == Some(&ring.target)
            {
                "verified"
            } else {
                "rewrap"
            },
        });
        verified.insert(
            name.clone(),
            Verified {
                reference,
                ciphertext_hash: hash(&snap.data.content),
            },
        );
    }
    inventory.recheck(files, vault).await?;
    ring.recheck(keys, vault).await?;
    if !apply {
        return Ok(report);
    }
    for entry in &mut report.files {
        let snap = snapshot(files, vault, &entry.name).await?;
        let check = &verified[&entry.name];
        if !same_info(&inventory.managed[&entry.name], &snap.info)
            || hash(&snap.data.content) != check.ciphertext_hash
            || source_ref(&ring, &snap)? != check.reference
        {
            return Err(conflict());
        }
        let plaintext = authenticate(keys, vault, &check.reference, &snap).await?;
        if entry.outcome == "verified" {
            continue;
        }
        let target_identity = exact_identity(keys, vault, &ring.target).await?;
        let ciphertext = crypto::encrypt_bytes(&plaintext, &[target_identity.to_public()])?;
        let ciphertext_hash = hash(&ciphertext);
        let mut metadata = snap.data.metadata.clone();
        key::apply_crypto_metadata(&mut metadata, &ring.target);
        // The final check occurs after reading and encrypting the source, as close
        // to replacement as the portable provider APIs permit.
        inventory.recheck(files, vault).await?;
        ring.recheck(keys, vault).await?;
        files
            .restore_file(
                vault,
                FileUploadRequest {
                    name: entry.name.clone(),
                    content: ciphertext,
                    content_type: Some(snap.info.content_type.clone()),
                    groups: snap.info.groups.clone(),
                    tags: snap.info.tags.clone(),
                    metadata: metadata.clone(),
                },
            )
            .await?;
        let actual = snapshot(files, vault, &entry.name).await?;
        if hash(&actual.data.content) != ciphertext_hash
            || actual.data.metadata != metadata
            || actual.info.tags != snap.info.tags
            || actual.info.groups != snap.info.groups
            || actual.info.content_type != snap.info.content_type
            || source_ref(&ring, &actual)? != ring.target
        {
            return Err(conflict());
        }
        let readback = authenticate(keys, vault, &ring.target, &actual).await?;
        if *readback != *plaintext {
            return Err(conflict());
        }
        inventory.managed.insert(entry.name.clone(), actual.info);
        verified.insert(
            entry.name.clone(),
            Verified {
                reference: ring.target.clone(),
                ciphertext_hash,
            },
        );
        entry.outcome = "rewrapped";
    }
    inventory.recheck(files, vault).await?;
    ring.recheck(keys, vault).await?;
    for (name, info) in &inventory.managed {
        let snap = snapshot(files, vault, name).await?;
        if !same_info(info, &snap.info)
            || hash(&snap.data.content) != verified[name].ciphertext_hash
            || key::parse_key_ref_from_metadata(&snap.data.metadata).as_ref() != Some(&ring.target)
            || source_ref(&ring, &snap)? != ring.target
        {
            return Err(conflict());
        }
        let _plaintext = authenticate(keys, vault, &ring.target, &snap).await?;
    }
    inventory.recheck(files, vault).await?;
    ring.recheck(keys, vault).await?;
    Ok(report)
}
#[cfg(test)]
#[path = "attachment_rewrap_tests.rs"]
mod tests;
