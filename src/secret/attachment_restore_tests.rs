use super::*;
use crate::backend::{local::LocalBackend, Backend};
use crate::config::settings::LocalConfig;
use crate::secret::attachment_backup_codec::{IdentityRecord, ManifestFile, SourceRef};
use crate::secret::domain::SecretMetadata;
use age::secrecy::ExposeSecret;

async fn fixture() -> (tempfile::TempDir, LocalBackend, Bundle, Vec<u8>) {
    let tmp = tempfile::tempdir().unwrap();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(tmp.path().join("store").display().to_string()),
        key_file: Some(tmp.path().join("identity").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    backend
        .secrets()
        .set_secret(
            "default",
            request("db", Zeroizing::new("ordinary secret".into()), false),
        )
        .await
        .unwrap();
    let identity = age::x25519::Identity::generate();
    let id = key::AttachmentKeyId::derive(&identity.to_public().to_string());
    let ciphertext = crypto::encrypt_bytes(b"original plaintext", &[identity.to_public()]).unwrap();
    let reference = SourceRef {
        key_id: id.as_str().into(),
        slot: "legacy".into(),
        provider_version: "source-version-token".into(),
    };
    let mut metadata = std::collections::HashMap::from([("user".into(), "preserved".into())]);
    key::apply_crypto_metadata(
        &mut metadata,
        &key::AttachmentKeyRef {
            key_id: id.clone(),
            slot: KeySlot::Legacy,
            provider_version: SecretVersion::new("source-version-token"),
        },
    );
    backend
        .files()
        .unwrap()
        .upload_file(
            "default",
            FileUploadRequest {
                name: "attachments/db/file".into(),
                content: ciphertext.clone(),
                content_type: Some("application/custom".into()),
                groups: vec!["group".into()],
                metadata,
                tags: std::collections::HashMap::from([("tag".into(), "value".into())]),
            },
            None,
        )
        .await
        .unwrap();
    let bundle = Bundle {
        format: "xv-attachment-key-backup".into(),
        schema_version: 1,
        source_backend: "local".into(),
        source_vault: "source".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
        active_key_id: id.as_str().into(),
        legacy_key_id: Some(id.as_str().into()),
        identities: vec![IdentityRecord {
            key_id: id.as_str().into(),
            identity: Zeroizing::new(identity.to_string().expose_secret().to_owned()),
        }],
        references: vec![reference.clone()],
        files: vec![ManifestFile {
            name: "attachments/db/file".into(),
            ciphertext_sha256: hex::encode(Sha256::digest(&ciphertext)),
            key_id: id.as_str().into(),
            source_ref: Some(reference),
        }],
    };
    (tmp, backend, bundle, ciphertext)
}

#[tokio::test]
async fn attachment_restore_cross_vault_preview_apply_and_retry() {
    let (_tmp, backend, bundle, ciphertext) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let before = files
        .get_file_info("default", &bundle.files[0].name)
        .await
        .unwrap();
    let preview = restore(keys.as_ref(), files, "default", &bundle, false, false)
        .await
        .unwrap();
    assert_eq!(preview.outcome, "ready");
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
    assert!(matches!(
        keys.get_secret("default", key::ACTIVE_POINTER_SECRET).await,
        Err(BackendError::NotFound { .. })
    ));
    restore(keys.as_ref(), files, "default", &bundle, true, false)
        .await
        .unwrap();
    let snapshot = files
        .download_file_snapshot("default", &bundle.files[0].name, None)
        .await
        .unwrap();
    assert_eq!(snapshot.content, ciphertext);
    assert_ne!(
        snapshot.metadata[key::META_KEY_VERSION],
        "source-version-token"
    );
    assert_eq!(snapshot.metadata["user"], "preserved");
    let after = files
        .get_file_info("default", &bundle.files[0].name)
        .await
        .unwrap();
    assert_eq!(after.tags, before.tags);
    assert_eq!(after.groups, before.groups);
    assert_eq!(after.content_type, before.content_type);
    assert_eq!(
        crate::secret::attachments::download_decrypted(
            keys.as_ref(),
            files,
            "default",
            &bundle.files[0].name,
            None
        )
        .await
        .unwrap(),
        b"original plaintext"
    );
    restore(keys.as_ref(), files, "default", &bundle, true, false)
        .await
        .unwrap();
    assert_eq!(
        after.etag,
        files
            .get_file_info("default", &bundle.files[0].name)
            .await
            .unwrap()
            .etag
    );
}

#[tokio::test]
async fn attachment_restore_malformed_pointer_requires_explicit_repair() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    keys.set_secret(
        "default",
        request(
            key::ACTIVE_POINTER_SECRET,
            Zeroizing::new("malformed-private-value".into()),
            false,
        ),
    )
    .await
    .unwrap();
    assert!(
        restore(keys.as_ref(), files, "default", &bundle, true, false)
            .await
            .is_err()
    );
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
    let preview = restore(keys.as_ref(), files, "default", &bundle, false, true)
        .await
        .unwrap();
    assert_eq!(preview.pointer_outcome, "repair");
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("malformed-private-value"));
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
    restore(keys.as_ref(), files, "default", &bundle, true, true)
        .await
        .unwrap();
    restore(keys.as_ref(), files, "default", &bundle, true, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn attachment_restore_conflicts_fail_before_retained_writes() {
    for damage in [
        "v1",
        "v2-active",
        "v2-legacy",
        "collision",
        "disabled",
        "ciphertext",
        "reference",
        "extra",
    ] {
        let (_tmp, backend, bundle, _) = fixture().await;
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        let other = age::x25519::Identity::generate();
        let other_id = AttachmentKeyId::derive(&other.to_public().to_string());
        match damage {
            "v1" => {
                keys.set_secret(
                    "default",
                    request(
                        key::ACTIVE_POINTER_SECRET,
                        Zeroizing::new(other.to_string().expose_secret().to_owned()),
                        false,
                    ),
                )
                .await
                .unwrap();
            }
            "v2-active" | "v2-legacy" => {
                let active = if damage == "v2-active" {
                    other_id.clone()
                } else {
                    id(&bundle.active_key_id).unwrap()
                };
                keys.set_secret(
                    "default",
                    request(
                        key::ACTIVE_POINTER_SECRET,
                        Zeroizing::new(key::format_v2_pointer(&active, Some(&other_id))),
                        false,
                    ),
                )
                .await
                .unwrap();
            }
            "collision" | "disabled" => {
                let mut req = request(
                    &key::retained_record_name(&id(&bundle.active_key_id).unwrap()),
                    bundle.identities[0].identity.clone(),
                    damage == "disabled",
                );
                if damage == "disabled" {
                    req.enabled = Some(false);
                }
                backend.secrets().set_secret("default", req).await.unwrap();
            }
            _ => {
                let snap = files
                    .download_file_snapshot("default", &bundle.files[0].name, None)
                    .await
                    .unwrap();
                let mut req = FileUploadRequest {
                    name: bundle.files[0].name.clone(),
                    content: snap.content,
                    content_type: None,
                    groups: vec![],
                    metadata: snap.metadata,
                    tags: HashMap::new(),
                };
                match damage {
                    "ciphertext" => req.content.push(1),
                    "reference" => {
                        req.metadata
                            .insert(key::META_KEY_ID.into(), other_id.as_str().into());
                    }
                    "extra" => req.name = "attachments/db/extra".into(),
                    _ => unreachable!(),
                }
                files.upload_file("default", req, None).await.unwrap();
            }
        }
        let before = keys.list_retained_keys("default").await.unwrap().len();
        assert!(
            restore(keys.as_ref(), files, "default", &bundle, true, true)
                .await
                .is_err(),
            "{damage}"
        );
        assert_eq!(
            keys.list_retained_keys("default").await.unwrap().len(),
            before,
            "{damage}"
        );
    }
}

struct InterruptFiles<'a> {
    inner: &'a dyn FileBackend,
    after_write: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl FileBackend for InterruptFiles<'_> {
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
        let out = self.inner.restore_file(v, r).await?;
        if self
            .after_write
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(BackendError::Network(
                "interrupted after replacement".into(),
            ));
        }
        Ok(out)
    }

    async fn upload_file(
        &self,
        v: &str,
        r: FileUploadRequest,
        p: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<FileInfo, BackendError> {
        let out = self.inner.upload_file(v, r, p).await?;
        if self
            .after_write
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(BackendError::Network(
                "interrupted after replacement".into(),
            ));
        }
        Ok(out)
    }
    async fn download_file(
        &self,
        v: &str,
        n: &str,
        p: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<Vec<u8>, BackendError> {
        self.inner.download_file(v, n, p).await
    }
    async fn download_file_snapshot(
        &self,
        v: &str,
        n: &str,
        p: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<FileDownloadSnapshot, BackendError> {
        self.inner.download_file_snapshot(v, n, p).await
    }
    async fn list_files(
        &self,
        v: &str,
        r: FileListRequest,
    ) -> std::result::Result<Vec<FileInfo>, BackendError> {
        self.inner.list_files(v, r).await
    }
    async fn delete_file(&self, v: &str, n: &str) -> std::result::Result<(), BackendError> {
        self.inner.delete_file(v, n).await
    }
    async fn get_file_info(&self, v: &str, n: &str) -> std::result::Result<FileInfo, BackendError> {
        self.inner.get_file_info(v, n).await
    }
}
#[tokio::test]
async fn attachment_restore_retry_after_unconfirmed_file_replacement() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let interrupted = InterruptFiles {
        inner: files,
        after_write: std::sync::atomic::AtomicBool::new(true),
    };
    assert!(
        restore(keys.as_ref(), &interrupted, "default", &bundle, true, false)
            .await
            .is_err()
    );
    assert!(matches!(
        keys.get_secret("default", key::ACTIVE_POINTER_SECRET).await,
        Err(BackendError::NotFound { .. })
    ));
    let key_before = retained(keys.as_ref(), "default", &bundle.active_key_id)
        .await
        .unwrap()
        .unwrap();
    let file_before = files
        .get_file_info("default", &bundle.files[0].name)
        .await
        .unwrap();
    restore(keys.as_ref(), files, "default", &bundle, true, false)
        .await
        .unwrap();
    assert_eq!(
        retained(keys.as_ref(), "default", &bundle.active_key_id)
            .await
            .unwrap()
            .unwrap(),
        key_before
    );
    assert_eq!(
        files
            .get_file_info("default", &bundle.files[0].name)
            .await
            .unwrap()
            .etag,
        file_before.etag
    );
}

struct FaultKeys<'a> {
    inner: &'a dyn AttachmentKeyStore,
    mode: &'static str,
    pointer_reads: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl AttachmentKeyStore for FaultKeys<'_> {
    async fn preflight_set_secret(
        &self,
        v: &str,
        n: &str,
    ) -> std::result::Result<(), BackendError> {
        self.inner.preflight_set_secret(v, n).await
    }

    async fn get_secret_metadata(
        &self,
        v: &str,
        n: &str,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        if n == key::ACTIVE_POINTER_SECRET
            && self.mode == "pointer-drift"
            && self
                .pointer_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 1
        {
            self.inner
                .set_secret(
                    v,
                    request(n, Zeroizing::new("changed after preflight".into()), false),
                )
                .await?;
        }
        // A value-free custody read is no longer representable on the metadata
        // path; the omitted-pointer fault is injected on the value read instead.
        self.inner.get_secret_metadata(v, n).await
    }

    async fn get_secret(&self, v: &str, n: &str) -> std::result::Result<Secret, BackendError> {
        if n == key::ACTIVE_POINTER_SECRET
            && self.mode == "pointer-drift"
            && self
                .pointer_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 1
        {
            self.inner
                .set_secret(
                    v,
                    request(n, Zeroizing::new("changed after preflight".into()), false),
                )
                .await?;
        }
        let mut p = self.inner.get_secret(v, n).await?;
        if n == key::ACTIVE_POINTER_SECRET && self.mode == "omitted-pointer" {
            p.value = crate::secret::domain::SecretValue::new(String::new());
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
            "wrong-version" => p.version = "different-returned-version".into(),
            "disabled-exact" => p.enabled = false,
            "omitted-exact" => {}
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
            "wrong-version" => p.version = "different-returned-version".into(),
            "disabled-exact" => p.enabled = false,
            "omitted-exact" => p.value = crate::secret::domain::SecretValue::new(String::new()),
            _ => {}
        }
        Ok(p)
    }
    async fn commit_retained_key(
        &self,
        v: &str,
        r: SecretRequest,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        let p = self.inner.commit_retained_key(v, r).await?;
        if self.mode == "commit-conflict" {
            return Err(BackendError::Conflict("concurrent winner".into()));
        }
        if self.mode == "after-commit" {
            return Err(BackendError::Network("interrupted after commit".into()));
        }
        Ok(p)
    }
    async fn set_secret(
        &self,
        v: &str,
        r: SecretRequest,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        let mut p = self.inner.set_secret(v, r).await?;
        if self.mode == "after-pointer" {
            return Err(BackendError::Network("interrupted after pointer".into()));
        }
        if self.mode == "pointer-readback" {
            p.version = "incorrect-commit-version".into();
        }
        Ok(p)
    }
}
#[tokio::test]
async fn attachment_restore_faults_fail_and_retry_reuses_committed_keys() {
    for mode in [
        "wrong-version",
        "disabled-exact",
        "omitted-exact",
        "after-commit",
        "pointer-drift",
        "after-pointer",
        "pointer-readback",
        "omitted-pointer",
    ] {
        let (_tmp, backend, bundle, _) = fixture().await;
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        if mode == "omitted-pointer" {
            keys.set_secret(
                "default",
                request(
                    key::ACTIVE_POINTER_SECRET,
                    Zeroizing::new("malformed".into()),
                    false,
                ),
            )
            .await
            .unwrap();
        }
        let faulty = FaultKeys {
            inner: keys.as_ref(),
            mode,
            pointer_reads: std::sync::atomic::AtomicUsize::new(0),
        };
        assert!(
            restore(&faulty, files, "default", &bundle, true, true)
                .await
                .is_err(),
            "{mode}"
        );
        let retained_before = retained(keys.as_ref(), "default", &bundle.active_key_id)
            .await
            .unwrap();
        restore(keys.as_ref(), files, "default", &bundle, true, true)
            .await
            .unwrap();
        if let Some(before) = retained_before {
            assert_eq!(
                retained(keys.as_ref(), "default", &bundle.active_key_id)
                    .await
                    .unwrap()
                    .unwrap(),
                before,
                "{mode}"
            );
        }
    }
}

struct FaultSnapshots<'a> {
    inner: &'a dyn FileBackend,
    fail_at: usize,
    reads: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl FileBackend for FaultSnapshots<'_> {
    async fn get_file_restore_info(
        &self,
        v: &str,
        n: &str,
    ) -> std::result::Result<FileInfo, BackendError> {
        if self.fail_at == 0 {
            return Err(BackendError::PermissionDenied(
                "strict tag read denied".into(),
            ));
        }
        self.inner.get_file_restore_info(v, n).await
    }
    async fn restore_file(
        &self,
        v: &str,
        r: FileUploadRequest,
    ) -> std::result::Result<FileInfo, BackendError> {
        self.inner.restore_file(v, r).await
    }

    async fn upload_file(
        &self,
        v: &str,
        r: FileUploadRequest,
        p: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<FileInfo, BackendError> {
        self.inner.upload_file(v, r, p).await
    }
    async fn download_file(
        &self,
        v: &str,
        n: &str,
        p: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<Vec<u8>, BackendError> {
        self.inner.download_file(v, n, p).await
    }
    async fn download_file_snapshot(
        &self,
        v: &str,
        n: &str,
        p: Option<&dyn crate::utils::progress::ProgressReporter>,
    ) -> std::result::Result<FileDownloadSnapshot, BackendError> {
        let mut snap = self.inner.download_file_snapshot(v, n, p).await?;
        if self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == self.fail_at {
            snap.content[0] ^= 1;
        }
        Ok(snap)
    }
    async fn list_files(
        &self,
        v: &str,
        r: FileListRequest,
    ) -> std::result::Result<Vec<FileInfo>, BackendError> {
        let mut listed = self.inner.list_files(v, r).await?;
        for entry in &mut listed {
            entry.metadata.clear();
        }
        Ok(listed)
    }
    async fn delete_file(&self, v: &str, n: &str) -> std::result::Result<(), BackendError> {
        self.inner.delete_file(v, n).await
    }
    async fn get_file_info(&self, v: &str, n: &str) -> std::result::Result<FileInfo, BackendError> {
        self.inner.get_file_info(v, n).await
    }
}
#[tokio::test]
async fn attachment_restore_snapshot_drift_and_bad_readback_do_not_publish() {
    for fail_at in [2, 3] {
        let (_tmp, backend, bundle, _) = fixture().await;
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        let faulty = FaultSnapshots {
            inner: files,
            fail_at,
            reads: std::sync::atomic::AtomicUsize::new(0),
        };
        assert!(
            restore(keys.as_ref(), &faulty, "default", &bundle, true, false)
                .await
                .is_err()
        );
        assert!(matches!(
            keys.get_secret("default", key::ACTIVE_POINTER_SECRET).await,
            Err(BackendError::NotFound { .. })
        ));
        restore(keys.as_ref(), files, "default", &bundle, true, false)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn attachment_restore_empty_manifest_repairs_missing_ring() {
    let (_tmp, backend, mut bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    files
        .delete_file("default", &bundle.files[0].name)
        .await
        .unwrap();
    bundle.files.clear();
    restore(keys.as_ref(), files, "default", &bundle, true, false)
        .await
        .unwrap();
    assert!(retained(keys.as_ref(), "default", &bundle.active_key_id)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn attachment_restore_verifies_concurrent_create_winner() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let faulty = FaultKeys {
        inner: keys.as_ref(),
        mode: "commit-conflict",
        pointer_reads: std::sync::atomic::AtomicUsize::new(0),
    };
    restore(
        &faulty,
        backend.files().unwrap(),
        "default",
        &bundle,
        true,
        false,
    )
    .await
    .unwrap();
    assert_eq!(keys.list_retained_keys("default").await.unwrap().len(), 1);
}

#[tokio::test]
async fn attachment_restore_policy_pointer_denial_preflights_before_all_writes() {
    use crate::agent::{
        enforce::PolicyEnforcedBackend, policy::CompiledPolicy, AgentIdentity, IdentitySource,
    };
    use crate::config::settings::{AgentConfig, AgentPolicyRule};
    let (tmp, backend, bundle, ciphertext) = fixture().await;
    let raw = std::sync::Arc::new(backend);
    let policy = CompiledPolicy::compile(&AgentConfig {
        enforce: true,
        policy: vec![
            AgentPolicyRule {
                name: "read custody".into(),
                identity: "github:o/r:*".into(),
                identity_source: "github-oidc".into(),
                workspace: "default".into(),
                secrets: vec!["xv-attachment-key".into(), "xv-attachment-key-ak1-*".into()],
                operations: vec!["get".into()],
                raw_disclosure: true,
                ..Default::default()
            },
            AgentPolicyRule {
                name: "write retained only".into(),
                identity: "github:o/r:*".into(),
                identity_source: "github-oidc".into(),
                workspace: "default".into(),
                secrets: vec!["xv-attachment-key-ak1-*".into()],
                operations: vec!["set".into()],
                ..Default::default()
            },
        ],
        ..Default::default()
    })
    .unwrap();
    let log_path = tmp.path().join("restore-decisions.jsonl");
    let wrapped = PolicyEnforcedBackend::for_test(
        raw.clone(),
        AgentIdentity::new(IdentitySource::GithubOidc, "github:o/r:ci"),
        policy,
        log_path.clone(),
    );
    for apply in [false, true] {
        assert!(restore(
            wrapped.attachment_keys().as_ref(),
            wrapped.files().unwrap(),
            "default",
            &bundle,
            apply,
            false
        )
        .await
        .is_err());
        assert!(raw
            .attachment_keys()
            .list_retained_keys("default")
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            raw.attachment_keys()
                .get_secret("default", key::ACTIVE_POINTER_SECRET)
                .await,
            Err(BackendError::NotFound { .. })
        ));
        let snap = raw
            .files()
            .unwrap()
            .download_file_snapshot("default", &bundle.files[0].name, None)
            .await
            .unwrap();
        assert_eq!(snap.content, ciphertext);
        assert_eq!(snap.metadata[key::META_KEY_VERSION], "source-version-token");
    }
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(!log.contains("AGE-SECRET-KEY-"));
    assert!(!log.contains("original plaintext"));
}

#[tokio::test]
async fn attachment_restore_refreshes_sparse_listing_metadata_before_mutation() {
    let (_tmp, backend, bundle, ciphertext) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    files
        .upload_file(
            "default",
            FileUploadRequest {
                name: "docs/managed.bin".into(),
                content: ciphertext,
                content_type: None,
                groups: vec![],
                metadata: HashMap::from([(key::META_ENCRYPTED.into(), key::ENC_VALUE_AGE.into())]),
                tags: HashMap::new(),
            },
            None,
        )
        .await
        .unwrap();
    let sparse = FaultSnapshots {
        inner: files,
        fail_at: usize::MAX,
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    assert!(
        restore(keys.as_ref(), &sparse, "default", &bundle, true, false)
            .await
            .is_err()
    );
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
}

#[tokio::test]
async fn attachment_restore_strict_tag_read_denial_has_zero_writes() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = FaultSnapshots {
        inner: backend.files().unwrap(),
        fail_at: 0,
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    assert!(
        restore(keys.as_ref(), &files, "default", &bundle, true, false)
            .await
            .is_err()
    );
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
}

#[tokio::test]
async fn attachment_restore_policy_exact_readback_denial_preflights_before_writes() {
    use crate::agent::{
        enforce::PolicyEnforcedBackend, policy::CompiledPolicy, AgentIdentity, IdentitySource,
    };
    use crate::config::settings::{AgentConfig, AgentPolicyRule};
    let (tmp, backend, bundle, _) = fixture().await;
    let raw = std::sync::Arc::new(backend);
    let rule = |name: &str, secrets: Vec<String>, operations: Vec<String>, raw_disclosure| {
        AgentPolicyRule {
            name: name.into(),
            identity: "github:o/r:*".into(),
            identity_source: "github-oidc".into(),
            workspace: "default".into(),
            secrets,
            operations,
            raw_disclosure,
            ..Default::default()
        }
    };
    let policy = CompiledPolicy::compile(&AgentConfig {
        enforce: true,
        policy: vec![
            rule(
                "pointer access",
                vec!["xv-attachment-key".into()],
                vec!["get".into(), "set".into()],
                true,
            ),
            rule(
                "retained metadata and writes",
                vec!["xv-attachment-key-ak1-*".into()],
                vec!["get".into(), "set".into()],
                false,
            ),
        ],
        ..Default::default()
    })
    .unwrap();
    let wrapped = PolicyEnforcedBackend::for_test(
        raw.clone(),
        AgentIdentity::new(IdentitySource::GithubOidc, "github:o/r:ci"),
        policy,
        tmp.path().join("readback-decisions.jsonl"),
    );
    for apply in [false, true] {
        assert!(restore(
            wrapped.attachment_keys().as_ref(),
            wrapped.files().unwrap(),
            "default",
            &bundle,
            apply,
            false
        )
        .await
        .is_err());
        assert!(raw
            .attachment_keys()
            .list_retained_keys("default")
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            raw.attachment_keys()
                .get_secret("default", key::ACTIVE_POINTER_SECRET)
                .await,
            Err(BackendError::NotFound { .. })
        ));
    }
}

#[tokio::test]
async fn attachment_restore_corrupt_v1_identity_prefix_requires_explicit_repair() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    keys.set_secret(
        "default",
        request(
            key::ACTIVE_POINTER_SECRET,
            Zeroizing::new("AGE-SECRET-KEY-1corrupt".into()),
            false,
        ),
    )
    .await
    .unwrap();
    for apply in [false, true] {
        assert!(
            restore(keys.as_ref(), files, "default", &bundle, apply, false)
                .await
                .is_err()
        );
        assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
    }
    let preview = restore(keys.as_ref(), files, "default", &bundle, false, true)
        .await
        .unwrap();
    assert_eq!(preview.pointer_outcome, "repair");
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
    restore(keys.as_ref(), files, "default", &bundle, true, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn attachment_restore_reuses_retained_identity_with_surrounding_whitespace() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let stored = keys
        .commit_retained_key(
            "default",
            request(
                &key::retained_record_name(&id(&bundle.active_key_id).unwrap()),
                Zeroizing::new(format!(" \n{}\n ", bundle.identities[0].identity.as_str())),
                true,
            ),
        )
        .await
        .unwrap();
    restore(keys.as_ref(), files, "default", &bundle, false, false)
        .await
        .unwrap();
    restore(keys.as_ref(), files, "default", &bundle, true, false)
        .await
        .unwrap();
    assert_eq!(
        retained(keys.as_ref(), "default", &bundle.active_key_id)
            .await
            .unwrap()
            .unwrap()
            .provider_version
            .as_str(),
        stored.version
    );
}

#[tokio::test]
async fn attachment_restore_valid_v1_with_whitespace_is_never_repaired() {
    let (_tmp, backend, bundle, _) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let stored = keys
        .set_secret(
            "default",
            request(
                key::ACTIVE_POINTER_SECRET,
                Zeroizing::new(format!(" \n{}\n ", bundle.identities[0].identity.as_str())),
                false,
            ),
        )
        .await
        .unwrap();
    assert!(
        restore(keys.as_ref(), files, "default", &bundle, true, true)
            .await
            .is_err()
    );
    assert!(keys.list_retained_keys("default").await.unwrap().is_empty());
    assert_eq!(
        keys.get_secret("default", key::ACTIVE_POINTER_SECRET)
            .await
            .unwrap()
            .version,
        stored.version
    );
}
