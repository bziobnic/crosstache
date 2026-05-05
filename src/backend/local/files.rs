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
use crate::utils::helpers::create_private_dir;
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
    fs::write(path, json)
        .map_err(|e| BackendError::Internal(format!("write file meta {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// LocalFileBackend
// ---------------------------------------------------------------------------

/// File-backed file/blob operations using age encryption.
pub struct LocalFileBackend {
    store_path: PathBuf,
    vault: String,
    identity: age::x25519::Identity,
    recipients: Vec<age::x25519::Recipient>,
}

impl LocalFileBackend {
    /// Create a new `LocalFileBackend`.
    pub fn new(
        store_path: PathBuf,
        vault: String,
        identity: age::x25519::Identity,
        recipients: Vec<age::x25519::Recipient>,
    ) -> Self {
        Self {
            store_path,
            vault,
            identity,
            recipients,
        }
    }
}

#[async_trait]
impl FileBackend for LocalFileBackend {
    async fn upload_file(
        &self,
        request: FileUploadRequest,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        let fdir = files_dir(&self.store_path, &self.vault)?;
        create_private_dir(&fdir)
            .map_err(|e| BackendError::Internal(format!("mkdir files: {e}")))?;

        let original_size = request.content.len() as u64;
        let ap = file_age_path(&self.store_path, &self.vault, &request.name)?;
        let mp = file_meta_path(&self.store_path, &self.vault, &request.name)?;

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
        name: &str,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<Vec<u8>, BackendError> {
        let ap = file_age_path(&self.store_path, &self.vault, name)?;
        if !ap.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        crypto::decrypt_bytes_from_file(&ap, &self.identity)
    }

    async fn list_files(&self, request: FileListRequest) -> Result<Vec<FileInfo>, BackendError> {
        let fdir = files_dir(&self.store_path, &self.vault)?;
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

    async fn delete_file(&self, name: &str) -> Result<(), BackendError> {
        let ap = file_age_path(&self.store_path, &self.vault, name)?;
        let mp = file_meta_path(&self.store_path, &self.vault, name)?;

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

    async fn get_file_info(&self, name: &str) -> Result<FileInfo, BackendError> {
        let mp = file_meta_path(&self.store_path, &self.vault, name)?;
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

        let backend = LocalFileBackend::new(store, "default".into(), identity, recipients);
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

        let info = backend.upload_file(request, None).await.unwrap();
        assert_eq!(info.name, "cert.pem");
        assert_eq!(info.content_type, "application/x-pem-file");
        assert!(info.size > 0);

        // Download and verify
        let bytes = backend.download_file("cert.pem", None).await.unwrap();
        assert_eq!(bytes, b"-----BEGIN CERTIFICATE-----\nMIIBxTCCA...");
    }

    #[tokio::test]
    async fn rejects_traversal_vault_name_for_file_writes() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();
        let backend = LocalFileBackend::new(store, "../../outside".into(), identity, recipients);

        let result = backend
            .upload_file(
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
            backend.upload_file(request, None).await.unwrap();
        }

        // List all
        let all = backend
            .list_files(FileListRequest {
                prefix: None,
                groups: None,
                limit: None,
                delimiter: None,
            })
            .await
            .unwrap();
        assert_eq!(all.len(), 3);

        // Filter by prefix
        let configs = backend
            .list_files(FileListRequest {
                prefix: Some("configs/".into()),
                groups: None,
                limit: None,
                delimiter: None,
            })
            .await
            .unwrap();
        assert_eq!(configs.len(), 2);

        // Filter by group
        let db_files = backend
            .list_files(FileListRequest {
                prefix: None,
                groups: Some(vec!["db".into()]),
                limit: None,
                delimiter: None,
            })
            .await
            .unwrap();
        assert_eq!(db_files.len(), 1);
        assert_eq!(db_files[0].name, "configs/db.yaml");

        // Limit
        let limited = backend
            .list_files(FileListRequest {
                prefix: None,
                groups: None,
                limit: Some(2),
                delimiter: None,
            })
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
        backend.upload_file(request, None).await.unwrap();

        // File exists
        let info = backend.get_file_info("to-delete.txt").await.unwrap();
        assert_eq!(info.name, "to-delete.txt");

        // Delete
        backend.delete_file("to-delete.txt").await.unwrap();

        // Should be gone
        let result = backend.get_file_info("to-delete.txt").await;
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
        backend.upload_file(request, None).await.unwrap();

        let info = backend.get_file_info("info-test.bin").await.unwrap();
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

        let result = backend.download_file("nonexistent.txt", None).await;
        assert!(matches!(result, Err(BackendError::NotFound { .. })));
    }
}
