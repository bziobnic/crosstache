//! Collector regressions using real Local custody plus narrowly injected faults.
use super::*;
use crate::backend::{
    attachment_keys::RetainedKeySummary, file::FileDownloadSnapshot, local::LocalBackend, Backend,
    BackendError,
};
use crate::blob::models::{FileInfo, FileUploadRequest};
use crate::config::settings::LocalConfig;
use crate::secret::attachments;
use crate::secret::domain::SecretRequest;
use crate::utils::progress::ProgressReporter;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

type ProviderResult<T> = std::result::Result<T, BackendError>;

async fn fixture() -> (tempfile::TempDir, LocalBackend) {
    let dir = tempfile::tempdir().unwrap();
    let backend = LocalBackend::new(Some(&LocalConfig {
        store_path: Some(dir.path().join("store").display().to_string()),
        key_file: Some(dir.path().join("identity").display().to_string()),
        default_vault: Some("default".into()),
        ..Default::default()
    }))
    .unwrap();
    attachments::upload_encrypted(
        backend.attachment_keys().as_ref(),
        backend.files().unwrap(),
        "default",
        FileUploadRequest {
            name: "managed".into(),
            content: b"private payload".to_vec(),
            content_type: None,
            groups: vec![],
            metadata: HashMap::new(),
            tags: HashMap::new(),
        },
        None,
    )
    .await
    .unwrap();
    (dir, backend)
}

#[derive(Clone, Copy)]
enum KeyFault {
    PointerDrift,
    ExactVersion,
    Disabled,
    MissingValue,
}

struct FaultKeys<'a> {
    inner: &'a dyn AttachmentKeyStore,
    fault: KeyFault,
    pointer_reads: AtomicUsize,
}

#[async_trait]
impl AttachmentKeyStore for FaultKeys<'_> {
    async fn list_retained_keys(&self, vault: &str) -> ProviderResult<Vec<RetainedKeySummary>> {
        self.inner.list_retained_keys(vault).await
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> ProviderResult<SecretProperties> {
        let mut props = self.inner.get_secret(vault, name, include_value).await?;
        if name == key::ACTIVE_POINTER_SECRET
            && self.pointer_reads.fetch_add(1, Ordering::SeqCst) > 0
            && matches!(self.fault, KeyFault::PointerDrift)
        {
            props.version.push_str("-changed");
        }
        Ok(props)
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> ProviderResult<SecretProperties> {
        let mut props = self
            .inner
            .get_secret_version(vault, name, version, include_value)
            .await?;
        match self.fault {
            KeyFault::ExactVersion => props.version.push_str("-wrong-version"),
            KeyFault::Disabled => props.enabled = false,
            KeyFault::MissingValue => props.value = None,
            KeyFault::PointerDrift => {}
        }
        Ok(props)
    }

    async fn set_secret(&self, _: &str, _: SecretRequest) -> ProviderResult<SecretProperties> {
        panic!("collector must never write custody")
    }
}

struct FaultFiles<'a> {
    inner: &'a dyn FileBackend,
    ordinary_count: usize,
    corrupt: bool,
}

fn ordinary_info(name: &str) -> FileInfo {
    FileInfo {
        name: name.into(),
        size: 0,
        content_type: "text/plain".into(),
        last_modified: chrono::Utc::now(),
        etag: String::new(),
        groups: vec![],
        metadata: HashMap::new(),
        tags: HashMap::new(),
    }
}

#[async_trait]
impl FileBackend for FaultFiles<'_> {
    async fn upload_file(
        &self,
        _: &str,
        _: FileUploadRequest,
        _: Option<&dyn ProgressReporter>,
    ) -> ProviderResult<FileInfo> {
        panic!("collector must never upload")
    }

    async fn download_file(
        &self,
        _: &str,
        _: &str,
        _: Option<&dyn ProgressReporter>,
    ) -> ProviderResult<Vec<u8>> {
        panic!("collector must use coherent snapshots")
    }

    async fn download_file_snapshot(
        &self,
        vault: &str,
        name: &str,
        reporter: Option<&dyn ProgressReporter>,
    ) -> ProviderResult<FileDownloadSnapshot> {
        assert!(!name.starts_with("ordinary-"));
        let mut snapshot = self
            .inner
            .download_file_snapshot(vault, name, reporter)
            .await?;
        if self.corrupt {
            // Keep a valid age header while breaking payload authentication.
            *snapshot.content.last_mut().unwrap() ^= 1;
        }
        Ok(snapshot)
    }

    async fn list_files(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> ProviderResult<Vec<FileInfo>> {
        assert!(
            request.limit.is_none(),
            "export must enumerate the full set"
        );
        let mut entries = self.inner.list_files(vault, request).await?;
        entries.extend((0..self.ordinary_count).map(|i| ordinary_info(&format!("ordinary-{i}"))));
        Ok(entries)
    }

    async fn delete_file(&self, _: &str, _: &str) -> ProviderResult<()> {
        panic!("collector must never delete")
    }

    async fn get_file_info(&self, vault: &str, name: &str) -> ProviderResult<FileInfo> {
        if name.starts_with("ordinary-") {
            Ok(ordinary_info(name))
        } else {
            self.inner.get_file_info(vault, name).await
        }
    }
}

#[tokio::test]
async fn attachment_backup_manifest_limit_excludes_ordinary_files() {
    let (_dir, backend) = fixture().await;
    let files = FaultFiles {
        inner: backend.files().unwrap(),
        ordinary_count: 100_001,
        corrupt: false,
    };
    let bundle = collect(
        backend.attachment_keys().as_ref(),
        &files,
        "local",
        "default",
    )
    .await
    .unwrap();
    assert_eq!(bundle.files.len(), 1);
    assert_eq!(bundle.files[0].name, "managed");
    assert_eq!(bundle.identities.len(), 1);
}

async fn reject_key_fault(fault: KeyFault) -> String {
    let (_dir, backend) = fixture().await;
    let inner = backend.attachment_keys();
    let keys = FaultKeys {
        inner: inner.as_ref(),
        fault,
        pointer_reads: AtomicUsize::new(0),
    };
    let result = collect(&keys, backend.files().unwrap(), "local", "default").await;
    let error = match result {
        Ok(_) => panic!("collector accepted injected custody fault"),
        Err(error) => error.to_string(),
    };
    assert!(!error.contains("AGE-SECRET-KEY"));
    if matches!(fault, KeyFault::PointerDrift) {
        assert_eq!(keys.pointer_reads.load(Ordering::SeqCst), 2);
    }
    error
}

#[tokio::test]
async fn attachment_backup_rejects_pointer_drift_before_finalization() {
    assert!(reject_key_fault(KeyFault::PointerDrift)
        .await
        .contains("source changed"));
}

#[tokio::test]
async fn attachment_backup_rejects_wrong_returned_exact_version() {
    reject_key_fault(KeyFault::ExactVersion).await;
}

#[tokio::test]
async fn attachment_backup_rejects_disabled_or_omitted_exact_identity() {
    for fault in [KeyFault::Disabled, KeyFault::MissingValue] {
        reject_key_fault(fault).await;
    }
}

#[tokio::test]
async fn attachment_backup_rejects_ciphertext_authentication_failure() {
    let (_dir, backend) = fixture().await;
    let files = FaultFiles {
        inner: backend.files().unwrap(),
        ordinary_count: 0,
        corrupt: true,
    };
    assert!(collect(
        backend.attachment_keys().as_ref(),
        &files,
        "local",
        "default"
    )
    .await
    .is_err());
}
