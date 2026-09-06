//! Secret management module
//!
//! This module provides functionality for managing secrets in Azure Key Vault,
//! including name sanitization, group management, and advanced secret operations.

#[cfg(feature = "file-ops")]
pub mod attachment_inventory;
pub mod attachment_key;
#[cfg(feature = "file-ops")]
pub mod attachment_lifecycle;
pub mod attachments;
pub mod manager;
pub mod models;
pub mod name_manager;
pub mod rotation;
