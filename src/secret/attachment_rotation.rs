//! Offline rotation preserves every historical identity and the legacy binding.
//! Writers must be stopped: custody providers do not offer portable pointer CAS.
//! An interrupted pre-publication attempt may leave an unreferenced retained key.

use crate::backend::{attachment_keys::AttachmentKeyStore, BackendError};
use crate::error::{AttachmentError, CrosstacheError, Result};
use crate::secret::attachment_key::{
    self as key, AttachmentKeyId, AttachmentKeyRef, KeySlot, PointerKind, SecretVersion,
};
use crate::secret::domain::{SecretProperties, SecretRequest};
use age::secrecy::ExposeSecret;
use serde::Serialize;
use zeroize::Zeroizing;

/// Public identifiers and provider versions only; no private key material.
#[derive(Debug, Serialize)]
pub struct RotationReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub old_key_id: String,
    pub new_key_id: Option<String>,
    pub legacy_key_id: Option<String>,
    pub destination_version: Option<String>,
    pub pointer_version: Option<String>,
}

fn conflict() -> CrosstacheError {
    CrosstacheError::conflict("Attachment rotation expected a matching, unchanged V2 key ring; stop all writers and retry with the current active key ID.")
}
fn request(name: &str, value: Zeroizing<String>, marked: bool) -> SecretRequest {
    SecretRequest {
        name: name.into(),
        value,
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
fn same_pointer(a: &SecretProperties, b: &SecretProperties) -> bool {
    a.version == b.version && a.value == b.value && a.enabled == b.enabled
}
async fn exact(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    reference: &AttachmentKeyRef,
) -> Result<()> {
    if reference.provider_version.as_str().is_empty() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    let props = keys
        .get_secret_version(
            vault,
            &key::retained_record_name(&reference.key_id),
            reference.provider_version.as_str(),
            true,
        )
        .await?;
    if props.version != reference.provider_version.as_str() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    if !props.enabled || !key::is_marked_key_record(&props.content_type) {
        return Err(AttachmentError::KeyInvalid.into());
    }
    let identity = props
        .value
        .ok_or(AttachmentError::KeyInvalid)?
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
    id: &AttachmentKeyId,
) -> Result<AttachmentKeyRef> {
    let props = keys
        .get_secret(vault, &key::retained_record_name(id), false)
        .await?;
    if !props.enabled || !key::is_marked_key_record(&props.content_type) {
        return Err(AttachmentError::KeyInvalid.into());
    }
    let reference = AttachmentKeyRef {
        key_id: id.clone(),
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(props.version),
    };
    exact(keys, vault, &reference).await?;
    Ok(reference)
}
async fn unchanged_refs(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    references: &[AttachmentKeyRef],
) -> Result<()> {
    for reference in references {
        if retained(keys, vault, &reference.key_id).await? != *reference {
            return Err(conflict());
        }
    }
    Ok(())
}

/// Preview or apply one rotation, only while `expected` remains the active V2 ID.
/// Preview never generates an identity. A successful rotation makes retries using
/// the previous ID fail before any write; failed attempts can retain orphan keys.
pub async fn rotate(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    expected: &AttachmentKeyId,
    apply: bool,
) -> Result<RotationReport> {
    let original = pointer(keys, vault).await?;
    let (active, legacy) = match original
        .value
        .as_deref()
        .and_then(|s| key::parse_pointer_value(s))
    {
        Some(PointerKind::V2 { active, legacy }) if &active == expected => (active, legacy),
        _ => return Err(conflict()),
    };
    let mut references = vec![retained(keys, vault, &active).await?];
    if let Some(id) = &legacy {
        if id != &active {
            references.push(retained(keys, vault, id).await?);
        }
    }
    // Deterministic policy denial must precede random generation and mutation.
    keys.preflight_set_secret(vault, key::ACTIVE_POINTER_SECRET)
        .await?;
    let mut report = RotationReport {
        schema_version: 1,
        operation: "rotate",
        outcome: "ready",
        old_key_id: active.as_str().into(),
        new_key_id: None,
        legacy_key_id: legacy.as_ref().map(|id| id.as_str().into()),
        destination_version: None,
        pointer_version: None,
    };
    if !apply {
        return Ok(report);
    }

    let identity = age::x25519::Identity::generate();
    let new_id = AttachmentKeyId::derive(&identity.to_public().to_string());
    let name = key::retained_record_name(&new_id);
    keys.preflight_set_secret(vault, &name).await?;
    // Azure's retained commit is versioned, so explicitly refuse any existing
    // candidate name before committing as well as using create-only custody APIs.
    match keys.get_secret(vault, &name, false).await {
        Err(BackendError::NotFound { .. }) => {}
        Ok(_) => return Err(conflict()),
        Err(e) => return Err(e.into()),
    }
    let committed = keys
        .commit_retained_key(
            vault,
            request(
                &name,
                Zeroizing::new(identity.to_string().expose_secret().into()),
                true,
            ),
        )
        .await?;
    let candidate = AttachmentKeyRef {
        key_id: new_id.clone(),
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(committed.version),
    };
    exact(keys, vault, &candidate).await?;
    // Validate both the current records and their immutable versions immediately
    // before publication, including the just-committed candidate.
    references.push(candidate.clone());
    unchanged_refs(keys, vault, &references).await?;
    if !same_pointer(&original, &pointer(keys, vault).await?) {
        return Err(conflict());
    }
    let value = key::format_v2_pointer(&new_id, legacy.as_ref());
    let published = keys
        .set_secret(
            vault,
            request(
                key::ACTIVE_POINTER_SECRET,
                Zeroizing::new(value.clone()),
                false,
            ),
        )
        .await?;
    if published.version.is_empty() {
        return Err(AttachmentError::CommitUnconfirmed.into());
    }
    let confirmed = pointer(keys, vault).await?;
    if confirmed.version != published.version
        || confirmed.value.as_deref().map(|s| s.as_str()) != Some(value.as_str())
    {
        return Err(AttachmentError::CommitUnconfirmed.into());
    }
    let exact_pointer = keys
        .get_secret_version(vault, key::ACTIVE_POINTER_SECRET, &published.version, true)
        .await?;
    if !same_pointer(&confirmed, &exact_pointer) {
        return Err(AttachmentError::CommitUnconfirmed.into());
    }
    unchanged_refs(keys, vault, &references).await?;
    report.outcome = "applied";
    report.new_key_id = Some(new_id.as_str().into());
    report.destination_version = Some(candidate.provider_version.as_str().into());
    report.pointer_version = Some(published.version);
    Ok(report)
}

#[cfg(test)]
#[path = "attachment_rotation_tests.rs"]
mod tests;
