//! Azure file/blob backend adapter.
//!
//! Wraps the existing [`BlobManager`] behind the new [`FileBackend`] trait.
//! Feature-gated behind `file-ops`.

#[allow(unused_imports)]
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::error::BackendError;
use crate::backend::file::FileBackend;
use crate::blob::manager::BlobManager;
use crate::blob::models::{FileDownloadRequest, FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::progress::{NoopReporter, ProgressReporter};

use super::map_error;

/// Adapter that implements [`FileBackend`] by delegating to an existing
/// [`BlobManager`] instance.
#[allow(dead_code)]
pub struct AzureFileBackend {
    inner: Arc<BlobManager>,
}

impl AzureFileBackend {
    /// Wrap an existing `BlobManager`.
    #[allow(dead_code)]
    pub fn new(inner: Arc<BlobManager>) -> Self {
        Self { inner }
    }
}

/// Azure blob file storage is scoped to one container per storage account,
/// not per vault, so the `vault` argument is ignored.
#[async_trait]
impl FileBackend for AzureFileBackend {
    async fn upload_file(
        &self,
        _vault: &str,
        request: FileUploadRequest,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        let null = NoopReporter;
        let reporter: &dyn ProgressReporter = reporter.unwrap_or(&null);
        self.inner
            .upload_file(request, reporter)
            .await
            .map_err(map_error)
    }

    async fn download_file(
        &self,
        _vault: &str,
        name: &str,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<Vec<u8>, BackendError> {
        let null = NoopReporter;
        let reporter: &dyn ProgressReporter = reporter.unwrap_or(&null);
        let request = FileDownloadRequest {
            name: name.to_string(),
        };
        self.inner
            .download_file(request, reporter)
            .await
            .map_err(map_error)
    }

    async fn list_files(
        &self,
        _vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<FileInfo>, BackendError> {
        self.inner.list_files(request).await.map_err(map_error)
    }

    async fn delete_file(&self, _vault: &str, name: &str) -> Result<(), BackendError> {
        self.inner.delete_file(name).await.map_err(map_error)
    }

    async fn get_file_info(&self, _vault: &str, name: &str) -> Result<FileInfo, BackendError> {
        self.inner.get_file_info(name).await.map_err(map_error)
    }
}
