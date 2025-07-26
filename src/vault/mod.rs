//! Vault management module
//!
//! This module provides functionality for managing Azure Key Vaults,
//! including creation, deletion, access control, and metadata management.

pub mod manager;
pub mod models;
pub mod operations;

pub use manager::*;
pub use models::*;
