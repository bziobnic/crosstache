//! Secret management module
//!
//! This module provides functionality for managing secrets in Azure Key Vault,
//! including name sanitization, group management, and advanced secret operations.

#[cfg(feature = "file-ops")]
pub(crate) mod attachment_backup;
#[cfg(feature = "file-ops")]
pub(crate) mod attachment_backup_codec;
#[cfg(feature = "file-ops")]
pub mod attachment_inventory;
pub mod attachment_key;
#[cfg(feature = "file-ops")]
pub mod attachment_lifecycle;
#[cfg(feature = "file-ops")]
pub(crate) mod attachment_restore;
#[cfg(feature = "file-ops")]
pub(crate) mod attachment_retirement;
#[cfg(feature = "file-ops")]
pub(crate) mod attachment_rewrap;
#[cfg(feature = "file-ops")]
pub(crate) mod attachment_rotation;
pub mod attachments;
pub mod domain;
pub mod manager;
pub mod models;
pub mod name_manager;
pub mod rotation;
pub mod scheduled_rotation;

#[cfg(feature = "file-ops")]
pub mod attachment_transfer;

#[cfg(feature = "file-ops")]
pub mod attachment_transfer_execution;
