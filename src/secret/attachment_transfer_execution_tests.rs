use super::*;
use crate::backend::local::LocalBackend;
use crate::config::settings::LocalConfig;
use crate::secret::domain::SecretRequest;
use crate::secret::domain::SecretValue;
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
        destination_folder: None,
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
                value: SecretValue::new("secret-value-canary"),
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
        dest.value.as_ref().map(SecretValue::expose_secret),
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
    request.value = SecretValue::new("changed");
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

#[tokio::test]
async fn copy_preserves_source_and_completed_resume() {
    let (_dir, b, recovery) = fixture().await;
    let mut copy = intent();
    copy.operation = TransferOperation::Copy;
    let original = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db/first")
        .await
        .unwrap();
    let report = apply(&b, &b, copy.clone(), true, &recovery).await.unwrap();
    assert!(report.complete);
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .is_ok());
    let copied = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db-new/first")
        .await
        .unwrap();
    assert_eq!(original.data.content, copied.data.content);
    assert!(
        resume(&b, &b, copy, &report.id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
}

#[tokio::test]
async fn cross_key_copy_and_move_authenticate_under_destination_key() {
    for operation in [TransferOperation::Copy, TransferOperation::Move] {
        let (_source_dir, source, recovery) = fixture().await;
        let (_destination_dir, destination, _) = fixture().await;
        let pointer = destination
            .attachment_keys()
            .get_secret(
                "default",
                super::super::attachment_key::ACTIVE_POINTER_SECRET,
                true,
            )
            .await
            .unwrap();
        let active = match super::super::attachment_key::parse_pointer_value(
            pointer
                .value
                .as_ref()
                .map(SecretValue::expose_secret)
                .unwrap(),
        ) {
            Some(super::super::attachment_key::PointerKind::V2 { active, .. }) => active,
            _ => panic!("healthy fixture"),
        };
        let mut cross = intent();
        cross.destination.identity = "local:b".into();
        cross.destination_key_id = Some(active.as_str().into());
        cross.operation = operation.clone();
        let before = rewrap::snapshot(source.files().unwrap(), "default", "attachments/db/first")
            .await
            .unwrap();
        let report = apply(&source, &destination, cross.clone(), true, &recovery)
            .await
            .unwrap();
        let after = rewrap::snapshot(
            destination.files().unwrap(),
            "default",
            "attachments/db-new/first",
        )
        .await
        .unwrap();
        assert_ne!(before.data.content, after.data.content);
        let reference =
            super::super::attachment_key::parse_key_ref_from_metadata(&after.info.metadata)
                .unwrap();
        assert_eq!(reference.key_id, active);
        assert_eq!(
            &*rewrap::authenticate(
                destination.attachment_keys().as_ref(),
                "default",
                &reference,
                &after
            )
            .await
            .unwrap(),
            b"attachment-plaintext-canary"
        );
        assert_eq!(
            source
                .guarded_secrets()
                .get_secret("default", "db", true)
                .await
                .is_ok(),
            operation == TransferOperation::Copy
        );
        assert!(
            resume(&source, &destination, cross, &report.id, true, &recovery)
                .await
                .unwrap()
                .complete
        );
    }
}

#[tokio::test]
async fn distinct_logical_aliases_refuse_same_physical_keyspace_without_writes() {
    let (_dir, b, recovery) = fixture().await;
    let mut alias = intent();
    alias.operation = TransferOperation::Copy;
    alias.destination.identity = "another-alias".into();
    assert!(apply(&b, &b, alias, true, &recovery).await.is_err());
    assert!(!recovery.root.exists());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    assert_eq!(b.attachment_names("default", "db").await.unwrap().len(), 2);
}

#[tokio::test]
async fn cross_key_restarts_after_pending_and_lost_upload_responses() {
    let mut interruptions = 0;
    for index in 0..45 {
        let (_source_dir, source, recovery) = fixture().await;
        let (_destination_dir, destination, _) = fixture().await;
        let pointer = destination
            .attachment_keys()
            .get_secret(
                "default",
                super::super::attachment_key::ACTIVE_POINTER_SECRET,
                true,
            )
            .await
            .unwrap();
        let active = match super::super::attachment_key::parse_pointer_value(
            pointer
                .value
                .as_ref()
                .map(SecretValue::expose_secret)
                .unwrap(),
        ) {
            Some(super::super::attachment_key::PointerKind::V2 { active, .. }) => active,
            _ => panic!("healthy fixture"),
        };
        let mut cross = intent();
        cross.destination.identity = "local:b".into();
        cross.destination_key_id = Some(active.as_str().into());
        cross.operation = TransferOperation::Copy;
        FAIL_AT.with(|f| f.set(Some(index)));
        let result = apply(&source, &destination, cross.clone(), true, &recovery).await;
        FAIL_AT.with(|f| f.set(None));
        if result.is_ok() {
            break;
        }
        interruptions += 1;
        let entries = recovery.list().unwrap();
        assert_eq!(entries.len(), 1, "boundary {index}");
        assert!(
            resume(
                &source,
                &destination,
                cross,
                &entries[0].id,
                true,
                &recovery
            )
            .await
            .unwrap_or_else(|e| panic!("boundary {index}: {e}"))
            .complete
        );
        assert_eq!(
            source
                .attachment_names("default", "db")
                .await
                .unwrap()
                .len(),
            2
        );
    }
    assert!(interruptions >= 12);
}

#[tokio::test]
async fn destination_pointer_republication_blocks_cross_key_resume() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let key = super::super::attachment_key::ACTIVE_POINTER_SECRET;
    let pointer = destination
        .attachment_keys()
        .get_secret("default", key, true)
        .await
        .unwrap();
    let active = match super::super::attachment_key::parse_pointer_value(
        pointer
            .value
            .as_ref()
            .map(SecretValue::expose_secret)
            .unwrap(),
    ) {
        Some(super::super::attachment_key::PointerKind::V2 { active, .. }) => active,
        _ => panic!("healthy fixture"),
    };
    let mut cross = intent();
    cross.destination.identity = "local:b".into();
    cross.destination_key_id = Some(active.as_str().into());
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&source, &destination, cross.clone(), true, &recovery)
        .await
        .is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    let temporary = age::x25519::Identity::generate();
    let temporary_id =
        super::super::attachment_key::AttachmentKeyId::derive(&temporary.to_public().to_string());
    let mut away = rename_request_from_properties(key, &pointer).unwrap();
    away.value = SecretValue::new(super::super::attachment_key::format_v2_pointer(
        &temporary_id,
        None,
    ));
    destination
        .secrets()
        .set_secret("default", away)
        .await
        .unwrap();
    destination
        .secrets()
        .set_secret(
            "default",
            rename_request_from_properties(key, &pointer).unwrap(),
        )
        .await
        .unwrap();
    assert!(resume(&source, &destination, cross, &id, true, &recovery)
        .await
        .is_err());
    assert!(destination
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn folder_override_is_durable_and_wrong_resume_intent_refuses() {
    for folder in ["/", "team/database"] {
        let (_dir, b, recovery) = fixture().await;
        let mut value = serde_json::to_value(intent()).unwrap();
        value["destination_folder"] = folder.into();
        let expected: TransferIntent = serde_json::from_value(value).unwrap();
        FAIL_AT.with(|f| f.set(Some(0)));
        assert!(apply(&b, &b, expected.clone(), true, &recovery)
            .await
            .is_err());
        FAIL_AT.with(|f| f.set(None));
        let id = recovery.list().unwrap()[0].id.clone();
        assert!(resume(&b, &b, intent(), &id, true, &recovery)
            .await
            .is_err());
        assert!(
            resume(&b, &b, expected, &id, true, &recovery)
                .await
                .unwrap()
                .complete
        );
        let destination = b
            .guarded_secrets()
            .get_secret("default", "db-new", true)
            .await
            .unwrap();
        assert_eq!(
            destination.tags.get("folder").map(String::as_str),
            if folder == "/" { None } else { Some(folder) }
        );
    }
}

#[tokio::test]
async fn fresh_destination_namespace_is_preflighted_read_only_and_recovered() {
    for boundary_index in 0..5 {
        let (_source_dir, source, recovery) = fixture().await;
        let destination_dir = tempfile::tempdir().unwrap();
        let destination = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(destination_dir.path().join("store").display().to_string()),
            key_file: Some(destination_dir.path().join("key").display().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        }))
        .unwrap();
        let initialized = super::super::attachment_lifecycle::initialize(
            destination.attachment_keys().as_ref(),
            destination.files().unwrap(),
            "default",
            true,
        )
        .await
        .unwrap();
        let before = destination.transfer_location("default").await.unwrap();
        assert!(before.files.starts_with("local-pending-files:"));
        let mut cross = intent();
        cross.destination.identity = "local:b".into();
        cross.destination_key_id = initialized.active_key_id;
        cross.operation = TransferOperation::Copy;
        assert!(
            preflight(&source, &destination, cross.clone())
                .await
                .unwrap()
                .execution_supported
        );
        assert_eq!(
            destination.transfer_location("default").await.unwrap(),
            before
        );
        assert!(!recovery.root.exists());
        FAIL_AT.with(|f| f.set(Some(boundary_index)));
        assert!(apply(&source, &destination, cross.clone(), true, &recovery)
            .await
            .is_err());
        FAIL_AT.with(|f| f.set(None));
        let id = recovery.list().unwrap()[0].id.clone();
        assert!(
            resume(&source, &destination, cross, &id, true, &recovery)
                .await
                .unwrap()
                .complete
        );
        assert!(!destination
            .transfer_location("default")
            .await
            .unwrap()
            .files
            .starts_with("local-pending-files:"));
    }
}

#[tokio::test]
async fn source_folder_drift_is_not_masked_by_destination_override() {
    let (_dir, b, recovery) = fixture().await;
    let mut expected = intent();
    expected.destination_folder = Some("fixed".into());
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, expected.clone(), true, &recovery)
        .await
        .is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    let source = b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .unwrap();
    let mut request = rename_request_from_properties("db", &source).unwrap();
    request.folder = Some("changed".into());
    b.guarded_secrets()
        .set_secret("default", request)
        .await
        .unwrap();
    assert!(resume(&b, &b, expected, &id, true, &recovery)
        .await
        .is_err());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
}

#[tokio::test]
async fn interrupted_secret_create_rejects_wrong_destination_folder() {
    let (_dir, b, recovery) = fixture().await;
    let mut expected = intent();
    expected.destination_folder = Some("fixed".into());
    FAIL_AT.with(|f| f.set(Some(1)));
    assert!(apply(&b, &b, expected.clone(), true, &recovery)
        .await
        .is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    let source = b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .unwrap();
    let mut request = rename_request_from_properties("db-new", &source).unwrap();
    request.folder = Some("wrong".into());
    b.guarded_secrets()
        .create_secret_if_absent("default", request)
        .await
        .unwrap();
    assert!(resume(&b, &b, expected, &id, true, &recovery)
        .await
        .is_err());
    assert_eq!(b.attachment_names("default", "db").await.unwrap().len(), 2);
}

#[tokio::test]
async fn original_v2_envelope_migrates_without_changing_ciphertext_intent() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    {
        let session = storage::Session::open(&recovery.root, false).unwrap();
        let current = load(&session, &id).unwrap();
        let old = JournalV2 {
            schema: 2,
            id: current.id,
            plan: current.plan,
            location: current.source_location,
            source_revision: current.source_revision,
            secret_commitment: current.secret_commitment,
            destination_revision: current.destination_revision,
            phase: current.phase,
            files: current.files,
            sequence: current.sequence,
        };
        old.validate().unwrap();
        let encrypted = crate::backend::local::crypto::encrypt_bytes(
            &serde_json::to_vec(&old).unwrap(),
            &[session.identity.to_public()],
        )
        .unwrap();
        let mut auth = mac(&session.identity, MAGIC_V2);
        auth.update(&encrypted);
        let mut bytes = MAGIC_V2.to_vec();
        bytes.extend_from_slice(&auth.finalize().into_bytes());
        bytes.extend(encrypted);
        session.write(&format!("{id}.age"), &bytes).unwrap();
    }
    assert!(
        resume(&b, &b, intent(), &id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
    assert!(std::fs::read(recovery.root.join(format!("{id}.age")))
        .unwrap()
        .starts_with(MAGIC_V2));
}

#[tokio::test]
async fn original_v2_recovery_accepts_republished_retained_identity() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    {
        let session = storage::Session::open(&recovery.root, false).unwrap();
        let current = load(&session, &id).unwrap();
        let old = JournalV2 {
            schema: 2,
            id: current.id,
            plan: current.plan,
            location: current.source_location,
            source_revision: current.source_revision,
            secret_commitment: current.secret_commitment,
            destination_revision: current.destination_revision,
            phase: current.phase,
            files: current.files,
            sequence: current.sequence,
        };
        old.validate().unwrap();
        let encrypted = crate::backend::local::crypto::encrypt_bytes(
            &serde_json::to_vec(&old).unwrap(),
            &[session.identity.to_public()],
        )
        .unwrap();
        let mut auth = mac(&session.identity, MAGIC_V2);
        auth.update(&encrypted);
        let mut bytes = MAGIC_V2.to_vec();
        bytes.extend_from_slice(&auth.finalize().into_bytes());
        bytes.extend(encrypted);
        session.write(&format!("{id}.age"), &bytes).unwrap();
    }
    let binding = {
        let session = storage::Session::open(&recovery.root, false).unwrap();
        load(&session, &id).unwrap().plan.files[0]
            .source_key
            .clone()
    };
    let reference = key_reference(&binding).unwrap();
    assert_eq!(
        reference.slot,
        super::super::attachment_key::KeySlot::Retained
    );
    let name = super::super::attachment_key::retained_record_name(&reference.key_id);
    let current = b
        .secrets()
        .get_secret("default", &name, true)
        .await
        .unwrap();
    let request = rename_request_from_properties(&name, &current).unwrap();
    b.secrets().set_secret("default", request).await.unwrap();
    let newer = b
        .secrets()
        .get_secret("default", &name, true)
        .await
        .unwrap();
    assert_ne!(newer.version, binding.provider_version);
    let historical = b
        .secrets()
        .get_secret_version("default", &name, &binding.provider_version, true)
        .await
        .unwrap();
    assert!(historical.value == newer.value);
    assert_eq!(historical.version, binding.provider_version);
    assert!(
        resume(&b, &b, intent(), &id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
    assert!(std::fs::read(recovery.root.join(format!("{id}.age")))
        .unwrap()
        .starts_with(MAGIC_V2));
}

#[tokio::test]
async fn v3_recovery_refuses_republished_retained_identity() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    let binding = {
        let session = storage::Session::open(&recovery.root, false).unwrap();
        load(&session, &id).unwrap().plan.files[0]
            .source_key
            .clone()
    };
    let reference = key_reference(&binding).unwrap();
    assert_eq!(
        reference.slot,
        super::super::attachment_key::KeySlot::Retained
    );
    let name = super::super::attachment_key::retained_record_name(&reference.key_id);
    let current = b
        .secrets()
        .get_secret("default", &name, true)
        .await
        .unwrap();
    let request = rename_request_from_properties(&name, &current).unwrap();
    b.secrets().set_secret("default", request).await.unwrap();
    let newer = b
        .secrets()
        .get_secret("default", &name, true)
        .await
        .unwrap();
    assert_ne!(newer.version, binding.provider_version);
    let historical = b
        .secrets()
        .get_secret_version("default", &name, &binding.provider_version, true)
        .await
        .unwrap();
    assert!(historical.value == newer.value);
    assert_eq!(historical.version, binding.provider_version);
    assert!(resume(&b, &b, intent(), &id, true, &recovery)
        .await
        .is_err());
    assert!(b
        .secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    assert_eq!(b.attachment_names("default", "db").await.unwrap().len(), 2);
}

// Real local storage with independently controlled secret capabilities models
// cloud comparison-only snapshots without inventing CAS deletion authority.
struct CapabilityBackend<'a> {
    file_override: Option<&'a dyn crate::backend::FileBackend>,
    inner: &'a LocalBackend,
    create: bool,
    delete: bool,
    tag_limit: Option<usize>,
    refuse_metadata: bool,
    deny_transfer_read: bool,
    deny_transfer_delete: bool,
}
#[async_trait::async_trait]
impl SecretBackend for CapabilityBackend<'_> {
    async fn validate_transfer_delete(
        &self,
        _vault: &str,
        _name: &str,
    ) -> std::result::Result<(), BackendError> {
        if self.deny_transfer_delete {
            Err(BackendError::PermissionDenied(
                "source delete denied".into(),
            ))
        } else {
            Ok(())
        }
    }

    async fn validate_transfer_metadata(
        &self,
        _vault: &str,
        _request: &SecretRequest,
    ) -> std::result::Result<(), BackendError> {
        if self.refuse_metadata {
            Err(BackendError::Unsupported(
                "metadata cannot be represented by this destination".into(),
            ))
        } else {
            Ok(())
        }
    }
    fn supports_atomic_create(&self) -> bool {
        self.create
    }
    fn supports_conditional_delete(&self) -> bool {
        self.delete
    }
    async fn get_transfer_snapshot(
        &self,
        vault: &str,
        name: &str,
        value: bool,
    ) -> std::result::Result<crate::secret::domain::SecretSnapshot, BackendError> {
        if self.deny_transfer_read && value && name == "db-new" {
            return Err(BackendError::PermissionDenied(
                "raw destination read denied".into(),
            ));
        }
        self.inner
            .secrets()
            .get_transfer_snapshot(vault, name, value)
            .await
    }
    async fn create_secret_if_absent(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> std::result::Result<SecretProperties, BackendError> {
        assert!(
            self.create,
            "engine must refuse unsupported creation before mutation"
        );
        self.inner
            .secrets()
            .create_secret_if_absent(vault, request)
            .await
    }
    async fn delete_secret_if_revision(
        &self,
        vault: &str,
        name: &str,
        revision: &str,
    ) -> std::result::Result<(), BackendError> {
        assert!(
            self.delete,
            "comparison-only cloud revision cannot authorize deletion"
        );
        self.inner
            .secrets()
            .delete_secret_if_revision(vault, name, revision)
            .await
    }
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> std::result::Result<SecretProperties, BackendError> {
        self.inner.secrets().set_secret(vault, request).await
    }
    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        value: bool,
    ) -> std::result::Result<SecretProperties, BackendError> {
        self.inner.secrets().get_secret(vault, name, value).await
    }
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        value: bool,
    ) -> std::result::Result<SecretProperties, BackendError> {
        self.inner
            .secrets()
            .get_secret_version(vault, name, version, value)
            .await
    }
    async fn list_secrets(
        &self,
        vault: &str,
        group: Option<&str>,
    ) -> std::result::Result<Vec<crate::secret::domain::SecretSummary>, BackendError> {
        self.inner.secrets().list_secrets(vault, group).await
    }
    async fn delete_secret(
        &self,
        _vault: &str,
        _name: &str,
    ) -> std::result::Result<(), BackendError> {
        panic!("transfer must never use unconditional secret deletion")
    }
    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: crate::secret::domain::SecretUpdateRequest,
    ) -> std::result::Result<SecretProperties, BackendError> {
        self.inner
            .secrets()
            .update_secret(vault, name, request)
            .await
    }
}
#[async_trait::async_trait]
impl Backend for CapabilityBackend<'_> {
    fn name(&self) -> &'static str {
        "comparison-only-cloud"
    }
    fn kind(&self) -> crate::backend::BackendKind {
        crate::backend::BackendKind::Local
    }
    fn capabilities(&self) -> crate::backend::BackendCapabilities {
        let mut caps = self.inner.capabilities();
        caps.max_tags = self.tag_limit;
        caps
    }
    fn secrets(&self) -> &dyn SecretBackend {
        self
    }
    fn attachment_keys(&self) -> Box<dyn crate::backend::attachment_keys::AttachmentKeyStore + '_> {
        self.inner.attachment_keys()
    }
    fn files(&self) -> Option<&dyn crate::backend::FileBackend> {
        self.file_override.or_else(|| self.inner.files())
    }
    async fn transfer_location(
        &self,
        vault: &str,
    ) -> std::result::Result<TransferLocation, BackendError> {
        self.inner.transfer_location(vault).await
    }
    async fn validate_transfer_recovery_path(
        &self,
        vault: &str,
        path: &std::path::Path,
    ) -> std::result::Result<(), BackendError> {
        self.inner
            .validate_transfer_recovery_path(vault, path)
            .await
    }
    async fn health_check(&self) -> std::result::Result<(), BackendError> {
        self.inner.health_check().await
    }
}

async fn cross_intent(destination: &LocalBackend, operation: TransferOperation) -> TransferIntent {
    let pointer = destination
        .attachment_keys()
        .get_secret(
            "default",
            super::super::attachment_key::ACTIVE_POINTER_SECRET,
            true,
        )
        .await
        .unwrap();
    let active = match super::super::attachment_key::parse_pointer_value(
        pointer
            .value
            .as_ref()
            .map(SecretValue::expose_secret)
            .unwrap(),
    ) {
        Some(super::super::attachment_key::PointerKind::V2 { active, .. }) => active,
        _ => panic!("healthy fixture"),
    };
    let mut cross = intent();
    cross.destination.identity = "other".into();
    cross.destination_key_id = Some(active.as_str().into());
    cross.operation = operation;
    cross
}

struct SharedFileLeaf<'a> {
    secrets: &'a LocalBackend,
    files: &'a LocalBackend,
}
#[async_trait::async_trait]
impl Backend for SharedFileLeaf<'_> {
    fn name(&self) -> &'static str {
        "local"
    }
    fn kind(&self) -> crate::backend::BackendKind {
        crate::backend::BackendKind::Local
    }
    fn capabilities(&self) -> crate::backend::BackendCapabilities {
        self.secrets.capabilities()
    }
    fn secrets(&self) -> &dyn SecretBackend {
        self.secrets.secrets()
    }
    fn files(&self) -> Option<&dyn crate::backend::FileBackend> {
        self.files.files()
    }
    async fn health_check(&self) -> std::result::Result<(), BackendError> {
        Ok(())
    }
    async fn transfer_location(
        &self,
        vault: &str,
    ) -> std::result::Result<TransferLocation, BackendError> {
        self.secrets.transfer_location(vault).await
    }
    async fn transfer_secret_physical_namespace(
        &self,
        vault: &str,
    ) -> std::result::Result<String, BackendError> {
        self.secrets.transfer_secret_physical_namespace(vault).await
    }
    async fn transfer_file_physical_namespace(
        &self,
        vault: &str,
    ) -> std::result::Result<String, BackendError> {
        self.files.transfer_file_physical_namespace(vault).await
    }
}

#[tokio::test]
async fn shared_file_leaf_under_different_parents_refuses_physical_overlap() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    // Independent secret stores, with the destination's file operations reaching
    // the source's actual files leaf, as a bind mount does without a symlink.
    let alias = SharedFileLeaf {
        secrets: &destination,
        files: &source,
    };
    assert_ne!(
        source.transfer_location("default").await.unwrap().files,
        alias.transfer_location("default").await.unwrap().files
    );
    let mut cross = cross_intent(&destination, TransferOperation::Copy).await;
    cross.destination_name = cross.source_name.clone();
    let error = supported(&source, &alias, &cross)
        .await
        .expect_err("equal physical files must refuse even with different parent-chain hashes");
    assert!(error.to_string().contains("overlap"), "{error}");
    assert!(!recovery.root.exists());
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        destination
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn destination_file_collisions_within_plan_precede_recovery_and_secret_writes() {
    let (source_dir, _, recovery) = fixture().await;
    let source = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(source_dir.path().join("store").display().to_string()),
        key_file: Some(source_dir.path().join("key").display().to_string()),
        default_vault: Some("default".into()),
        opaque_filenames: Some(true),
        ..Default::default()
    }))
    .unwrap();
    let destination_dir = tempfile::tempdir().unwrap();
    let destination = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(destination_dir.path().join("store").display().to_string()),
        key_file: Some(destination_dir.path().join("key").display().to_string()),
        default_vault: Some("default".into()),
        opaque_filenames: Some(true),
        ..Default::default()
    }))
    .unwrap();
    super::super::attachment_lifecycle::initialize(
        destination.attachment_keys().as_ref(),
        destination.files().unwrap(),
        "default",
        true,
    )
    .await
    .unwrap();
    // Both source keys hash to distinct stems; the shorter destination keys
    // encode to stems differing only by ASCII case. This uses real Local I/O.
    let owner = "s".repeat(220);
    let props = source
        .secrets()
        .get_secret("default", "db", true)
        .await
        .unwrap();
    source
        .secrets()
        .set_secret(
            "default",
            rename_request_from_properties(&owner, &props).unwrap(),
        )
        .await
        .unwrap();
    for suffix in ["proof.txt", "PROOF.txt"] {
        crate::secret::attachments::upload_encrypted(
            source.attachment_keys().as_ref(),
            source.files().unwrap(),
            "default",
            crate::blob::models::FileUploadRequest {
                name: format!("attachments/{owner}/{suffix}"),
                content: suffix.as_bytes().to_vec(),
                content_type: Some("text/plain".into()),
                groups: vec![],
                tags: HashMap::new(),
                metadata: HashMap::new(),
            },
            None,
        )
        .await
        .unwrap();
    }
    // Test-only observation; production preflight must never create a probe.
    let parent = destination_dir.path().join("store/vaults/default");
    std::fs::write(parent.join("case-probe"), b"").unwrap();
    let insensitive = parent.join("CASE-PROBE").exists();
    std::fs::remove_file(parent.join("case-probe")).unwrap();
    let mut cross = cross_intent(&destination, TransferOperation::Copy).await;
    cross.source_name = owner.clone();
    cross.destination_name = "cert".into();
    let result = preflight(&source, &destination, cross.clone()).await;
    let unknown = result.as_ref().err().is_some_and(|e| {
        e.to_string()
            .contains("cannot establish destination filesystem case semantics")
    });
    if insensitive || unknown {
        let error =
            result.expect_err("proof.txt and PROOF.txt must be reserved as one destination object");
        assert!(error.to_string().contains("collid") || unknown, "{error}");
        assert!(apply(&source, &destination, cross, true, &recovery)
            .await
            .is_err());
        assert!(destination
            .secrets()
            .get_secret("default", "cert", false)
            .await
            .is_err());
        assert!(!parent.join("files").exists());
        assert!(!recovery.root.exists());
    } else {
        result.unwrap();
        assert!(
            apply(&source, &destination, cross, true, &recovery)
                .await
                .unwrap()
                .complete
        );
        assert_eq!(
            destination
                .attachment_names("default", "cert")
                .await
                .unwrap()
                .len(),
            2
        );
    }
    assert_eq!(
        source
            .attachment_names("default", &owner)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[cfg(feature = "aws")]
mod s3_request_preflight {
    use super::*;
    use crate::backend::{file::FileTransferRequest, FileBackend};
    use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
    use crate::utils::progress::ProgressReporter;

    // Real Local encrypted snapshots and the real S3 request validator. Only
    // transport/storage is substituted; no cloud request is issued by this fixture.
    // A long S3 source key maps to a short Local backing key for portable storage.
    struct S3Files<'a> {
        inner: &'a dyn FileBackend,
        validator: crate::backend::aws::files::AwsFileBackend,
        alias: Option<(String, String)>,
    }
    impl<'a> S3Files<'a> {
        fn new(inner: &'a dyn FileBackend, alias: Option<(String, String)>) -> Self {
            let config = aws_sdk_s3::Config::builder()
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new("us-east-1"))
                .credentials_provider(aws_sdk_s3::config::Credentials::new(
                    "test", "test", None, None, "test",
                ))
                .build();
            Self {
                inner,
                validator: crate::backend::aws::files::AwsFileBackend::new(
                    aws_sdk_s3::Client::from_conf(config),
                    "bucket".into(),
                ),
                alias,
            }
        }
        fn backing<'b>(&'b self, name: &'b str) -> &'b str {
            self.alias
                .as_ref()
                .filter(|(logical, _)| logical == name)
                .map_or(name, |(_, stored)| stored)
        }
        fn logical(&self, mut info: FileInfo) -> FileInfo {
            if let Some((logical, stored)) = &self.alias {
                if &info.name == stored {
                    info.name = logical.clone();
                }
            }
            info
        }
    }
    #[async_trait::async_trait]
    impl FileBackend for S3Files<'_> {
        fn validate_transfer_request(
            &self,
            request: &FileTransferRequest<'_>,
        ) -> std::result::Result<(), BackendError> {
            self.validator.validate_transfer_request(request)
        }
        fn validate_file_name(&self, name: &str) -> std::result::Result<(), BackendError> {
            self.validator.validate_file_name(name)
        }
        fn prepare_transfer_metadata(
            &self,
            groups: &[String],
            metadata: &HashMap<String, String>,
        ) -> std::result::Result<HashMap<String, String>, BackendError> {
            self.validator.prepare_transfer_metadata(groups, metadata)
        }
        async fn transfer_file_names_collide(
            &self,
            vault: &str,
            left: &str,
            right: &str,
        ) -> std::result::Result<bool, BackendError> {
            self.validator
                .transfer_file_names_collide(vault, left, right)
                .await
        }
        fn supports_atomic_create(&self) -> bool {
            true
        }
        async fn upload_file(
            &self,
            _: &str,
            _: FileUploadRequest,
            _: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<FileInfo, BackendError> {
            panic!("preflight must prevent upload")
        }
        async fn upload_file_if_absent(
            &self,
            _: &str,
            _: FileUploadRequest,
            _: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<FileInfo, BackendError> {
            panic!("preflight must prevent conditional upload")
        }
        async fn download_file(
            &self,
            vault: &str,
            name: &str,
            reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<Vec<u8>, BackendError> {
            self.inner
                .download_file(vault, self.backing(name), reporter)
                .await
        }
        async fn download_file_snapshot(
            &self,
            vault: &str,
            name: &str,
            reporter: Option<&dyn ProgressReporter>,
        ) -> std::result::Result<crate::backend::file::FileDownloadSnapshot, BackendError> {
            self.inner
                .download_file_snapshot(vault, self.backing(name), reporter)
                .await
        }
        async fn list_files(
            &self,
            vault: &str,
            mut request: FileListRequest,
        ) -> std::result::Result<Vec<FileInfo>, BackendError> {
            let prefix = request.prefix.take().unwrap_or_default();
            Ok(self
                .inner
                .list_files(vault, request)
                .await?
                .into_iter()
                .map(|info| self.logical(info))
                .filter(|info| info.name.starts_with(&prefix))
                .collect())
        }
        async fn get_file_info(
            &self,
            vault: &str,
            name: &str,
        ) -> std::result::Result<FileInfo, BackendError> {
            Ok(self.logical(self.inner.get_file_info(vault, self.backing(name)).await?))
        }
        async fn get_file_restore_info(
            &self,
            vault: &str,
            name: &str,
        ) -> std::result::Result<FileInfo, BackendError> {
            Ok(self.logical(
                self.inner
                    .get_file_restore_info(vault, self.backing(name))
                    .await?,
            ))
        }
        async fn delete_file(&self, _: &str, _: &str) -> std::result::Result<(), BackendError> {
            panic!("preflight must prevent deletion")
        }
    }

    async fn rejects_later_request_before_any_writes(long_key: bool) {
        let temp = tempfile::tempdir().unwrap();
        let make = |store: &str, vault: &str| {
            LocalBackend::new(Some(&LocalConfig {
                store_path: Some(temp.path().join(store).display().to_string()),
                key_file: Some(
                    temp.path()
                        .join(format!("{store}-key"))
                        .display()
                        .to_string(),
                ),
                default_vault: Some(vault.into()),
                ..Default::default()
            }))
            .unwrap()
        };
        let source = make("source", "a");
        let destination = make("destination", "destination-vault");
        for name in ["good", "s"] {
            source
                .secrets()
                .set_secret(
                    "a",
                    SecretRequest {
                        name: name.into(),
                        value: SecretValue::new(format!("value-{name}")),
                        content_type: None,
                        enabled: Some(true),
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
            crate::secret::attachments::upload_encrypted(
                source.attachment_keys().as_ref(),
                source.files().unwrap(),
                "a",
                FileUploadRequest {
                    name: format!("attachments/{name}/proof"),
                    content: name.as_bytes().to_vec(),
                    content_type: Some("text/plain".into()),
                    groups: vec![],
                    metadata: HashMap::new(),
                    tags: if name == "s" && !long_key {
                        (0..11)
                            .map(|i| (format!("tag-{i}"), "value".into()))
                            .collect()
                    } else {
                        HashMap::new()
                    },
                },
                None,
            )
            .await
            .unwrap();
        }
        let initialized = super::super::super::attachment_lifecycle::initialize(
            destination.attachment_keys().as_ref(),
            destination.files().unwrap(),
            "destination-vault",
            true,
        )
        .await
        .unwrap();
        let alias = long_key.then(|| {
            (
                format!("attachments/s/{}", "x".repeat(990)),
                "attachments/s/proof".into(),
            )
        });
        let sf = S3Files::new(source.files().unwrap(), alias);
        let df = S3Files::new(destination.files().unwrap(), None);
        let wrap = |inner, files| CapabilityBackend {
            inner,
            file_override: Some(files),
            create: true,
            delete: false,
            tag_limit: None,
            refuse_metadata: false,
            deny_transfer_read: false,
            deny_transfer_delete: false,
        };
        let from = wrap(&source, &sf as &dyn FileBackend);
        let to = wrap(&destination, &df as &dyn FileBackend);
        let intents: Vec<_> = ["good", "s"]
            .into_iter()
            .map(|name| TransferIntent {
                source: transfer::TransferEndpoint {
                    identity: "source".into(),
                    vault: "a".into(),
                },
                destination: transfer::TransferEndpoint {
                    identity: "destination".into(),
                    vault: "destination-vault".into(),
                },
                source_name: name.into(),
                destination_name: name.into(),
                operation: TransferOperation::Copy,
                destination_key_id: initialized.active_key_id.clone(),
                destination_folder: None,
            })
            .collect();
        assert!(preflight(&from, &to, intents[0].clone()).await.is_ok());
        assert!(
            preflight(&from, &to, intents[1].clone()).await.is_err(),
            "deterministic S3 request failure must precede destination creation"
        );
        assert!(
            preflight_batch(&from, &to, &intents).await.is_err(),
            "later request failure must refuse the complete batch"
        );
        let recovery = RecoveryStore::new(temp.path().join("recovery"));
        assert!(apply(&from, &to, intents[1].clone(), true, &recovery)
            .await
            .is_err());
        assert!(!recovery.root.exists());
        assert!(!temp
            .path()
            .join("destination/vaults/destination-vault/files")
            .exists());
        for name in ["good", "s"] {
            assert!(destination
                .secrets()
                .get_secret("destination-vault", name, false)
                .await
                .is_err());
            assert_eq!(
                source
                    .secrets()
                    .get_secret("a", name, true)
                    .await
                    .unwrap()
                    .value
                    .unwrap()
                    .expose_secret(),
                format!("value-{name}")
            );
            assert_eq!(source.attachment_names("a", name).await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn s3_complete_file_preflight_rejects_later_eleven_tag_entry_without_writes() {
        rejects_later_request_before_any_writes(false).await;
    }
    #[tokio::test]
    async fn s3_complete_file_preflight_rejects_later_long_destination_key_without_writes() {
        rejects_later_request_before_any_writes(true).await;
    }
}

#[tokio::test]
async fn capability_matrix_refuses_cloud_move_and_unsafe_destination_before_writes() {
    for unsafe_destination in [false, true] {
        let (_source_dir, source, recovery) = fixture().await;
        let (_destination_dir, destination, _) = fixture().await;
        let from = CapabilityBackend {
            file_override: None,
            inner: &source,
            create: true,
            delete: false,
            tag_limit: None,
            refuse_metadata: false,
            deny_transfer_read: false,
            deny_transfer_delete: false,
        };
        let to = CapabilityBackend {
            file_override: None,
            inner: &destination,
            create: !unsafe_destination,
            delete: false,
            tag_limit: None,
            refuse_metadata: false,
            deny_transfer_read: false,
            deny_transfer_delete: false,
        };
        let cross = cross_intent(
            &destination,
            if unsafe_destination {
                TransferOperation::Copy
            } else {
                TransferOperation::Move
            },
        )
        .await;
        assert!(preflight(&from, &to, cross.clone()).await.is_err());
        assert!(apply(&from, &to, cross, true, &recovery).await.is_err());
        assert!(!recovery.root.exists());
        assert!(destination
            .guarded_secrets()
            .get_secret("default", "db-new", false)
            .await
            .is_err());
        assert_eq!(
            source
                .attachment_names("default", "db")
                .await
                .unwrap()
                .len(),
            2
        );
    }
}
#[tokio::test]
async fn capability_matrix_allows_local_to_cloud_move_and_cloud_to_local_copy() {
    for source_cloud in [false, true] {
        let (_source_dir, source, recovery) = fixture().await;
        let (_destination_dir, destination, _) = fixture().await;
        let from = CapabilityBackend {
            file_override: None,
            inner: &source,
            create: true,
            delete: !source_cloud,
            tag_limit: None,
            refuse_metadata: false,
            deny_transfer_read: false,
            deny_transfer_delete: false,
        };
        let to = CapabilityBackend {
            file_override: None,
            inner: &destination,
            create: true,
            delete: false,
            tag_limit: None,
            refuse_metadata: false,
            deny_transfer_read: false,
            deny_transfer_delete: false,
        };
        let cross = cross_intent(
            &destination,
            if source_cloud {
                TransferOperation::Copy
            } else {
                TransferOperation::Move
            },
        )
        .await;
        assert!(
            preflight(&from, &to, cross.clone())
                .await
                .unwrap()
                .execution_supported
        );
        assert!(
            apply(&from, &to, cross, true, &recovery)
                .await
                .unwrap()
                .complete
        );
        assert_eq!(
            source
                .guarded_secrets()
                .get_secret("default", "db", true)
                .await
                .is_ok(),
            source_cloud
        );
    }
}
#[tokio::test]
async fn strict_preflight_checks_transformed_folder_budget_without_writes() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let to = CapabilityBackend {
        file_override: None,
        inner: &destination,
        create: true,
        delete: false,
        tag_limit: Some(5),
        refuse_metadata: false,
        deny_transfer_read: false,
        deny_transfer_delete: false,
    };
    let mut cross = cross_intent(&destination, TransferOperation::Copy).await;
    cross.destination_folder = Some("/".into());
    assert!(preflight(&source, &to, cross.clone()).await.is_ok());
    cross.destination_folder = Some("new-folder".into());
    assert!(preflight(&source, &to, cross.clone()).await.is_err());
    assert!(apply(&source, &to, cross, true, &recovery).await.is_err());
    assert!(!recovery.root.exists());
}

#[tokio::test]
async fn pending_destination_must_match_saved_ciphertext_before_reencryption() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let cross = cross_intent(&destination, TransferOperation::Move).await;
    FAIL_AT.with(|f| f.set(Some(7)));
    assert!(apply(&source, &destination, cross.clone(), true, &recovery)
        .await
        .is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    let current = rewrap::snapshot(
        destination.files().unwrap(),
        "default",
        "attachments/db-new/first",
    )
    .await
    .unwrap();
    let reference =
        super::super::attachment_key::parse_key_ref_from_metadata(&current.info.metadata).unwrap();
    let identity = rewrap::exact_identity(
        destination.attachment_keys().as_ref(),
        "default",
        &reference,
    )
    .await
    .unwrap();
    let content = crate::backend::local::crypto::encrypt_bytes(
        b"attachment-plaintext-canary",
        &[identity.to_public()],
    )
    .unwrap();
    assert_ne!(content, current.data.content);
    destination
        .files()
        .unwrap()
        .restore_file(
            "default",
            crate::blob::models::FileUploadRequest {
                name: current.info.name,
                content,
                content_type: Some(current.info.content_type),
                metadata: current.info.metadata,
                groups: current.info.groups,
                tags: current.info.tags,
            },
        )
        .await
        .unwrap();
    assert!(resume(&source, &destination, cross, &id, true, &recovery)
        .await
        .is_err());
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn transfer_folder_canonical_validation_and_legacy_none_roundtrip() {
    for folder in ["", "//", "a/../b", "a/./b", "a/ b", "a\\b", "a\nb", "a/"] {
        let mut value = intent();
        value.destination_folder = Some(folder.into());
        assert!(value.validate().is_err(), "{folder:?}");
    }
    let value = serde_json::to_value(intent()).unwrap();
    assert!(value.get("destination_folder").is_none());
    let decoded: TransferIntent = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.destination_folder, None);
}

#[tokio::test]
async fn empty_source_missing_files_refuses_in_read_only_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let b = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(dir.path().join("store").display().to_string()),
        key_file: Some(dir.path().join("identity").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    let recovery = RecoveryStore::new(dir.path().join("recovery"));
    b.secrets()
        .set_secret(
            "default",
            SecretRequest {
                name: "db".into(),
                value: SecretValue::new("plain"),
                content_type: None,
                enabled: None,
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
    let before = b.transfer_location("default").await.unwrap();
    assert!(before.files.starts_with("local-pending-files:"));
    assert!(preflight(&b, &b, intent())
        .await
        .unwrap_err()
        .to_string()
        .contains("ordinary copy or move"));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    assert_eq!(before, b.transfer_location("default").await.unwrap());
    assert!(!recovery.root.exists());
}

#[tokio::test]
async fn copy_journal_rejects_cleanup_authority_and_v3_under_v2_magic() {
    let (_dir, b, recovery) = fixture().await;
    let mut copy = intent();
    copy.operation = TransferOperation::Copy;
    let report = apply(&b, &b, copy, true, &recovery).await.unwrap();
    let session = storage::Session::open(&recovery.root, false).unwrap();
    let mut journal = load(&session, &report.id).unwrap();
    let encrypted = crate::backend::local::crypto::encrypt_bytes(
        &serde_json::to_vec(&journal).unwrap(),
        &[session.identity.to_public()],
    )
    .unwrap();
    let mut auth = mac(&session.identity, MAGIC_V2);
    auth.update(&encrypted);
    let mut wrong_magic = MAGIC_V2.to_vec();
    wrong_magic.extend_from_slice(&auth.finalize().into_bytes());
    wrong_magic.extend(encrypted);
    assert!(decode(&wrong_magic, &session.identity).is_err());
    journal.phase = Phase::Cleanup;
    assert!(journal.validate().is_err());
    journal.phase = Phase::Complete;
    let generation = match &journal.files[0] {
        FileState::Verified { generation } => generation.clone(),
        _ => panic!("copy verified"),
    };
    journal.files[0] = FileState::DeletePending { generation };
    assert!(journal.validate().is_err());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .is_ok());
}

#[tokio::test]
async fn oversized_source_is_refused_before_ciphertext_download() {
    let (dir, b, recovery) = fixture().await;
    let files = dir.path().join("store/vaults/default/files");
    let mut altered = false;
    for entry in std::fs::read_dir(&files).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if let Some(stem) = name.strip_suffix(".meta.json") {
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if metadata["name"] == "attachments/db/first" {
                metadata["size"] = (256u64 * 1024 * 1024 + 1).into();
                std::fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
                // A download would now fail age decoding, so the size refusal
                // demonstrates the check precedes provider byte allocation.
                std::fs::write(files.join(format!("{stem}.age")), b"invalid ciphertext").unwrap();
                altered = true;
            }
        }
    }
    assert!(altered);
    let error = preflight(&b, &b, intent()).await.unwrap_err().to_string();
    assert!(error.contains("256 MiB"), "{error}");
    assert!(!recovery.root.exists());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
}

#[tokio::test]
async fn strict_preflight_reserves_all_future_journal_metadata_before_writes() {
    let (_dir, b, recovery) = fixture().await;
    for name in ["attachments/db/first", "attachments/db/nested/second"] {
        let current = rewrap::snapshot(b.files().unwrap(), "default", name)
            .await
            .unwrap();
        let mut metadata = current.info.metadata;
        metadata.insert("large-user-metadata".into(), "x".repeat(2 * 1024 * 1024));
        b.files()
            .unwrap()
            .restore_file(
                "default",
                crate::blob::models::FileUploadRequest {
                    name: name.into(),
                    content: current.data.content,
                    content_type: Some(current.info.content_type),
                    metadata,
                    tags: current.info.tags,
                    groups: current.info.groups,
                },
            )
            .await
            .unwrap();
    }
    let error = preflight(&b, &b, intent()).await.unwrap_err().to_string();
    assert!(error.contains("journal budget"), "{error}");
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    assert!(!recovery.root.exists());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
}

#[tokio::test]
async fn original_v2_empty_alias_move_resumes_through_original_v2() {
    let (_dir, b, recovery) = fixture().await;
    for name in b.attachment_names("default", "db").await.unwrap() {
        b.files()
            .unwrap()
            .delete_file("default", &name)
            .await
            .unwrap();
    }
    let mut alias = intent();
    alias.destination.identity = "old-alias".into();
    let plan = transfer::plan(&b, &b, alias.clone()).await.unwrap();
    let source = b
        .guarded_secrets()
        .get_transfer_snapshot("default", "db", true)
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    {
        let session = storage::Session::open(&recovery.root, true).unwrap();
        let old = JournalV2 {
            schema: 2,
            id: id.clone(),
            plan,
            location: b.transfer_location("default").await.unwrap(),
            source_revision: source.revision,
            secret_commitment: commitment(&session.identity, &source.properties, "db-new").unwrap(),
            destination_revision: None,
            phase: Phase::Prepared,
            files: vec![],
            sequence: 1,
        };
        old.validate().unwrap();
        let encrypted = crate::backend::local::crypto::encrypt_bytes(
            &serde_json::to_vec(&old).unwrap(),
            &[session.identity.to_public()],
        )
        .unwrap();
        let mut auth = mac(&session.identity, MAGIC_V2);
        auth.update(&encrypted);
        let mut bytes = MAGIC_V2.to_vec();
        bytes.extend_from_slice(&auth.finalize().into_bytes());
        bytes.extend(encrypted);
        session.write(&format!("{id}.age"), &bytes).unwrap();
    }
    assert!(preflight(&b, &b, alias.clone()).await.is_err());
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(resume(&b, &b, alias.clone(), &id, true, &recovery)
        .await
        .is_err());
    FAIL_AT.with(|f| f.set(None));
    assert!(std::fs::read(recovery.root.join(format!("{id}.age")))
        .unwrap()
        .starts_with(MAGIC_V2));
    assert!(
        resume(&b, &b, alias, &id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .is_err());
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", true)
        .await
        .is_ok());
}

#[tokio::test]
async fn strict_preflight_refuses_unrepresentable_destination_metadata_before_writes() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let to = CapabilityBackend {
        file_override: None,
        inner: &destination,
        create: true,
        delete: false,
        tag_limit: None,
        refuse_metadata: true,
        deny_transfer_read: false,
        deny_transfer_delete: false,
    };
    let cross = cross_intent(&destination, TransferOperation::Move).await;
    assert!(preflight(&source, &to, cross.clone()).await.is_err());
    assert!(apply(&source, &to, cross, true, &recovery).await.is_err());
    assert!(!recovery.root.exists());
    assert!(destination
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn legacy_v2_near_limit_codec_adds_no_migration_overhead() {
    let (_dir, b, _recovery) = fixture().await;
    let mut plan = transfer::plan(&b, &b, intent()).await.unwrap();
    plan.files[0]
        .metadata
        .insert("large".into(), "x".repeat(MAX - 80 * 1024));
    let identity = age::x25519::Identity::generate();
    let old = JournalV2 {
        schema: 2,
        id: uuid::Uuid::new_v4().to_string(),
        files: vec![FileState::Prepared; plan.files.len()],
        plan,
        location: b.transfer_location("default").await.unwrap(),
        source_revision: "old-large-token".repeat(100),
        secret_commitment: "0".repeat(64),
        destination_revision: None,
        phase: Phase::Prepared,
        sequence: 1,
    };
    old.validate().unwrap();
    let plain = serde_json::to_vec(&old).unwrap();
    assert!(plain.len() > MAX - 100 * 1024 && plain.len() < MAX - 65536);
    let encrypted =
        crate::backend::local::crypto::encrypt_bytes(&plain, &[identity.to_public()]).unwrap();
    let mut auth = mac(&identity, MAGIC_V2);
    auth.update(&encrypted);
    let mut bytes = MAGIC_V2.to_vec();
    bytes.extend_from_slice(&auth.finalize().into_bytes());
    bytes.extend(encrypted);
    let journal = decode(&bytes, &identity).unwrap();
    let saved = encode(&journal, &identity).unwrap();
    assert!(saved.starts_with(MAGIC_V2));
    // age may vary padding stanza length; compatibility concerns the exact
    // plaintext serialization budget, not randomized envelope padding.
    let decoded_plain =
        crate::backend::local::crypto::decrypt_bytes(&saved[MAGIC_V2.len() + 32..], &identity)
            .unwrap();
    assert_eq!(decoded_plain.len(), plain.len());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&decoded_plain).unwrap(),
        serde_json::from_slice::<serde_json::Value>(&plain).unwrap()
    );
}

#[tokio::test]
async fn serialized_v3_cannot_enable_legacy_validation_route() {
    let (_dir, b, recovery) = fixture().await;
    FAIL_AT.with(|f| f.set(Some(0)));
    assert!(apply(&b, &b, intent(), true, &recovery).await.is_err());
    FAIL_AT.with(|f| f.set(None));
    let id = recovery.list().unwrap()[0].id.clone();
    let session = storage::Session::open(&recovery.root, false).unwrap();
    let journal = load(&session, &id).unwrap();
    let mut value = serde_json::to_value(journal).unwrap();
    assert!(value.get("legacy_v2").is_none());
    value["legacy_v2"] = true.into();
    assert!(serde_json::from_value::<Journal>(value).is_err());
}

async fn denied_policy_preflight(deny_delete: bool) {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let from = CapabilityBackend {
        file_override: None,
        inner: &source,
        create: true,
        delete: true,
        tag_limit: None,
        refuse_metadata: false,
        deny_transfer_read: false,
        deny_transfer_delete: deny_delete,
    };
    let to = CapabilityBackend {
        file_override: None,
        inner: &destination,
        create: true,
        delete: true,
        tag_limit: None,
        refuse_metadata: false,
        deny_transfer_read: !deny_delete,
        deny_transfer_delete: false,
    };
    let cross = cross_intent(&destination, TransferOperation::Move).await;
    assert!(preflight(&from, &to, cross.clone()).await.is_err());
    assert!(apply(&from, &to, cross, true, &recovery).await.is_err());
    assert!(!recovery.root.exists());
    assert!(destination
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test]
async fn destination_raw_policy_preflight_precedes_destination_creation() {
    denied_policy_preflight(false).await;
}
#[tokio::test]
async fn source_delete_policy_preflight_precedes_destination_creation() {
    denied_policy_preflight(true).await;
}

#[tokio::test]
async fn copy_does_not_require_source_delete_policy_permission() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let from = CapabilityBackend {
        file_override: None,
        inner: &source,
        create: true,
        delete: true,
        tag_limit: None,
        refuse_metadata: false,
        deny_transfer_read: false,
        deny_transfer_delete: true,
    };
    let cross = cross_intent(&destination, TransferOperation::Copy).await;
    assert!(preflight(&from, &destination, cross.clone()).await.is_ok());
    assert!(
        apply(&from, &destination, cross, true, &recovery)
            .await
            .unwrap()
            .complete
    );
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}

// Models S3's persisted groups user-metadata while retaining Local atomic I/O.
struct S3GroupFiles<'a>(&'a dyn crate::backend::FileBackend);
#[async_trait::async_trait]
impl crate::backend::FileBackend for S3GroupFiles<'_> {
    fn prepare_transfer_metadata(
        &self,
        groups: &[String],
        metadata: &HashMap<String, String>,
    ) -> std::result::Result<HashMap<String, String>, BackendError> {
        let mut prepared = metadata.clone();
        if !groups.is_empty() {
            let joined = groups.join(",");
            if joined
                .split(',')
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
                != groups
                || prepared.get("groups").is_some_and(|old| old != &joined)
            {
                return Err(BackendError::Unsupported("lossy file groups".into()));
            }
            prepared.insert("groups".into(), joined);
        }
        Ok(prepared)
    }
    fn supports_atomic_create(&self) -> bool {
        true
    }
    async fn upload_file(
        &self,
        _: &str,
        _: crate::blob::models::FileUploadRequest,
        _: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<crate::blob::models::FileInfo, BackendError> {
        panic!("unconditional upload")
    }
    async fn upload_file_if_absent(
        &self,
        vault: &str,
        mut request: crate::blob::models::FileUploadRequest,
        reporter: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<crate::blob::models::FileInfo, BackendError> {
        request.metadata = self.prepare_transfer_metadata(&request.groups, &request.metadata)?;
        self.0.upload_file_if_absent(vault, request, reporter).await
    }
    async fn download_file(
        &self,
        vault: &str,
        name: &str,
        reporter: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<Vec<u8>, BackendError> {
        self.0.download_file(vault, name, reporter).await
    }
    async fn download_file_snapshot(
        &self,
        vault: &str,
        name: &str,
        reporter: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<crate::backend::file::FileDownloadSnapshot, BackendError> {
        self.0.download_file_snapshot(vault, name, reporter).await
    }
    async fn list_files(
        &self,
        vault: &str,
        request: crate::blob::models::FileListRequest,
    ) -> std::result::Result<Vec<crate::blob::models::FileInfo>, BackendError> {
        self.0.list_files(vault, request).await
    }
    async fn delete_file(&self, _: &str, _: &str) -> std::result::Result<(), BackendError> {
        panic!("unconditional deletion")
    }
    async fn get_file_info(
        &self,
        vault: &str,
        name: &str,
    ) -> std::result::Result<crate::blob::models::FileInfo, BackendError> {
        self.0.get_file_info(vault, name).await
    }
    async fn get_file_restore_info(
        &self,
        vault: &str,
        name: &str,
    ) -> std::result::Result<crate::blob::models::FileInfo, BackendError> {
        self.0.get_file_restore_info(vault, name).await
    }
}

#[tokio::test]
async fn local_to_s3_group_metadata_is_prepared_before_evidence_and_upload() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let files = S3GroupFiles(destination.files().unwrap());
    let to = CapabilityBackend {
        file_override: Some(&files),
        inner: &destination,
        create: true,
        delete: false,
        tag_limit: None,
        refuse_metadata: false,
        deny_transfer_read: false,
        deny_transfer_delete: false,
    };
    let before = source
        .files()
        .unwrap()
        .get_file_restore_info("default", "attachments/db/first")
        .await
        .unwrap();
    assert_eq!(before.groups, vec!["group"]);
    assert!(!before.metadata.contains_key("groups"));
    let cross = cross_intent(&destination, TransferOperation::Copy).await;
    preflight(&source, &to, cross.clone()).await.unwrap();
    let report = apply(&source, &to, cross.clone(), true, &recovery)
        .await
        .unwrap();
    assert!(report.complete);
    let id = recovery.list().unwrap()[0].id.clone();
    assert!(
        resume(&source, &to, cross.clone(), &id, true, &recovery)
            .await
            .unwrap()
            .complete
    );
    // A provider with different preparation rules cannot adopt saved S3 evidence.
    assert!(resume(&source, &destination, cross, &id, true, &recovery)
        .await
        .is_err());
    let actual = destination
        .files()
        .unwrap()
        .get_file_restore_info("default", "attachments/db-new/first")
        .await
        .unwrap();
    assert_eq!(actual.groups, before.groups);
    assert_eq!(
        actual.metadata.get("groups").map(String::as_str),
        Some("group")
    );
    let source_after = source
        .files()
        .unwrap()
        .get_file_restore_info("default", "attachments/db/first")
        .await
        .unwrap();
    assert_eq!(source_after.metadata, before.metadata);
}

#[tokio::test]
async fn s3_file_group_preflight_refuses_loss_before_writes() {
    let (_source_dir, source, recovery) = fixture().await;
    let (_destination_dir, destination, _) = fixture().await;
    let snapshot = rewrap::snapshot(source.files().unwrap(), "default", "attachments/db/first")
        .await
        .unwrap();
    source
        .files()
        .unwrap()
        .restore_file(
            "default",
            crate::blob::models::FileUploadRequest {
                name: snapshot.info.name,
                content: snapshot.data.content,
                content_type: Some(snapshot.info.content_type),
                groups: vec!["comma,group".into()],
                metadata: snapshot.info.metadata,
                tags: snapshot.info.tags,
            },
        )
        .await
        .unwrap();
    let files = S3GroupFiles(destination.files().unwrap());
    let to = CapabilityBackend {
        file_override: Some(&files),
        inner: &destination,
        create: true,
        delete: false,
        tag_limit: None,
        refuse_metadata: false,
        deny_transfer_read: false,
        deny_transfer_delete: false,
    };
    let cross = cross_intent(&destination, TransferOperation::Move).await;
    assert!(preflight(&source, &to, cross.clone())
        .await
        .unwrap_err()
        .to_string()
        .contains("lossy file groups"));
    assert!(apply(&source, &to, cross, true, &recovery).await.is_err());
    assert!(!recovery.root.exists());
    assert!(destination
        .secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    assert!(destination
        .attachment_names("default", "db-new")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        source
            .attachment_names("default", "db")
            .await
            .unwrap()
            .len(),
        2
    );
}
