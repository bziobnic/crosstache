//! Offline advisory retirement of unused retained attachment identities.
use crate::backend::{attachment_keys::AttachmentKeyStore, FileBackend};
use crate::error::Result;
use crate::secret::attachment_key::AttachmentKeyId;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct RetirementReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub key_id: String,
    pub provider_version: String,
    pub files_verified: usize,
}
fn conflict() -> crate::error::CrosstacheError {
    crate::error::CrosstacheError::conflict("Attachment retirement verification failed or the key ring/files changed; stop all writers and retry. A retirement marker is advisory and never permits key deletion.")
}
use crate::secret::attachment_key::{
    self as key, AttachmentKeyRef, KeySlot, PointerKind, SecretVersion,
};
use crate::secret::attachment_rewrap::{self as maintenance, Ring};
use crate::secret::domain::SecretProperties;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

async fn candidate_record(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    id: &AttachmentKeyId,
) -> Result<(AttachmentKeyRef, SecretProperties)> {
    let name = key::retained_record_name(id);
    let p = keys.get_secret(vault, &name, false).await?;
    if p.name != name || !p.enabled || !key::is_marked_key_record(&p.content_type) {
        return Err(conflict());
    }
    let reference = AttachmentKeyRef {
        key_id: id.clone(),
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(p.version.clone()),
    };
    maintenance::exact_identity(keys, vault, &reference).await?;
    Ok((reference, p))
}
async fn recheck_candidate(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    reference: &AttachmentKeyRef,
    original: &SecretProperties,
    retired: bool,
) -> Result<()> {
    let (actual_ref, p) = candidate_record(keys, vault, &reference.key_id).await?;
    let mut tags = original.tags.clone();
    if retired {
        tags.insert(key::KEY_RETIRED_TAG.into(), "true".into());
    }
    if actual_ref != *reference
        || p.tags != tags
        || p.content_type != original.content_type
        || p.enabled != original.enabled
        || p.expires_on != original.expires_on
        || p.not_before != original.not_before
        || p.original_name != original.original_name
        || p.created_on != original.created_on
        || p.created_timestamp != original.created_timestamp
        || p.recovery_level != original.recovery_level
    {
        return Err(conflict());
    }
    Ok(())
}
async fn recheck_files(
    keys: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    ring: &Ring,
    inventory: &maintenance::Inventory,
    verified: &BTreeMap<String, (AttachmentKeyRef, [u8; 32])>,
) -> Result<()> {
    inventory.recheck(files, vault).await?;
    for (name, info) in &inventory.managed {
        let snap = maintenance::snapshot(files, vault, name).await?;
        let (reference, digest) = &verified[name];
        let actual_hash: [u8; 32] = Sha256::digest(&snap.data.content).into();
        if !maintenance::same_info(info, &snap.info)
            || actual_hash != *digest
            || maintenance::source_ref(ring, &snap)? != *reference
        {
            return Err(conflict());
        }
        let _plaintext = maintenance::authenticate(keys, vault, reference, &snap).await?;
    }
    inventory.recheck(files, vault).await?;
    Ok(())
}

/// Authenticate every current managed attachment before an advisory metadata
/// update. Full vault permissions and stopped writers are required: historical
/// blob versions, external backups and ciphertext elsewhere remain out of scope.
/// Provider APIs are not transactional; a failed final check may leave the safe
/// advisory marker in place, and retry repeats every verification.
pub async fn retire(
    keys: &dyn AttachmentKeyStore,
    files: &dyn FileBackend,
    vault: &str,
    candidate: &AttachmentKeyId,
    apply: bool,
) -> Result<RetirementReport> {
    keys.assert_complete_visibility(vault).await?;
    let pointer = keys
        .get_secret(vault, key::ACTIVE_POINTER_SECRET, true)
        .await?;
    let active = match pointer
        .value
        .as_deref()
        .and_then(|v| key::parse_pointer_value(v))
    {
        Some(PointerKind::V2 { active, .. }) => active,
        _ => return Err(conflict()),
    };
    let ring = Ring::load(keys, vault, &active).await?;
    if ring.target.key_id == *candidate
        || ring.legacy.as_ref().is_some_and(|r| r.key_id == *candidate)
    {
        return Err(conflict());
    }
    let (reference, original) = candidate_record(keys, vault, candidate).await?;
    keys.preflight_retirement(vault, &reference).await?;
    let inventory = maintenance::inventory(files, vault).await?;
    let mut verified = BTreeMap::new();
    for (name, info) in &inventory.managed {
        let snap = maintenance::snapshot(files, vault, name).await?;
        if !maintenance::same_info(info, &snap.info) {
            return Err(conflict());
        }
        let source = maintenance::source_ref(&ring, &snap)?;
        // Compare the derived ID across every source slot and provider version.
        if source.key_id == *candidate {
            return Err(conflict());
        }
        let _plaintext = maintenance::authenticate(keys, vault, &source, &snap).await?;
        verified.insert(
            name.clone(),
            (source, Sha256::digest(&snap.data.content).into()),
        );
    }
    recheck_files(keys, files, vault, &ring, &inventory, &verified).await?;
    ring.recheck(keys, vault).await?;
    recheck_candidate(keys, vault, &reference, &original, false).await?;
    let already = original.tags.get(key::KEY_RETIRED_TAG).map(String::as_str) == Some("true");
    if apply && !already {
        keys.mark_retired(vault, &reference).await?;
    }
    // Retries and previews are verified with the same complete safety checks.
    recheck_files(keys, files, vault, &ring, &inventory, &verified).await?;
    ring.recheck(keys, vault).await?;
    recheck_candidate(keys, vault, &reference, &original, apply).await?;
    Ok(RetirementReport {
        schema_version: 1,
        operation: "retire",
        outcome: if already {
            "already_retired"
        } else if apply {
            "retired"
        } else {
            "ready"
        },
        key_id: candidate.as_str().into(),
        provider_version: reference.provider_version.as_str().into(),
        files_verified: verified.len(),
    })
}
#[cfg(test)]
#[path = "attachment_retirement_tests.rs"]
mod tests;
