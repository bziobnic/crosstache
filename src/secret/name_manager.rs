//! Secret name management and mapping
//!
//! This module provides data structures for secret name mapping
//! used for handling user-friendly to sanitized name conversions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;

/// Name mapping entry for persistent storage
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct NameMapping {
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(rename = "Sanitized Name")]
    pub sanitized_name: String,
    #[tabled(rename = "Vault")]
    pub vault_name: String,
    #[tabled(rename = "Group")]
    pub group: String,
    #[tabled(skip)]
    pub is_hashed: bool,
    #[tabled(rename = "Created")]
    pub created_at: DateTime<Utc>,
    #[tabled(rename = "Updated")]
    pub updated_at: DateTime<Utc>,
    #[tabled(skip)]
    pub metadata: HashMap<String, String>,
}

/// Name mapping statistics
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct NameMappingStats {
    #[tabled(rename = "Total Mappings")]
    pub total_mappings: usize,
    #[tabled(rename = "Hashed Names")]
    pub hashed_names: usize,
    #[tabled(rename = "Simple Names")]
    pub simple_names: usize,
    #[tabled(rename = "Unique Vaults")]
    pub unique_vaults: usize,
    #[tabled(rename = "Unique Groups")]
    pub unique_groups: usize,
    #[tabled(rename = "Last Updated")]
    pub last_updated: String,
}
