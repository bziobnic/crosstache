//! File/blob backend trait.
//!
//! [`FileBackend`] defines the contract for file storage operations.
//! Only backends that advertise `has_file_storage` need to implement this.

use async_trait::async_trait;

use crate::blob::models::{BlobListItem, FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::progress::ProgressReporter;

use super::error::BackendError;

/// Trait for file/blob storage operations.
///
/// The five lifecycle methods are required — if a backend exposes `files()`,
/// it must support upload/download/list/delete/info. [`list_files_hierarchical`]
/// is provided by a default derivation and overridden by backends with a native
/// delimited listing.
///
/// Every method takes the target `vault` per call, mirroring
/// [`SecretBackend`](super::SecretBackend), so file operations follow the
/// active vault selection. Backends whose file storage is not vault-scoped
/// (e.g. Azure, where files live in one blob container per storage account)
/// may ignore the argument.
///
/// [`list_files_hierarchical`]: FileBackend::list_files_hierarchical
#[async_trait]
pub trait FileBackend: Send + Sync {
    /// Upload a file. The optional [`ProgressReporter`] enables progress bars.
    async fn upload_file(
        &self,
        vault: &str,
        request: FileUploadRequest,
        reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError>;

    /// Create a file only when the destination is absent at the backend's
    /// commit point. Implementations must never replace an existing file.
    ///
    /// Backends that cannot provide this atomic guarantee reject the
    /// operation; callers must not emulate it with a check-then-upload race.
    async fn upload_file_if_absent(
        &self,
        _vault: &str,
        _request: FileUploadRequest,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        Err(BackendError::Unsupported(
            "atomic create-only file upload".into(),
        ))
    }

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

    /// List files one level deep, collapsing everything below the next
    /// `request.delimiter` boundary into [`BlobListItem::Directory`] entries
    /// (the `xv file ls` non-recursive view).
    ///
    /// The default derives the hierarchy from the flat [`list_files`] result:
    /// a non-empty `prefix` is treated as a FOLDER (the delimiter is appended)
    /// so `docs` lists the contents of `docs/` rather than sibling trees like
    /// `docs-extra/`, matching the native Azure/AWS delimited listing. Backends
    /// with a native delimited API (Azure blob prefixes, S3 `CommonPrefixes`)
    /// override this for efficiency. With no delimiter every entry is a file.
    ///
    /// [`list_files`]: FileBackend::list_files
    async fn list_files_hierarchical(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<BlobListItem>, BackendError> {
        use std::collections::BTreeMap;

        let delimiter = request.delimiter.clone().filter(|d| !d.is_empty());
        let prefix = request.prefix.clone().unwrap_or_default();
        let files = self.list_files(vault, request).await?;

        // No delimiter → flat listing (every entry is a file).
        let Some(delimiter) = delimiter else {
            return Ok(files.into_iter().map(BlobListItem::File).collect());
        };

        // Treat a non-empty prefix as a FOLDER (append the delimiter).
        let folder = if prefix.is_empty() || prefix.ends_with(&delimiter) {
            prefix
        } else {
            format!("{prefix}{delimiter}")
        };

        let mut dirs: BTreeMap<String, String> = BTreeMap::new(); // name -> full_path
        let mut plain_files: Vec<FileInfo> = Vec::new();
        for f in files {
            let Some(rest) = f.name.strip_prefix(&folder) else {
                continue; // a sibling tree outside this folder
            };
            match rest.split_once(&delimiter) {
                Some((dir, _)) if !dir.is_empty() => {
                    dirs.entry(dir.to_string())
                        .or_insert_with(|| format!("{folder}{dir}{delimiter}"));
                }
                _ => plain_files.push(f),
            }
        }

        let mut items: Vec<BlobListItem> = dirs
            .into_iter()
            .map(|(name, full_path)| BlobListItem::Directory { name, full_path })
            .collect();
        items.extend(plain_files.into_iter().map(BlobListItem::File));
        Ok(items)
    }
}

#[cfg(test)]
mod default_hierarchical_tests {
    //! Behavior lock for the default `list_files_hierarchical` derivation
    //! (the impl local backends inherit). A flat-list stub feeds canned
    //! `FileInfo`s through the default, asserting one-level dir/file collapse
    //! and folder-prefix semantics.

    use super::*;
    use crate::blob::models::BlobListItem;
    use std::collections::HashMap;

    fn fi(name: &str) -> FileInfo {
        FileInfo {
            name: name.to_string(),
            size: 0,
            content_type: "application/octet-stream".into(),
            last_modified: chrono::Utc::now(),
            etag: String::new(),
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        }
    }

    /// Stub whose `list_files` returns a canned flat list, prefix-filtered by
    /// exact `starts_with` (as the local/aws flat listers do). Only the default
    /// `list_files_hierarchical` is exercised.
    struct FlatStub {
        files: Vec<FileInfo>,
    }

    #[async_trait]
    impl FileBackend for FlatStub {
        async fn upload_file(
            &self,
            _v: &str,
            _r: FileUploadRequest,
            _p: Option<&dyn ProgressReporter>,
        ) -> Result<FileInfo, BackendError> {
            unimplemented!()
        }
        async fn download_file(
            &self,
            _v: &str,
            _n: &str,
            _p: Option<&dyn ProgressReporter>,
        ) -> Result<Vec<u8>, BackendError> {
            unimplemented!()
        }
        async fn list_files(
            &self,
            _v: &str,
            request: FileListRequest,
        ) -> Result<Vec<FileInfo>, BackendError> {
            Ok(self
                .files
                .iter()
                .filter(|f| {
                    request
                        .prefix
                        .as_ref()
                        .is_none_or(|p| f.name.starts_with(p))
                })
                .cloned()
                .collect())
        }
        async fn delete_file(&self, _v: &str, _n: &str) -> Result<(), BackendError> {
            unimplemented!()
        }
        async fn get_file_info(&self, _v: &str, _n: &str) -> Result<FileInfo, BackendError> {
            unimplemented!()
        }
    }

    fn req(prefix: Option<&str>, delimiter: Option<&str>) -> FileListRequest {
        FileListRequest {
            prefix: prefix.map(str::to_string),
            groups: None,
            limit: None,
            delimiter: delimiter.map(str::to_string),
        }
    }

    fn names(items: &[BlobListItem]) -> (Vec<String>, Vec<String>) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for it in items {
            match it {
                BlobListItem::Directory { full_path, .. } => dirs.push(full_path.clone()),
                BlobListItem::File(f) => files.push(f.name.clone()),
            }
        }
        (dirs, files)
    }

    fn stub() -> FlatStub {
        FlatStub {
            files: vec![
                fi("a.txt"),
                fi("docs/x.txt"),
                fi("docs/y.txt"),
                fi("docs/sub/z.txt"),
                fi("docs-extra/e.txt"),
                fi("img/p.png"),
            ],
        }
    }

    #[tokio::test]
    async fn top_level_collapses_subtrees_into_directories() {
        let items = stub()
            .list_files_hierarchical("v", req(None, Some("/")))
            .await
            .unwrap();
        let (dirs, files) = names(&items);
        // Directories sorted by name: "docs" < "docs-extra" < "img".
        assert_eq!(dirs, vec!["docs/", "docs-extra/", "img/"]);
        assert_eq!(files, vec!["a.txt"]);
    }

    #[tokio::test]
    async fn folder_prefix_lists_only_that_folders_contents() {
        // `docs` is treated as the folder `docs/`, so `docs-extra/...` (a
        // sibling tree that shares the raw prefix) is excluded.
        let items = stub()
            .list_files_hierarchical("v", req(Some("docs"), Some("/")))
            .await
            .unwrap();
        let (dirs, files) = names(&items);
        assert_eq!(dirs, vec!["docs/sub/"]);
        assert_eq!(files, vec!["docs/x.txt", "docs/y.txt"]);
    }

    #[tokio::test]
    async fn no_delimiter_is_flat_files_only() {
        let items = stub()
            .list_files_hierarchical("v", req(None, None))
            .await
            .unwrap();
        let (dirs, files) = names(&items);
        assert!(dirs.is_empty());
        assert_eq!(files.len(), 6);
    }
}
