//! Authentication module for Azure services
//!
//! This module provides authentication capabilities for Azure Key Vault
//! and other Azure services using various authentication methods including
//! DefaultAzureCredential, client secrets, and Graph API integration.

pub mod azure;
pub mod graph;
pub mod provider;

pub use azure::*;
pub use graph::*;
pub use provider::*;
