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
}

impl BlobManager {
    /// Create a new BlobManager instance
    pub fn new(
        auth_provider: Arc<dyn AzureAuthProvider>,
        storage_account: String,
        container_name: String,
    ) -> Result<Self> {
        Ok(Self {
            storage_account,
            container_name,
            auth_provider,
        })
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
        
        // Perform the upload
        let response = blob_client
            .put_block_blob(request.content)
            .content_type(&content_type)
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to upload blob: {e}")))?;
        
        // TODO: Set metadata (requires separate API call)
        // Azure SDK v0.21 API for metadata is not yet stable
        // Will implement when the API stabilizes
        if !metadata.is_empty() {
            // The metadata setting requires investigation of the exact API in v0.21
            tracing::warn!("Metadata setting not yet implemented for Azure SDK v0.21");
        }
        
        // TODO: Set tags if provided (requires separate API call) 
        // Azure SDK v0.21 API for tags is not yet stable
        // Will implement when the API stabilizes
        if !request.tags.is_empty() {
            // The tag setting requires investigation of the exact API in v0.21
            tracing::warn!("Tag setting not yet implemented for Azure SDK v0.21");
        }
        
        // Extract response data and build FileInfo
        let etag = response.etag.to_string();
        
        // Convert Azure response datetime from time::OffsetDateTime to chrono::DateTime<Utc>
        let last_modified = {
            let timestamp = response.last_modified.unix_timestamp();
            chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap_or_else(Utc::now)
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
        
        // Enable metadata inclusion
        list_builder = list_builder.include_metadata(true);
        
        // Execute the list request - collect all pages
        let mut stream = list_builder.into_stream();
        let mut file_infos = Vec::new();
        
        // Process each page of results
        while let Some(page) = stream.try_next().await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to list blobs: {e}")))? {
            
            // Process each blob in this page
            for blob_item in page.blobs.blobs() {
                // Extract blob information
                let name = blob_item.name.clone();
                let size = blob_item.properties.content_length;
                let content_type = blob_item.properties.content_type.clone();
                
                // Convert time::OffsetDateTime to chrono::DateTime<Utc>
                let last_modified = {
                    let timestamp = blob_item.properties.last_modified.unix_timestamp();
                    chrono::DateTime::from_timestamp(timestamp, 0)
                        .unwrap_or_else(Utc::now)
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
                
                // For now, skip tags retrieval (requires separate API call)
                // TODO: Implement tags retrieval strategy
                let tags = HashMap::new();
                
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

    /// Download a file from blob storage
    pub async fn download_file(&self, request: FileDownloadRequest) -> Result<Vec<u8>> {
        // Validate download request parameters
        if request.name.trim().is_empty() {
            return Err(CrosstacheError::config("File name cannot be empty".to_string()));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);
        
        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(&request.name);
        
        // Check if blob exists before attempting download
        let _properties = blob_client
            .get_properties()
            .await
            .map_err(|e| {
                let error_msg = e.to_string().to_lowercase();
                if error_msg.contains("404") || error_msg.contains("not found") {
                    CrosstacheError::vault_not_found(format!("File '{}' not found", request.name))
                } else {
                    CrosstacheError::azure_api(format!("Failed to check if blob exists: {e}"))
                }
            })?;
        
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
            return Err(CrosstacheError::config("File name cannot be empty".to_string()));
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
            return Err(CrosstacheError::vault_not_found(format!("File '{name}' not found")));
        }
        
        // Implement blob deletion
        blob_client
            .delete()
            .await
            .map_err(|e| {
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
            return Err(CrosstacheError::config("File name cannot be empty".to_string()));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);
        
        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(name);
        
        // Get blob properties
        let properties = blob_client
            .get_properties()
            .await
            .map_err(|e| {
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
            chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap_or_else(Utc::now)
        };
        let etag = properties.blob.properties.etag.to_string();
        
        // Get custom metadata including groups
        let metadata = properties.blob.metadata.clone().unwrap_or_default();
        
        // Extract groups from metadata
        let groups: Vec<String> = metadata
            .get("groups")
            .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        
        // For now, skip tags retrieval (requires separate API call)
        // TODO: Implement tags retrieval if needed
        let tags = HashMap::new();
        
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
    pub async fn download_file_stream<W: AsyncWrite + Unpin>(
        &self,
        name: &str,
        mut writer: W,
    ) -> Result<()> {
        // Validate file name
        if name.trim().is_empty() {
            return Err(CrosstacheError::config("File name cannot be empty".to_string()));
        }

        // Create BlobServiceClient using token credential
        let token_credential = self.auth_provider.get_token_credential();
        let blob_service = BlobServiceClient::new(&self.storage_account, token_credential);
        
        // Get container and blob clients
        let container_client = blob_service.container_client(&self.container_name);
        let blob_client = container_client.blob_client(name);
        
        // Check if blob exists before attempting download
        let _properties = blob_client
            .get_properties()
            .await
            .map_err(|e| {
                let error_msg = e.to_string().to_lowercase();
                if error_msg.contains("404") || error_msg.contains("not found") {
                    CrosstacheError::vault_not_found(format!("File '{name}' not found"))
                } else {
                    CrosstacheError::azure_api(format!("Failed to check if blob exists: {e}"))
                }
            })?;

        // For streaming large files, we'll use the get_content method for now
        // The Azure SDK v0.21 handles chunking internally for better reliability
        let blob_content = blob_client
            .get_content()
            .await
            .map_err(|e| CrosstacheError::azure_api(format!("Failed to download blob: {e}")))?;

        // Stream the data and write to the provided writer
        use tokio::io::AsyncWriteExt;
        
        // Write all content at once (Azure SDK already optimized the download)
        writer.write_all(&blob_content).await
            .map_err(|e| CrosstacheError::unknown(format!("Failed to write blob data: {e}")))?;
        
        // Ensure all data is flushed
        writer.flush().await
            .map_err(|e| CrosstacheError::unknown(format!("Failed to flush data: {e}")))?;
        
        Ok(())
    }

    /// Upload large file with block-based chunking
    pub async fn upload_large_file<R: tokio::io::AsyncRead + Unpin>(
        &self,
        name: &str,
        mut _reader: R,
        file_size: u64,
        metadata: HashMap<String, String>,
        tags: HashMap<String, String>,
    ) -> Result<FileInfo> {
        // TODO: Implement actual large file upload using Azure Blob Storage SDK
        println!(
            "Would upload large file '{}' ({} bytes) to storage account '{}'", 
            name, 
            file_size, 
            self.storage_account
        );
        
        let groups = metadata.get("groups")
            .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        
        Ok(FileInfo {
            name: name.to_string(),
            size: file_size,
            content_type: mime_guess::from_path(name).first_or_octet_stream().to_string(),
            last_modified: Utc::now(),
            etag: format!("etag-large-{}", uuid::Uuid::new_v4()),
            groups,
            metadata,
            tags,
        })
    }

    /// Get the container name
    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    /// Get the storage account name
    pub fn storage_account(&self) -> &str {
        &self.storage_account
    }
}

/// Helper function to create a BlobManager from configuration
pub fn create_blob_manager(config: &crate::config::Config) -> Result<BlobManager> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    
    let blob_config = config.get_blob_config();
    
    if blob_config.storage_account.is_empty() {
        return Err(CrosstacheError::config(
            "No blob storage configured. Run 'xv init' to set up blob storage."
        ));
    }

    let auth_provider = Arc::new(DefaultAzureCredentialProvider::new()?) as Arc<dyn AzureAuthProvider>;
    
    BlobManager::new(
        auth_provider,
        blob_config.storage_account,
        blob_config.container_name,
    )
}

/// Create a blob manager with context-aware container selection
/// 
/// This function uses the storage_container from the current vault context if available,
/// otherwise falls back to the global blob configuration container.
pub fn create_context_aware_blob_manager(
    config: &crate::config::Config, 
    context_manager: &crate::config::context::ContextManager
) -> Result<BlobManager> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    
    let blob_config = config.get_blob_config();
    
    if blob_config.storage_account.is_empty() {
        return Err(CrosstacheError::config(
            "No blob storage configured. Run 'xv init' to set up blob storage."
        ));
    }

    // Use context storage container if available, otherwise use config default
    let container_name = context_manager
        .current_storage_container()
        .unwrap_or(&blob_config.container_name)
        .to_string();

    let auth_provider = Arc::new(DefaultAzureCredentialProvider::new()?) as Arc<dyn AzureAuthProvider>;
    
    BlobManager::new(
        auth_provider,
        blob_config.storage_account,
        container_name,
    )
}