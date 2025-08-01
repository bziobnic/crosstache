//! Azure Blob Storage operations for file management
//!
//! This module provides functionality for storing and managing files
//! in Azure Blob Storage, including upload, download, listing, and deletion.

pub mod manager;
pub mod models;
pub mod operations;

// Re-export commonly used types
pub use manager::{BlobManager, create_blob_manager, create_context_aware_blob_manager};
pub use models::*;