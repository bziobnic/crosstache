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
    Directory { name: String, full_path: String },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file_info(name: &str, size: u64) -> FileInfo {
        FileInfo {
            name: name.to_string(),
            size,
            content_type: "application/octet-stream".to_string(),
            last_modified: chrono::Utc::now(),
            etag: "\"abc123\"".to_string(),
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        }
    }

    // --- FileInfo construction ---

    #[test]
    fn test_file_info_fields() {
        let fi = make_file_info("config.yaml", 1024);
        assert_eq!(fi.name, "config.yaml");
        assert_eq!(fi.size, 1024);
        assert_eq!(fi.content_type, "application/octet-stream");
    }

    #[test]
    fn test_file_info_with_groups() {
        let mut fi = make_file_info("deploy.sh", 512);
        fi.groups = vec!["prod".to_string(), "infra".to_string()];
        assert_eq!(fi.groups.len(), 2);
        assert!(fi.groups.contains(&"prod".to_string()));
    }

    #[test]
    fn test_file_info_with_metadata() {
        let mut fi = make_file_info("secret.env", 256);
        fi.metadata
            .insert("uploaded_by".to_string(), "xv-cli".to_string());
        assert_eq!(fi.metadata.get("uploaded_by").unwrap(), "xv-cli");
    }

    // --- BlobListItem serde ---

    #[test]
    fn test_blob_list_item_file_serializes_with_type_tag() {
        let fi = make_file_info("myfile.txt", 100);
        let item = BlobListItem::File(fi);
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""type":"file""#), "json: {json}");
        assert!(json.contains("myfile.txt"), "json: {json}");
    }

    #[test]
    fn test_blob_list_item_directory_serializes_with_type_tag() {
        let item = BlobListItem::Directory {
            name: "configs".to_string(),
            full_path: "configs/".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains(r#""type":"directory""#), "json: {json}");
        assert!(json.contains("configs"), "json: {json}");
    }

    #[test]
    fn test_blob_list_item_file_round_trip() {
        let fi = make_file_info("data.bin", 8192);
        let item = BlobListItem::File(fi);
        let json = serde_json::to_string(&item).unwrap();
        let decoded: BlobListItem = serde_json::from_str(&json).unwrap();
        match decoded {
            BlobListItem::File(info) => {
                assert_eq!(info.name, "data.bin");
                assert_eq!(info.size, 8192);
            }
            BlobListItem::Directory { .. } => panic!("expected File variant"),
        }
    }

    #[test]
    fn test_blob_list_item_directory_round_trip() {
        let item = BlobListItem::Directory {
            name: "subdir".to_string(),
            full_path: "parent/subdir/".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let decoded: BlobListItem = serde_json::from_str(&json).unwrap();
        match decoded {
            BlobListItem::Directory { name, full_path } => {
                assert_eq!(name, "subdir");
                assert_eq!(full_path, "parent/subdir/");
            }
            BlobListItem::File(_) => panic!("expected Directory variant"),
        }
    }

    // --- FileUploadRequest construction ---

    #[test]
    fn test_file_upload_request_fields() {
        let req = FileUploadRequest {
            name: "app.env".to_string(),
            content: b"KEY=value".to_vec(),
            content_type: Some("text/plain".to_string()),
            groups: vec!["prod".to_string()],
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        assert_eq!(req.name, "app.env");
        assert_eq!(req.content, b"KEY=value");
        assert_eq!(req.content_type, Some("text/plain".to_string()));
        assert_eq!(req.groups, vec!["prod"]);
    }

    // --- FileListRequest construction ---

    #[test]
    fn test_file_list_request_defaults() {
        let req = FileListRequest {
            prefix: None,
            groups: None,
            limit: Some(100),
            delimiter: Some("/".to_string()),
            recursive: false,
        };
        assert!(req.prefix.is_none());
        assert!(req.groups.is_none());
        assert_eq!(req.limit, Some(100));
        assert_eq!(req.delimiter.as_deref(), Some("/"));
    }
}
