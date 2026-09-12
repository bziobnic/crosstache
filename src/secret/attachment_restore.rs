//! Explicit offline custody recovery; file payloads are never re-encrypted.
use crate::backend::{
    attachment_keys::AttachmentKeyStore, file::FileDownloadSnapshot, local::crypto, BackendError,
    FileBackend,
};
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::error::{AttachmentError, CrosstacheError, Result};
use crate::secret::attachment_backup_codec::{self as codec, Bundle, ManifestFile, SourceRef};
use crate::secret::attachment_key::{
    self as key, AttachmentKeyId, AttachmentKeyRef, KeySlot, PointerKind, SecretVersion,
};
use crate::secret::domain::SecretValue;
use crate::secret::domain::{Secret, SecretRequest};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use zeroize::Zeroizing;

#[derive(Debug, Serialize)]
pub struct RestoreReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub source_backend: String,
    pub source_vault: String,
    pub destination_vault: String,
    pub active_key_id: String,
    pub legacy_key_id: Option<String>,
    pub pointer_outcome: &'static str,
    pub keys: Vec<RestoredKey>,
    pub files: Vec<RestoredFile>,
    pub mappings: Vec<VersionMapping>,
}
#[derive(Debug, Serialize)]
pub struct RestoredKey {
    pub key_id: String,
    pub outcome: &'static str,
    pub destination_version: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct RestoredFile {
    pub name: String,
    pub outcome: &'static str,
}
#[derive(Debug, Serialize)]
pub struct VersionMapping {
    pub key_id: String,
    pub source_slot: String,
    pub source_version: String,
    pub destination_version: Option<String>,
}

fn conflict() -> CrosstacheError {
    CrosstacheError::conflict("Attachment restore verification failed or destination changed; stop writers and retry with the original backup.")
}
fn id(value: &str) -> Result<AttachmentKeyId> {
    AttachmentKeyId::parse(value).ok_or_else(|| AttachmentError::KeyInvalid.into())
}
fn request(name: &str, value: Zeroizing<String>, marked: bool) -> SecretRequest {
    SecretRequest {
        name: name.into(),
        value: SecretValue::new(value.as_str()),
        content_type: marked.then(|| key::KEY_RECORD_CONTENT_TYPE.into()),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    }
}
async fn pointer(keys: &dyn AttachmentKeyStore, vault: &str) -> Result<Option<Secret>> {
    match keys.get_secret(vault, key::ACTIVE_POINTER_SECRET).await {
        Ok(p) => {
            if !p.enabled {
                return Err(AttachmentError::PointerInvalid.into());
            }
            if p.version.is_empty() {
                return Err(AttachmentError::KeyVersionInvalid.into());
            }
            Ok(Some(p))
        }
        Err(BackendError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn same_pointer(a: &Option<Secret>, b: &Option<Secret>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.version == b.version && a.value == b.value && a.enabled == b.enabled
        }
        _ => false,
    }
}
async fn exact(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    reference: &AttachmentKeyRef,
) -> Result<()> {
    let name = match reference.slot {
        KeySlot::Legacy => key::ACTIVE_POINTER_SECRET.into(),
        KeySlot::Retained => key::retained_record_name(&reference.key_id),
    };
    if reference.provider_version.as_str().is_empty() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    let p = keys
        .get_secret_version(vault, &name, reference.provider_version.as_str())
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
        .expose_secret()
        .trim()
        .parse::<age::x25519::Identity>()
        .map_err(|_| AttachmentError::KeyInvalid)?;
    if AttachmentKeyId::derive(&identity.to_public().to_string()) != reference.key_id {
        return Err(AttachmentError::KeyMismatch.into());
    }
    Ok(())
}
async fn retained(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    key_id: &str,
) -> Result<Option<AttachmentKeyRef>> {
    let key_id = id(key_id)?;
    let p = match keys
        .get_secret_metadata(vault, &key::retained_record_name(&key_id))
        .await
    {
        Ok(p) => p,
        Err(BackendError::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !p.enabled || !key::is_marked_key_record(&p.content_type) {
        return Err(conflict());
    }
    let reference = AttachmentKeyRef {
        key_id,
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(p.version),
    };
    exact(keys, vault, &reference).await?;
    Ok(Some(reference))
}
fn same_info(a: &FileInfo, b: &FileInfo) -> bool {
    a.name == b.name
        && a.size == b.size
        && a.etag == b.etag
        && a.last_modified == b.last_modified
        && a.content_type == b.content_type
        && a.groups == b.groups
        && a.tags == b.tags
        && a.metadata == b.metadata
}
struct Snapshot {
    data: FileDownloadSnapshot,
    info: FileInfo,
}
async fn snapshot(files: &dyn FileBackend, vault: &str, name: &str) -> Result<Snapshot> {
    let before = files.get_file_restore_info(vault, name).await?;
    let data = files.download_file_snapshot(vault, name, None).await?;
    let info = files.get_file_restore_info(vault, name).await?;
    if !same_info(&before, &info)
        || info.metadata != data.metadata
        || info.size != data.content.len() as u64
    {
        return Err(conflict());
    }
    Ok(Snapshot { data, info })
}
fn source(reference: &AttachmentKeyRef) -> SourceRef {
    SourceRef {
        key_id: reference.key_id.as_str().into(),
        slot: reference.slot.as_str().into(),
        provider_version: reference.provider_version.as_str().into(),
    }
}
fn authenticate(bundle: &Bundle, manifest: &ManifestFile, snapshot: &Snapshot) -> Result<()> {
    if hex::encode(Sha256::digest(&snapshot.data.content)) != manifest.ciphertext_sha256 {
        return Err(conflict());
    }
    let record = bundle
        .identities
        .iter()
        .find(|r| r.key_id == manifest.key_id)
        .ok_or(AttachmentError::KeyMissing)?;
    let identity = record
        .identity
        .trim()
        .parse::<age::x25519::Identity>()
        .map_err(|_| AttachmentError::KeyInvalid)?;
    let _plaintext = crypto::decrypt_bytes(&snapshot.data.content, &identity)
        .map_err(|_| AttachmentError::DecryptionFailed)?;
    Ok(())
}
async fn file_binding(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    manifest: &ManifestFile,
    snapshot: &Snapshot,
) -> Result<()> {
    match key::classify_download(
        &manifest.name,
        &snapshot.data.metadata,
        crypto::is_age_encrypted(&snapshot.data.content),
    ) {
        key::DownloadPlan::LegacyNoSchema if manifest.source_ref.is_none() => {
            if [key::META_KEY_ID, key::META_KEY_SLOT, key::META_KEY_VERSION]
                .iter()
                .any(|k| snapshot.data.metadata.contains_key(*k))
            {
                return Err(AttachmentError::ReferenceInvalid.into());
            }
            Ok(())
        }
        key::DownloadPlan::Schema1 { key_ref } => {
            if manifest.source_ref.as_ref() == Some(&source(&key_ref)) {
                return Ok(());
            }
            if key_ref.slot != KeySlot::Retained || key_ref.key_id.as_str() != manifest.key_id {
                return Err(AttachmentError::ReferenceInvalid.into());
            }
            exact(keys, vault, &key_ref).await
        }
        _ => Err(AttachmentError::ReferenceInvalid.into()),
    }
}
async fn file_set(files: &dyn FileBackend, vault: &str, bundle: &Bundle) -> Result<()> {
    let expected: HashSet<_> = bundle.files.iter().map(|f| f.name.as_str()).collect();
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
    for listed_file in listed {
        let f = files.get_file_info(vault, &listed_file.name).await?;
        if key::classify_download(&f.name, &f.metadata, true) != key::DownloadPlan::Passthrough
            && !expected.contains(f.name.as_str())
        {
            return Err(conflict());
        }
    }
    Ok(())
}
async fn verify_file(
    keys: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    bundle: &Bundle,
    manifest: &ManifestFile,
    expected: &Snapshot,
    reference: &AttachmentKeyRef,
) -> Result<Snapshot> {
    let actual = snapshot(files, vault, &manifest.name).await?;
    let mut metadata = expected.data.metadata.clone();
    key::apply_crypto_metadata(&mut metadata, reference);
    if actual.data.content != expected.data.content
        || actual.data.metadata != metadata
        || actual.info.tags != expected.info.tags
        || actual.info.groups != expected.info.groups
        || actual.info.content_type != expected.info.content_type
    {
        return Err(conflict());
    }
    authenticate(bundle, manifest, &actual)?;
    exact(keys, vault, reference).await?;
    let _plaintext = Zeroizing::new(
        crate::secret::attachments::download_decrypted(keys, files, vault, &manifest.name, None)
            .await?,
    );
    Ok(actual)
}

pub(crate) async fn restore(
    keys: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    bundle: &Bundle,
    apply: bool,
    repair_pointer: bool,
) -> Result<RestoreReport> {
    codec::validate(bundle)?;
    for f in &bundle.files {
        files.validate_file_name(&f.name)?;
    }
    let active = id(&bundle.active_key_id)?;
    let legacy = bundle.legacy_key_id.as_deref().map(id).transpose()?;
    let before_pointer = pointer(keys, vault).await?;
    let pointer_outcome = match before_pointer.as_ref() {
        None => "create",
        Some(p) => {
            let raw = &p.value;
            // The shared classifier recognizes V1 by prefix alone. Only a
            // valid private identity is a V1 binding that repair must preserve.
            let kind = match key::parse_pointer_value(raw.expose_secret()) {
                Some(PointerKind::V1RawIdentity)
                    if raw
                        .expose_secret()
                        .trim()
                        .parse::<age::x25519::Identity>()
                        .is_err() =>
                {
                    None
                }
                kind => kind,
            };
            match kind {
                Some(PointerKind::V2 { active: a, legacy: l }) if a == active && l == legacy => "unchanged",
                Some(_) => return Err(conflict()),
                None if repair_pointer => "repair",
                None => return Err(CrosstacheError::conflict(
                    "Malformed attachment pointer; preview restore with --repair-pointer to explicitly authorize repair.",
                )),
            }
        }
    };
    let mut refs = HashMap::new();
    let mut report = RestoreReport {
        schema_version: 1,
        operation: "restore",
        outcome: if apply { "applied" } else { "ready" },
        source_backend: bundle.source_backend.clone(),
        source_vault: bundle.source_vault.clone(),
        destination_vault: vault.into(),
        active_key_id: bundle.active_key_id.clone(),
        legacy_key_id: bundle.legacy_key_id.clone(),
        pointer_outcome,
        keys: Vec::new(),
        files: Vec::new(),
        mappings: Vec::new(),
    };
    for identity in &bundle.identities {
        let reference = retained(keys, vault, &identity.key_id).await?;
        report.keys.push(RestoredKey {
            key_id: identity.key_id.clone(),
            outcome: if reference.is_some() {
                "reuse"
            } else {
                "create"
            },
            destination_version: reference
                .as_ref()
                .map(|r| r.provider_version.as_str().into()),
        });
        if let Some(reference) = reference {
            refs.insert(identity.key_id.clone(), reference);
        }
    }
    file_set(files, vault, bundle).await?;
    let mut snapshots = Vec::new();
    for manifest in &bundle.files {
        let snap = snapshot(files, vault, &manifest.name).await?;
        authenticate(bundle, manifest, &snap)?;
        file_binding(keys, vault, manifest, &snap).await?;
        let already = refs.get(&manifest.key_id).is_some_and(|r| {
            key::parse_key_ref_from_metadata(&snap.data.metadata).as_ref() == Some(r)
        });
        report.files.push(RestoredFile {
            name: manifest.name.clone(),
            outcome: if already { "verified" } else { "rebind" },
        });
        snapshots.push(snap.info);
    }
    // Local authorization is knowable before mutation; remote write failures
    // can still occur later and are not represented as preflight guarantees.
    for identity in &bundle.identities {
        if !refs.contains_key(&identity.key_id) {
            keys.preflight_set_secret(vault, &key::retained_record_name(&id(&identity.key_id)?))
                .await?;
        }
    }
    if pointer_outcome != "unchanged" {
        keys.preflight_set_secret(vault, key::ACTIVE_POINTER_SECRET)
            .await?;
    }
    if apply {
        for (index, identity) in bundle.identities.iter().enumerate() {
            if !refs.contains_key(&identity.key_id) {
                let key_id = id(&identity.key_id)?;
                let reference = match keys
                    .commit_retained_key(
                        vault,
                        request(
                            &key::retained_record_name(&key_id),
                            identity.identity.clone(),
                            true,
                        ),
                    )
                    .await
                {
                    Ok(p) => {
                        if !p.enabled || !key::is_marked_key_record(&p.content_type) {
                            return Err(AttachmentError::KeyInvalid.into());
                        }
                        let r = AttachmentKeyRef {
                            key_id,
                            slot: KeySlot::Retained,
                            provider_version: SecretVersion::new(p.version),
                        };
                        exact(keys, vault, &r).await?;
                        r
                    }
                    Err(BackendError::Conflict(_)) => retained(keys, vault, &identity.key_id)
                        .await?
                        .ok_or(AttachmentError::KeyMissing)?,
                    Err(e) => return Err(e.into()),
                };
                refs.insert(identity.key_id.clone(), reference);
            }
            report.keys[index].destination_version =
                Some(refs[&identity.key_id].provider_version.as_str().into());
        }
        for (index, manifest) in bundle.files.iter().enumerate() {
            let old = snapshot(files, vault, &manifest.name).await?;
            if !same_info(&snapshots[index], &old.info) {
                return Err(conflict());
            }
            authenticate(bundle, manifest, &old)?;
            let reference = &refs[&manifest.key_id];
            let mut metadata = old.data.metadata.clone();
            key::apply_crypto_metadata(&mut metadata, reference);
            if metadata != old.data.metadata {
                files
                    .restore_file(
                        vault,
                        FileUploadRequest {
                            name: manifest.name.clone(),
                            content: old.data.content.clone(),
                            content_type: Some(old.info.content_type.clone()),
                            groups: old.info.groups.clone(),
                            tags: old.info.tags.clone(),
                            metadata,
                        },
                    )
                    .await?;
                report.files[index].outcome = "rebound";
            }
            snapshots[index] = verify_file(keys, files, vault, bundle, manifest, &old, reference)
                .await?
                .info;
        }
        file_set(files, vault, bundle).await?;
        for reference in refs.values() {
            if retained(keys, vault, reference.key_id.as_str())
                .await?
                .as_ref()
                != Some(reference)
            {
                return Err(conflict());
            }
        }
        if !same_pointer(&before_pointer, &pointer(keys, vault).await?) {
            return Err(conflict());
        }
        let published_pointer = if pointer_outcome != "unchanged" {
            let value = key::format_v2_pointer(&active, legacy.as_ref());
            let written = keys
                .set_secret(
                    vault,
                    request(
                        key::ACTIVE_POINTER_SECRET,
                        Zeroizing::new(value.clone()),
                        false,
                    ),
                )
                .await?;
            let confirmed = pointer(keys, vault)
                .await?
                .ok_or(AttachmentError::CommitUnconfirmed)?;
            if written.version.is_empty()
                || confirmed.version != written.version
                || confirmed.value.expose_secret() != value.as_str()
            {
                return Err(AttachmentError::CommitUnconfirmed.into());
            }
            Some(confirmed)
        } else {
            before_pointer.clone()
        };
        for (manifest, expected_info) in bundle.files.iter().zip(&snapshots) {
            let snap = snapshot(files, vault, &manifest.name).await?;
            if !same_info(expected_info, &snap.info) {
                return Err(conflict());
            }
            verify_file(
                keys,
                files,
                vault,
                bundle,
                manifest,
                &snap,
                &refs[&manifest.key_id],
            )
            .await?;
        }
        for reference in refs.values() {
            if retained(keys, vault, reference.key_id.as_str())
                .await?
                .as_ref()
                != Some(reference)
            {
                return Err(conflict());
            }
        }
        file_set(files, vault, bundle).await?;
        if !same_pointer(&published_pointer, &pointer(keys, vault).await?) {
            return Err(conflict());
        }
    }
    report.mappings = bundle
        .references
        .iter()
        .map(|r| VersionMapping {
            key_id: r.key_id.clone(),
            source_slot: r.slot.clone(),
            source_version: r.provider_version.clone(),
            destination_version: refs
                .get(&r.key_id)
                .map(|r| r.provider_version.as_str().into()),
        })
        .collect();
    Ok(report)
}
#[cfg(test)]
#[path = "attachment_restore_tests.rs"]
mod tests;
