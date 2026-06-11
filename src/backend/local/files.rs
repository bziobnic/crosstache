//! Local file/blob backend — age-encrypted file storage.
//!
//! Each file is stored as two files inside
//! `<store>/vaults/<vault>/files/`:
//!
//! - `<encoded_name>.age`       — age-encrypted file content
//! - `<encoded_name>.meta.json` — plaintext metadata

use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;

use crate::backend::error::BackendError;
use crate::backend::file::FileBackend;
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::helpers::{create_private_dir, write_private};
use crate::utils::progress::ProgressReporter;

use super::{crypto, paths};

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// URL-encode a file name for safe use as a filename component.
fn encode_name(name: &str) -> String {
    url::form_urlencoded::byte_serialize(name.as_bytes()).collect()
}

fn files_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::files_dir(store_path, vault)
}

fn file_age_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = encode_name(name);
    Ok(files_dir(store_path, vault)?.join(format!("{enc}.age")))
}

fn file_meta_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = encode_name(name);
    Ok(files_dir(store_path, vault)?.join(format!("{enc}.meta.json")))
}

// ---------------------------------------------------------------------------
// Metadata persisted alongside each file
// ---------------------------------------------------------------------------

fn read_file_meta(path: &Path) -> Result<FileInfo, BackendError> {
    let data = fs::read_to_string(path)
        .map_err(|e| BackendError::Internal(format!("read file meta {}: {e}", path.display())))?;
    serde_json::from_str(&data)
        .map_err(|e| BackendError::Internal(format!("parse file meta {}: {e}", path.display())))
}

fn write_file_meta(path: &Path, info: &FileInfo) -> Result<(), BackendError> {
    let json = serde_json::to_string_pretty(info)
        .map_err(|e| BackendError::Internal(format!("serialize file meta: {e}")))?;
    write_private(path, json.as_bytes())
        .map_err(|e| BackendError::Internal(format!("write file meta {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// LocalFileBackend
// ---------------------------------------------------------------------------

/// File-backed file/blob operations using age encryption.
///
/// The target vault is supplied per call (see [`FileBackend`]), so one
/// instance serves every vault in the store.
pub struct LocalFileBackend {
    store_path: PathBuf,
    identity: age::x25519::Identity,
    recipients: Vec<age::x25519::Recipient>,
}

impl LocalFileBackend {
    /// Create a new `LocalFileBackend`.
    pub fn new(
        store_path: PathBuf,
        identity: age::x25519::Identity,
        recipients: Vec<age::x25519::Recipient>,
    ) -> Self {
        Self {
            store_path,
            identity,
            recipients,
        }
    }
}

#[async_trait]
impl FileBackend for LocalFileBackend {
    async fn upload_file(
        &self,
        vault: &str,
        request: FileUploadRequest,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        let fdir = files_dir(&self.store_path, vault)?;
        create_private_dir(&fdir)
            .map_err(|e| BackendError::Internal(format!("mkdir files: {e}")))?;

        let original_size = request.content.len() as u64;
        let ap = file_age_path(&self.store_path, vault, &request.name)?;
        let mp = file_meta_path(&self.store_path, vault, &request.name)?;

        // Encrypt and write file content
        crypto::encrypt_to_file(&ap, &request.content, &self.recipients)?;

        let now = Utc::now();
        let info = FileInfo {
            name: request.name.clone(),
            size: original_size,
            content_type: request
                .content_type
                .unwrap_or_else(|| "application/octet-stream".into()),
            last_modified: now,
            etag: format!("\"{}\"", uuid::Uuid::new_v4()),
            groups: request.groups,
            metadata: request.metadata,
            tags: request.tags,
        };

        write_file_meta(&mp, &info)?;

        Ok(info)
    }

    async fn download_file(
        &self,
        vault: &str,
        name: &str,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<Vec<u8>, BackendError> {
        let ap = file_age_path(&self.store_path, vault, name)?;
        if !ap.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        crypto::decrypt_bytes_from_file(&ap, &self.identity)
    }

    async fn list_files(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<FileInfo>, BackendError> {
        let fdir = files_dir(&self.store_path, vault)?;
        if !fdir.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let entries = fs::read_dir(&fdir)
            .map_err(|e| BackendError::Internal(format!("read files dir: {e}")))?;

        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !fname.ends_with(".meta.json") {
                continue;
            }

            let info = match read_file_meta(&entry.path()) {
                Ok(i) => i,
                Err(_) => continue,
            };

            // Apply prefix filter
            if let Some(ref prefix) = request.prefix {
                if !info.name.starts_with(prefix) {
                    continue;
                }
            }

            // Apply groups filter
            if let Some(ref groups) = request.groups {
                if !groups.iter().any(|g| info.groups.contains(g)) {
                    continue;
                }
            }

            results.push(info);
        }

        // Sort by name for deterministic output
        results.sort_by(|a, b| a.name.cmp(&b.name));

        // Apply limit
        if let Some(limit) = request.limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    async fn delete_file(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        let ap = file_age_path(&self.store_path, vault, name)?;
        let mp = file_meta_path(&self.store_path, vault, name)?;

        if !ap.exists() && !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        if ap.exists() {
            fs::remove_file(&ap)
                .map_err(|e| BackendError::Internal(format!("remove file age: {e}")))?;
        }
        if mp.exists() {
            fs::remove_file(&mp)
                .map_err(|e| BackendError::Internal(format!("remove file meta: {e}")))?;
        }

        Ok(())
    }

    async fn get_file_info(&self, vault: &str, name: &str) -> Result<FileInfo, BackendError> {
        let mp = file_meta_path(&self.store_path, vault, name)?;
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        read_file_meta(&mp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::local::crypto::generate_keypair;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn test_file_backend() -> (LocalFileBackend, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        // Create default vault
        let vault_dir = store.join("vaults").join("default");
        fs::create_dir_all(vault_dir.join("files")).unwrap();
        let vault_meta = serde_json::json!({
            "name": "default",
            "created_at": Utc::now().to_rfc3339(),
            "tags": {}
        });
        fs::write(
            vault_dir.join(".vault.json"),
            serde_json::to_string_pretty(&vault_meta).unwrap(),
        )
        .unwrap();

        let backend = LocalFileBackend::new(store, identity, recipients);
        (backend, tmp)
    }

    #[tokio::test]
    async fn upload_and_download_file() {
        let (backend, _tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "cert.pem".into(),
            content: b"-----BEGIN CERTIFICATE-----\nMIIBxTCCA...".to_vec(),
            content_type: Some("application/x-pem-file".into()),
            groups: vec!["infra".into()],
            metadata: HashMap::new(),
            tags: HashMap::from([("env".into(), "prod".into())]),
        };

        let info = backend.upload_file("default", request, None).await.unwrap();
        assert_eq!(info.name, "cert.pem");
        assert_eq!(info.content_type, "application/x-pem-file");
        assert!(info.size > 0);

        // Download and verify
        let bytes = backend
            .download_file("default", "cert.pem", None)
            .await
            .unwrap();
        assert_eq!(bytes, b"-----BEGIN CERTIFICATE-----\nMIIBxTCCA...");
    }

    #[tokio::test]
    async fn rejects_traversal_vault_name_for_file_writes() {
        let (backend, tmp) = test_file_backend();

        let result = backend
            .upload_file(
                "../../outside",
                FileUploadRequest {
                    name: "escape.txt".into(),
                    content: b"content".to_vec(),
                    content_type: None,
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                },
                None,
            )
            .await;

        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert!(!tmp.path().join("outside").exists());
    }

    #[tokio::test]
    async fn rejects_traversal_vault_name_for_file_reads() {
        let (backend, _tmp) = test_file_backend();

        for result in [
            backend
                .download_file("../../outside", "any.txt", None)
                .await
                .map(|_| ()),
            backend
                .list_files(
                    "../../outside",
                    FileListRequest {
                        prefix: None,
                        groups: None,
                        limit: None,
                        delimiter: None,
                    },
                )
                .await
                .map(|_| ()),
            backend.delete_file("../../outside", "any.txt").await,
            backend
                .get_file_info("../../outside", "any.txt")
                .await
                .map(|_| ()),
        ] {
            assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        }
    }

    #[tokio::test]
    async fn operations_target_the_requested_vault_not_default() {
        let (backend, tmp) = test_file_backend();

        let upload = |vault: &'static str, content: &'static [u8]| {
            let backend = &backend;
            async move {
                backend
                    .upload_file(
                        vault,
                        FileUploadRequest {
                            name: "config.txt".into(),
                            content: content.to_vec(),
                            content_type: None,
                            groups: Vec::new(),
                            metadata: HashMap::new(),
                            tags: HashMap::new(),
                        },
                        None,
                    )
                    .await
                    .unwrap()
            }
        };

        // Same file name uploaded to two non-default vaults with different content.
        upload("dev", b"dev content").await;
        upload("prod", b"prod content").await;

        // Each vault stores its files under its own directory; the default
        // vault's files dir (pre-created by the test helper) stays empty.
        assert!(tmp.path().join("vaults/dev/files").exists());
        assert!(tmp.path().join("vaults/prod/files").exists());
        let mut default_entries = fs::read_dir(tmp.path().join("vaults/default/files")).unwrap();
        assert!(default_entries.next().is_none());

        // Downloads resolve per vault, not to the default vault.
        let dev = backend
            .download_file("dev", "config.txt", None)
            .await
            .unwrap();
        let prod = backend
            .download_file("prod", "config.txt", None)
            .await
            .unwrap();
        assert_eq!(dev, b"dev content");
        assert_eq!(prod, b"prod content");

        // Listing and metadata are scoped to the requested vault.
        let list_req = || FileListRequest {
            prefix: None,
            groups: None,
            limit: None,
            delimiter: None,
        };
        assert_eq!(
            backend.list_files("dev", list_req()).await.unwrap().len(),
            1
        );
        assert!(backend
            .list_files("default", list_req())
            .await
            .unwrap()
            .is_empty());
        backend.get_file_info("prod", "config.txt").await.unwrap();
        let missing = backend.get_file_info("default", "config.txt").await;
        assert!(matches!(missing, Err(BackendError::NotFound { .. })));

        // Deleting in one vault leaves the other vault untouched.
        backend.delete_file("dev", "config.txt").await.unwrap();
        let gone = backend.download_file("dev", "config.txt", None).await;
        assert!(matches!(gone, Err(BackendError::NotFound { .. })));
        let still_there = backend
            .download_file("prod", "config.txt", None)
            .await
            .unwrap();
        assert_eq!(still_there, b"prod content");
    }

    #[tokio::test]
    async fn list_files_with_filters() {
        let (backend, _tmp) = test_file_backend();

        // Upload several files
        for (name, groups) in [
            ("configs/app.yaml", vec!["prod"]),
            ("configs/db.yaml", vec!["prod", "db"]),
            ("scripts/deploy.sh", vec!["ops"]),
        ] {
            let request = FileUploadRequest {
                name: name.into(),
                content: b"content".to_vec(),
                content_type: None,
                groups: groups.into_iter().map(String::from).collect(),
                metadata: HashMap::new(),
                tags: HashMap::new(),
            };
            backend.upload_file("default", request, None).await.unwrap();
        }

        // List all
        let all = backend
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
        assert_eq!(all.len(), 3);

        // Filter by prefix
        let configs = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: Some("configs/".into()),
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(configs.len(), 2);

        // Filter by group
        let db_files = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: None,
                    groups: Some(vec!["db".into()]),
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(db_files.len(), 1);
        assert_eq!(db_files[0].name, "configs/db.yaml");

        // Limit
        let limited = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: None,
                    groups: None,
                    limit: Some(2),
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[tokio::test]
    async fn delete_file() {
        let (backend, _tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "to-delete.txt".into(),
            content: b"delete me".to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        backend.upload_file("default", request, None).await.unwrap();

        // File exists
        let info = backend
            .get_file_info("default", "to-delete.txt")
            .await
            .unwrap();
        assert_eq!(info.name, "to-delete.txt");

        // Delete
        backend
            .delete_file("default", "to-delete.txt")
            .await
            .unwrap();

        // Should be gone
        let result = backend.get_file_info("default", "to-delete.txt").await;
        assert!(matches!(result, Err(BackendError::NotFound { .. })));
    }

    #[tokio::test]
    async fn get_file_info_returns_metadata() {
        let (backend, _tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "info-test.bin".into(),
            content: vec![0u8; 1024],
            content_type: Some("application/octet-stream".into()),
            groups: vec!["test".into()],
            metadata: HashMap::from([("uploaded_by".into(), "test-agent".into())]),
            tags: HashMap::from([("version".into(), "1.0".into())]),
        };
        backend.upload_file("default", request, None).await.unwrap();

        let info = backend
            .get_file_info("default", "info-test.bin")
            .await
            .unwrap();
        assert_eq!(info.name, "info-test.bin");
        assert_eq!(info.size, 1024);
        assert_eq!(info.content_type, "application/octet-stream");
        assert_eq!(info.groups, vec!["test"]);
        assert_eq!(info.metadata.get("uploaded_by").unwrap(), "test-agent");
        assert_eq!(info.tags.get("version").unwrap(), "1.0");
    }

    #[tokio::test]
    async fn download_nonexistent_file_returns_not_found() {
        let (backend, _tmp) = test_file_backend();

        let result = backend
            .download_file("default", "nonexistent.txt", None)
            .await;
        assert!(matches!(result, Err(BackendError::NotFound { .. })));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn meta_json_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let (backend, tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "perm-test.bin".into(),
            content: b"secret data".to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        backend.upload_file("default", request, None).await.unwrap();

        let files_dir = tmp.path().join("vaults").join("default").join("files");
        let meta_path = fs::read_dir(&files_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with(".meta.json"))
            .expect("meta.json not found after upload");

        let mode = fs::metadata(&meta_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "meta.json mode should be 0600, got {mode:o}");
    }
}
