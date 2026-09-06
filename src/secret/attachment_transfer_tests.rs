use super::*;
fn intent() -> TransferIntent {
    TransferIntent {
        source: TransferEndpoint {
            identity: "local:a".into(),
            vault: "default".into(),
        },
        destination: TransferEndpoint {
            identity: "local:a".into(),
            vault: "default".into(),
        },
        source_name: "db".into(),
        destination_name: "db-new".into(),
        operation: TransferOperation::Move,
        destination_key_id: None,
    }
}
fn manifest() -> TransferPlan {
    TransferPlan {
        schema_version: 1,
        intent: intent(),
        source_version: "v1".into(),
        files: vec![],
        destination_key: None,
        execution_supported: false,
    }
}
#[test]
fn authenticated_roundtrip_rejects_corruption_wrong_key_and_intent() {
    let identity = age::x25519::Identity::generate();
    let bytes = encode(&manifest(), &identity).unwrap();
    assert!(decode(&bytes, &identity, &intent()).is_ok());
    assert!(decode(&bytes, &age::x25519::Identity::generate(), &intent()).is_err());
    assert!(decode(&bytes[..bytes.len() - 1], &identity, &intent()).is_err());
    let mut changed = bytes.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    assert!(decode(&changed, &identity, &intent()).is_err());
    let mut other = intent();
    other.operation = TransferOperation::Copy;
    assert!(decode(&bytes, &identity, &other).is_err());
}
#[test]
fn rejects_invalid_intent_and_unknown_schema() {
    let mut m = manifest();
    m.intent.source_name = "../db".into();
    assert!(m.validate().is_err());
    m = manifest();
    m.schema_version = 2;
    assert!(m.validate().is_err());
    m = manifest();
    m.intent.source.identity.clear();
    assert!(m.validate().is_err());
    m = manifest();
    m.intent.destination_name = m.intent.source_name.clone();
    assert!(m.validate().is_err());
}

async fn fixture() -> (tempfile::TempDir, crate::backend::local::LocalBackend) {
    use crate::backend::{Backend, SecretBackend};
    use crate::config::settings::LocalConfig;
    use crate::secret::manager::SecretRequest;
    let dir = tempfile::tempdir().unwrap();
    let b = crate::backend::local::LocalBackend::new(Some(&LocalConfig {
        store_path: Some(dir.path().join("store").display().to_string()),
        key_file: Some(dir.path().join("identity").display().to_string()),
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
        b.attachment_keys().as_ref(),
        b.files().unwrap(),
        "default",
        upload("attachments/db/file"),
        None,
    )
    .await
    .unwrap();
    (dir, b)
}
fn upload(name: &str) -> crate::blob::models::FileUploadRequest {
    crate::blob::models::FileUploadRequest {
        name: name.into(),
        content: b"attachment-plaintext-canary".to_vec(),
        content_type: Some("text/custom".into()),
        groups: vec!["group".into()],
        tags: HashMap::from([("tag".into(), "private-tag-canary".into())]),
        metadata: HashMap::new(),
    }
}
#[tokio::test]
async fn preview_is_read_only_and_authenticates_source() {
    use crate::backend::{Backend, SecretBackend};
    let (_dir, b) = fixture().await;
    let before = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db/file")
        .await
        .unwrap();
    let secret_before = b
        .guarded_secrets()
        .get_secret("default", "db", false)
        .await
        .unwrap();
    let planned = plan(&b, &b, intent()).await.unwrap();
    assert_eq!(planned.files.len(), 1);
    let json = serde_json::to_string(&planned.preview()).unwrap();
    for forbidden in [
        "secret-value-canary",
        "attachment-plaintext-canary",
        "private-tag-canary",
    ] {
        assert!(!json.contains(forbidden));
    }
    let after = rewrap::snapshot(b.files().unwrap(), "default", "attachments/db/file")
        .await
        .unwrap();
    assert!(rewrap::same_info(&before.info, &after.info));
    assert_eq!(before.data.content, after.data.content);
    assert_eq!(
        secret_before.version,
        b.guarded_secrets()
            .get_secret("default", "db", false)
            .await
            .unwrap()
            .version
    );
    assert!(b
        .guarded_secrets()
        .get_secret("default", "db-new", false)
        .await
        .is_err());
    let mut tampered = upload("attachments/db/file");
    tampered.metadata = before.info.metadata;
    tampered.content = before.data.content;
    let last = tampered.content.len() - 1;
    tampered.content[last] ^= 1;
    b.files()
        .unwrap()
        .upload_file("default", tampered, None)
        .await
        .unwrap();
    assert!(plan(&b, &b, intent()).await.is_err());
}
#[tokio::test]
async fn orphan_destination_blocks_but_sibling_prefix_does_not() {
    use crate::backend::{Backend, SecretBackend};
    use crate::secret::manager::SecretRequest;
    let (dir, b) = fixture().await;
    for name in ["db-newer", "db-new"] {
        let secret_dir = dir.path().join("store/vaults/default/secrets");
        let before: BTreeSet<_> = std::fs::read_dir(&secret_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        b.guarded_secrets()
            .set_secret(
                "default",
                SecretRequest {
                    name: name.into(),
                    value: Zeroizing::new("x".into()),
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
        b.files()
            .unwrap()
            .upload_file(
                "default",
                upload(&format!("attachments/{name}/orphan")),
                None,
            )
            .await
            .unwrap();
        // Simulate an externally orphaned attachment by removing this fixture's
        // newly created active secret files, leaving the attachment in place.
        for entry in std::fs::read_dir(&secret_dir).unwrap() {
            let path = entry.unwrap().path();
            if !before.contains(&path) && path.is_file() {
                std::fs::remove_file(path).unwrap();
            }
        }
        if name == "db-newer" {
            assert!(plan(&b, &b, intent()).await.is_ok());
        }
    }
    assert!(plan(&b, &b, intent()).await.is_err());
}
#[tokio::test]
async fn manifest_rejects_duplicate_foreign_paths_unknown_fields_and_limits() {
    let (_dir, b) = fixture().await;
    let good = plan(&b, &b, intent()).await.unwrap();
    let identity = age::x25519::Identity::generate();
    let mut bad = good.clone();
    bad.files.push(bad.files[0].clone());
    assert!(encode(&bad, &identity).is_err());
    bad = good.clone();
    bad.files[0].source_name = "attachments/foreign/file".into();
    assert!(encode(&bad, &identity).is_err());
    bad = good.clone();
    bad.files[0].destination_name = "attachments/db-new/../file".into();
    assert!(encode(&bad, &identity).is_err());
    let mut json = serde_json::to_value(&good).unwrap();
    json["files"][0]["unknown"] = true.into();
    let encrypted = seal_json(&serde_json::to_vec(&json).unwrap(), &identity);
    assert!(decode(&encrypted, &identity, &intent()).is_err());
    assert!(read_manifest(std::io::repeat(0), &identity, &intent()).is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.age");
    persist(&path, &good, &identity).unwrap();
    assert!(read_manifest(std::fs::File::open(&path).unwrap(), &identity, &intent()).is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn public_recipient_cannot_forge_recovery_manifest() {
    let identity = age::x25519::Identity::generate();
    let forged = crypto::encrypt_bytes(
        &serde_json::to_vec(&manifest()).unwrap(),
        &[identity.to_public()],
    )
    .unwrap();
    assert!(decode(&forged, &identity, &intent()).is_err());
}

fn seal_json(json: &[u8], identity: &age::x25519::Identity) -> Vec<u8> {
    let ciphertext = crypto::encrypt_bytes(json, &[identity.to_public()]).unwrap();
    let mut mac = envelope_mac(identity);
    mac.update(&ciphertext);
    let mut bytes = ENVELOPE_MAGIC.to_vec();
    bytes.extend_from_slice(&mac.finalize().into_bytes());
    bytes.extend_from_slice(&ciphertext);
    bytes
}
#[tokio::test]
async fn cross_vault_requires_exact_destination_key_and_secret_collision_blocks() {
    use crate::backend::{Backend, SecretBackend};
    let (_source_dir, source) = fixture().await;
    let (_dest_dir, destination) = fixture().await;
    let mut i = intent();
    i.destination.identity = "local:b".into();
    assert!(plan(&source, &destination, i.clone()).await.is_err());
    let pointer = destination
        .attachment_keys()
        .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
        .await
        .unwrap();
    let Some(key::PointerKind::V2 { active, .. }) =
        key::parse_pointer_value(pointer.value.as_ref().unwrap())
    else {
        panic!("fixture ring is V2")
    };
    i.destination_key_id = Some(active.as_str().into());
    let planned = plan(&source, &destination, i.clone()).await.unwrap();
    assert_eq!(planned.destination_key.unwrap().key_id, active.as_str());
    i.destination_key_id = Some(
        key::AttachmentKeyId::derive("wrong-recipient")
            .as_str()
            .into(),
    );
    assert!(plan(&source, &destination, i).await.is_err());
    let original = source
        .guarded_secrets()
        .get_secret("default", "db", true)
        .await
        .unwrap();
    source
        .guarded_secrets()
        .set_secret(
            "default",
            crate::secret::manager::SecretRequest {
                name: "db-new".into(),
                value: original.value.unwrap(),
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
    assert!(plan(&source, &source, intent()).await.is_err());
}
#[test]
fn authenticated_schema_validation_rejects_unknown_progress_and_oversized_plaintext() {
    let identity = age::x25519::Identity::generate();
    let mut json = serde_json::to_value(manifest()).unwrap();
    json["progress"] = "destination_verified".into();
    assert!(decode(
        &seal_json(&serde_json::to_vec(&json).unwrap(), &identity),
        &identity,
        &intent()
    )
    .is_err());
    let oversized = vec![b' '; MAX_PLAINTEXT_BYTES + 1];
    assert!(decode(&seal_json(&oversized, &identity), &identity, &intent()).is_err());
}

#[tokio::test]
async fn legacy_pointer_is_refused_without_upgrading_it() {
    use crate::backend::Backend;
    let (_dir, backend) = fixture().await;
    let keys = backend.attachment_keys();
    let private = age::x25519::Identity::generate().to_string();
    keys.set_secret(
        "default",
        crate::secret::manager::SecretRequest {
            name: key::ACTIVE_POINTER_SECRET.into(),
            value: Zeroizing::new(private.expose_secret().clone()),
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
    let before = keys
        .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
        .await
        .unwrap();
    assert!(plan(&backend, &backend, intent()).await.is_err());
    let after = keys
        .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
        .await
        .unwrap();
    assert_eq!(before.version, after.version);
    assert_eq!(before.value, after.value);
}
