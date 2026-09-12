use super::*;
use crate::backend::{
    local::{crypto, LocalBackend},
    Backend,
};
use crate::config::settings::LocalConfig;
use crate::secret::domain::SecretRequest;
use crate::secret::domain::SecretValue;
use crate::secret::{
    attachment_key::{self as key, AttachmentKeyRef, KeySlot, SecretVersion},
    attachment_rewrap, attachment_rotation,
};
use age::secrecy::ExposeSecret;
use std::collections::HashMap;

fn request(name: String, value: String) -> SecretRequest {
    SecretRequest {
        name,
        value: SecretValue::new(value),
        content_type: Some(key::KEY_RECORD_CONTENT_TYPE.into()),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: Some(HashMap::from([("custom".into(), "kept".into())])),
        groups: Some(vec!["group".into()]),
        note: Some("note".into()),
        folder: Some("folder".into()),
    }
}
async fn fixture() -> (tempfile::TempDir, LocalBackend, AttachmentKeyId, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(dir.path().join("store").display().to_string()),
        key_file: Some(dir.path().join("identity").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    let identity = age::x25519::Identity::generate();
    let id = AttachmentKeyId::derive(&identity.to_public().to_string());
    let keys = backend.attachment_keys();
    keys.commit_retained_key(
        "default",
        request(
            key::retained_record_name(&id),
            identity.to_string().expose_secret().into(),
        ),
    )
    .await
    .unwrap();
    keys.set_secret(
        "default",
        request(
            key::ACTIVE_POINTER_SECRET.into(),
            key::format_v2_pointer(&id, None),
        ),
    )
    .await
    .unwrap();
    attachment_rotation::rotate(keys.as_ref(), "default", &id, true)
        .await
        .unwrap();
    drop(keys);
    let historical = crypto::encrypt_bytes(b"historical payload", &[identity.to_public()]).unwrap();
    (dir, backend, id, historical)
}
#[tokio::test]
async fn preview_mark_retry_preserve_metadata_version_value_and_historical_decryption() {
    let (_dir, backend, id, historical) = fixture().await;
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let name = key::retained_record_name(&id);
    let before = keys.get_secret("default", &name).await.unwrap();
    assert_eq!(
        retire(keys.as_ref(), files, "default", &id, false)
            .await
            .unwrap()
            .outcome,
        "ready"
    );
    assert_eq!(
        keys.get_secret("default", &name).await.unwrap().tags,
        before.tags
    );
    assert_eq!(
        retire(keys.as_ref(), files, "default", &id, true)
            .await
            .unwrap()
            .outcome,
        "retired"
    );
    let after = keys.get_secret("default", &name).await.unwrap();
    assert_eq!(after.version, before.version);
    assert_eq!(after.value, before.value);
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.content_type, before.content_type);
    assert_eq!(after.expires_on, before.expires_on);
    assert_eq!(after.not_before, before.not_before);
    for (k, v) in before.tags.clone() {
        assert_eq!(after.tags.get(&k), Some(&v));
    }
    assert_eq!(
        after
            .tags
            .get("xv_attachment_key_retired")
            .map(String::as_str),
        Some("true")
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
    assert_eq!(
        retire(keys.as_ref(), files, "default", &id, true)
            .await
            .unwrap()
            .outcome,
        "already_retired"
    );
    let reference = AttachmentKeyRef {
        key_id: id,
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(before.version.clone()),
    };
    let identity = attachment_rewrap::exact_identity(keys.as_ref(), "default", &reference)
        .await
        .unwrap();
    assert_eq!(
        crypto::decrypt_bytes(&historical, &identity)
            .unwrap()
            .as_slice(),
        b"historical payload"
    );
}

#[tokio::test]
async fn active_legacy_missing_invalid_and_disabled_candidates_are_refused() {
    for mode in [
        "active", "legacy", "missing", "invalid", "disabled", "unmarked",
    ] {
        let (_dir, backend, id, _) = fixture().await;
        let keys = backend.attachment_keys();
        let pointer = keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET)
            .await
            .unwrap();
        let key::PointerKind::V2 { active, .. } =
            key::parse_pointer_value(pointer.value.expose_secret()).unwrap()
        else {
            panic!()
        };
        let candidate = match mode {
            "active" => active.clone(),
            "legacy" => {
                keys.set_secret(
                    "default",
                    request(
                        key::ACTIVE_POINTER_SECRET.into(),
                        key::format_v2_pointer(&active, Some(&id)),
                    ),
                )
                .await
                .unwrap();
                id.clone()
            }
            "missing" => AttachmentKeyId::derive("missing"),
            "invalid" | "disabled" | "unmarked" => {
                let p = keys
                    .get_secret("default", &key::retained_record_name(&id))
                    .await
                    .unwrap();
                let mut r = request(p.name.clone(), p.value.expose_secret().to_string());
                if mode == "invalid" {
                    r.value = SecretValue::new("invalid");
                }
                if mode == "disabled" {
                    r.enabled = Some(false);
                }
                if mode == "unmarked" {
                    r.content_type = None;
                }
                backend.secrets().set_secret("default", r).await.unwrap();
                id.clone()
            }
            _ => unreachable!(),
        };
        assert!(
            retire(
                keys.as_ref(),
                backend.files().unwrap(),
                "default",
                &candidate,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
        let p = keys
            .get_secret_metadata("default", &key::retained_record_name(&id))
            .await
            .unwrap();
        assert!(!p.tags.contains_key("xv_attachment_key_retired"), "{mode}");
    }
}

#[tokio::test]
async fn current_references_and_invalid_managed_files_block_retirement() {
    for mode in [
        "candidate",
        "unknown-key",
        "partial",
        "unknown-schema",
        "tamper",
        "legacy-slot",
    ] {
        let (_dir, backend, id, historical) = fixture().await;
        let keys = backend.attachment_keys();
        let p = keys
            .get_secret_metadata("default", &key::retained_record_name(&id))
            .await
            .unwrap();
        let reference = AttachmentKeyRef {
            key_id: id.clone(),
            slot: if mode == "legacy-slot" {
                KeySlot::Legacy
            } else {
                KeySlot::Retained
            },
            provider_version: SecretVersion::new(p.version),
        };
        let mut metadata = HashMap::new();
        key::apply_crypto_metadata(&mut metadata, &reference);
        let mut content = historical;
        match mode {
            "unknown-key" => {
                metadata.insert(
                    key::META_KEY_ID.into(),
                    AttachmentKeyId::derive("unknown").as_str().into(),
                );
            }
            "partial" => {
                metadata.remove(key::META_CRYPTO_SCHEMA);
            }
            "unknown-schema" => {
                metadata.insert(key::META_CRYPTO_SCHEMA.into(), "2".into());
            }
            "tamper" => {
                let last = content.len() - 1;
                content[last] ^= 1;
            }
            _ => {}
        }
        backend
            .files()
            .unwrap()
            .upload_file(
                "default",
                crate::blob::models::FileUploadRequest {
                    name: "managed".into(),
                    content,
                    content_type: None,
                    groups: vec![],
                    tags: HashMap::new(),
                    metadata,
                },
                None,
            )
            .await
            .unwrap();
        assert!(
            retire(
                keys.as_ref(),
                backend.files().unwrap(),
                "default",
                &id,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
        assert!(!keys
            .get_secret_metadata("default", &key::retained_record_name(&id))
            .await
            .unwrap()
            .tags
            .contains_key("xv_attachment_key_retired"));
    }
}

use crate::backend::BackendError;
use crate::secret::domain::Secret;
use std::sync::atomic::{AtomicUsize, Ordering};
struct FaultKeys<'a> {
    inner: Box<dyn AttachmentKeyStore + 'a>,
    candidate: AttachmentKeyId,
    mode: &'static str,
    reads: AtomicUsize,
    writes: AtomicUsize,
}
#[async_trait::async_trait]
impl AttachmentKeyStore for FaultKeys<'_> {
    async fn assert_complete_visibility(&self, v: &str) -> std::result::Result<(), BackendError> {
        if self.mode == "incomplete" {
            return Err(BackendError::Unsupported("incomplete visibility".into()));
        }
        self.inner.assert_complete_visibility(v).await
    }
    async fn preflight_retirement(
        &self,
        v: &str,
        r: &AttachmentKeyRef,
    ) -> std::result::Result<(), BackendError> {
        if self.mode == "preflight" {
            return Err(BackendError::PermissionDenied("no update".into()));
        }
        self.inner.preflight_retirement(v, r).await
    }
    async fn mark_retired(
        &self,
        v: &str,
        r: &AttachmentKeyRef,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.mode == "mark-failure" {
            return Err(BackendError::Network("interrupted".into()));
        }
        let result = self.inner.mark_retired(v, r).await?;
        if self.mode == "readback-failure" {
            return Err(BackendError::Network("readback interrupted".into()));
        }
        Ok(result)
    }
    async fn get_secret_metadata(
        &self,
        v: &str,
        n: &str,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        let mut p = self.inner.get_secret_metadata(v, n).await?;
        if n == key::retained_record_name(&self.candidate) {
            let count = self.reads.fetch_add(1, Ordering::SeqCst);
            if (self.mode == "candidate-drift" && count > 0)
                || (self.mode == "final-drift" && self.writes.load(Ordering::SeqCst) > 0)
            {
                p.tags.insert("concurrent".into(), "changed".into());
            }
        }
        if n == key::ACTIVE_POINTER_SECRET
            && self.mode == "pointer-drift"
            && self.reads.load(Ordering::SeqCst) > 0
        {
            p.version = "changed".into();
        }
        Ok(p)
    }

    async fn get_secret(&self, v: &str, n: &str) -> std::result::Result<Secret, BackendError> {
        let mut p = self.inner.get_secret(v, n).await?;
        if n == key::retained_record_name(&self.candidate) {
            let count = self.reads.fetch_add(1, Ordering::SeqCst);
            if (self.mode == "candidate-drift" && count > 0)
                || (self.mode == "final-drift" && self.writes.load(Ordering::SeqCst) > 0)
            {
                p.tags.insert("concurrent".into(), "changed".into());
            }
        }
        if n == key::ACTIVE_POINTER_SECRET
            && self.mode == "pointer-drift"
            && self.reads.load(Ordering::SeqCst) > 0
        {
            p.version = "changed".into();
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
        if n == key::retained_record_name(&self.candidate) {
            match self.mode {
                "exact-wrong-version" => p.version = "wrong".into(),
                "exact-disabled" => p.enabled = false,
                "exact-unmarked" => p.content_type = "ordinary".into(),
                // The wrong-key fault is a value substitution; it has no
                // metadata-path equivalent.
                _ => {}
            }
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
        if n == key::retained_record_name(&self.candidate) {
            match self.mode {
                "exact-wrong-version" => p.version = "wrong".into(),
                "exact-disabled" => p.enabled = false,
                "exact-unmarked" => p.content_type = "ordinary".into(),
                "exact-wrong-key" => {
                    p.value = SecretValue::new(
                        age::x25519::Identity::generate()
                            .to_string()
                            .expose_secret(),
                    )
                }
                _ => {}
            }
        }
        Ok(p)
    }
    async fn set_secret(
        &self,
        _: &str,
        _: SecretRequest,
    ) -> std::result::Result<SecretMetadata, BackendError> {
        panic!("retirement cannot write values")
    }
}
#[tokio::test]
async fn incomplete_visibility_preflight_exact_identity_and_drift_fail_before_marking() {
    for mode in [
        "incomplete",
        "preflight",
        "candidate-drift",
        "pointer-drift",
        "exact-wrong-version",
        "exact-disabled",
        "exact-unmarked",
        "exact-wrong-key",
    ] {
        let (_dir, backend, id, _) = fixture().await;
        let keys = FaultKeys {
            inner: backend.attachment_keys(),
            candidate: id.clone(),
            mode,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
        };
        assert!(
            retire(&keys, backend.files().unwrap(), "default", &id, true)
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(keys.writes.load(Ordering::SeqCst), 0, "{mode}");
        assert!(!keys
            .inner
            .get_secret_metadata("default", &key::retained_record_name(&id))
            .await
            .unwrap()
            .tags
            .contains_key(key::KEY_RETIRED_TAG));
    }
}
#[tokio::test]
async fn interrupted_marking_and_final_drift_are_reported_and_retry_is_verified() {
    for mode in ["mark-failure", "readback-failure", "final-drift"] {
        let (_dir, backend, id, _) = fixture().await;
        let keys = FaultKeys {
            inner: backend.attachment_keys(),
            candidate: id.clone(),
            mode,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
        };
        assert!(
            retire(&keys, backend.files().unwrap(), "default", &id, true)
                .await
                .is_err(),
            "{mode}"
        );
        assert_eq!(keys.writes.load(Ordering::SeqCst), 1);
        let report = retire(
            keys.inner.as_ref(),
            backend.files().unwrap(),
            "default",
            &id,
            true,
        )
        .await
        .unwrap();
        assert_eq!(
            report.outcome,
            if mode == "mark-failure" {
                "retired"
            } else {
                "already_retired"
            }
        );
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("AGE-SECRET"));
    }
}

use crate::backend::file::FileDownloadSnapshot;
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::progress::ProgressReporter;
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
        if self.mode == "ciphertext-drift" && reads >= 1 {
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
async fn populate(backend: &LocalBackend) {
    crate::secret::attachments::upload_encrypted(
        backend.attachment_keys().as_ref(),
        backend.files().unwrap(),
        "default",
        FileUploadRequest {
            name: "target".into(),
            content: b"current".to_vec(),
            content_type: None,
            groups: vec![],
            tags: HashMap::new(),
            metadata: HashMap::new(),
        },
        None,
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn sparse_inventory_succeeds_but_duplicate_inventory_and_ciphertext_drift_prevent_marking() {
    for mode in ["sparse", "duplicate", "inventory-drift", "ciphertext-drift"] {
        let (_dir, backend, id, _) = fixture().await;
        populate(&backend).await;
        let keys = FaultKeys {
            inner: backend.attachment_keys(),
            candidate: id.clone(),
            mode: "none",
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
        };
        let files = FaultFiles::new(backend.files().unwrap(), mode);
        let result = retire(&keys, &files, "default", &id, true).await;
        assert_eq!(result.is_ok(), mode == "sparse", "{mode}: {result:?}");
        assert_eq!(
            keys.writes.load(Ordering::SeqCst),
            usize::from(mode == "sparse")
        );
        assert_eq!(files.writes.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn tampered_current_files_and_new_candidate_references_block_already_retired_retry() {
    for mode in ["tamper", "candidate", "legacy"] {
        let (_dir, backend, id, historical) = fixture().await;
        let keys = backend.attachment_keys();
        retire(
            keys.as_ref(),
            backend.files().unwrap(),
            "default",
            &id,
            true,
        )
        .await
        .unwrap();
        if mode == "legacy" {
            let p = keys
                .get_secret("default", key::ACTIVE_POINTER_SECRET)
                .await
                .unwrap();
            let key::PointerKind::V2 { active, .. } =
                key::parse_pointer_value(p.value.expose_secret()).unwrap()
            else {
                panic!()
            };
            keys.set_secret(
                "default",
                request(
                    key::ACTIVE_POINTER_SECRET.into(),
                    key::format_v2_pointer(&active, Some(&id)),
                ),
            )
            .await
            .unwrap();
        } else if mode == "candidate" {
            let p = keys
                .get_secret_metadata("default", &key::retained_record_name(&id))
                .await
                .unwrap();
            let mut metadata = HashMap::new();
            key::apply_crypto_metadata(
                &mut metadata,
                &AttachmentKeyRef {
                    key_id: id.clone(),
                    slot: KeySlot::Retained,
                    provider_version: SecretVersion::new(p.version),
                },
            );
            backend
                .files()
                .unwrap()
                .upload_file(
                    "default",
                    FileUploadRequest {
                        name: "candidate".into(),
                        content: historical,
                        content_type: None,
                        groups: vec![],
                        tags: HashMap::new(),
                        metadata,
                    },
                    None,
                )
                .await
                .unwrap();
        } else {
            populate(&backend).await;
            let mut snap = backend
                .files()
                .unwrap()
                .download_file_snapshot("default", "target", None)
                .await
                .unwrap();
            let last = snap.content.len() - 1;
            snap.content[last] ^= 1;
            backend
                .files()
                .unwrap()
                .restore_file(
                    "default",
                    FileUploadRequest {
                        name: "target".into(),
                        content: snap.content,
                        content_type: None,
                        groups: vec![],
                        tags: HashMap::new(),
                        metadata: snap.metadata,
                    },
                )
                .await
                .unwrap();
        }
        assert!(
            retire(
                keys.as_ref(),
                backend.files().unwrap(),
                "default",
                &id,
                true
            )
            .await
            .is_err(),
            "{mode}"
        );
    }
}
#[tokio::test]
async fn agent_policy_refuses_complete_visibility_and_narrow_mark_enforces_update_and_raw_get() {
    use crate::agent::{
        enforce::PolicyEnforcedBackend, policy::CompiledPolicy, AgentIdentity, IdentitySource,
    };
    use crate::config::settings::{AgentConfig, AgentPolicyRule};
    for mode in ["no-update", "no-raw", "allow"] {
        let (dir, backend, id, _) = fixture().await;
        let raw = std::sync::Arc::new(backend);
        let name = key::retained_record_name(&id);
        let before = raw
            .attachment_keys()
            .get_secret_metadata("default", &name)
            .await
            .unwrap();
        let reference = AttachmentKeyRef {
            key_id: id.clone(),
            slot: KeySlot::Retained,
            provider_version: SecretVersion::new(before.version),
        };
        let policy = CompiledPolicy::compile(&AgentConfig {
            enforce: true,
            policy: vec![AgentPolicyRule {
                name: "custody".into(),
                identity: "github:o/r:*".into(),
                identity_source: "github-oidc".into(),
                workspace: "default".into(),
                secrets: vec!["*".into()],
                operations: if mode == "no-update" {
                    vec!["get".into()]
                } else {
                    vec!["get".into(), "update".into(), "list".into()]
                },
                raw_disclosure: mode != "no-raw",
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
        let path = dir.path().join("retirement-decisions.jsonl");
        let wrapped = PolicyEnforcedBackend::for_test(
            raw.clone(),
            AgentIdentity::new(IdentitySource::GithubOidc, "github:o/r:ci"),
            policy,
            path.clone(),
        );
        let keys = wrapped.attachment_keys();
        assert!(retire(
            keys.as_ref(),
            wrapped.files().unwrap(),
            "default",
            &id,
            true
        )
        .await
        .is_err());
        assert!(!raw
            .attachment_keys()
            .get_secret_metadata("default", &name)
            .await
            .unwrap()
            .tags
            .contains_key(key::KEY_RETIRED_TAG));
        assert_eq!(
            keys.preflight_retirement("default", &reference)
                .await
                .is_ok(),
            mode == "allow"
        );
        assert_eq!(
            keys.mark_retired("default", &reference).await.is_ok(),
            mode == "allow"
        );
        let after = raw
            .attachment_keys()
            .get_secret_metadata("default", &name)
            .await
            .unwrap();
        assert_eq!(
            after.tags.contains_key(key::KEY_RETIRED_TAG),
            mode == "allow"
        );
        let decisions = std::fs::read_to_string(path).unwrap();
        assert!(decisions.contains("update"));
        assert!(!decisions.contains("AGE-SECRET"));
        if mode != "no-update" {
            assert!(decisions.contains("get"));
        }
    }
}
#[tokio::test]
async fn unused_candidate_retirement_authenticates_unrelated_current_files_before_marking() {
    let (_dir, backend, id, _) = fixture().await;
    populate(&backend).await;
    let files = backend.files().unwrap();
    let mut snap = files
        .download_file_snapshot("default", "target", None)
        .await
        .unwrap();
    let last = snap.content.len() - 1;
    snap.content[last] ^= 1;
    files
        .restore_file(
            "default",
            FileUploadRequest {
                name: "target".into(),
                content: snap.content,
                content_type: None,
                groups: vec![],
                tags: HashMap::new(),
                metadata: snap.metadata,
            },
        )
        .await
        .unwrap();
    let keys = FaultKeys {
        inner: backend.attachment_keys(),
        candidate: id.clone(),
        mode: "none",
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
    };
    assert!(retire(&keys, files, "default", &id, true).await.is_err());
    assert_eq!(keys.writes.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn retirement_keeps_all_existing_provider_versions_and_both_exact_identities_readable() {
    let (_dir, backend, id, historical) = fixture().await;
    let keys = backend.attachment_keys();
    let name = key::retained_record_name(&id);
    let old = keys.get_secret("default", &name).await.unwrap();
    // Existing retained records may have multiple versions from prior recovery.
    let new = backend
        .secrets()
        .set_secret(
            "default",
            request(name.clone(), old.value.expose_secret().to_string()),
        )
        .await
        .unwrap();
    assert_ne!(old.version, new.version);
    let versions_before = backend
        .secrets()
        .list_versions("default", &name)
        .await
        .unwrap();
    retire(
        keys.as_ref(),
        backend.files().unwrap(),
        "default",
        &id,
        true,
    )
    .await
    .unwrap();
    let versions_after = backend
        .secrets()
        .list_versions("default", &name)
        .await
        .unwrap();
    assert_eq!(versions_after.len(), versions_before.len());
    for version in [old.version.clone(), new.version.clone()] {
        let reference = AttachmentKeyRef {
            key_id: id.clone(),
            slot: KeySlot::Retained,
            provider_version: SecretVersion::new(version),
        };
        let identity = attachment_rewrap::exact_identity(keys.as_ref(), "default", &reference)
            .await
            .unwrap();
        assert_eq!(
            crypto::decrypt_bytes(&historical, &identity)
                .unwrap()
                .as_slice(),
            b"historical payload"
        );
    }
    let list = keys.list_retained_keys("default").await.unwrap();
    assert!(
        list.iter()
            .find(|r| r.key_id == id.as_str())
            .unwrap()
            .retired
    );
}
