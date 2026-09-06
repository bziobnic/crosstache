use super::*;
use crate::backend::local::LocalBackend;
use crate::config::settings::LocalConfig;
use crate::secret::manager::SecretRequest;
use std::collections::HashMap;
fn intent() -> TransferIntent {
    use transfer::TransferEndpoint;
    let endpoint = TransferEndpoint {
        identity: "local:a".into(),
        vault: "default".into(),
    };
    TransferIntent {
        source: endpoint.clone(),
        destination: endpoint,
        source_name: "db".into(),
        destination_name: "db-new".into(),
        operation: TransferOperation::Move,
        destination_key_id: None,
    }
}
async fn fixture() -> (tempfile::TempDir, LocalBackend, RecoveryStore) {
    let dir = tempfile::tempdir().unwrap();
    let b = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(dir.path().join("store").display().to_string()),
        key_file: Some(dir.path().join("key").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    b.guarded_secrets()
        .set_secret(
            "default",
            SecretRequest {
                name: "db".into(),
                value: Zeroizing::new("secret-value-canary".into()),
                content_type: Some("text/plain".into()),
                enabled: Some(false),
                expires_on: None,
                not_before: None,
                tags: Some(HashMap::from([(
                    "label".into(),
                    "secret-metadata-canary".into(),
                )])),
                groups: Some(vec!["engineering".into()]),
                note: Some("retained note".into()),
                folder: Some("folder".into()),
            },
        )
        .await
        .unwrap();
    for suffix in ["first", "nested/second"] {
        crate::secret::attachments::upload_encrypted(
            b.attachment_keys().as_ref(),
            b.files().unwrap(),
            "default",
            crate::blob::models::FileUploadRequest {
                name: format!("attachments/db/{suffix}"),
                content: b"attachment-plaintext-canary".to_vec(),
                content_type: Some("text/custom".into()),
                groups: vec!["group".into()],
                metadata: HashMap::from([("custom".into(), "metadata-canary".into())]),
                tags: HashMap::from([("tag".into(), "tag-canary".into())]),
            },
            None,
        )
        .await
        .unwrap();
    }
    let store = RecoveryStore::new(dir.path().join("recovery"));
    (dir, b, store)
}
#[tokio::test]
async fn moves_exact_ciphertext_metadata_and_secret_semantics() {
    let (_dir, b, recovery) = fixture().await;
    let before = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db/first")
        .await
        .unwrap();
    let report = apply(&b, &b, intent(), true, &recovery).await.unwrap();
    assert!(report.complete);
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .is_err());
    let dest = b
        .guarded_secrets()
        .get_secret("default", "db-new", true)
        .await
        .unwrap();
    assert_eq!(
        dest.value.as_deref().map(|s| s.as_str()),
        Some("secret-value-canary")
    );
    assert!(!dest.enabled);
    assert_eq!(
        dest.tags.get("groups").map(String::as_str),
        Some("engineering")
    );
    let after = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db-new/first")
        .await
        .unwrap();
    assert_eq!(before.data.content, after.data.content);
    assert_eq!(before.info.metadata, after.info.metadata);
    assert_eq!(before.info.tags, after.info.tags);
    assert_eq!(before.info.groups, after.info.groups);
    assert_eq!(before.info.content_type, after.info.content_type);
    assert!(
        resume(&b, &b, intent(), &report.id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
    let listing = recovery.list().unwrap();
    assert_eq!(listing.len(), 1);
    assert!(listing[0].complete);
    for entry in std::fs::read_dir(&recovery.root).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        for marker in [
            "secret-value-canary",
            "secret-metadata-canary",
            "attachment-plaintext-canary",
            "metadata-canary",
            "tag-canary",
        ] {
            assert!(!bytes.windows(marker.len()).any(|w| w == marker.as_bytes()));
        }
    }
}
#[tokio::test]
async fn restarts_after_every_durable_and_provider_boundary() {
    // Includes pending-before-write, committed-create lost response, partially
    // completed cleanup, final-delete lost response, and completion persistence.
    let mut interruptions = 0;
    for index in 0..50 {
        let (_dir, b, recovery) = fixture().await;
        FAIL_AT.with(|f| f.set(Some(index)));
        let result = apply(&b, &b, intent(), true, &recovery).await;
        FAIL_AT.with(|f| f.set(None));
        if result.is_ok() {
            break;
        }
        interruptions += 1;
        let summaries = recovery.list().unwrap();
        assert_eq!(summaries.len(), 1, "boundary {index}");
        let restarted = RecoveryStore::new(recovery.root.clone());
        assert!(
            resume(&b, &b, intent(), &summaries[0].id, true, &restarted)
                .await
                .unwrap_or_else(|e| panic!("boundary {index}: {e}"))
                .complete
        );
    }
    assert!(interruptions >= 20, "all mutation boundaries exercised");
}
#[tokio::test]
async fn offline_is_required_without_creating_recovery_root() {
    let (_dir, b, recovery) = fixture().await;
    assert!(preview(&b, &b, intent()).await.unwrap().execution_supported);
    assert!(!recovery.root.exists());
    assert!(apply(&b, &b, intent(), false, &recovery).await.is_err());
    assert!(!recovery.root.exists());
}
#[tokio::test]
async fn altered_journal_and_source_drift_are_rejected_before_destination_write() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    let id = recovery.list().unwrap()[0].id.clone();
    let snapshot = b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .unwrap();
    let mut request = rename_request_from_properties("db", &snapshot).unwrap();
    request.value = Zeroizing::new("changed".into());
    b.guarded_secrets()
        .set_secret("default", request)
        .await
        .unwrap();
    assert!(resume(&b, &b, intent(), &id, true, &recovery)
        .await
        .is_err());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", true)
        .await
        .is_err());
    let path = recovery.root.join(format!("{id}.age"));
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(resume(&b, &b, intent(), &id, true, &recovery)
        .await
        .is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn recovery_rejects_symlink_root_identity_and_public_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (dir, b, recovery) = fixture().await;
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, &recovery.root).unwrap();
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    std::fs::remove_file(&recovery.root).unwrap();
    std::fs::create_dir(&recovery.root).unwrap();
    std::fs::set_permissions(&recovery.root, std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink(dir.path().join("key"), recovery.root.join("identity")).unwrap();
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    std::fs::remove_file(recovery.root.join("identity")).unwrap();
    std::fs::set_permissions(&recovery.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
}
#[tokio::test]
async fn verified_destination_generation_change_blocks_source_cleanup() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(13)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    let id = recovery.list().unwrap()[0].id.clone();
    let old = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db-new/first")
        .await
        .unwrap();
    b.files()
        .unwrap()
        .restore_file(
            "default",
            crate::blob::models::FileUploadRequest {
                name: old.info.name,
                content: old.data.content,
                content_type: Some(old.info.content_type),
                groups: old.info.groups,
                metadata: old.info.metadata,
                tags: old.info.tags,
            },
        )
        .await
        .unwrap();
    assert!(resume(&b, &b, intent(), &id, true, &recovery)
        .await
        .is_err());
    assert_eq!(b.attachment_names("default", "db").await.unwrap().len(), 2);
}
#[tokio::test]
async fn new_source_attachment_and_unexplained_absence_block_cleanup() {
    for add in [false, true] {
        let (_dir, b, recovery) = fixture().await;
        FAIL_AT.with(|f| f.set(Some(13)));
        assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
        let id = recovery.list().unwrap()[0].id.clone();
        if add {
            b.files()
                .unwrap()
                .upload_file(
                    "default",
                    crate::blob::models::FileUploadRequest {
                        name: "attachments/db/unexpected".into(),
                        content: b"new".to_vec(),
                        content_type: None,
                        groups: vec![],
                        metadata: HashMap::new(),
                        tags: HashMap::new(),
                    },
                    None,
                )
                .await
                .unwrap();
        } else {
            b.files()
                .unwrap()
                .delete_file("default", "attachments/db/first")
                .await
                .unwrap();
        }
        assert!(resume(&b, &b, intent(), &id, true, &recovery)
            .await
            .is_err());
        assert!(b
            .guarded_secrets()
            .get_secret("default", "db", false)
            .await
            .is_ok());
        assert!(b
            .files()
            .unwrap()
            .get_file_info("default", "attachments/db/nested/second")
            .await
            .is_ok());
    }
}
#[tokio::test]
async fn recreated_source_after_final_delete_is_never_removed() {
    let (_dir, b, recovery) = fixture().await;
    let original = b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .unwrap();
    FAIL_AT.with(|f| f.set(Some(24)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    let id = recovery.list().unwrap()[0].id.clone();
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db", false)
        .await
        .is_err());
    b.guarded_secrets()
        .create_secret_if_absent(
            "default",
            rename_request_from_properties("db", &original).unwrap(),
        )
        .await
        .unwrap();
    assert!(resume(&b, &b, intent(), &id, true, &recovery)
        .await
        .is_err());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .is_ok());
}
#[tokio::test]
async fn unreadable_current_key_blocks_cleanup_even_with_pinned_ciphertext() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(13)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    let id = recovery.list().unwrap()[0].id.clone();
    let journal = {
        let session = storage::Session::open(&recovery.root, false).unwrap();
        load(&session, &id).unwrap()
    };
    let key_id = super::super::attachment_key::AttachmentKeyId::parse(
        &journal.plan.files[0].source_key.key_id,
    )
    .unwrap();
    let name = super::super::attachment_key::retained_record_name(&key_id);
    let props = b
        .secrets()
        .get_secret("default", &name, true)
        .await
        .unwrap();
    let mut request = rename_request_from_properties(&name, &props).unwrap();
    request.enabled = Some(false);
    b.secrets().set_secret("default", request).await.unwrap();
    assert!(resume(&b, &b, intent(), &id, true, &recovery)
        .await
        .is_err());
    assert_eq!(b.attachment_names("default", "db").await.unwrap().len(), 2);
}
#[test]
fn schema_two_authentication_rejects_schema_one_and_forged_recipient_ciphertext() {
    let identity = age::x25519::Identity::generate();
    let bytes =
        crate::backend::local::crypto::encrypt_bytes(b"{}", &[identity.to_public()]).unwrap();
    let mut forged = MAGIC.to_vec();
    forged.extend([0u8; 32]);
    forged.extend(bytes);
    assert!(decode(&forged, &identity).is_err());
    let old = TransferPlan {
        schema_version: 1,
        intent: intent(),
        source_version: "v1".into(),
        files: vec![],
        destination_key: None,
        execution_supported: false,
    };
    assert!(decode(&transfer::encode(&old, &identity).unwrap(), &identity).is_err());
}
#[tokio::test]
async fn recovery_lock_serializes_sessions_and_missing_identity_never_regenerates() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    let id = recovery.list().unwrap()[0].id.clone();
    {
        let _held = storage::Session::open(&recovery.root, false).unwrap();
        assert!(storage::Session::open(&recovery.root, false).is_err());
        assert!(resume(&b, &b, intent(), &id, true, &recovery)
            .await
            .is_err());
    }
    std::fs::remove_file(recovery.root.join("identity")).unwrap();
    assert!(storage::Session::open(&recovery.root, true).is_err());
    assert!(!recovery.root.join("identity").exists());
}

#[tokio::test]
async fn parent_component_recovery_path_applies_lists_and_resumes() {
    let (dir, backend, _) = fixture().await;
    let recovery = RecoveryStore::new(dir.path().join("never-created/../recovery"));
    let report = apply(&backend, &backend, intent(), true, &recovery)
        .await
        .unwrap();
    assert!(report.complete);
    assert!(!dir.path().join("never-created").exists());
    let entries = recovery.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, report.id);
    assert!(
        resume(&backend, &backend, intent(), &report.id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
}
