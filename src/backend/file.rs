//! File/blob backend trait.
//!
//! [`FileBackend`] defines the contract for file storage operations.
//! Only backends that advertise `has_file_storage` need to implement this.

use async_trait::async_trait;

use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::progress::ProgressReporter;

use super::error::BackendError;

/// Trait for file/blob storage operations.
///
/// All methods are required — if a backend exposes `files()`, it must
/// support the full file lifecycle.
///
/// Every method takes the target `vault` per call, mirroring
/// [`SecretBackend`](super::SecretBackend), so file operations follow the
/// active vault selection. Backends whose file storage is not vault-scoped
/// (e.g. Azure, where files live in one blob container per storage account)
/// may ignore the argument.
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — consumed by future backends.
#[async_trait]
pub trait FileBackend: Send + Sync {
    /// Upload a file. The optional [`ProgressReporter`] enables progress bars.
    async fn upload_file(
        &self,
        vault: &str,
        request: FileUploadRequest,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError>;

    /// Download a file's contents by name.
    async fn download_file(
        &self,
        vault: &str,
        name: &str,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<Vec<u8>, BackendError>;

    /// List files matching the request criteria.
    async fn list_files(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<FileInfo>, BackendError>;

    /// Delete a file by name.
    async fn delete_file(&self, vault: &str, name: &str) -> Result<(), BackendError>;

    /// Get metadata about a file without downloading it.
    async fn get_file_info(&self, vault: &str, name: &str) -> Result<FileInfo, BackendError>;
}
