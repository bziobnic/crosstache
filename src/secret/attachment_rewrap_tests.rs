use super::*;
use crate::backend::{local::LocalBackend, Backend};
use crate::config::settings::LocalConfig;
use crate::secret::domain::SecretValue;
use crate::secret::domain::{SecretMetadata, SecretRequest};
use crate::secret::{attachment_lifecycle, attachment_rotation, attachments};
use age::secrecy::ExposeSecret;
use std::collections::HashMap;

fn request(name: &str, content: Vec<u8>) -> FileUploadRequest {
    FileUploadRequest {
        name: name.into(),
        content,
        content_type: Some("application/custom".into()),
        groups: vec!["group".into()],
        tags: HashMap::from([("tag".into(), "kept".into())]),
        metadata: HashMap::from([("user".into(), "kept".into())]),
    }
}
async fn fixture() -> (tempfile::TempDir, LocalBackend, AttachmentKeyId) {
    let dir = tempfile::tempdir().unwrap();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(dir.path().join("store").display().to_string()),
        key_file: Some(dir.path().join("identity").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    backend
        .secrets()
        .set_secret(
            "default",
            SecretRequest {
                name: "db".into(),
                value: SecretValue::new("db"),
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
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let identity = age::x25519::Identity::generate();
    keys.set_secret(
        "default",
        SecretRequest {
            name: key::ACTIVE_POINTER_SECRET.into(),
            value: SecretValue::new(identity.to_string().expose_secret()),
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
    let mut old = request(
        "attachments/db/a-preschema",
        crypto::encrypt_bytes(b"payload", &[identity.to_public()]).unwrap(),
    );
    old.metadata
        .insert(key::META_ENCRYPTED.into(), "age".into());
    files.upload_file("default", old, None).await.unwrap();
    attachments::upload_encrypted(
        keys.as_ref(),
        files,
        "default",
        request("attachments/db/b-legacy", b"payload".to_vec()),
        None,
    )
    .await
    .unwrap();
    attachment_lifecycle::upgrade(keys.as_ref(), "default", true)
        .await
        .unwrap();
    attachments::upload_encrypted(
        keys.as_ref(),
        files,
        "default",
        request("c-retained", b"payload".to_vec()),
        None,
    )
    .await
    .unwrap();
    let old_id = AttachmentKeyId::derive(&identity.to_public().to_string());
    let rotated = attachment_rotation::rotate(keys.as_ref(), "default", &old_id, true)
        .await
        .unwrap();
    let target = AttachmentKeyId::parse(rotated.new_key_id.as_deref().unwrap()).unwrap();
    attachments::upload_encrypted(
        keys.as_ref(),
        files,
        "default",
        request("attachments/db/d-target", b"payload".to_vec()),
        None,
    )
    .await
    .unwrap();
    drop(keys);
    (dir, backend, target)
}
#[tokio::test]
async fn mixed_sources_preview_apply_preserves_metadata_and_retry_skips() {
    let (_dir, backend, target) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let before = files
        .list_files(
            "default",
            FileListRequest {
                prefix: None,
                groups: None,
                limit: None,
                delimiter: None,
            },
        )
        .await
        .unwrap();
    let preview = rewrap(keys.as_ref(), files, "default", &target, false)
        .await
        .unwrap();
    assert_eq!(
        preview
            .files
            .iter()
            .filter(|f| f.outcome == "rewrap")
            .count(),
        3
    );
    for f in &before {
        assert_eq!(
            files.get_file_info("default", &f.name).await.unwrap().etag,
            f.etag
        );
    }
    rewrap(keys.as_ref(), files, "default", &target, true)
        .await
        .unwrap();
    for f in &before {
        let after = files
            .get_file_restore_info("default", &f.name)
            .await
            .unwrap();
        assert_eq!(after.tags, f.tags);
        assert_eq!(after.groups, f.groups);
        assert_eq!(after.content_type, f.content_type);
        for (k, v) in &f.metadata {
            if !key::RESERVED_CRYPTO_METADATA_KEYS.contains(&k.as_str()) {
                assert_eq!(after.metadata.get(k), Some(v));
            }
        }
        assert_eq!(after.metadata[key::META_KEY_ID], target.as_str());
        assert_eq!(
            attachments::download_decrypted(keys.as_ref(), files, "default", &f.name, None)
                .await
                .unwrap(),
            b"payload"
        );
        if f.name.ends_with("d-target") {
            assert_eq!(after.etag, f.etag);
        }
    }
    let retry = rewrap(keys.as_ref(), files, "default", &target, true)
        .await
        .unwrap();
    assert!(retry.files.iter().all(|f| f.outcome == "verified"));
    assert!(!serde_json::to_string(&retry)
        .unwrap()
        .contains("AGE-SECRET"));
}
#[tokio::test]
async fn invalid_target_or_last_tampered_file_prevents_every_write() {
    for mode in ["target", "tampered", "partial", "unknown"] {
        let (_dir, backend, target) = fixture().await;
        let files = backend.files().unwrap();
        let before = files
            .get_file_restore_info("default", "attachments/db/a-preschema")
            .await
            .unwrap();
        if mode != "target" {
            let mut req = request("z-outside", b"garbage".to_vec());
            req.metadata.insert(
                if mode == "partial" {
                    key::META_KEY_ID
                } else {
                    key::META_ENCRYPTED
                }
                .into(),
                if mode == "unknown" { "other" } else { "age" }.into(),
            );
            files.upload_file("default", req, None).await.unwrap();
        }
        let expected = if mode == "target" {
            AttachmentKeyId::derive("wrong")
        } else {
            target
        };
        assert!(
            rewrap(
                backend.attachment_keys().as_ref(),
                files,
                "default",
                &expected,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
        assert_eq!(
            files
                .get_file_restore_info("default", &before.name)
                .await
                .unwrap()
                .etag,
            before.etag
        );
    }
}

use crate::backend::{file::FileDownloadSnapshot, BackendError};
use crate::blob::models::FileInfo;
use crate::utils::progress::ProgressReporter;
use std::sync::atomic::{AtomicUsize, Ordering};
struct FaultFiles<'a> {
    inner: &'a dyn FileBackend,
    mode: &'static str,
    writes: AtomicUsize,
    lists: AtomicUsize,
    snapshots: AtomicUsize,
}
impl<'a> FaultFiles<'a> {
    fn new(inner: &'a dyn FileBackend, mode: &'static str) -> Self {
        Self {
            inner,
            mode,
            writes: AtomicUsize::new(0),
            lists: AtomicUsize::new(0),
            snapshots: AtomicUsize::new(0),
        }
    }
}
#[async_trait::async_trait]
impl FileBackend for FaultFiles<'_> {
    async fn get_file_restore_info(
        &self,
        v: &str,
        n: &str,
    ) -> std::result::Result<FileInfo, BackendError> {
        self.inner.get_file_restore_info(v, n).await
    }
    async fn restore_file(
        &self,
        v: &str,
        r: FileUploadRequest,
    ) -> std::result::Result<FileInfo, BackendError> {
        let writes = self.writes.fetch_add(1, Ordering::SeqCst);
        if self.mode == "upload" && writes == 1 {
            return Err(BackendError::Network("interrupted".into()));
        }
        self.inner.restore_file(v, r).await
    }
    async fn upload_file(
        &self,
        v: &str,
        r: FileUploadRequest,
        p: Option<&dyn ProgressReporter>,
    ) -> std::result::Result<FileInfo, BackendError> {
        self.inner.upload_file(v, r, p).await
    }
    async fn download_file(
        &self,
        v: &str,
        n: &str,
        p: Option<&dyn ProgressReporter>,
    ) -> std::result::Result<Vec<u8>, BackendError> {
        self.inner.download_file(v, n, p).await
    }
    async fn download_file_snapshot(
        &self,
        v: &str,
        n: &str,
        p: Option<&dyn ProgressReporter>,
    ) -> std::result::Result<FileDownloadSnapshot, BackendError> {
        let reads = self.snapshots.fetch_add(1, Ordering::SeqCst);
        if self.mode == "readback" && self.writes.load(Ordering::SeqCst) == 1 {
            return Err(BackendError::Network("readback interrupted".into()));
        }
        let mut snap = self.inner.download_file_snapshot(v, n, p).await?;
        if self.mode == "ciphertext-drift" && reads >= 4 {
            let last = snap.content.len() - 1;
            snap.content[last] ^= 1;
        }
        Ok(snap)
    }
    async fn list_files(
        &self,
        v: &str,
        r: FileListRequest,
    ) -> std::result::Result<Vec<FileInfo>, BackendError> {
        let read = self.lists.fetch_add(1, Ordering::SeqCst);
        let mut list = self.inner.list_files(v, r).await?;
        if self.mode == "sparse" {
            for f in &mut list {
                f.metadata.clear();
            }
        }
        if self.mode == "duplicate" {
            list.push(list[0].clone());
        }
        if self.mode == "inventory-drift" && read > 0 {
            list.pop();
        }
        Ok(list)
    }
    async fn delete_file(&self, v: &str, n: &str) -> std::result::Result<(), BackendError> {
        self.inner.delete_file(v, n).await
    }
    async fn get_file_info(&self, v: &str, n: &str) -> std::result::Result<FileInfo, BackendError> {
        self.inner.get_file_info(v, n).await
    }
}
#[tokio::test]
async fn sparse_listing_refreshes_metadata_and_preview_never_writes() {
    let (_dir, backend, target) = fixture().await;
    let files = FaultFiles::new(backend.files().unwrap(), "sparse");
    let report = rewrap(
        backend.attachment_keys().as_ref(),
        &files,
        "default",
        &target,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.files.len(), 4);
    assert_eq!(files.writes.load(Ordering::SeqCst), 0);
    rewrap(
        backend.attachment_keys().as_ref(),
        &files,
        "default",
        &target,
        true,
    )
    .await
    .unwrap();
    assert_eq!(files.writes.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn duplicate_inventory_and_drift_are_rejected_before_write() {
    for mode in ["duplicate", "inventory-drift", "ciphertext-drift"] {
        let (_dir, backend, target) = fixture().await;
        let files = FaultFiles::new(backend.files().unwrap(), mode);
        assert!(
            rewrap(
                backend.attachment_keys().as_ref(),
                &files,
                "default",
                &target,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
        assert_eq!(files.writes.load(Ordering::SeqCst), 0, "{mode}");
    }
}
#[tokio::test]
async fn interrupted_replacement_and_readback_resume_without_rewriting_completed_files() {
    for mode in ["upload", "readback"] {
        let (_dir, backend, target) = fixture().await;
        let files = FaultFiles::new(backend.files().unwrap(), mode);
        assert!(
            rewrap(
                backend.attachment_keys().as_ref(),
                &files,
                "default",
                &target,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
        let done = backend
            .files()
            .unwrap()
            .get_file_restore_info("default", "attachments/db/a-preschema")
            .await
            .unwrap();
        assert_eq!(done.metadata[key::META_KEY_ID], target.as_str());
        let retry = rewrap(
            backend.attachment_keys().as_ref(),
            backend.files().unwrap(),
            "default",
            &target,
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            retry
                .files
                .iter()
                .filter(|f| f.outcome == "rewrapped")
                .count(),
            2
        );
        assert_eq!(
            backend
                .files()
                .unwrap()
                .get_file_restore_info("default", &done.name)
                .await
                .unwrap()
                .etag,
            done.etag
        );
    }
}

struct FaultKeys<'a> {
    inner: Box<dyn AttachmentKeyStore + 'a>,
    mode: &'static str,
    reads: AtomicUsize,
}
#[async_trait::async_trait]
impl AttachmentKeyStore for FaultKeys<'_> {
    async fn get_secret_metadata(
        &self,
        v: &str,
        n: &str,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        let mut p = self.inner.get_secret_metadata(v, n).await?;
        if n == key::ACTIVE_POINTER_SECRET {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            if self.mode == "pointer-drift" && read >= 2 {
                p.version = "changed".into();
            }
            if self.mode == "pointer-disabled" {
                p.enabled = false;
            }
            // The "v1" and "no-legacy" faults substitute the pointer value;
            // neither has a metadata-path equivalent.
        } else {
            if self.mode == "current-unmarked" {
                p.content_type = "ordinary".into();
            }
            if self.mode == "target-drift" && self.reads.load(Ordering::SeqCst) >= 2 {
                p.version = "changed".into();
            }
        }
        Ok(p)
    }

    async fn get_secret(&self, v: &str, n: &str) -> std::result::Result<Secret, BackendError> {
        let mut p = self.inner.get_secret(v, n).await?;
        if n == key::ACTIVE_POINTER_SECRET {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            if self.mode == "pointer-drift" && read >= 2 {
                p.version = "changed".into();
            }
            if self.mode == "pointer-disabled" {
                p.enabled = false;
            }
            if self.mode == "v1" {
                p.value = SecretValue::new(
                    age::x25519::Identity::generate()
                        .to_string()
                        .expose_secret(),
                );
            }
            if self.mode == "no-legacy" {
                if let Some(PointerKind::V2 { active, .. }) =
                    key::parse_pointer_value(p.value.expose_secret())
                {
                    p.value = SecretValue::new(key::format_v2_pointer(&active, None));
                }
            }
        } else {
            if self.mode == "current-unmarked" {
                p.content_type = "ordinary".into();
            }
            if self.mode == "target-drift" && self.reads.load(Ordering::SeqCst) >= 2 {
                p.version = "changed".into();
            }
        }
        Ok(p)
    }
    async fn get_secret_version_metadata(
        &self,
        v: &str,
        n: &str,
        version: &str,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        let mut p = self
            .inner
            .get_secret_version_metadata(v, n, version)
            .await?;
        match self.mode {
            "exact-version" => p.version = "wrong".into(),
            "exact-disabled" => p.enabled = false,
            "exact-unmarked" => p.content_type = "ordinary".into(),
            // The wrong-identity fault is a value substitution; it has no
            // metadata-path equivalent.
            _ => {}
        }
        Ok(p)
    }

    async fn get_secret_version(
        &self,
        v: &str,
        n: &str,
        version: &str,
    ) -> std::result::Result<Secret, BackendError> {
        let mut p = self.inner.get_secret_version(v, n, version).await?;
        match self.mode {
            "exact-version" => p.version = "wrong".into(),
            "exact-disabled" => p.enabled = false,
            "exact-unmarked" => p.content_type = "ordinary".into(),
            "exact-wrong-identity" => {
                p.value = SecretValue::new(
                    age::x25519::Identity::generate()
                        .to_string()
                        .expose_secret(),
                )
            }
            _ => {}
        }
        Ok(p)
    }
    async fn set_secret(
        &self,
        _: &str,
        _: SecretRequest,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        panic!("rewrap must never mutate custody")
    }
    async fn commit_retained_key(
        &self,
        _: &str,
        _: SecretRequest,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        panic!("rewrap must never mutate custody")
    }
}
#[tokio::test]
async fn unhealthy_ring_missing_binding_and_custody_drift_never_replace_files() {
    for mode in [
        "pointer-drift",
        "target-drift",
        "pointer-disabled",
        "v1",
        "no-legacy",
        "current-unmarked",
        "exact-version",
        "exact-disabled",
        "exact-unmarked",
        "exact-wrong-identity",
    ] {
        let (_dir, backend, target) = fixture().await;
        let keys = FaultKeys {
            inner: backend.attachment_keys(),
            mode,
            reads: AtomicUsize::new(0),
        };
        let files = FaultFiles::new(backend.files().unwrap(), "none");
        assert!(
            rewrap(&keys, &files, "default", &target, true)
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(files.writes.load(Ordering::SeqCst), 0, "{mode}");
    }
}
#[tokio::test]
async fn schema1_missing_encryption_marker_is_partial_even_inside_namespace() {
    let (_dir, backend, target) = fixture().await;
    let files = backend.files().unwrap();
    let name = "attachments/db/d-target";
    let snap = files
        .download_file_snapshot("default", name, None)
        .await
        .unwrap();
    let mut req = request(name, snap.content);
    req.metadata = snap.metadata;
    req.metadata.remove(key::META_ENCRYPTED);
    files.restore_file("default", req).await.unwrap();
    let wrapped = FaultFiles::new(files, "none");
    assert!(rewrap(
        backend.attachment_keys().as_ref(),
        &wrapped,
        "default",
        &target,
        true
    )
    .await
    .is_err());
    assert_eq!(wrapped.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn authenticates_already_target_ciphertext_and_rejects_unknown_schema_before_writes() {
    for mode in ["tamper", "schema"] {
        let (_dir, backend, target) = fixture().await;
        let files = backend.files().unwrap();
        let name = "attachments/db/d-target";
        let mut snap = files
            .download_file_snapshot("default", name, None)
            .await
            .unwrap();
        if mode == "tamper" {
            let last = snap.content.len() - 1;
            snap.content[last] ^= 1;
        } else {
            snap.metadata
                .insert(key::META_CRYPTO_SCHEMA.into(), "2".into());
        }
        let mut req = request(name, snap.content);
        req.metadata = snap.metadata;
        files.restore_file("default", req).await.unwrap();
        let wrapped = FaultFiles::new(files, "none");
        assert!(
            rewrap(
                backend.attachment_keys().as_ref(),
                &wrapped,
                "default",
                &target,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
        assert_eq!(wrapped.writes.load(Ordering::SeqCst), 0);
    }
}
