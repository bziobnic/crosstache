use super::*;
use crate::backend::{local::LocalBackend, Backend};
use crate::blob::models::FileUploadRequest;
use crate::config::settings::LocalConfig;
use crate::secret::attachments;
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
fn blob(name: &str, content: &[u8]) -> FileUploadRequest {
    FileUploadRequest {
        name: name.into(),
        content: content.to_vec(),
        content_type: None,
        groups: vec![],
        metadata: HashMap::new(),
        tags: HashMap::new(),
    }
}
async fn seed(keys: &dyn AttachmentKeyStore, legacy: bool) -> AttachmentKeyId {
    let identity = age::x25519::Identity::generate();
    let id = AttachmentKeyId::derive(&identity.to_public().to_string());
    keys.commit_retained_key(
        "default",
        request(
            &key::retained_record_name(&id),
            Zeroizing::new(identity.to_string().expose_secret().into()),
            true,
        ),
    )
    .await
    .unwrap();
    keys.set_secret(
        "default",
        request(
            key::ACTIVE_POINTER_SECRET,
            Zeroizing::new(key::format_v2_pointer(&id, legacy.then_some(&id))),
            false,
        ),
    )
    .await
    .unwrap();
    id
}
#[tokio::test]
async fn preview_has_no_writes_and_apply_changes_once_preserving_readability() {
    for legacy in [false, true] {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let id = seed(keys.as_ref(), legacy).await;
        let files = backend.files().unwrap();
        attachments::upload_encrypted(
            keys.as_ref(),
            files,
            "default",
            blob("before", b"before"),
            None,
        )
        .await
        .unwrap();
        let before = keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
            .await
            .unwrap();
        let preview = rotate(keys.as_ref(), "default", &id, false).await.unwrap();
        assert_eq!(preview.outcome, "ready");
        assert!(preview.new_key_id.is_none());
        assert_eq!(keys.list_retained_keys("default").await.unwrap().len(), 1);
        assert_eq!(
            keys.get_secret("default", key::ACTIVE_POINTER_SECRET, true)
                .await
                .unwrap()
                .version,
            before.version
        );
        let applied = rotate(keys.as_ref(), "default", &id, true).await.unwrap();
        assert_eq!(applied.outcome, "applied");
        assert_ne!(applied.new_key_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            applied.legacy_key_id.as_deref(),
            legacy.then_some(id.as_str())
        );
        assert!(!serde_json::to_string(&applied)
            .unwrap()
            .contains("AGE-SECRET"));
        attachments::upload_encrypted(
            keys.as_ref(),
            files,
            "default",
            blob("after", b"after"),
            None,
        )
        .await
        .unwrap();
        for name in ["before", "after"] {
            assert_eq!(
                attachments::download_decrypted(keys.as_ref(), files, "default", name, None)
                    .await
                    .unwrap(),
                name.as_bytes()
            );
        }
        assert_eq!(
            rotate(keys.as_ref(), "default", &id, true)
                .await
                .unwrap_err()
                .code(),
            "xv-conflict"
        );
        assert_eq!(keys.list_retained_keys("default").await.unwrap().len(), 2);
    }
}
#[tokio::test]
async fn refuses_v1_mismatch_and_invalid_or_disabled_records() {
    for case in [
        "v1",
        "mismatch",
        "disabled",
        "unmarked",
        "invalid",
        "pointer-disabled",
    ] {
        let (_dir, backend) = fixture();
        let keys = backend.attachment_keys();
        let id = seed(keys.as_ref(), false).await;
        let mut expected = id.clone();
        if case == "mismatch" {
            expected = AttachmentKeyId::derive("other");
        }
        if case == "v1" || case == "pointer-disabled" {
            let mut req = request(
                key::ACTIVE_POINTER_SECRET,
                Zeroizing::new(if case == "v1" {
                    age::x25519::Identity::generate()
                        .to_string()
                        .expose_secret()
                        .into()
                } else {
                    key::format_v2_pointer(&id, None)
                }),
                false,
            );
            req.enabled = Some(case != "pointer-disabled");
            keys.set_secret("default", req).await.unwrap();
        } else if case != "mismatch" {
            let name = key::retained_record_name(&id);
            let original = keys.get_secret("default", &name, true).await.unwrap();
            let mut req = request(
                &name,
                if case == "invalid" {
                    Zeroizing::new("invalid".into())
                } else {
                    original.value.unwrap()
                },
                case != "unmarked",
            );
            req.enabled = Some(case != "disabled");
            backend.secrets().set_secret("default", req).await.unwrap();
        }
        let before = keys
            .get_secret("default", key::ACTIVE_POINTER_SECRET, false)
            .await
            .unwrap()
            .version;
        for apply in [false, true] {
            assert!(
                rotate(keys.as_ref(), "default", &expected, apply)
                    .await
                    .is_err(),
                "{case}"
            );
        }
        assert_eq!(
            keys.get_secret("default", key::ACTIVE_POINTER_SECRET, false)
                .await
                .unwrap()
                .version,
            before
        );
    }
}

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
struct FaultKeys<'a> {
    inner: Box<dyn AttachmentKeyStore + 'a>,
    mode: &'static str,
    writes: AtomicUsize,
    pointer_reads: AtomicUsize,
    triggered: AtomicBool,
    candidate: Mutex<Option<String>>,
    old_name: String,
}
impl<'a> FaultKeys<'a> {
    fn new(backend: &'a LocalBackend, id: &AttachmentKeyId, mode: &'static str) -> Self {
        Self {
            inner: backend.attachment_keys(),
            mode,
            writes: AtomicUsize::new(0),
            pointer_reads: AtomicUsize::new(0),
            triggered: AtomicBool::new(false),
            candidate: Mutex::new(None),
            old_name: key::retained_record_name(id),
        }
    }
}
#[async_trait::async_trait]
impl AttachmentKeyStore for FaultKeys<'_> {
    async fn preflight_set_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> std::result::Result<(), BackendError> {
        if self.mode == "policy-pointer"
            || (self.mode == "policy-candidate" && name != key::ACTIVE_POINTER_SECRET)
        {
            return Err(BackendError::PermissionDenied("test denial".into()));
        }
        if name != key::ACTIVE_POINTER_SECRET {
            *self.candidate.lock().unwrap() = Some(name.into());
        }
        self.inner.preflight_set_secret(vault, name).await
    }
    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> std::result::Result<SecretProperties, BackendError> {
        let candidate = self.candidate.lock().unwrap().clone();
        if self.mode == "collision" && candidate.as_deref() == Some(name) {
            let mut p = self
                .inner
                .get_secret(vault, &self.old_name, include_value)
                .await?;
            p.content_type = "ordinary".into();
            return Ok(p);
        }
        let mut props = self.inner.get_secret(vault, name, include_value).await?;
        if name == key::ACTIVE_POINTER_SECRET {
            let read = self.pointer_reads.fetch_add(1, Ordering::SeqCst);
            if (self.mode == "pointer-drift" && read == 1)
                || (self.mode == "publication-version" && self.writes.load(Ordering::SeqCst) == 2)
            {
                props.version = "other-version".into();
            }
            if self.mode == "publication-value" && self.writes.load(Ordering::SeqCst) == 2 {
                props.value = Some(Zeroizing::new("changed".into()));
            }
        }
        if name == self.old_name
            && self.mode == "ref-drift"
            && self.writes.load(Ordering::SeqCst) > 0
        {
            props.version = "changed".into();
        }
        Ok(props)
    }
    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> std::result::Result<SecretProperties, BackendError> {
        let is_candidate = self.candidate.lock().unwrap().as_deref() == Some(name);
        if self.mode == "readback" && is_candidate {
            return Err(BackendError::Network("readback interrupted".into()));
        }
        let mut props = self
            .inner
            .get_secret_version(vault, name, version, include_value)
            .await?;
        if self.mode == "exact-version" {
            props.version = "wrong-version".into();
        }
        if self.mode == "exact-disabled" {
            props.enabled = false;
        }
        if self.mode == "exact-unmarked" {
            props.content_type = "ordinary".into();
        }
        Ok(props)
    }
    async fn commit_retained_key(
        &self,
        vault: &str,
        req: SecretRequest,
    ) -> std::result::Result<SecretProperties, BackendError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.mode == "commit" {
            return Err(BackendError::Network("commit interrupted".into()));
        }
        self.inner.commit_retained_key(vault, req).await
    }
    async fn set_secret(
        &self,
        vault: &str,
        req: SecretRequest,
    ) -> std::result::Result<SecretProperties, BackendError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.mode == "interrupted" && !self.triggered.swap(true, Ordering::SeqCst) {
            return Err(BackendError::Network("publication interrupted".into()));
        }
        self.inner.set_secret(vault, req).await
    }
}
#[tokio::test]
async fn preview_and_early_failures_never_write() {
    for mode in [
        "none",
        "policy-pointer",
        "policy-candidate",
        "collision",
        "exact-version",
        "exact-disabled",
        "exact-unmarked",
    ] {
        let (_dir, backend) = fixture();
        let id = seed(backend.attachment_keys().as_ref(), false).await;
        let keys = FaultKeys::new(&backend, &id, mode);
        let preview = rotate(&keys, "default", &id, false).await;
        assert_eq!(
            preview.is_ok(),
            matches!(mode, "none" | "policy-candidate" | "collision"),
            "{mode}"
        );
        assert!(
            keys.candidate.lock().unwrap().is_none(),
            "preview must not generate/preflight a candidate"
        );
        assert_eq!(keys.writes.load(Ordering::SeqCst), 0);
        if mode != "none" {
            assert!(rotate(&keys, "default", &id, true).await.is_err(), "{mode}");
        }
        assert_eq!(keys.writes.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn failures_do_not_publish_before_commit_verification_or_after_drift() {
    for mode in ["commit", "readback", "pointer-drift", "ref-drift"] {
        let (_dir, backend) = fixture();
        let inner = backend.attachment_keys();
        let id = seed(inner.as_ref(), true).await;
        let before = inner
            .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
            .await
            .unwrap();
        let keys = FaultKeys::new(&backend, &id, mode);
        assert!(rotate(&keys, "default", &id, true).await.is_err(), "{mode}");
        assert_eq!(keys.writes.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(
            inner
                .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
                .await
                .unwrap()
                .version,
            before.version
        );
        assert!(retained(inner.as_ref(), "default", &id).await.is_ok());
    }
}
#[tokio::test]
async fn publication_requires_matching_returned_version_and_value() {
    for mode in ["publication-version", "publication-value"] {
        let (_dir, backend) = fixture();
        let id = seed(backend.attachment_keys().as_ref(), false).await;
        let keys = FaultKeys::new(&backend, &id, mode);
        assert_eq!(
            rotate(&keys, "default", &id, true)
                .await
                .unwrap_err()
                .code(),
            "xv-attachment-commit-unconfirmed"
        );
        assert_eq!(
            backend
                .attachment_keys()
                .list_retained_keys("default")
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(rotate(&keys, "default", &id, true).await.is_err());
        assert_eq!(keys.writes.load(Ordering::SeqCst), 2);
    }
}
#[tokio::test]
async fn interrupted_retry_preserves_orphan_but_success_cannot_rotate_twice() {
    let (_dir, backend) = fixture();
    let id = seed(backend.attachment_keys().as_ref(), true).await;
    let keys = FaultKeys::new(&backend, &id, "interrupted");
    assert!(rotate(&keys, "default", &id, true).await.is_err());
    let orphan = keys.candidate.lock().unwrap().clone().unwrap();
    let retained_orphan = backend
        .attachment_keys()
        .get_secret("default", &orphan, true)
        .await
        .unwrap();
    let report = rotate(&keys, "default", &id, true).await.unwrap();
    assert_eq!(report.outcome, "applied");
    assert_ne!(
        key::retained_record_name(
            &AttachmentKeyId::parse(report.new_key_id.as_deref().unwrap()).unwrap()
        ),
        orphan
    );
    assert_eq!(
        backend
            .attachment_keys()
            .get_secret("default", &orphan, true)
            .await
            .unwrap()
            .version,
        retained_orphan.version
    );
    assert_eq!(
        backend
            .attachment_keys()
            .list_retained_keys("default")
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(rotate(&keys, "default", &id, true).await.is_err());
    assert_eq!(keys.writes.load(Ordering::SeqCst), 4);
}
#[tokio::test]
async fn rotated_upgraded_vault_keeps_preschema_and_exact_legacy_versions_readable() {
    let (_dir, backend) = fixture();
    let keys = backend.attachment_keys();
    let files = backend.files().unwrap();
    let identity = age::x25519::Identity::generate();
    let id = AttachmentKeyId::derive(&identity.to_public().to_string());
    keys.set_secret(
        "default",
        request(
            key::ACTIVE_POINTER_SECRET,
            Zeroizing::new(identity.to_string().expose_secret().into()),
            false,
        ),
    )
    .await
    .unwrap();
    let mut preschema = blob(
        "preschema",
        &crate::backend::local::crypto::encrypt_bytes(b"legacy plaintext", &[identity.to_public()])
            .unwrap(),
    );
    preschema
        .metadata
        .insert(key::META_ENCRYPTED.into(), key::ENC_VALUE_AGE.into());
    files.upload_file("default", preschema, None).await.unwrap();
    attachments::upload_encrypted(
        keys.as_ref(),
        files,
        "default",
        blob("pinned", b"legacy plaintext"),
        None,
    )
    .await
    .unwrap();
    crate::secret::attachment_lifecycle::upgrade(keys.as_ref(), "default", true)
        .await
        .unwrap();
    rotate(keys.as_ref(), "default", &id, true).await.unwrap();
    for name in ["preschema", "pinned"] {
        assert_eq!(
            attachments::download_decrypted(keys.as_ref(), files, "default", name, None)
                .await
                .unwrap(),
            b"legacy plaintext"
        );
    }
}
#[tokio::test]
async fn policy_wrapper_denies_pointer_write_and_candidate_raw_readback_before_mutation() {
    use crate::agent::{
        enforce::PolicyEnforcedBackend, policy::CompiledPolicy, AgentIdentity, IdentitySource,
    };
    use crate::config::settings::{AgentConfig, AgentPolicyRule};
    for pointer_allowed in [false, true] {
        let (tmp, backend) = fixture();
        let id = seed(backend.attachment_keys().as_ref(), false).await;
        let raw = std::sync::Arc::new(backend);
        let original = raw
            .attachment_keys()
            .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
            .await
            .unwrap();
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
                    "pointer",
                    vec![key::ACTIVE_POINTER_SECRET.into()],
                    if pointer_allowed {
                        vec!["get".into(), "set".into()]
                    } else {
                        vec!["get".into()]
                    },
                    true,
                ),
                rule(
                    "existing identity",
                    vec![key::retained_record_name(&id)],
                    vec!["get".into()],
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
            tmp.path().join("rotation-decisions.jsonl"),
        );
        assert_eq!(
            rotate(wrapped.attachment_keys().as_ref(), "default", &id, false)
                .await
                .is_ok(),
            pointer_allowed
        );
        assert!(
            rotate(wrapped.attachment_keys().as_ref(), "default", &id, true)
                .await
                .is_err()
        );
        assert_eq!(
            raw.attachment_keys()
                .list_retained_keys("default")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            raw.attachment_keys()
                .get_secret("default", key::ACTIVE_POINTER_SECRET, true)
                .await
                .unwrap()
                .version,
            original.version
        );
    }
}
