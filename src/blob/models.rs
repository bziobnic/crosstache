//! Data models for blob storage operations
//!
//! This module defines the data structures used for file operations
//! including requests, responses, and metadata.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Information about a stored file
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

/// Request for uploading a file
#[derive(Debug, Clone)]
pub struct FileUploadRequest {
    pub name: String,
    pub content: Vec<u8>,
    pub content_type: Option<String>,
    pub groups: Vec<String>,
    pub metadata: HashMap<String, String>,
    pub tags: HashMap<String, String>,
}

/// Request for downloading a file
#[derive(Debug, Clone)]
pub struct FileDownloadRequest {
    pub name: String,
    pub output_path: Option<String>,
    pub stream: bool,
}

/// Request for listing files
#[derive(Debug, Clone)]
pub struct FileListRequest {
    pub prefix: Option<String>,
    pub groups: Option<Vec<String>>,
    pub limit: Option<usize>,
}

/// Local file information for sync operations
#[derive(Debug, Clone)]
pub struct LocalFileInfo {
    pub name: String,
    pub path: std::path::PathBuf,
    pub size: u64,
    pub modified: DateTime<Utc>,
}

/// Storage account creation request
#[derive(Debug, Clone)]
pub struct StorageAccountCreateRequest {
    pub name: String,
    pub resource_group: String,
    pub location: String,
    pub sku: StorageAccountSku,
    pub kind: StorageAccountKind,
    pub enable_blob_public_access: bool,
    pub minimum_tls_version: TlsVersion,
}

/// Storage account SKU options
#[derive(Debug, Clone)]
pub enum StorageAccountSku {
    StandardLRS,
    StandardGRS,
    StandardRAGRS,
    StandardZRS,
    PremiumLRS,
    PremiumZRS,
}

/// Storage account kind options
#[derive(Debug, Clone)]
pub enum StorageAccountKind {
    Storage,
    StorageV2,
    BlobStorage,
}

/// TLS version options
#[derive(Debug, Clone)]
pub enum TlsVersion {
    TLS1_0,
    TLS1_1,
    TLS1_2,
}