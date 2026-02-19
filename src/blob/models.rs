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

/// Represents either a file or a directory prefix in blob listing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BlobListItem {
    #[serde(rename = "file")]
    File(FileInfo),
    #[serde(rename = "directory")]
    Directory {
        name: String,
        full_path: String,
    },
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
    #[allow(dead_code)]
    pub output_path: Option<String>,
    #[allow(dead_code)]
    pub stream: bool,
}

/// Request for listing files
#[derive(Debug, Clone)]
pub struct FileListRequest {
    pub prefix: Option<String>,
    pub groups: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub delimiter: Option<String>,
    #[allow(dead_code)]
    pub recursive: bool,
}

