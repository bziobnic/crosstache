//! Read-only collection of verified custody and current managed-file references.
use super::attachment_backup_codec::{
    self as codec, Bundle, IdentityRecord, ManifestFile, SourceRef,
};
use super::attachment_key::{self as key, AttachmentKeyId, DownloadPlan, KeySlot, PointerKind};
use crate::backend::{attachment_keys::AttachmentKeyStore, file::FileBackend, local::crypto};
use crate::blob::models::FileListRequest;
use crate::error::{AttachmentError, CrosstacheError, Result};
use crate::secret::domain::SecretProperties;
use crate::secret::domain::SecretValue;
use age::secrecy::ExposeSecret;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

fn drift() -> CrosstacheError {
    CrosstacheError::conflict("Attachment backup source changed; stop all writers and retry.")
}

fn require_enabled(props: &SecretProperties) -> Result<()> {
    if !props.enabled || props.version.is_empty() {
        return Err(AttachmentError::KeyInvalid.into());
    }
    Ok(())
}

async fn read_identity(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    reference: &SourceRef,
) -> Result<IdentityRecord> {
    let id = AttachmentKeyId::parse(&reference.key_id).ok_or(AttachmentError::KeyInvalid)?;
    let slot = KeySlot::parse(&reference.slot).ok_or(AttachmentError::KeyInvalid)?;
    let name = match slot {
        KeySlot::Legacy => key::ACTIVE_POINTER_SECRET.into(),
        KeySlot::Retained => key::retained_record_name(&id),
    };
    let props = keys
        .get_secret_version(vault, &name, &reference.provider_version, true)
        .await?;
    require_enabled(&props)?;
    if props.version != reference.provider_version {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    if slot == KeySlot::Retained && !key::is_marked_key_record(&props.content_type) {
        return Err(AttachmentError::KeyInvalid.into());
    }
    let raw = props.value.ok_or(AttachmentError::KeyInvalid)?;
    let identity = raw
        .expose_secret()
        .trim()
        .parse::<age::x25519::Identity>()
        .map_err(|_| AttachmentError::KeyInvalid)?;
    if AttachmentKeyId::derive(&identity.to_public().to_string()) != id {
        return Err(AttachmentError::KeyMismatch.into());
    }
    Ok(IdentityRecord {
        key_id: reference.key_id.clone(),
        identity: zeroize::Zeroizing::new(identity.to_string().expose_secret().clone()),
    })
}

async fn list_names(files: &dyn FileBackend, vault: &str) -> Result<Vec<String>> {
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
    let mut names: Vec<_> = listed.into_iter().map(|f| f.name).collect();
    names.sort();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return Err(CrosstacheError::invalid_argument(
            "Attachment backup file listing is ambiguous.",
        ));
    }
    Ok(names)
}

/// Collect a complete recovery set for visible current managed files. Requires
/// offline writers; rechecks detect drift but are not a distributed transaction.
pub(crate) async fn collect(
    keys: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    backend: &str,
    vault: &str,
) -> Result<Bundle> {
    let pointer = keys
        .get_secret(vault, key::ACTIVE_POINTER_SECRET, true)
        .await?;
    require_enabled(&pointer)?;
    let pointer_value = pointer
        .value
        .as_ref()
        .ok_or(AttachmentError::PointerInvalid)?;
    let mut identities = BTreeMap::new();
    let mut references = Vec::<SourceRef>::new();
    let (active, legacy) = match key::parse_pointer_value(pointer_value.expose_secret()) {
        Some(PointerKind::V1RawIdentity) => {
            let identity = pointer_value
                .expose_secret()
                .trim()
                .parse::<age::x25519::Identity>()
                .map_err(|_| AttachmentError::KeyInvalid)?;
            let id = AttachmentKeyId::derive(&identity.to_public().to_string())
                .as_str()
                .to_owned();
            let reference = SourceRef {
                key_id: id.clone(),
                slot: "legacy".into(),
                provider_version: pointer.version.clone(),
            };
            identities.insert(id.clone(), read_identity(keys, vault, &reference).await?);
            references.push(reference);
            (id.clone(), Some(id))
        }
        Some(PointerKind::V2 { active, legacy }) => (
            active.as_str().to_owned(),
            legacy.map(|id| id.as_str().to_owned()),
        ),
        None => return Err(AttachmentError::PointerInvalid.into()),
    };
    let listed = keys.list_retained_keys(vault).await?;
    if listed.len() > 10_000 || listed.iter().any(|s| !s.enabled) {
        return Err(AttachmentError::KeyInvalid.into());
    }
    let mut required: Vec<String> = listed.iter().map(|s| s.key_id.clone()).collect();
    if !identities.contains_key(&active) {
        required.push(active.clone());
    }
    if let Some(id) = &legacy {
        if !identities.contains_key(id) {
            required.push(id.clone());
        }
    }
    required.sort();
    required.dedup();
    let mut current_records = Vec::new();
    for id in required {
        let parsed = AttachmentKeyId::parse(&id).ok_or(AttachmentError::KeyInvalid)?;
        let name = key::retained_record_name(&parsed);
        let props = keys.get_secret(vault, &name, false).await?;
        require_enabled(&props)?;
        if !key::is_marked_key_record(&props.content_type) {
            return Err(AttachmentError::KeyInvalid.into());
        }
        let reference = SourceRef {
            key_id: id.clone(),
            slot: "retained".into(),
            provider_version: props.version.clone(),
        };
        identities.insert(id, read_identity(keys, vault, &reference).await?);
        references.push(reference);
        current_records.push((name, props.version));
    }
    let names = list_names(files, vault).await?;
    let mut manifest = Vec::new();
    // Retain only hashes and metadata, never all file bytes or plaintexts.
    let mut observed = HashMap::new();
    for name in &names {
        let info = files.get_file_info(vault, name).await?;
        if key::classify_download(name, &info.metadata, true) == DownloadPlan::Passthrough {
            observed.insert(name.clone(), (None, info.metadata));
            continue;
        }
        if manifest.len() == 100_000 {
            return Err(CrosstacheError::invalid_argument(
                "Attachment backup exceeds the managed-file limit.",
            ));
        }
        let snapshot = files.download_file_snapshot(vault, name, None).await?;
        if snapshot.metadata != info.metadata {
            return Err(drift());
        }
        let source_ref = match key::classify_download(
            name,
            &snapshot.metadata,
            crypto::is_age_encrypted(&snapshot.content),
        ) {
            DownloadPlan::Schema1 { key_ref } => Some(SourceRef {
                key_id: key_ref.key_id.as_str().into(),
                slot: key_ref.slot.as_str().into(),
                provider_version: key_ref.provider_version.as_str().into(),
            }),
            DownloadPlan::LegacyNoSchema => {
                if [key::META_KEY_ID, key::META_KEY_SLOT, key::META_KEY_VERSION]
                    .iter()
                    .any(|k| snapshot.metadata.contains_key(*k))
                {
                    return Err(AttachmentError::ReferenceInvalid.into());
                }
                None
            }
            _ => return Err(AttachmentError::ReferenceInvalid.into()),
        };
        let id = match &source_ref {
            Some(reference) => {
                // Verify each historical binding even if its identity is already known.
                let record = read_identity(keys, vault, reference).await?;
                identities.insert(reference.key_id.clone(), record);
                if !references.contains(reference) {
                    references.push(reference.clone());
                }
                reference.key_id.clone()
            }
            None => legacy.clone().ok_or(AttachmentError::KeyMissing)?,
        };
        let identity = identities
            .get(&id)
            .ok_or(AttachmentError::KeyMissing)?
            .identity
            .parse::<age::x25519::Identity>()
            .map_err(|_| AttachmentError::KeyInvalid)?;
        let _authenticated = crypto::decrypt_bytes(&snapshot.content, &identity)
            .map_err(|_| AttachmentError::DecryptionFailed)?;
        let digest = hex::encode(Sha256::digest(&snapshot.content));
        observed.insert(name.clone(), (Some(digest.clone()), snapshot.metadata));
        manifest.push(ManifestFile {
            name: name.clone(),
            ciphertext_sha256: digest,
            key_id: id,
            source_ref,
        });
    }
    if list_names(files, vault).await? != names {
        return Err(drift());
    }
    for (name, (digest, metadata)) in observed {
        if let Some(digest) = digest {
            let snapshot = files.download_file_snapshot(vault, &name, None).await?;
            if hex::encode(Sha256::digest(&snapshot.content)) != digest
                || snapshot.metadata != metadata
            {
                return Err(drift());
            }
        } else if files.get_file_info(vault, &name).await?.metadata != metadata {
            return Err(drift());
        }
    }
    for (name, version) in current_records {
        let props = keys.get_secret(vault, &name, false).await?;
        if props.version != version
            || !props.enabled
            || !key::is_marked_key_record(&props.content_type)
        {
            return Err(drift());
        }
    }
    for reference in &references {
        read_identity(keys, vault, reference).await?;
    }
    let again = keys.list_retained_keys(vault).await?;
    let signature = |items: &[crate::backend::attachment_keys::RetainedKeySummary]| {
        let mut items: Vec<_> = items
            .iter()
            .map(|s| (s.name.clone(), s.key_id.clone(), s.enabled))
            .collect();
        items.sort();
        items
    };
    if signature(&again) != signature(&listed) {
        return Err(drift());
    }
    let latest = keys
        .get_secret(vault, key::ACTIVE_POINTER_SECRET, true)
        .await?;
    if latest.version != pointer.version
        || latest.value.as_ref().map(SecretValue::expose_secret)
            != pointer.value.as_ref().map(SecretValue::expose_secret)
        || !latest.enabled
    {
        return Err(drift());
    }
    let bundle = Bundle {
        format: "xv-attachment-key-backup".into(),
        schema_version: 1,
        source_backend: backend.into(),
        source_vault: vault.into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        active_key_id: active,
        legacy_key_id: legacy,
        identities: identities.into_values().collect(),
        references,
        files: manifest,
    };
    codec::validate(&bundle)?;
    Ok(bundle)
}

#[cfg(test)]
#[path = "attachment_backup_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "attachment_backup_fault_tests.rs"]
mod fault_tests;
