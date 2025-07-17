# File Support Implementation Plan for crosstache

## Overview

This document outlines the comprehensive implementation plan for adding file support to crosstache, enabling users to store, manage, and manipulate files alongside their Azure Key Vault secrets using Azure Blob Storage.

## Implementation Goals

- **Seamless Integration**: File operations should feel natural within the existing crosstache CLI
- **Automatic Storage Setup**: The `init` command should automatically create blob storage infrastructure
- **Intuitive Commands**: Simple, memorable commands for file operations (upload, download, edit, list, delete)
- **Consistent Authentication**: Leverage existing Azure authentication patterns
- **Efficient Operations**: Support for large files with streaming and resumable uploads
- **Metadata Management**: Rich metadata support for file organization and searching

## Phase 1: Foundation and Storage Setup

### 1.1 Dependency Updates

Update `Cargo.toml` to include Azure Blob Storage dependencies:

```toml
[dependencies]
# Existing dependencies...
azure_storage_blobs = "0.20"
azure_mgmt_storage = "0.20"  # For storage account management
tempfile = "3.0"            # For temporary file handling
mime_guess = "2.0"          # For MIME type detection
```

### 1.2 Configuration Extensions

#### 1.2.1 Update Configuration Structure

Extend `src/config/settings.rs` to include blob storage configuration:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobConfig {
    pub storage_account: String,
    pub container_name: String,
    pub endpoint: Option<String>,
    pub enable_large_file_support: bool,
    pub chunk_size_mb: usize,
    pub max_concurrent_uploads: usize,
}

// Add to main Config struct
pub struct Config {
    // ... existing fields
    pub blob_config: Option<BlobConfig>,
}
```

#### 1.2.2 Environment Variable Support

Add environment variable support for blob configuration:

```bash
# New environment variables
AZURE_STORAGE_ACCOUNT=mystorageaccount
AZURE_STORAGE_CONTAINER=crosstache-files
AZURE_STORAGE_ENDPOINT=https://mystorageaccount.blob.core.windows.net
BLOB_CHUNK_SIZE_MB=4
BLOB_MAX_CONCURRENT_UPLOADS=3
```

### 1.3 Storage Account Creation in Init

#### 1.3.1 Extend ConfigInitializer

Update `src/config/init.rs` to include storage account creation:

```rust
// Add to InitConfig struct
pub struct InitConfig {
    // ... existing fields
    pub storage_account_name: String,
    pub blob_container_name: String,
    pub create_storage_account: bool,
}

// Add new method to ConfigInitializer
impl ConfigInitializer {
    /// Configure blob storage during initialization
    async fn configure_blob_storage(
        &self,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<(String, String)> {
        let create_storage = self.prompt.confirm(
            "Create blob storage for file operations?",
            true,
        )?;

        if !create_storage {
            return Ok((String::new(), String::new()));
        }

        // Generate unique storage account name
        let default_storage_name = SetupHelper::generate_storage_account_name();
        let storage_name = self.prompt.input_text_validated(
            "Enter storage account name",
            Some(&default_storage_name),
            SetupHelper::validate_storage_account_name,
        )?;

        let container_name = self.prompt.input_text_validated(
            "Enter container name for files",
            Some("crosstache-files"),
            SetupHelper::validate_container_name,
        )?;

        // Create storage account
        self.create_storage_account(&storage_name, subscription, resource_group, location).await?;

        Ok((storage_name, container_name))
    }

    /// Create storage account and container
    async fn create_storage_account(
        &self,
        storage_name: &str,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<()> {
        let progress = ProgressIndicator::new("Creating storage account...");

        // Use Azure Management API to create storage account
        let storage_manager = StorageManager::new(
            Arc::new(DefaultAzureCredentialProvider::new()?) as Arc<dyn AzureAuthProvider>,
            subscription.id.clone(),
        )?;

        let storage_request = StorageAccountCreateRequest {
            name: storage_name.to_string(),
            location: location.to_string(),
            resource_group: resource_group.to_string(),
            sku: StorageAccountSku::StandardLRS,
            kind: StorageAccountKind::StorageV2,
            enable_blob_public_access: false,
            minimum_tls_version: TlsVersion::TLS1_2,
        };

        progress.set_message("Creating storage account...");
        storage_manager.create_storage_account(storage_request).await?;

        progress.set_message("Creating blob container...");
        storage_manager.create_container(storage_name, "crosstache-files").await?;

        progress.finish_success(&format!("Created storage account '{}'", storage_name));
        Ok(())
    }
}
```

## Phase 2: Core File Operations Module

### 2.1 Module Structure

Create new module structure for file operations:

```
src/
├── blob/
│   ├── mod.rs           # Module declarations
│   ├── manager.rs       # Core blob operations
│   ├── models.rs        # Blob-related data structures
│   ├── operations.rs    # Specific blob operations
│   └── storage.rs       # Storage account management
```

### 2.2 Core Data Models

#### 2.2.1 File Models (`src/blob/models.rs`)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
    pub content_type: String,
    pub last_modified: DateTime<Utc>,
    pub etag: String,
    pub groups: Vec<String>,
    pub metadata: HashMap<String, String>,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct FileUploadRequest {
    pub name: String,
    pub content: Vec<u8>,
    pub content_type: Option<String>,
    pub groups: Vec<String>,
    pub metadata: HashMap<String, String>,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct FileDownloadRequest {
    pub name: String,
    pub output_path: Option<String>,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct FileListRequest {
    pub prefix: Option<String>,
    pub group_filter: Option<String>,
    pub include_metadata: bool,
    pub max_results: Option<usize>,
}
```

### 2.3 Blob Manager Implementation

#### 2.3.1 Core Manager (`src/blob/manager.rs`)

```rust
use crate::auth::provider::AzureAuthProvider;
use crate::blob::models::*;
use crate::error::{crosstacheError, Result};
use crate::utils::format::OutputFormat;
use azure_storage_blobs::{BlobServiceClient, BlobClient};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct BlobManager {
    client: BlobServiceClient,
    container_name: String,
    auth_provider: Arc<dyn AzureAuthProvider>,
}

impl BlobManager {
    pub fn new(
        auth_provider: Arc<dyn AzureAuthProvider>,
        storage_account: String,
        container_name: String,
    ) -> Result<Self> {
        let endpoint = format!("https://{}.blob.core.windows.net", storage_account);
        let credential = auth_provider.get_credential()?;
        
        let client = BlobServiceClient::new(endpoint, credential)?;
        
        Ok(Self {
            client,
            container_name,
            auth_provider,
        })
    }

    /// Upload a file to blob storage
    pub async fn upload_file(&self, request: FileUploadRequest) -> Result<FileInfo> {
        let blob_client = self.get_blob_client(&request.name);
        
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

        // Upload blob
        let result = blob_client
            .put_blob(request.content)
            .content_type(content_type.clone())
            .metadata(metadata.clone())
            .tags(request.tags.clone())
            .await?;

        Ok(FileInfo {
            name: request.name,
            size: result.content_length,
            content_type,
            last_modified: result.last_modified,
            etag: result.etag,
            groups: request.groups,
            metadata,
            tags: request.tags,
        })
    }

    /// Download a file from blob storage
    pub async fn download_file(&self, request: FileDownloadRequest) -> Result<Vec<u8>> {
        let blob_client = self.get_blob_client(&request.name);
        
        let response = blob_client.get_blob().await?;
        Ok(response.data.to_vec())
    }

    /// Stream download a large file
    pub async fn download_file_stream<W: AsyncWrite + Unpin>(
        &self,
        name: &str,
        writer: W,
    ) -> Result<()> {
        let blob_client = self.get_blob_client(name);
        
        // Stream download in chunks
        let mut stream = blob_client.get_blob().stream(1024 * 1024).await?; // 1MB chunks
        let mut writer = writer;
        
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            writer.write_all(&chunk).await?;
        }
        
        Ok(())
    }

    /// List files in the container
    pub async fn list_files(&self, request: FileListRequest) -> Result<Vec<FileInfo>> {
        let container_client = self.client.container_client(&self.container_name);
        
        let mut list_request = container_client.list_blobs();
        
        if let Some(prefix) = request.prefix {
            list_request = list_request.prefix(prefix);
        }
        
        if let Some(max_results) = request.max_results {
            list_request = list_request.max_results(max_results as u32);
        }

        let mut files = Vec::new();
        
        while let Some(blob) = list_request.next().await {
            let blob = blob?;
            
            // Parse metadata for groups
            let groups = blob.metadata.get("groups")
                .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            
            // Apply group filter if specified
            if let Some(group_filter) = &request.group_filter {
                if !groups.contains(group_filter) {
                    continue;
                }
            }
            
            files.push(FileInfo {
                name: blob.name,
                size: blob.properties.content_length,
                content_type: blob.properties.content_type.unwrap_or_default(),
                last_modified: blob.properties.last_modified,
                etag: blob.properties.etag,
                groups,
                metadata: blob.metadata,
                tags: blob.tags.unwrap_or_default(),
            });
        }
        
        Ok(files)
    }

    /// Delete a file from blob storage
    pub async fn delete_file(&self, name: &str) -> Result<()> {
        let blob_client = self.get_blob_client(name);
        blob_client.delete_blob().await?;
        Ok(())
    }

    /// Get file metadata without downloading content
    pub async fn get_file_info(&self, name: &str) -> Result<FileInfo> {
        let blob_client = self.get_blob_client(name);
        
        let properties = blob_client.get_properties().await?;
        
        let groups = properties.metadata.get("groups")
            .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        
        Ok(FileInfo {
            name: name.to_string(),
            size: properties.content_length,
            content_type: properties.content_type.unwrap_or_default(),
            last_modified: properties.last_modified,
            etag: properties.etag,
            groups,
            metadata: properties.metadata,
            tags: properties.tags.unwrap_or_default(),
        })
    }

    fn get_blob_client(&self, name: &str) -> BlobClient {
        self.client
            .container_client(&self.container_name)
            .blob_client(name)
    }
}
```

### 2.4 Large File Support

#### 2.4.1 Streaming Upload (`src/blob/operations.rs`)

```rust
impl BlobManager {
    /// Upload large file with block-based chunking
    pub async fn upload_large_file<R: AsyncRead + Unpin>(
        &self,
        name: &str,
        mut reader: R,
        file_size: u64,
        metadata: HashMap<String, String>,
        tags: HashMap<String, String>,
    ) -> Result<FileInfo> {
        let blob_client = self.get_blob_client(name);
        
        const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4MB chunks
        let mut block_list = Vec::new();
        let mut block_id = 0u32;
        let mut total_uploaded = 0u64;
        
        // Progress tracking
        let progress = ProgressIndicator::new(&format!("Uploading {}...", name));
        
        loop {
            let mut buffer = vec![0u8; CHUNK_SIZE];
            let bytes_read = reader.read(&mut buffer).await?;
            
            if bytes_read == 0 {
                break;
            }
            
            buffer.truncate(bytes_read);
            let block_id_str = format!("{:08}", block_id);
            
            // Upload block
            blob_client
                .put_block(block_id_str.clone(), buffer)
                .await?;
                
            block_list.push(block_id_str);
            block_id += 1;
            total_uploaded += bytes_read as u64;
            
            // Update progress
            let percent = (total_uploaded as f64 / file_size as f64 * 100.0) as u32;
            progress.set_message(&format!("Uploading {}... {}%", name, percent));
        }
        
        // Commit block list with metadata
        let result = blob_client
            .put_block_list(block_list)
            .metadata(metadata.clone())
            .tags(tags.clone())
            .await?;
        
        progress.finish_success(&format!("Uploaded {} ({} bytes)", name, total_uploaded));
        
        Ok(FileInfo {
            name: name.to_string(),
            size: total_uploaded,
            content_type: mime_guess::from_path(name).first_or_octet_stream().to_string(),
            last_modified: result.last_modified,
            etag: result.etag,
            groups: metadata.get("groups")
                .map(|g| g.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
            metadata,
            tags,
        })
    }
}
```

## Phase 3: CLI Commands Integration

### 3.1 Command Structure

Add file commands to `src/cli/commands.rs`:

```rust
#[derive(Subcommand)]
pub enum Commands {
    // ... existing commands
    
    /// File operations
    #[command(subcommand)]
    File(FileCommands),
    
    /// Quick file upload (alias for file upload)
    #[command(alias = "up")]
    Upload {
        /// Local file path
        file_path: String,
        /// Remote name (optional, defaults to filename)
        #[arg(long)]
        name: Option<String>,
        /// Groups to assign to the file
        #[arg(long)]
        groups: Option<String>,
        /// Additional metadata (key=value pairs)
        #[arg(long)]
        metadata: Vec<String>,
    },
    
    /// Quick file download (alias for file download)
    #[command(alias = "down")]
    Download {
        /// Remote file name
        name: String,
        /// Local output path (optional, defaults to current directory)
        #[arg(long)]
        output: Option<String>,
        /// Open file after download
        #[arg(long)]
        open: bool,
    },
}

#[derive(Subcommand)]
pub enum FileCommands {
    /// Upload a file to blob storage
    Upload {
        /// Local file path
        file_path: String,
        /// Remote name (optional, defaults to filename)
        #[arg(long)]
        name: Option<String>,
        /// Groups to assign to the file
        #[arg(long)]
        groups: Option<String>,
        /// Additional metadata (key=value pairs)
        #[arg(long)]
        metadata: Vec<String>,
        /// Enable large file support (block-based upload)
        #[arg(long)]
        large: bool,
    },
    
    /// Download a file from blob storage
    Download {
        /// Remote file name
        name: String,
        /// Local output path (optional, defaults to current directory)
        #[arg(long)]
        output: Option<String>,
        /// Stream download (for large files)
        #[arg(long)]
        stream: bool,
        /// Open file after download
        #[arg(long)]
        open: bool,
    },
    
    /// List files in blob storage
    #[command(alias = "ls")]
    List {
        /// Filter by prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Filter by group
        #[arg(long)]
        group: Option<String>,
        /// Include metadata in output
        #[arg(long)]
        metadata: bool,
        /// Maximum number of results
        #[arg(long)]
        limit: Option<usize>,
    },
    
    /// Delete a file from blob storage
    #[command(alias = "rm")]
    Delete {
        /// Remote file name
        name: String,
        /// Force deletion without confirmation
        #[arg(long)]
        force: bool,
    },
    
    /// Get file information
    Info {
        /// Remote file name
        name: String,
    },
    
    /// Edit a file (download, edit, upload)
    Edit {
        /// Remote file name
        name: String,
        /// Editor to use (defaults to $EDITOR or vi)
        #[arg(long)]
        editor: Option<String>,
        /// Create new file if it doesn't exist
        #[arg(long)]
        create: bool,
    },
    
    /// Sync files between local directory and blob storage
    Sync {
        /// Local directory path
        local_path: String,
        /// Remote prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Direction (up, down, both)
        #[arg(long, default_value = "both")]
        direction: SyncDirection,
        /// Dry run (show what would be done)
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum SyncDirection {
    Up,
    Down,
    Both,
}
```

### 3.2 Command Implementation

#### 3.2.1 File Upload Command

```rust
// In command execution logic
pub async fn execute_file_upload(
    config: &Config,
    file_path: &str,
    name: Option<&str>,
    groups: Option<&str>,
    metadata: &[String],
    large: bool,
) -> Result<()> {
    // Validate file exists
    if !std::path::Path::new(file_path).exists() {
        return Err(crosstacheError::invalid_argument(
            format!("File not found: {}", file_path)
        ));
    }

    // Get file info
    let file_metadata = std::fs::metadata(file_path)?;
    let file_size = file_metadata.len();
    
    // Determine remote name
    let remote_name = name.unwrap_or_else(|| {
        std::path::Path::new(file_path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    });

    // Parse groups
    let groups = groups.map(|g| 
        g.split(',').map(|s| s.trim().to_string()).collect()
    ).unwrap_or_default();

    // Parse metadata
    let mut metadata_map = HashMap::new();
    for meta in metadata {
        if let Some((key, value)) = meta.split_once('=') {
            metadata_map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    // Create blob manager
    let blob_manager = create_blob_manager(config)?;

    // Upload file
    if large || file_size > 10 * 1024 * 1024 { // Use large file upload for files > 10MB
        let file = tokio::fs::File::open(file_path).await?;
        let reader = tokio::io::BufReader::new(file);
        
        let file_info = blob_manager.upload_large_file(
            remote_name,
            reader,
            file_size,
            metadata_map,
            HashMap::new(),
        ).await?;
        
        println!("✓ Uploaded {} ({} bytes)", file_info.name, file_info.size);
    } else {
        let content = tokio::fs::read(file_path).await?;
        
        let upload_request = FileUploadRequest {
            name: remote_name.to_string(),
            content,
            content_type: None,
            groups,
            metadata: metadata_map,
            tags: HashMap::new(),
        };
        
        let file_info = blob_manager.upload_file(upload_request).await?;
        println!("✓ Uploaded {} ({} bytes)", file_info.name, file_info.size);
    }

    Ok(())
}
```

#### 3.2.2 File Edit Command

```rust
pub async fn execute_file_edit(
    config: &Config,
    name: &str,
    editor: Option<&str>,
    create: bool,
) -> Result<()> {
    let blob_manager = create_blob_manager(config)?;
    
    // Create temporary file
    let temp_file = tempfile::NamedTempFile::new()?;
    let temp_path = temp_file.path();
    
    // Try to download existing file
    let file_exists = match blob_manager.get_file_info(name).await {
        Ok(_) => true,
        Err(crosstacheError::VaultNotFound { .. }) => false,
        Err(e) => return Err(e),
    };
    
    if file_exists {
        println!("Downloading {} for editing...", name);
        let content = blob_manager.download_file(FileDownloadRequest {
            name: name.to_string(),
            output_path: None,
            stream: false,
        }).await?;
        
        tokio::fs::write(temp_path, content).await?;
    } else if !create {
        return Err(crosstacheError::invalid_argument(
            format!("File '{}' not found. Use --create to create a new file.", name)
        ));
    }
    
    // Determine editor
    let editor_cmd = editor
        .map(|e| e.to_string())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_string());
    
    // Launch editor
    let mut cmd = tokio::process::Command::new(&editor_cmd)
        .arg(temp_path)
        .status()
        .await?;
    
    if !cmd.success() {
        return Err(crosstacheError::unknown("Editor exited with error"));
    }
    
    // Check if file was modified
    let new_content = tokio::fs::read(temp_path).await?;
    
    if file_exists {
        // Compare with original
        let original_content = blob_manager.download_file(FileDownloadRequest {
            name: name.to_string(),
            output_path: None,
            stream: false,
        }).await?;
        
        if new_content == original_content {
            println!("No changes made to {}", name);
            return Ok(());
        }
    }
    
    // Upload modified file
    let upload_request = FileUploadRequest {
        name: name.to_string(),
        content: new_content,
        content_type: None,
        groups: Vec::new(),
        metadata: {
            let mut meta = HashMap::new();
            meta.insert("edited_by".to_string(), "crosstache".to_string());
            meta.insert("edited_at".to_string(), Utc::now().to_rfc3339());
            meta
        },
        tags: HashMap::new(),
    };
    
    let file_info = blob_manager.upload_file(upload_request).await?;
    println!("✓ Saved {} ({} bytes)", file_info.name, file_info.size);
    
    Ok(())
}
```

## Phase 4: Advanced Features

### 4.1 File Synchronization

#### 4.1.1 Sync Implementation

```rust
pub async fn execute_file_sync(
    config: &Config,
    local_path: &str,
    prefix: Option<&str>,
    direction: SyncDirection,
    dry_run: bool,
) -> Result<()> {
    let blob_manager = create_blob_manager(config)?;
    let local_path = std::path::Path::new(local_path);
    
    if !local_path.exists() {
        return Err(crosstacheError::invalid_argument(
            format!("Local path does not exist: {}", local_path.display())
        ));
    }
    
    // Get local files
    let local_files = get_local_files(local_path)?;
    
    // Get remote files
    let remote_files = blob_manager.list_files(FileListRequest {
        prefix: prefix.map(|p| p.to_string()),
        group_filter: None,
        include_metadata: true,
        max_results: None,
    }).await?;
    
    match direction {
        SyncDirection::Up => sync_up(&blob_manager, &local_files, &remote_files, dry_run).await?,
        SyncDirection::Down => sync_down(&blob_manager, &local_files, &remote_files, local_path, dry_run).await?,
        SyncDirection::Both => {
            sync_up(&blob_manager, &local_files, &remote_files, dry_run).await?;
            sync_down(&blob_manager, &local_files, &remote_files, local_path, dry_run).await?;
        }
    }
    
    Ok(())
}

async fn sync_up(
    blob_manager: &BlobManager,
    local_files: &[LocalFileInfo],
    remote_files: &[FileInfo],
    dry_run: bool,
) -> Result<()> {
    for local_file in local_files {
        let remote_file = remote_files
            .iter()
            .find(|f| f.name == local_file.name);
        
        let should_upload = match remote_file {
            None => true, // File doesn't exist remotely
            Some(remote) => local_file.modified > remote.last_modified, // Local is newer
        };
        
        if should_upload {
            if dry_run {
                println!("[DRY RUN] Would upload: {}", local_file.name);
            } else {
                println!("Uploading: {}", local_file.name);
                
                let content = tokio::fs::read(&local_file.path).await?;
                let upload_request = FileUploadRequest {
                    name: local_file.name.clone(),
                    content,
                    content_type: None,
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                };
                
                blob_manager.upload_file(upload_request).await?;
            }
        }
    }
    
    Ok(())
}
```

### 4.2 Integration with Existing Commands

#### 4.2.1 Enhanced Init Command

Update the init command to prompt for file storage setup:

```rust
// In init command execution
pub async fn execute_init_command(config: &Config) -> Result<()> {
    let initializer = ConfigInitializer::new();
    
    // Run the interactive setup with file storage
    let final_config = initializer.run_interactive_setup_with_files().await?;
    
    // Show enhanced summary
    initializer.show_enhanced_setup_summary(&final_config)?;
    
    Ok(())
}
```

#### 4.2.2 Global File Operations

Add file operations to the main command execution:

```rust
// In main command dispatch
pub async fn execute_command(cli: Cli, config: Config) -> Result<()> {
    match cli.command {
        // ... existing commands
        
        Commands::File(file_cmd) => {
            execute_file_command(file_cmd, &config).await?;
        }
        
        Commands::Upload { file_path, name, groups, metadata } => {
            execute_file_upload(&config, &file_path, name.as_deref(), groups.as_deref(), &metadata, false).await?;
        }
        
        Commands::Download { name, output, open } => {
            execute_file_download(&config, &name, output.as_deref(), false, open).await?;
        }
        
        // ... rest of commands
    }
    
    Ok(())
}
```

## Phase 5: Testing and Documentation

### 5.1 Test Structure

Create comprehensive tests for file operations:

```rust
// tests/file_tests.rs
use crosstache::{
    blob::manager::BlobManager,
    config::Config,
    error::Result,
};
use tempfile::TempDir;

#[tokio::test]
async fn test_file_upload_download() -> Result<()> {
    let config = get_test_config();
    let blob_manager = create_test_blob_manager(&config)?;
    
    // Create test file
    let test_content = b"Hello, World!";
    let upload_request = FileUploadRequest {
        name: "test.txt".to_string(),
        content: test_content.to_vec(),
        content_type: Some("text/plain".to_string()),
        groups: vec!["test".to_string()],
        metadata: HashMap::new(),
        tags: HashMap::new(),
    };
    
    // Upload file
    let file_info = blob_manager.upload_file(upload_request).await?;
    assert_eq!(file_info.name, "test.txt");
    assert_eq!(file_info.size, test_content.len() as u64);
    
    // Download file
    let download_request = FileDownloadRequest {
        name: "test.txt".to_string(),
        output_path: None,
        stream: false,
    };
    
    let downloaded_content = blob_manager.download_file(download_request).await?;
    assert_eq!(downloaded_content, test_content);
    
    // Clean up
    blob_manager.delete_file("test.txt").await?;
    
    Ok(())
}

#[tokio::test]
async fn test_large_file_upload() -> Result<()> {
    let config = get_test_config();
    let blob_manager = create_test_blob_manager(&config)?;
    
    // Create large test file (10MB)
    let temp_dir = TempDir::new()?;
    let temp_file = temp_dir.path().join("large_test.bin");
    
    let large_content = vec![0u8; 10 * 1024 * 1024]; // 10MB
    tokio::fs::write(&temp_file, &large_content).await?;
    
    // Upload large file
    let file = tokio::fs::File::open(&temp_file).await?;
    let reader = tokio::io::BufReader::new(file);
    
    let file_info = blob_manager.upload_large_file(
        "large_test.bin",
        reader,
        large_content.len() as u64,
        HashMap::new(),
        HashMap::new(),
    ).await?;
    
    assert_eq!(file_info.size, large_content.len() as u64);
    
    // Clean up
    blob_manager.delete_file("large_test.bin").await?;
    
    Ok(())
}
```

### 5.2 Documentation Updates

#### 5.2.1 README Updates

Update the main README to include file operations:

```markdown
# crosstache - Azure Key Vault & File Management CLI

## File Operations

crosstache now supports file storage and management using Azure Blob Storage.

### Quick Start

```bash
# Initialize with file storage
xv init

# Upload a file
xv upload config.json --groups=config,prod

# Download a file
xv download config.json --output=./downloads/

# List files
xv file list --group=config

# Edit a file
xv file edit config.json

# Sync directory
xv file sync ./local-files --prefix=config/
```

### Commands

- `xv file upload <file>` - Upload a file
- `xv file download <name>` - Download a file
- `xv file list` - List files
- `xv file delete <name>` - Delete a file
- `xv file edit <name>` - Edit a file
- `xv file sync <directory>` - Sync files
- `xv upload <file>` - Quick upload alias
- `xv download <name>` - Quick download alias
```

#### 5.2.2 Help Documentation

Add comprehensive help text to all file commands:

```rust
/// Upload a file to blob storage
/// 
/// Examples:
///   xv file upload config.json
///   xv file upload large-file.zip --large
///   xv file upload doc.pdf --groups=docs,public --metadata=version=1.0
Upload {
    /// Local file path to upload
    file_path: String,
    
    /// Remote name (optional, defaults to filename)
    /// If not specified, uses the filename from the local path
    #[arg(long, help = "Remote name for the file")]
    name: Option<String>,
    
    // ... rest of fields with detailed help
}
```

## Implementation Timeline

### Phase 1: Foundation (Week 1-2)
- [ ] Add Azure Blob Storage dependencies
- [ ] Extend configuration system
- [ ] Update init command for storage account creation
- [ ] Create basic blob module structure

### Phase 2: Core Operations (Week 3-4)
- [ ] Implement BlobManager with basic operations
- [ ] Add file upload/download functionality
- [ ] Implement file listing and deletion
- [ ] Add large file support

### Phase 3: CLI Integration (Week 5-6)
- [ ] Add file commands to CLI
- [ ] Implement command execution logic
- [ ] Add file edit functionality
- [ ] Create quick upload/download aliases

### Phase 4: Advanced Features (Week 7-8)
- [ ] Implement file synchronization
- [ ] Add metadata and tagging support
- [ ] Create progress indicators
- [ ] Add error handling and recovery

### Phase 5: Testing and Polish (Week 9-10)
- [ ] Write comprehensive tests
- [ ] Update documentation
- [ ] Performance optimization
- [ ] User experience improvements

## Success Metrics

- **Intuitive Commands**: Users can perform file operations without reading documentation
- **Seamless Integration**: File operations feel natural within the crosstache ecosystem
- **Performance**: Large files (>100MB) upload/download efficiently
- **Reliability**: Operations are resilient to network interruptions
- **Consistency**: Same authentication and configuration patterns as existing features

## Risk Mitigation

1. **Azure SDK Stability**: The Azure SDK for Rust is in beta
   - **Mitigation**: Use hybrid approach with REST API fallback
   - **Contingency**: Implement direct REST API calls for critical operations

2. **Large File Performance**: Blob storage performance for large files
   - **Mitigation**: Implement chunked uploads with progress tracking
   - **Monitoring**: Add performance metrics and optimization

3. **Storage Costs**: Blob storage costs for users
   - **Mitigation**: Document pricing clearly, provide cost estimation tools
   - **Controls**: Add storage usage reporting and cleanup commands

4. **User Experience**: File operations complexity
   - **Mitigation**: Provide simple aliases and intuitive defaults
   - **Feedback**: Implement user feedback collection and iteration

This implementation plan provides a comprehensive roadmap for adding robust file support to crosstache while maintaining consistency with the existing architecture and user experience.