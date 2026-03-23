//! Core blob storage manager for file operations
//!
//! This module provides the main BlobManager struct and basic file operations
//! including upload, download, list, and delete functionality.
//!
//! This is currently a working placeholder implementation that demonstrates
//! the expected interface. The actual Azure Blob Storage integration would
//! be implemented here using the azure_storage_blobs crate.

use crate::auth::provider::AzureAuthProvider;
use crate::blob::models::*;
use crate::error::{CrosstacheError, Result};
use azure_core::request_options::Metadata;
use azure_storage_blobs::prelude::*;
// use azure_core::auth::TokenCredential; // Not needed for current implementation
use chrono::Utc;
use futures::TryStreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWrite;

/// Core blob storage manager
pub struct BlobManager {
    storage_account: String,
    container_name: String,
    auth_provider: Arc<dyn AzureAuthProvider>,
    /// Chunk size for block-based large file uploads (megabytes).
    chunk_size_mb: usize,
    /// Maximum number of concurrent block uploads.
    max_concurrent_uploads: usize,
}

impl BlobManager {
    /// Create a new BlobManager instance with default chunk/concurrency settings.
    pub fn new(
        auth_provider: Arc<dyn AzureAuthProvider>,
        storage_account: String,
        container_name: String,
    ) -> Result<Self> {
        Ok(Self {
            storage_account,
            container_name,
            auth_provider,
            chunk_size_mb: 4,
            max_concurrent_uploads: 3,
        })
    }

    /// Override the chunk size (MB) and maximum concurrent uploads used by
    /// `upload_large_file`.  Returns `self` for builder-style chaining.
    pub fn with_blob_config(mut self, chunk_size_mb: usize, max_concurrent_uploads: usize) -> Self {
        self.chunk_size_mb = chunk_size_mb.max(1);
        self.max_concurrent_uploads = max_concurrent_uploads.max(1);
        self
    }

    /// Upload a file to blob storage
    pub async fn upload_file(&self, request: FileUploadRequest) -> Result<FileInfo> {
        // Determine content type
        let content_type = request.content_type.unwrap_or_else(|| {
            mime_guess::from_path(&request.name)
                .first_or_octet_stream()
                .to_string()
        });

        // Build metadata with groups
        let mut metadata = request.metadata.clone();
        if !request.groups.is_empty() {
            metadata.insert("groups".to_string(), request.groups.join(","));
        }
        metadata.insert("uploaded_by".to_string(), "crosstache".to_string());
        metadata.insert("uploaded_at".to_string(), Utc::now().to_rfc3339());

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();

        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container client
        let container_client = blob_service.container_client(&self.container_name);

        // Get blob client for the specific file
        let blob_client = container_client.blob_client(&request.name);

        // Store content length before moving request.content
        let content_length = request.content.len() as u64;

        // Build SDK Metadata from our HashMap
        let mut sdk_metadata = Metadata::new();
        for (k, v) in &metadata {
            sdk_metadata.insert(k.clone(), v.clone());
        }

        // Perform the upload, setting metadata in a single API call.
        // Note: .tags() is intentionally omitted — setting blob tags requires
        // Storage Blob Data Owner; using it causes 403 for accounts with only
        // Storage Blob Data Contributor.
        let response = blob_client
            .put_block_blob(request.content)
            .content_type(&content_type)
            .metadata(sdk_metadata)
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to upload blob: {e}")))?;

        // Extract response data and build FileInfo
        let etag = response.etag.to_string();

        // Convert Azure response datetime from time::OffsetDateTime to chrono::DateTime<Utc>
        let last_modified = {
            let timestamp = response.last_modified.unix_timestamp();
            chrono::DateTime::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now)
        };

        Ok(FileInfo {
            name: request.name,
            size: content_length,
            content_type,
            last_modified,
            etag,
            groups: request.groups,
            metadata,
            tags: request.tags,
        })
    }

    /// List files in the container
    pub async fn list_files(&self, request: FileListRequest) -> Result<Vec<FileInfo>> {
        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container client
        let container_client = blob_service.container_client(&self.container_name);

        // Create list blobs request with filters
        let mut list_builder = container_client.list_blobs();

        // Apply prefix filter if provided
        if let Some(prefix) = request.prefix.clone() {
            list_builder = list_builder.prefix(prefix);
        }

        // Enable metadata inclusion (tags are omitted: include=tags requires
        // Storage Blob Data Owner, which exceeds the typical Contributor role)
        list_builder = list_builder.include_metadata(true);

        // Execute the list request - collect all pages
        let mut stream = list_builder.into_stream();
        let mut file_infos = Vec::new();

        // Process each page of results
        while let Some(page) = stream
            .try_next()
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to list blobs: {e}")))?
        {
            // Process each blob in this page
            for blob_item in page.blobs.blobs() {
                // Extract blob information
                let name = blob_item.name.clone();
                let size = blob_item.properties.content_length;
                let content_type = blob_item.properties.content_type.clone();

                // Convert time::OffsetDateTime to chrono::DateTime<Utc>
                let last_modified = {
                    let timestamp = blob_item.properties.last_modified.unix_timestamp();
                    chrono::DateTime::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now)
                };

                let etag = blob_item.properties.etag.to_string();

                // Process metadata - handle Option<HashMap<String, String>>
                let metadata = blob_item.metadata.clone().unwrap_or_default();

                // Extract groups from metadata
                let groups: Vec<String> = metadata
                    .get("groups")
                    .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default();

                // Apply group-based filtering if requested
                if let Some(filter_groups) = &request.groups {
                    let matches_group = filter_groups.iter().any(|fg| groups.contains(fg));
                    if !matches_group {
                        continue; // Skip this blob
                    }
                }

                // Extract tags if they were requested and returned inline
                let tags: HashMap<String, String> = blob_item
                    .tags
                    .clone()
                    .map(HashMap::from)
                    .unwrap_or_default();

                // Build FileInfo struct
                let file_info = FileInfo {
                    name,
                    size,
                    content_type,
                    last_modified,
                    etag,
                    groups,
                    metadata,
                    tags,
                };

                file_infos.push(file_info);
            }
        }

        Ok(file_infos)
    }

    /// List files and directories hierarchically at a specific prefix level
    pub async fn list_files_hierarchical(
        &self,
        request: FileListRequest,
    ) -> Result<Vec<BlobListItem>> {
        use crate::blob::models::BlobListItem;

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container client
        let container_client = blob_service.container_client(&self.container_name);

        // Create list blobs request with delimiter for hierarchical listing
        let mut list_builder = container_client.list_blobs();

        // Apply prefix filter if provided (normalize it first)
        let normalized_prefix = normalize_prefix(request.prefix.clone());
        if let Some(prefix) = normalized_prefix.clone() {
            list_builder = list_builder.prefix(prefix);
        }

        // Apply delimiter for hierarchical listing
        if let Some(delimiter) = request.delimiter.clone() {
            list_builder = list_builder.delimiter(delimiter);
        }

        // Enable metadata inclusion for files (tags omitted: requires Storage Blob Data Owner)
        list_builder = list_builder.include_metadata(true);

        // Execute the list request - collect all pages
        let mut stream = list_builder.into_stream();
        let mut items = Vec::new();

        // Process each page of results
        while let Some(page) = stream
            .try_next()
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to list blobs: {e}")))?
        {
            // Process blob prefixes (directories) first
            for prefix_item in page.blobs.prefixes() {
                let full_path = prefix_item.name.clone();

                // Extract just the directory name (after the current prefix)
                let dir_name = if let Some(ref current_prefix) = normalized_prefix {
                    full_path
                        .strip_prefix(current_prefix)
                        .unwrap_or(&full_path)
                        .to_string()
                } else {
                    full_path.clone()
                };

                items.push(BlobListItem::Directory {
                    name: dir_name,
                    full_path,
                });
            }

            // Process blobs (files)
            for blob_item in page.blobs.blobs() {
                // Extract blob information
                let name = blob_item.name.clone();
                let size = blob_item.properties.content_length;
                let content_type = blob_item.properties.content_type.clone();

                // Convert time::OffsetDateTime to chrono::DateTime<Utc>
                let last_modified = {
                    let timestamp = blob_item.properties.last_modified.unix_timestamp();
                    chrono::DateTime::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now)
                };

                let etag = blob_item.properties.etag.to_string();

                // Process metadata - handle Option<HashMap<String, String>>
                let metadata = blob_item.metadata.clone().unwrap_or_default();

                // Extract groups from metadata
                let groups: Vec<String> = metadata
                    .get("groups")
                    .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default();

                // Apply group-based filtering if requested
                if let Some(filter_groups) = &request.groups {
                    let matches_group = filter_groups.iter().any(|fg| groups.contains(fg));
                    if !matches_group {
                        continue; // Skip this blob
                    }
                }

                // Extract tags if they were requested and returned inline
                let tags: HashMap<String, String> = blob_item
                    .tags
                    .clone()
                    .map(HashMap::from)
                    .unwrap_or_default();

                // Build FileInfo struct
                let file_info = FileInfo {
                    name,
                    size,
                    content_type,
                    last_modified,
                    etag,
                    groups,
                    metadata,
                    tags,
                };

                items.push(BlobListItem::File(file_info));
            }
        }

        // Sort items: directories first, then files (both alphabetically)
        sort_blob_items(&mut items);

        // Apply limit if specified
        if let Some(limit) = request.limit {
            items.truncate(limit);
        }

        Ok(items)
    }

    /// Download a file from blob storage
    pub async fn download_file(&self, request: FileDownloadRequest) -> Result<Vec<u8>> {
        // Validate download request parameters
        if request.name.trim().is_empty() {
            return Err(CrosstacheError::config(
                "File name cannot be empty".to_string(),
            ));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(&request.name);

        // Check if blob exists and get its size before attempting download
        let properties = blob_client.get_properties().await.map_err(|e| {
            let error_msg = e.to_string().to_lowercase();
            if error_msg.contains("404") || error_msg.contains("not found") {
                CrosstacheError::vault_not_found(format!("File '{}' not found", request.name))
            } else {
                CrosstacheError::azure_api(format!("Failed to check if blob exists: {e}"))
            }
        })?;

        // Handle empty files specially to avoid HTTP 416 error
        // Azure's get_content() fails with 416 Range Not Satisfiable for 0-byte blobs
        let content_length = properties.blob.properties.content_length;
        if content_length == 0 {
            // Return empty vec for 0-byte files
            return Ok(Vec::new());
        }

        // Download the entire blob at once (recommended for smaller files)
        let blob_content = blob_client
            .get_content()
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to download blob: {e}")))?;

        Ok(blob_content)
    }

    /// Delete a file from blob storage
    pub async fn delete_file(&self, name: &str) -> Result<()> {
        // Validate file name parameter
        if name.trim().is_empty() {
            return Err(CrosstacheError::config(
                "File name cannot be empty".to_string(),
            ));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(name);

        // Check if blob exists before deletion (optional - Azure will return error if not found)
        let exists = blob_client.get_properties().await.is_ok();
        if !exists {
            return Err(CrosstacheError::vault_not_found(format!(
                "File '{name}' not found"
            )));
        }

        // Implement blob deletion
        blob_client.delete().await.map_err(|e| {
            let error_msg = e.to_string().to_lowercase();
            if error_msg.contains("404") || error_msg.contains("not found") {
                CrosstacheError::vault_not_found(format!("File '{name}' not found"))
            } else {
                CrosstacheError::azure_api(format!("Failed to delete blob: {e}"))
            }
        })?;

        // Deletion was successful
        Ok(())
    }

    /// Get file metadata without downloading content
    pub async fn get_file_info(&self, name: &str) -> Result<FileInfo> {
        // Validate file name parameter
        if name.trim().is_empty() {
            return Err(CrosstacheError::config(
                "File name cannot be empty".to_string(),
            ));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(name);

        // Get blob properties
        let properties = blob_client.get_properties().await.map_err(|e| {
            let error_msg = e.to_string().to_lowercase();
            if error_msg.contains("404") || error_msg.contains("not found") {
                CrosstacheError::vault_not_found(format!("File '{name}' not found"))
            } else {
                CrosstacheError::azure_api(format!("Failed to get blob properties: {e}"))
            }
        })?;

        // Extract all properties
        let size = properties.blob.properties.content_length;
        let content_type = properties.blob.properties.content_type.clone();
        let last_modified = {
            let timestamp = properties.blob.properties.last_modified.unix_timestamp();
            chrono::DateTime::from_timestamp(timestamp, 0).unwrap_or_else(Utc::now)
        };
        let etag = properties.blob.properties.etag.to_string();

        // Get custom metadata including groups
        let metadata = properties.blob.metadata.clone().unwrap_or_default();

        // Extract groups from metadata
        let groups: Vec<String> = metadata
            .get("groups")
            .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        // Fetch tags via a separate API call (get_properties does not include them).
        // Silently falls back to empty tags on 403 (requires Storage Blob Data Owner
        // role or 't' SAS permission).
        let tags: HashMap<String, String> = match blob_client.get_tags().await {
            Ok(r) => HashMap::from(r.tags),
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                if msg.contains("403") || msg.contains("authorizationpermissionmismatch") {
                    tracing::debug!(
                        "Tag read for '{}' returned 403; tags will be empty. \
                         Grant Storage Blob Data Owner or add 't' to the SAS token.",
                        name
                    );
                } else {
                    tracing::warn!("Failed to fetch tags for '{}': {}", name, e);
                }
                HashMap::new()
            }
        };

        // Build complete FileInfo with all available data
        Ok(FileInfo {
            name: name.to_string(),
            size,
            content_type,
            last_modified,
            etag,
            groups,
            metadata,
            tags,
        })
    }

    /// Stream download a large file
    #[allow(dead_code)]
    pub async fn download_file_stream<W: AsyncWrite + Unpin>(
        &self,
        name: &str,
        mut writer: W,
    ) -> Result<()> {
        // Validate file name
        if name.trim().is_empty() {
            return Err(CrosstacheError::config(
                "File name cannot be empty".to_string(),
            ));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);

        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(name);

        // Check if blob exists and get its size before attempting download
        let properties = blob_client.get_properties().await.map_err(|e| {
            let error_msg = e.to_string().to_lowercase();
            if error_msg.contains("404") || error_msg.contains("not found") {
                CrosstacheError::vault_not_found(format!("File '{name}' not found"))
            } else {
                CrosstacheError::azure_api(format!("Failed to check if blob exists: {e}"))
            }
        })?;

        // Handle empty files specially to avoid HTTP 416 error
        // Azure's get_content() fails with 416 Range Not Satisfiable for 0-byte blobs
        let content_length = properties.blob.properties.content_length;
        if content_length == 0 {
            // For empty files, just flush the writer and return
            use tokio::io::AsyncWriteExt;
            writer
                .flush()
                .await
                .map_err(|e| CrosstacheError::unknown(format!("Failed to flush data: {e}")))?;
            return Ok(());
        }

        // For streaming large files, we'll use the get_content method for now
        // The Azure SDK v0.21 handles chunking internally for better reliability
        let blob_content = blob_client
            .get_content()
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to download blob: {e}")))?;

        // Stream the data and write to the provided writer
        use tokio::io::AsyncWriteExt;

        // Write all content at once (Azure SDK already optimized the download)
        writer
            .write_all(&blob_content)
            .await
            .map_err(|e| CrosstacheError::unknown(format!("Failed to write blob data: {e}")))?;

        // Ensure all data is flushed
        writer
            .flush()
            .await
            .map_err(|e| CrosstacheError::unknown(format!("Failed to flush data: {e}")))?;

        Ok(())
    }

    /// Upload a large file to blob storage using Azure block blob chunked upload.
    ///
    /// The file is read in `chunk_size_mb`-sized blocks. Each block is uploaded
    /// with [`BlobClient::put_block`] and, once all blocks are staged, committed
    /// with [`BlobClient::put_block_list`].  Up to `max_concurrent_uploads`
    /// block uploads run in parallel, controlled by a [`tokio::sync::Semaphore`].
    ///
    /// After the commit, blob properties are fetched so that the returned
    /// [`FileInfo`] contains the real server-side `size`, `etag`, and
    /// `last_modified` values.
    pub async fn upload_large_file<R: tokio::io::AsyncRead + Unpin>(
        &self,
        name: &str,
        mut reader: R,
        _file_size: u64,
        metadata: HashMap<String, String>,
        tags: HashMap<String, String>,
    ) -> Result<FileInfo> {
        use tokio::sync::Semaphore;

        let content_type = mime_guess::from_path(name)
            .first_or_octet_stream()
            .to_string();
        let chunk_size = self.chunk_size_mb * 1024 * 1024;

        // Build blob client.
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(name);

        let semaphore = Arc::new(Semaphore::new(self.max_concurrent_uploads));
        let mut block_list = BlockList::default();
        let mut block_idx: u32 = 0;
        let mut upload_tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

        // Read chunks and spawn concurrent block uploads.
        loop {
            let chunk = read_chunk(&mut reader, chunk_size)
                .await
                .map_err(|e| CrosstacheError::unknown(format!("Failed to read file data: {e}")))?;
            if chunk.is_empty() {
                break;
            }

            let block_id = generate_block_id(block_idx);
            block_list
                .blocks
                .push(BlobBlockType::new_uncommitted(block_id.clone()));
            block_idx += 1;

            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| CrosstacheError::unknown(format!("Semaphore error: {e}")))?;
            let blob_client = blob_client.clone();
            let chunk_bytes = chunk; // Vec<u8> satisfies Into<azure_core::Body>

            upload_tasks.push(tokio::spawn(async move {
                let _permit = permit; // held for the duration of the upload
                blob_client
                    .put_block(block_id, chunk_bytes)
                    .await
                    .map_err(|e| {
                        CrosstacheError::azure_api(format!("Failed to upload block: {e}"))
                    })?;
                Ok(())
            }));
        }

        // Wait for all block uploads to finish.
        for task in upload_tasks {
            task.await
                .map_err(|e| CrosstacheError::unknown(format!("Upload task panicked: {e}")))?
                .map_err(|e: CrosstacheError| e)?;
        }

        if block_list.blocks.is_empty() {
            // Zero-byte file: fall back to a simple put_block_blob so the blob
            // actually exists before we query its properties.
            blob_client
                .put_block_blob(vec![])
                .content_type(&content_type)
                .await
                .map_err(|e| {
                    CrosstacheError::azure_api(format!("Failed to upload empty blob: {e}"))
                })?;
        } else {
            // Commit the staged blocks.
            blob_client
                .put_block_list(block_list)
                .content_type(&content_type)
                .await
                .map_err(|e| {
                    CrosstacheError::azure_api(format!("Failed to commit block list: {e}"))
                })?;
        }

        // Fetch the committed blob's server-side properties for an accurate FileInfo.
        let properties = blob_client
            .get_properties()
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to get blob properties after upload: {e}")))?;

        let size = properties.blob.properties.content_length;
        let last_modified = {
            let ts = properties.blob.properties.last_modified.unix_timestamp();
            chrono::DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
        };
        let etag = properties.blob.properties.etag.to_string();

        let groups: Vec<String> = metadata
            .get("groups")
            .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        tracing::info!(
            name = name,
            blocks = block_idx,
            bytes = size,
            "Large file upload committed"
        );

        Ok(FileInfo {
            name: name.to_string(),
            size,
            content_type,
            last_modified,
            etag,
            groups,
            metadata,
            tags,
        })
    }

    /// Get the container name
    #[allow(dead_code)]
    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    /// Get the storage account name
    #[allow(dead_code)]
    pub fn storage_account(&self) -> &str {
        &self.storage_account
    }
}

/// Normalize a prefix by ensuring it ends with '/' if non-empty
fn normalize_prefix(prefix: Option<String>) -> Option<String> {
    prefix.and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            None
        } else if trimmed.ends_with('/') {
            Some(trimmed.to_string())
        } else {
            Some(format!("{}/", trimmed))
        }
    })
}

/// Sort blob items: directories first, then files (both alphabetically)
fn sort_blob_items(items: &mut [BlobListItem]) {
    use crate::blob::models::BlobListItem;

    items.sort_by(|a, b| {
        match (a, b) {
            // Directories before files
            (BlobListItem::Directory { .. }, BlobListItem::File(_)) => std::cmp::Ordering::Less,
            (BlobListItem::File(_), BlobListItem::Directory { .. }) => std::cmp::Ordering::Greater,

            // Both directories: alphabetical by name
            (
                BlobListItem::Directory { name: n1, .. },
                BlobListItem::Directory { name: n2, .. },
            ) => n1.to_lowercase().cmp(&n2.to_lowercase()),

            // Both files: alphabetical by name
            (BlobListItem::File(f1), BlobListItem::File(f2)) => {
                f1.name.to_lowercase().cmp(&f2.name.to_lowercase())
            }
        }
    });
}

/// Generate a fixed-length block ID for the given zero-based block index.
///
/// Azure requires all block IDs within a block list to have the same length.
/// We encode the 4-byte big-endian representation of `index`, giving a
/// constant 4-byte payload for every index value.
fn generate_block_id(index: u32) -> azure_storage_blobs::prelude::BlockId {
    // to_be_bytes() gives a fixed 4-byte array; Vec<u8> satisfies Into<bytes::Bytes>.
    azure_storage_blobs::prelude::BlockId::new(index.to_be_bytes().to_vec())
}

/// Read up to `chunk_size` bytes from `reader`, returning however many bytes
/// were actually available (may be less at EOF, zero when already at EOF).
async fn read_chunk<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    chunk_size: usize,
) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut chunk = vec![0u8; chunk_size];
    let mut bytes_read = 0;
    while bytes_read < chunk_size {
        let n = reader.read(&mut chunk[bytes_read..]).await?;
        if n == 0 {
            break;
        }
        bytes_read += n;
    }
    chunk.truncate(bytes_read);
    Ok(chunk)
}

/// Format file size in human-readable format
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];

    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

/// Helper function to create a BlobManager from configuration
pub fn create_blob_manager(config: &crate::config::Config) -> Result<BlobManager> {
    use crate::auth::provider::DefaultAzureCredentialProvider;

    let blob_config = config.get_blob_config();

    if blob_config.storage_account.is_empty() {
        return Err(CrosstacheError::config(
            "No blob storage configured. Run 'xv init' to set up blob storage.",
        ));
    }

    let auth_provider = Arc::new(DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )?) as Arc<dyn AzureAuthProvider>;

    Ok(BlobManager::new(
        auth_provider,
        blob_config.storage_account,
        blob_config.container_name,
    )?
    .with_blob_config(blob_config.chunk_size_mb, blob_config.max_concurrent_uploads))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── generate_block_id ────────────────────────────────────────────────────

    #[test]
    fn test_generate_block_id_fixed_length() {
        let id0 = generate_block_id(0);
        let id1 = generate_block_id(1);
        let id_max = generate_block_id(u32::MAX);
        // Azure requires every block ID in a list to have the same byte length.
        assert_eq!(id0.bytes().len(), id1.bytes().len());
        assert_eq!(id0.bytes().len(), id_max.bytes().len());
    }

    #[test]
    fn test_generate_block_id_unique() {
        let id0 = generate_block_id(0);
        let id1 = generate_block_id(1);
        let id2 = generate_block_id(2);
        assert_ne!(id0.bytes(), id1.bytes());
        assert_ne!(id1.bytes(), id2.bytes());
        assert_ne!(id0.bytes(), id2.bytes());
    }

    #[test]
    fn test_generate_block_id_within_azure_limit() {
        // Azure: block ID must not exceed 64 bytes before base64 encoding.
        let id = generate_block_id(u32::MAX);
        assert!(id.bytes().len() <= 64);
    }

    // ── read_chunk ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_read_chunk_returns_full_chunk_when_data_available() {
        let data = vec![0u8; 100];
        let mut cursor = std::io::Cursor::new(data.clone());
        let chunk = read_chunk(&mut cursor, 50).await.unwrap();
        assert_eq!(chunk.len(), 50);
        assert_eq!(chunk, &data[..50]);
    }

    #[tokio::test]
    async fn test_read_chunk_returns_partial_at_eof() {
        let data = vec![1u8; 30];
        let mut cursor = std::io::Cursor::new(data.clone());
        let chunk = read_chunk(&mut cursor, 50).await.unwrap();
        assert_eq!(chunk.len(), 30);
        assert_eq!(chunk, data);
    }

    #[tokio::test]
    async fn test_read_chunk_returns_empty_at_eof() {
        let data: Vec<u8> = vec![];
        let mut cursor = std::io::Cursor::new(data);
        let chunk = read_chunk(&mut cursor, 50).await.unwrap();
        assert!(chunk.is_empty());
    }

    #[tokio::test]
    async fn test_chunk_splitting_produces_correct_count_and_sizes() {
        // 250 bytes / 100-byte chunks → 3 chunks: 100, 100, 50
        let data = (0u8..=249).collect::<Vec<_>>();
        let mut cursor = std::io::Cursor::new(data.clone());

        let mut chunks: Vec<Vec<u8>> = Vec::new();
        loop {
            let chunk = read_chunk(&mut cursor, 100).await.unwrap();
            if chunk.is_empty() {
                break;
            }
            chunks.push(chunk);
        }

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 100);
        assert_eq!(chunks[1].len(), 100);
        assert_eq!(chunks[2].len(), 50);
        // Verify content integrity
        assert_eq!(chunks[0], &data[..100]);
        assert_eq!(chunks[1], &data[100..200]);
        assert_eq!(chunks[2], &data[200..]);
    }
}
