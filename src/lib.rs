//! crosstache - Azure Key Vault Management Tool
//!
//! A comprehensive CLI tool for managing Azure Key Vault operations
//! including vault management, secret operations, and access control.

pub mod auth;
pub mod backend;
#[cfg(feature = "file-ops")]
pub mod blob;
pub mod cache;
pub mod cli;
pub mod config;
pub mod error;
pub mod scan;
pub mod secret;
#[cfg(feature = "tui")]
pub mod tui;
pub mod utils;
pub mod vault;

// Re-export commonly used types
pub use error::{CrosstacheError, Result};
