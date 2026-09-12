//! Offline operations: verify retained custody before changing the active pointer.
//! Callers must stop writers; provider-portable compare-and-swap is unavailable.

use crate::backend::attachment_keys::AttachmentKeyStore;
use crate::backend::BackendError;
use crate::error::{AttachmentError, CrosstacheError, Result};
use crate::secret::attachment_key::{
    self as key, AttachmentKeyId, AttachmentKeyMaterial, KeySlot, PointerKind, SecretVersion,
};
use crate::secret::domain::{SecretProperties, SecretRequest};
use serde::Serialize;
use zeroize::Zeroizing;

#[derive(Debug, Serialize)]
pub struct InitializeReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub active_key_id: Option<String>,
    pub retained_version: Option<String>,
}

/// Explicit empty-ring initialization. Preview never generates a key; apply
/// requires stopped writers, which the CLI enforces through --apply --offline.
/// Existing managed files or retained-only custody require recovery instead.
pub async fn initialize(
    keys: &dyn AttachmentKeyStore,
    files: &dyn crate::backend::FileBackend,
    vault: &str,
    apply: bool,
) -> Result<InitializeReport> {
    use super::attachment_rewrap::{inventory, Ring};
    use age::secrecy::ExposeSecret;

    keys.assert_complete_visibility(vault).await?;
    let observed = pointer(keys, vault).await?;
    if let Some(existing) = &observed {
        if !existing.enabled || existing.version.is_empty() {
            return Err(CrosstacheError::conflict(
                "The attachment pointer is unhealthy; use attachment-key recover instead of initialize.",
            ));
        }
        let active = match value(existing).and_then(key::parse_pointer_value) {
            Some(PointerKind::V2 { active, .. }) => active,
            Some(PointerKind::V1RawIdentity) => return Err(CrosstacheError::conflict(
                "A legacy attachment identity exists; use attachment-key upgrade instead of initialize.",
            )),
            _ => return Err(CrosstacheError::conflict(
                "The attachment pointer is malformed; use attachment-key recover instead of initialize.",
            )),
        };
        let ring = Ring::load(keys, vault, &active).await?;
        return Ok(InitializeReport {
            schema_version: 1,
            operation: "initialize",
            outcome: "already_initialized",
            active_key_id: Some(active.as_str().into()),
            retained_version: Some(ring.target.provider_version.as_str().into()),
        });
    }
    refuse_retained_only_initialization(keys, vault).await?;
    let inventory = inventory(files, vault).await?;
    if !inventory.managed.is_empty() {
        return Err(CrosstacheError::conflict(
            "Managed attachments exist without an active pointer; recover the original keys before initialization.",
        ));
    }
    if !apply {
        return Ok(InitializeReport {
            schema_version: 1,
            operation: "initialize",
            outcome: "would_initialize",
            active_key_id: None,
            retained_version: None,
        });
    }
    keys.preflight_set_secret(vault, key::ACTIVE_POINTER_SECRET)
        .await?;
    let identity = age::x25519::Identity::generate();
    let id = AttachmentKeyId::derive(&identity.to_public().to_string());
    let candidate = Zeroizing::new(identity.to_string().expose_secret().to_string());
    keys.preflight_set_secret(vault, &key::retained_record_name(&id))
        .await?;
    inventory.recheck(files, vault).await?;
    unchanged(keys, vault, &observed).await?;
    refuse_retained_only_initialization(keys, vault).await?;
    // Reuse the upload initializer's immutable retained commit and exact readback.
    // Keep this preflighted candidate fixed: a collision fails rather than trying
    // another candidate whose custody permissions have not been checked.
    let material =
        super::attachments::initialize_v2(keys, vault, &mut || candidate.clone()).await?;
    let ring = Ring::load(keys, vault, &id).await?;
    if ring.target != *material.reference() || ring.legacy.is_some() {
        return Err(CrosstacheError::conflict(
            "Attachment initialization could not verify its published binding; inspect attachment-key status and recover retained custody.",
        ));
    }
    Ok(InitializeReport {
        schema_version: 1,
        operation: "initialize",
        outcome: "initialized",
        active_key_id: Some(id.as_str().into()),
        retained_version: Some(ring.target.provider_version.as_str().into()),
    })
}

async fn refuse_retained_only_initialization(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
) -> Result<()> {
    if !keys.list_retained_keys(vault).await?.is_empty() {
        return Err(CrosstacheError::conflict(
            "Retained attachment keys exist without an active pointer; use attachment-key recover with an explicit key ID.",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct LifecycleReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub active_key_id: String,
    pub legacy_key_id: Option<String>,
    pub retained_version: Option<String>,
}

fn report(
    operation: &'static str,
    outcome: &'static str,
    id: &AttachmentKeyId,
    legacy: Option<&AttachmentKeyId>,
    version: Option<String>,
) -> LifecycleReport {
    LifecycleReport {
        schema_version: 1,
        operation,
        outcome,
        active_key_id: id.as_str().into(),
        legacy_key_id: legacy.map(|id| id.as_str().into()),
        retained_version: version,
    }
}

async fn pointer(keys: &dyn AttachmentKeyStore, vault: &str) -> Result<Option<SecretProperties>> {
    match keys
        .get_secret(vault, key::ACTIVE_POINTER_SECRET, true)
        .await
    {
        Ok(value) => Ok(Some(value)),
        Err(BackendError::NotFound { .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn value(props: &SecretProperties) -> Option<&str> {
    props.value.as_ref().map(|v| v.as_str())
}

async fn unchanged(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    expected: &Option<SecretProperties>,
) -> Result<()> {
    let current = pointer(keys, vault).await?;
    let matches = match (expected, &current) {
        (None, None) => true,
        (Some(a), Some(b)) => a.version == b.version && value(a) == value(b),
        _ => false,
    };
    if !matches {
        return Err(CrosstacheError::conflict(
            "The attachment pointer changed during the operation; stop all writers and retry.",
        ));
    }
    Ok(())
}

async fn exact_identity(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    name: &str,
    version: &str,
    id: &AttachmentKeyId,
    slot: KeySlot,
) -> Result<AttachmentKeyMaterial> {
    if version.is_empty() {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    let props = keys
        .get_secret_version(vault, name, version, true)
        .await
        .map_err(|e| match e {
            BackendError::NotFound { .. } => CrosstacheError::from(AttachmentError::KeyMissing),
            other => other.into(),
        })?;
    if props.version != version {
        return Err(AttachmentError::KeyVersionInvalid.into());
    }
    let material = AttachmentKeyMaterial::from_identity(
        slot,
        SecretVersion::new(version),
        props.value.ok_or(AttachmentError::KeyInvalid)?,
    )
    .ok_or(AttachmentError::KeyInvalid)?;
    if !material.verify_id(id) {
        return Err(AttachmentError::KeyMismatch.into());
    }
    Ok(material)
}

async fn retained(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    id: &AttachmentKeyId,
) -> Result<Option<AttachmentKeyMaterial>> {
    let name = key::retained_record_name(id);
    let props = match keys.get_secret(vault, &name, false).await {
        Ok(props) => props,
        Err(BackendError::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !key::is_marked_key_record(&props.content_type) {
        return Err(CrosstacheError::conflict(
            "An unmarked secret occupies the required retained-key name; it was not modified.",
        ));
    }
    Ok(Some(
        exact_identity(keys, vault, &name, &props.version, id, KeySlot::Retained).await?,
    ))
}

fn write_request(name: &str, value: Zeroizing<String>, marked: bool) -> SecretRequest {
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

async fn publish(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    active: &AttachmentKeyId,
    legacy: Option<&AttachmentKeyId>,
) -> Result<()> {
    let expected = key::format_v2_pointer(active, legacy);
    keys.set_secret(
        vault,
        write_request(
            key::ACTIVE_POINTER_SECRET,
            Zeroizing::new(expected.clone()),
            false,
        ),
    )
    .await?;
    let confirmed = pointer(keys, vault).await?;
    if confirmed.as_ref().and_then(value) != Some(expected.as_str()) {
        return Err(AttachmentError::CommitUnconfirmed.into());
    }
    Ok(())
}

/// Preview or apply an offline V1-to-V2 conversion, retaining the SAME identity.
/// An already valid V2 ring is a no-op. Existing key versions are never deleted.
pub async fn upgrade(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    apply: bool,
) -> Result<LifecycleReport> {
    let source = pointer(keys, vault).await?;
    let props = source.as_ref().ok_or(AttachmentError::KeyMissing)?;
    let raw = props
        .value
        .as_ref()
        .ok_or(AttachmentError::PointerInvalid)?;
    match key::parse_pointer_value(raw) {
        Some(PointerKind::V2 { active, legacy }) => {
            let material = retained(keys, vault, &active)
                .await?
                .ok_or(AttachmentError::KeyMissing)?;
            if let Some(id) = &legacy {
                retained(keys, vault, id)
                    .await?
                    .ok_or(AttachmentError::KeyMissing)?;
            }
            Ok(report(
                "upgrade",
                "unchanged",
                &active,
                legacy.as_ref(),
                Some(material.reference().provider_version.as_str().into()),
            ))
        }
        Some(PointerKind::V1RawIdentity) => {
            if props.version.is_empty() {
                return Err(AttachmentError::KeyVersionInvalid.into());
            }
            let original = AttachmentKeyMaterial::from_identity(
                KeySlot::Legacy,
                SecretVersion::new(props.version.clone()),
                raw.clone(),
            )
            .ok_or(AttachmentError::KeyInvalid)?;
            let id = &original.reference().key_id;
            // Prove exact-version reads work before changing the current pointer.
            exact_identity(
                keys,
                vault,
                key::ACTIVE_POINTER_SECRET,
                &props.version,
                id,
                KeySlot::Legacy,
            )
            .await?;
            let mut kept = retained(keys, vault, id).await?;
            if !apply {
                return Ok(report(
                    "upgrade",
                    "ready",
                    id,
                    Some(id),
                    kept.as_ref()
                        .map(|m| m.reference().provider_version.as_str().into()),
                ));
            }
            if kept.is_none() {
                let name = key::retained_record_name(id);
                match keys
                    .commit_retained_key(vault, write_request(&name, raw.clone(), true))
                    .await
                {
                    Ok(committed) => {
                        kept = Some(
                            exact_identity(
                                keys,
                                vault,
                                &name,
                                &committed.version,
                                id,
                                KeySlot::Retained,
                            )
                            .await?,
                        );
                    }
                    Err(BackendError::Conflict(_)) => {
                        kept = Some(
                            retained(keys, vault, id)
                                .await?
                                .ok_or(AttachmentError::KeyMissing)?,
                        );
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            unchanged(keys, vault, &source).await?;
            publish(keys, vault, id, Some(id)).await?;
            // Schema-1 legacy blobs remain pinned to the original pointer version.
            exact_identity(
                keys,
                vault,
                key::ACTIVE_POINTER_SECRET,
                &props.version,
                id,
                KeySlot::Legacy,
            )
            .await?;
            Ok(report(
                "upgrade",
                "applied",
                id,
                Some(id),
                kept.map(|m| m.reference().provider_version.as_str().into()),
            ))
        }
        None => Err(AttachmentError::PointerInvalid.into()),
    }
}

/// Repair a missing/broken pointer from explicitly selected existing keys.
/// The caller explicitly chooses a legacy fallback (or none); no keys are generated.
pub async fn recover(
    keys: &dyn AttachmentKeyStore,
    vault: &str,
    active: &AttachmentKeyId,
    legacy: Option<&AttachmentKeyId>,
    apply: bool,
) -> Result<LifecycleReport> {
    let source = pointer(keys, vault).await?;
    if let Some(props) = &source {
        match value(props).and_then(key::parse_pointer_value) {
            Some(PointerKind::V1RawIdentity) => return Err(CrosstacheError::conflict(
                "A V1 pointer must be preserved with attachment-key upgrade, not replaced by recovery.",
            )),
            Some(PointerKind::V2 { active: current, legacy: previous_legacy }) => {
                if previous_legacy.as_ref() != legacy {
                    return Err(CrosstacheError::conflict(
                        "Recovery cannot change a known legacy fallback binding.",
                    ));
                }
                if &current == active {
                    // Validate selected records below before declaring the no-op.
                } else {
                    match retained(keys, vault, &current).await {
                        Ok(Some(_)) => return Err(CrosstacheError::conflict(
                            "The current active key is valid; recovery cannot rotate a healthy pointer.",
                        )),
                        Ok(None) | Err(CrosstacheError::Attachment(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            None => {}
        }
    }
    let selected = retained(keys, vault, active)
        .await?
        .ok_or(AttachmentError::KeyMissing)?;
    if let Some(id) = legacy {
        retained(keys, vault, id)
            .await?
            .ok_or(AttachmentError::KeyMissing)?;
    }
    let expected = key::format_v2_pointer(active, legacy);
    let outcome = if source.as_ref().and_then(value) == Some(expected.as_str()) {
        "unchanged"
    } else if apply {
        unchanged(keys, vault, &source).await?;
        publish(keys, vault, active, legacy).await?;
        "applied"
    } else {
        "ready"
    };
    Ok(report(
        "recover",
        outcome,
        active,
        legacy,
        Some(selected.reference().provider_version.as_str().into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::local::crypto;
    use crate::backend::{local::LocalBackend, Backend};
    use crate::blob::models::FileUploadRequest;
    use crate::config::settings::LocalConfig;
    use crate::secret::domain::SecretRequest;
    use crate::secret::{attachment_key as key, attachments};
    use age::secrecy::ExposeSecret;
    use std::collections::HashMap;
    use zeroize::Zeroizing;

    fn fixture() -> (tempfile::TempDir, LocalBackend) {
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(dir.path().join("store").display().to_string()),
            key_file: Some(dir.path().join("identity").display().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        }))
        .unwrap();
        (dir, backend)
    }

    fn request(name: &str, value: &str, marked: bool) -> SecretRequest {
        SecretRequest {
            name: name.into(),
            value: Zeroizing::new(value.into()),
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

    fn blob(name: &str, content: Vec<u8>) -> FileUploadRequest {
        FileUploadRequest {
            name: name.into(),
            content,
            content_type: None,
            groups: vec![],
            metadata: HashMap::new(),
            tags: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn initialize_preview_is_read_only_and_apply_is_idempotent_without_files() {
        let (dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        let file_dir = dir.path().join("store/vaults/default/files");
        let preview = initialize(keys.as_ref(), files, "default", false)
            .await
            .unwrap();
        assert_eq!(preview.outcome, "would_initialize");
        assert!(preview.active_key_id.is_none());
        assert!(pointer(keys.as_ref(), "default").await.unwrap().is_none());
        assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
        assert!(!file_dir.exists());
        let applied = initialize(keys.as_ref(), files, "default", true)
            .await
            .unwrap();
        assert_eq!(applied.outcome, "initialized");
        let original = pointer(keys.as_ref(), "default").await.unwrap().unwrap();
        let again = initialize(keys.as_ref(), files, "default", true)
            .await
            .unwrap();
        assert_eq!(again.outcome, "already_initialized");
        assert_eq!(again.active_key_id, applied.active_key_id);
        assert_eq!(again.retained_version, applied.retained_version);
        assert_eq!(
            pointer(keys.as_ref(), "default")
                .await
                .unwrap()
                .unwrap()
                .version,
            original.version
        );
        assert_eq!(keys.list_retained_keys("default").await.unwrap().len(), 1);
        assert!(
            !file_dir.exists(),
            "key initialization must not create file storage"
        );
        let json = serde_json::to_string(&applied).unwrap();
        assert!(!json.contains("AGE-SECRET-KEY"));
    }

    #[tokio::test]
    async fn initialize_refuses_retained_only_custody_and_managed_files() {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let identity = age::x25519::Identity::generate();
        let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
        keys.commit_retained_key(
            "default",
            request(
                &key::retained_record_name(&id),
                identity.to_string().expose_secret(),
                true,
            ),
        )
        .await
        .unwrap();
        for apply in [false, true] {
            let error = initialize(keys.as_ref(), backend.files().unwrap(), "default", apply)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("recover"));
            assert!(pointer(keys.as_ref(), "default").await.unwrap().is_none());
            assert_eq!(keys.list_retained_keys("default").await.unwrap().len(), 1);
        }
        let (_dir, backend) = fixture();
        backend
            .secrets()
            .set_secret("default", request("lost", "owner", false))
            .await
            .unwrap();
        let files = backend.files().unwrap();
        files
            .upload_file(
                "default",
                blob(
                    "attachments/lost/proof",
                    b"unrecoverable old ciphertext".to_vec(),
                ),
                None,
            )
            .await
            .unwrap();
        let keys = backend.attachment_keys();
        for apply in [false, true] {
            let error = initialize(keys.as_ref(), files, "default", apply)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("recover"));
            assert!(pointer(keys.as_ref(), "default").await.unwrap().is_none());
            assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn initialize_refuses_legacy_invalid_and_disabled_pointers_without_writes() {
        for raw in [
            age::x25519::Identity::generate()
                .to_string()
                .expose_secret()
                .to_string(),
            "invalid-pointer-canary".into(),
        ] {
            let (_dir, backend) = fixture();
            let keys = backend.attachment_keys();
            let original = keys
                .set_secret("default", request(key::ACTIVE_POINTER_SECRET, &raw, false))
                .await
                .unwrap();
            for apply in [false, true] {
                let error = initialize(keys.as_ref(), backend.files().unwrap(), "default", apply)
                    .await
                    .unwrap_err();
                assert!(
                    error.to_string().contains("upgrade") || error.to_string().contains("recover")
                );
                assert!(!error.to_string().contains(&raw));
                assert_eq!(
                    pointer(keys.as_ref(), "default")
                        .await
                        .unwrap()
                        .unwrap()
                        .version,
                    original.version
                );
                assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
            }
        }
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        initialize(keys.as_ref(), backend.files().unwrap(), "default", true)
            .await
            .unwrap();
        let mut disabled = crate::backend::secret::rename_request_from_properties(
            key::ACTIVE_POINTER_SECRET,
            &pointer(keys.as_ref(), "default").await.unwrap().unwrap(),
        )
        .unwrap();
        disabled.enabled = Some(false);
        let original = keys.set_secret("default", disabled).await.unwrap();
        assert!(
            initialize(keys.as_ref(), backend.files().unwrap(), "default", true)
                .await
                .unwrap_err()
                .to_string()
                .contains("recover")
        );
        assert_eq!(
            pointer(keys.as_ref(), "default")
                .await
                .unwrap()
                .unwrap()
                .version,
            original.version
        );
    }

    #[tokio::test]
    async fn upgrade_keeps_unversioned_pinned_and_new_attachments_readable() {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        backend
            .secrets()
            .set_secret("default", request("s", "owner", false))
            .await
            .unwrap();
        let identity = age::x25519::Identity::generate();
        let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
        let original = keys
            .set_secret(
                "default",
                request(
                    key::ACTIVE_POINTER_SECRET,
                    identity.to_string().expose_secret(),
                    false,
                ),
            )
            .await
            .unwrap();
        let ciphertext = crypto::encrypt_bytes(b"oldest", &[identity.to_public()]).unwrap();
        files
            .upload_file("default", blob("attachments/s/oldest", ciphertext), None)
            .await
            .unwrap();
        attachments::upload_encrypted(
            keys.as_ref(),
            files,
            "default",
            blob("attachments/s/pinned", b"pinned".to_vec()),
            None,
        )
        .await
        .unwrap();

        let preview = upgrade(keys.as_ref(), "default", false).await.unwrap();
        assert_eq!(preview.outcome, "ready");
        assert_eq!(
            keys.get_secret("default", key::ACTIVE_POINTER_SECRET, false)
                .await
                .unwrap()
                .version,
            original.version,
            "preview must not change pointer"
        );
        assert!(keys
            .get_secret("default", &key::retained_record_name(&id), false)
            .await
            .is_err());
        let applied = upgrade(keys.as_ref(), "default", true).await.unwrap();
        assert_eq!(applied.outcome, "applied");
        assert_eq!(applied.active_key_id, id.as_str());
        assert_eq!(applied.legacy_key_id.as_deref(), Some(id.as_str()));
        for (name, expected) in [("oldest", "oldest"), ("pinned", "pinned")] {
            assert_eq!(
                attachments::download_decrypted(
                    keys.as_ref(),
                    files,
                    "default",
                    &format!("attachments/s/{name}"),
                    None
                )
                .await
                .unwrap(),
                expected.as_bytes()
            );
        }
        attachments::upload_encrypted(
            keys.as_ref(),
            files,
            "default",
            blob("attachments/s/new", b"new".to_vec()),
            None,
        )
        .await
        .unwrap();
        let info = files
            .get_file_info("default", "attachments/s/new")
            .await
            .unwrap();
        assert_eq!(info.metadata[key::META_KEY_SLOT], "retained");
        assert_eq!(
            attachments::download_decrypted(
                keys.as_ref(),
                files,
                "default",
                "attachments/s/new",
                None
            )
            .await
            .unwrap(),
            b"new"
        );
        let pointer = keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
            .await
            .unwrap();
        assert_eq!(
            upgrade(keys.as_ref(), "default", true)
                .await
                .unwrap()
                .outcome,
            "unchanged"
        );
        assert_eq!(
            keys.get_secret("default", key::ACTIVE_POINTER_SECRET, false)
                .await
                .unwrap()
                .version,
            pointer.version
        );
        assert_eq!(
            keys.get_secret_version(
                "default",
                key::ACTIVE_POINTER_SECRET,
                &original.version,
                true
            )
            .await
            .unwrap()
            .value
            .unwrap()
            .as_str(),
            identity.to_string().expose_secret()
        );
    }

    #[tokio::test]
    async fn upgrade_refuses_unmarked_retained_collision_without_writes() {
        let (_dir, backend) = fixture();
        let identity = age::x25519::Identity::generate();
        let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
        let original = backend
            .secrets()
            .set_secret(
                "default",
                request(
                    key::ACTIVE_POINTER_SECRET,
                    identity.to_string().expose_secret(),
                    false,
                ),
            )
            .await
            .unwrap();
        let name = key::retained_record_name(&id);
        backend
            .secrets()
            .set_secret("default", request(&name, "ordinary-user-secret", false))
            .await
            .unwrap();
        for apply in [false, true] {
            let error = upgrade(backend.attachment_keys().as_ref(), "default", apply)
                .await
                .unwrap_err();
            assert_eq!(error.code(), "xv-conflict");
            assert!(!format!("{error:?}").contains("ordinary-user-secret"));
        }
        assert_eq!(
            backend
                .secrets()
                .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
                .await
                .unwrap()
                .version,
            original.version
        );
        assert_eq!(
            backend
                .secrets()
                .get_secret("default", &name, true)
                .await
                .unwrap()
                .value
                .unwrap()
                .as_str(),
            "ordinary-user-secret"
        );
    }

    struct FaultKeys<'a> {
        inner: Box<dyn AttachmentKeyStore + 'a>,
        fail_publish: std::sync::atomic::AtomicBool,
        drift_at: Option<usize>,
        pointer_reads: std::sync::atomic::AtomicUsize,
        wrong_version: bool,
        deny_pointer_preflight: bool,
        deny_retained_preflight: bool,
    }

    #[async_trait::async_trait]
    impl AttachmentKeyStore for FaultKeys<'_> {
        async fn assert_complete_visibility(
            &self,
            vault: &str,
        ) -> std::result::Result<(), BackendError> {
            self.inner.assert_complete_visibility(vault).await
        }
        async fn list_retained_keys(
            &self,
            vault: &str,
        ) -> std::result::Result<
            Vec<crate::backend::attachment_keys::RetainedKeySummary>,
            BackendError,
        > {
            self.inner.list_retained_keys(vault).await
        }
        async fn preflight_set_secret(
            &self,
            vault: &str,
            name: &str,
        ) -> std::result::Result<(), BackendError> {
            if (self.deny_pointer_preflight && name == key::ACTIVE_POINTER_SECRET)
                || (self.deny_retained_preflight && name != key::ACTIVE_POINTER_SECRET)
            {
                return Err(BackendError::PermissionDenied(
                    "test custody preflight denied".into(),
                ));
            }
            self.inner.preflight_set_secret(vault, name).await
        }
        async fn get_secret(
            &self,
            vault: &str,
            name: &str,
            include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            if name == key::ACTIVE_POINTER_SECRET {
                let read = self
                    .pointer_reads
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.drift_at == Some(read) {
                    self.inner
                        .set_secret(vault, request(name, "concurrent-pointer", false))
                        .await?;
                }
            }
            self.inner.get_secret(vault, name, include_value).await
        }
        async fn get_secret_version(
            &self,
            vault: &str,
            name: &str,
            version: &str,
            include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            let mut props = self
                .inner
                .get_secret_version(vault, name, version, include_value)
                .await?;
            if self.wrong_version && name != key::ACTIVE_POINTER_SECRET {
                props.version = "different-version".into();
            }
            Ok(props)
        }
        async fn commit_retained_key(
            &self,
            vault: &str,
            req: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            self.inner.commit_retained_key(vault, req).await
        }
        async fn set_secret(
            &self,
            vault: &str,
            req: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            if self
                .fail_publish
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(BackendError::Network(
                    "interrupted before pointer write".into(),
                ));
            }
            self.inner.set_secret(vault, req).await
        }
    }

    fn faulty(
        backend: &LocalBackend,
        fail: bool,
        drift_at: Option<usize>,
        wrong_version: bool,
    ) -> FaultKeys<'_> {
        FaultKeys {
            inner: backend.attachment_keys(),
            fail_publish: std::sync::atomic::AtomicBool::new(fail),
            drift_at,
            pointer_reads: std::sync::atomic::AtomicUsize::new(0),
            wrong_version,
            deny_pointer_preflight: false,
            deny_retained_preflight: false,
        }
    }

    #[tokio::test]
    async fn initialize_preflights_pointer_and_retained_custody_before_any_write() {
        for deny_pointer in [false, true] {
            let (_dir, backend) = fixture();
            let mut keys = faulty(&backend, false, None, false);
            keys.deny_pointer_preflight = deny_pointer;
            keys.deny_retained_preflight = !deny_pointer;
            assert!(matches!(
                initialize(&keys, backend.files().unwrap(), "default", true).await,
                Err(CrosstacheError::PermissionDenied(_))
            ));
            assert!(pointer(keys.inner.as_ref(), "default")
                .await
                .unwrap()
                .is_none());
            assert!(keys
                .inner
                .list_retained_keys("default")
                .await
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test]
    async fn initialize_interruption_preserves_retained_custody_and_requires_explicit_recovery() {
        for wrong_version in [false, true] {
            let (_dir, backend) = fixture();
            let keys = faulty(&backend, !wrong_version, None, wrong_version);
            assert!(initialize(&keys, backend.files().unwrap(), "default", true)
                .await
                .is_err());
            assert!(pointer(keys.inner.as_ref(), "default")
                .await
                .unwrap()
                .is_none());
            assert_eq!(
                keys.inner
                    .list_retained_keys("default")
                    .await
                    .unwrap()
                    .len(),
                1
            );
            let error = initialize(
                keys.inner.as_ref(),
                backend.files().unwrap(),
                "default",
                true,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("recover"));
            assert_eq!(
                keys.inner
                    .list_retained_keys("default")
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
    }

    #[tokio::test]
    async fn interrupted_upgrade_reuses_verified_retained_key_on_retry() {
        let (_dir, backend) = fixture();
        let identity = age::x25519::Identity::generate();
        let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
        backend
            .attachment_keys()
            .set_secret(
                "default",
                request(
                    key::ACTIVE_POINTER_SECRET,
                    identity.to_string().expose_secret(),
                    false,
                ),
            )
            .await
            .unwrap();
        let failing = faulty(&backend, true, None, false);
        assert_eq!(
            upgrade(&failing, "default", true).await.unwrap_err().code(),
            "xv-network"
        );
        let name = key::retained_record_name(&id);
        let committed = backend
            .attachment_keys()
            .get_secret("default", &name, false)
            .await
            .unwrap();
        assert_eq!(
            backend
                .attachment_keys()
                .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
                .await
                .unwrap()
                .value
                .unwrap()
                .as_str(),
            identity.to_string().expose_secret()
        );
        assert_eq!(
            upgrade(&failing, "default", true).await.unwrap().outcome,
            "applied"
        );
        assert_eq!(
            backend
                .attachment_keys()
                .get_secret("default", &name, false)
                .await
                .unwrap()
                .version,
            committed.version
        );
        assert_eq!(
            backend
                .secrets()
                .list_versions("default", &name)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn upgrade_refuses_changed_pointer_and_wrong_exact_version() {
        for (drift, wrong_version, expected) in [
            (Some(1), false, "xv-conflict"),
            (Some(2), false, "xv-attachment-commit-unconfirmed"),
            (None, true, "xv-attachment-key-version-invalid"),
        ] {
            let (_dir, backend) = fixture();
            let identity = age::x25519::Identity::generate();
            let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
            let original = backend
                .attachment_keys()
                .set_secret(
                    "default",
                    request(
                        key::ACTIVE_POINTER_SECRET,
                        identity.to_string().expose_secret(),
                        false,
                    ),
                )
                .await
                .unwrap();
            let injected = faulty(&backend, false, drift, wrong_version);
            assert_eq!(
                upgrade(&injected, "default", true)
                    .await
                    .unwrap_err()
                    .code(),
                expected
            );
            assert!(
                backend
                    .attachment_keys()
                    .get_secret("default", &key::retained_record_name(&id), false)
                    .await
                    .is_ok(),
                "a failed publication never removes committed custody"
            );
            if wrong_version {
                assert_eq!(
                    backend
                        .attachment_keys()
                        .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
                        .await
                        .unwrap()
                        .version,
                    original.version
                );
            }
        }
    }

    #[tokio::test]
    async fn recovery_repairs_missing_pointer_but_refuses_healthy_rotation_or_legacy_change() {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let old = age::x25519::Identity::generate();
        let id = key::AttachmentKeyId::derive(&old.to_public().to_string());
        let other = age::x25519::Identity::generate();
        let other_id = key::AttachmentKeyId::derive(&other.to_public().to_string());
        for (id, identity) in [(&id, &old), (&other_id, &other)] {
            keys.commit_retained_key(
                "default",
                request(
                    &key::retained_record_name(id),
                    identity.to_string().expose_secret(),
                    true,
                ),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            recover(keys.as_ref(), "default", &id, Some(&id), false)
                .await
                .unwrap()
                .outcome,
            "ready"
        );
        assert!(keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
            .await
            .is_err());
        assert_eq!(
            recover(keys.as_ref(), "default", &id, Some(&id), true)
                .await
                .unwrap()
                .outcome,
            "applied"
        );
        let original = keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
            .await
            .unwrap();
        assert_eq!(
            recover(keys.as_ref(), "default", &id, Some(&id), true)
                .await
                .unwrap()
                .outcome,
            "unchanged"
        );
        assert_eq!(
            recover(keys.as_ref(), "default", &other_id, Some(&id), true)
                .await
                .unwrap_err()
                .code(),
            "xv-conflict"
        );
        assert_eq!(
            recover(keys.as_ref(), "default", &id, None, true)
                .await
                .unwrap_err()
                .code(),
            "xv-conflict"
        );
        assert_eq!(
            keys.get_secret("default", key::ACTIVE_POINTER_SECRET, false)
                .await
                .unwrap()
                .version,
            original.version
        );
        keys.set_secret(
            "default",
            request(
                key::ACTIVE_POINTER_SECRET,
                old.to_string().expose_secret(),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            recover(keys.as_ref(), "default", &id, Some(&id), true)
                .await
                .unwrap_err()
                .code(),
            "xv-conflict"
        );
    }

    #[tokio::test]
    async fn recovery_requires_valid_marked_selected_keys_before_any_write() {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let id = key::AttachmentKeyId::derive(
            &age::x25519::Identity::generate().to_public().to_string(),
        );
        assert_eq!(
            recover(keys.as_ref(), "default", &id, None, true)
                .await
                .unwrap_err()
                .code(),
            "xv-attachment-key-missing"
        );
        backend
            .secrets()
            .set_secret(
                "default",
                request(&key::retained_record_name(&id), "user-value", false),
            )
            .await
            .unwrap();
        assert_eq!(
            recover(keys.as_ref(), "default", &id, None, true)
                .await
                .unwrap_err()
                .code(),
            "xv-conflict"
        );
        assert!(keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn preschema_download_uses_only_explicit_legacy_binding() {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        backend
            .secrets()
            .set_secret("default", request("s", "owner", false))
            .await
            .unwrap();
        let legacy = age::x25519::Identity::generate();
        let active = age::x25519::Identity::generate();
        let legacy_id = key::AttachmentKeyId::derive(&legacy.to_public().to_string());
        let active_id = key::AttachmentKeyId::derive(&active.to_public().to_string());
        for (id, identity) in [(&legacy_id, &legacy), (&active_id, &active)] {
            keys.commit_retained_key(
                "default",
                request(
                    &key::retained_record_name(id),
                    identity.to_string().expose_secret(),
                    true,
                ),
            )
            .await
            .unwrap();
        }
        files
            .upload_file(
                "default",
                blob(
                    "attachments/s/old",
                    crypto::encrypt_bytes(b"original", &[legacy.to_public()]).unwrap(),
                ),
                None,
            )
            .await
            .unwrap();
        keys.set_secret(
            "default",
            request(
                key::ACTIVE_POINTER_SECRET,
                &key::format_v2_pointer(&active_id, Some(&legacy_id)),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            attachments::download_decrypted(
                keys.as_ref(),
                files,
                "default",
                "attachments/s/old",
                None
            )
            .await
            .unwrap(),
            b"original"
        );
        // Even a matching active identity is never an implicit legacy fallback.
        keys.set_secret(
            "default",
            request(
                key::ACTIVE_POINTER_SECRET,
                &key::format_v2_pointer(&legacy_id, None),
                false,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            attachments::download_decrypted(
                keys.as_ref(),
                files,
                "default",
                "attachments/s/old",
                None
            )
            .await
            .unwrap_err()
            .code(),
            "xv-attachment-key-missing"
        );
        keys.set_secret(
            "default",
            request(
                key::ACTIVE_POINTER_SECRET,
                &key::format_v2_pointer(&active_id, Some(&legacy_id)),
                false,
            ),
        )
        .await
        .unwrap();
        backend
            .secrets()
            .set_secret(
                "default",
                request(
                    &key::retained_record_name(&legacy_id),
                    active.to_string().expose_secret(),
                    true,
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            attachments::download_decrypted(
                keys.as_ref(),
                files,
                "default",
                "attachments/s/old",
                None
            )
            .await
            .unwrap_err()
            .code(),
            "xv-attachment-key-mismatch"
        );
    }
}
