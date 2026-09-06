use super::*;
use crate::backend::{local::LocalBackend, Backend};
use crate::blob::models::FileUploadRequest;
use crate::config::settings::LocalConfig;
use crate::secret::{attachment_backup_codec as codec, attachment_key as key, attachments};
use std::collections::HashMap;

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

#[tokio::test]
async fn collects_verified_ciphertext_manifest_and_encrypted_ring() {
    let (_dir, backend) = fixture();
    let files = backend.files().unwrap();
    let keys = backend.attachment_keys();
    attachments::upload_encrypted(
        keys.as_ref(),
        files,
        "default",
        FileUploadRequest {
            name: "private.txt".into(),
            content: b"classified content".to_vec(),
            content_type: Some("text/plain".into()),
            groups: vec![],
            metadata: HashMap::new(),
            tags: HashMap::new(),
        },
        None,
    )
    .await
    .unwrap();
    let bundle = collect(keys.as_ref(), files, "local", "default")
        .await
        .unwrap();
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.identities.len(), 1);
    assert_eq!(bundle.files[0].key_id, bundle.active_key_id);
    assert!(bundle.files[0].source_ref.is_some());
    let recovery = age::x25519::Identity::generate();
    let encrypted = codec::encrypt(&bundle, &recovery.to_public()).unwrap();
    assert!(!String::from_utf8_lossy(&encrypted).contains("AGE-SECRET-KEY"));
    assert!(!String::from_utf8_lossy(&encrypted).contains("classified content"));
    let decoded = codec::decrypt(&encrypted, &recovery).unwrap();
    assert_eq!(decoded.active_key_id, bundle.active_key_id);
    assert_eq!(
        decoded.files[0].ciphertext_sha256,
        bundle.files[0].ciphertext_sha256
    );
    assert!(key::AttachmentKeyId::parse(&bundle.active_key_id).is_some());
}

#[tokio::test]
async fn export_of_missing_pointer_does_not_initialize_keys() {
    let (_dir, backend) = fixture();
    assert!(collect(
        backend.attachment_keys().as_ref(),
        backend.files().unwrap(),
        "local",
        "default"
    )
    .await
    .is_err());
    assert!(backend
        .attachment_keys()
        .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
        .await
        .is_err());
}

#[tokio::test]
async fn export_canonicalizes_whitespace_identities_without_changing_source() {
    use crate::secret::manager::SecretRequest;
    use age::secrecy::ExposeSecret;
    let (_dir, backend) = fixture();
    let identity = age::x25519::Identity::generate();
    let canonical = identity.to_string();
    let raw = format!("\n{}\n", canonical.expose_secret());
    let keys = backend.attachment_keys();
    keys.set_secret(
        "default",
        SecretRequest {
            name: key::ACTIVE_POINTER_SECRET.into(),
            value: zeroize::Zeroizing::new(raw.clone()),
            enabled: Some(true),
            content_type: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        },
    )
    .await
    .unwrap();
    let bundle = collect(keys.as_ref(), backend.files().unwrap(), "local", "default")
        .await
        .unwrap();
    assert_eq!(
        bundle.identities[0].identity.as_str(),
        canonical.expose_secret()
    );
    assert_eq!(
        keys.get_secret("default", key::ACTIVE_POINTER_SECRET, true)
            .await
            .unwrap()
            .value
            .unwrap()
            .as_str(),
        raw
    );
}

fn upload(name: &str, content: Vec<u8>) -> FileUploadRequest {
    FileUploadRequest {
        name: name.into(),
        content,
        content_type: Some("text/plain".into()),
        groups: vec!["group".into()],
        metadata: HashMap::from([("note".into(), "preserved".into())]),
        tags: HashMap::from([("owner".into(), "user".into())]),
    }
}

#[tokio::test]
async fn source_v1_history_and_v2_current_files_restore_with_new_versions() {
    use crate::backend::local::crypto;
    use crate::secret::{attachment_lifecycle, attachment_restore, manager::SecretRequest};
    use age::secrecy::ExposeSecret;
    let (_source_dir, source) = fixture();
    let (_target_dir, target) = fixture();
    let source_keys = source.attachment_keys();
    let source_files = source.files().unwrap();
    let target_files = target.files().unwrap();
    let original = age::x25519::Identity::generate();
    let req = |name: &str, identity: &age::x25519::Identity, marked: bool| SecretRequest {
        name: name.into(),
        value: zeroize::Zeroizing::new(identity.to_string().expose_secret().clone()),
        content_type: marked.then(|| key::KEY_RECORD_CONTENT_TYPE.into()),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    };
    for backend in [&source, &target] {
        let mut owner = req("s", &original, false);
        owner.value = zeroize::Zeroizing::new("owner".into());
        backend
            .secrets()
            .set_secret("default", owner)
            .await
            .unwrap();
    }
    let original_pointer = source_keys
        .set_secret("default", req(key::ACTIVE_POINTER_SECRET, &original, false))
        .await
        .unwrap();
    source_files
        .upload_file(
            "default",
            upload(
                "attachments/s/oldest",
                crypto::encrypt_bytes(b"oldest", &[original.to_public()]).unwrap(),
            ),
            None,
        )
        .await
        .unwrap();
    attachments::upload_encrypted(
        source_keys.as_ref(),
        source_files,
        "default",
        upload("attachments/s/legacy-pin", b"legacy-pin".to_vec()),
        None,
    )
    .await
    .unwrap();
    attachment_lifecycle::upgrade(source_keys.as_ref(), "default", true)
        .await
        .unwrap();
    attachments::upload_encrypted(
        source_keys.as_ref(),
        source_files,
        "default",
        upload("current", b"current".to_vec()),
        None,
    )
    .await
    .unwrap();
    let extra = age::x25519::Identity::generate();
    let extra_id = key::AttachmentKeyId::derive(&extra.to_public().to_string());
    source_keys
        .commit_retained_key(
            "default",
            req(&key::retained_record_name(&extra_id), &extra, true),
        )
        .await
        .unwrap();
    let source_pointer = source_keys
        .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
        .await
        .unwrap();
    let bundle = collect(source_keys.as_ref(), source_files, "local", "default")
        .await
        .unwrap();
    assert_eq!(
        bundle.identities.len(),
        2,
        "unreferenced marked keys are included"
    );
    assert_eq!(bundle.files.len(), 3);
    assert!(bundle
        .references
        .iter()
        .any(|r| r.slot == "legacy" && r.provider_version == original_pointer.version));
    assert!(bundle.files.iter().any(|f| f.source_ref.is_none()));
    for f in &bundle.files {
        let snapshot = source_files
            .download_file_snapshot("default", &f.name, None)
            .await
            .unwrap();
        let info = source_files
            .get_file_info("default", &f.name)
            .await
            .unwrap();
        target_files
            .upload_file(
                "default",
                FileUploadRequest {
                    name: f.name.clone(),
                    content: snapshot.content,
                    metadata: snapshot.metadata,
                    content_type: Some(info.content_type),
                    groups: info.groups,
                    tags: info.tags,
                },
                None,
            )
            .await
            .unwrap();
    }
    let recovery = age::x25519::Identity::generate();
    let bundle = codec::decrypt(
        &codec::encrypt(&bundle, &recovery.to_public()).unwrap(),
        &recovery,
    )
    .unwrap();
    let report = attachment_restore::restore(
        target.attachment_keys().as_ref(),
        target_files,
        "default",
        &bundle,
        false,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.outcome, "ready");
    assert!(target
        .attachment_keys()
        .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
        .await
        .is_err());
    attachment_restore::restore(
        target.attachment_keys().as_ref(),
        target_files,
        "default",
        &bundle,
        true,
        false,
    )
    .await
    .unwrap();
    for (name, expected) in [
        ("attachments/s/oldest", "oldest"),
        ("attachments/s/legacy-pin", "legacy-pin"),
        ("current", "current"),
    ] {
        assert_eq!(
            attachments::download_decrypted(
                target.attachment_keys().as_ref(),
                target_files,
                "default",
                name,
                None
            )
            .await
            .unwrap(),
            expected.as_bytes()
        );
        let src = source_files
            .download_file_snapshot("default", name, None)
            .await
            .unwrap();
        let dst = target_files
            .download_file_snapshot("default", name, None)
            .await
            .unwrap();
        assert_eq!(src.content, dst.content);
        assert_eq!(dst.metadata[key::META_KEY_SLOT], "retained");
        assert_eq!(dst.metadata["note"], "preserved");
    }
    let pointer_after = source_keys
        .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
        .await
        .unwrap();
    assert_eq!(source_pointer.version, pointer_after.version);
    assert_eq!(source_pointer.value, pointer_after.value);
    attachments::upload_encrypted(
        target.attachment_keys().as_ref(),
        target_files,
        "default",
        upload("new", b"after restore".to_vec()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        target_files
            .get_file_info("default", "new")
            .await
            .unwrap()
            .metadata[key::META_KEY_ID],
        bundle.active_key_id
    );
}

#[tokio::test]
async fn export_rejects_partial_crypto_reference() {
    let (_dir, backend) = fixture();
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    attachments::upload_encrypted(
        keys.as_ref(),
        files,
        "default",
        upload("bad", b"data".to_vec()),
        None,
    )
    .await
    .unwrap();
    let mut snapshot = files
        .download_file_snapshot("default", "bad", None)
        .await
        .unwrap();
    snapshot.metadata.remove(key::META_CRYPTO_SCHEMA);
    let mut req = upload("bad", snapshot.content);
    req.metadata = snapshot.metadata;
    files.upload_file("default", req, None).await.unwrap();
    assert!(collect(keys.as_ref(), files, "local", "default")
        .await
        .is_err());
}
