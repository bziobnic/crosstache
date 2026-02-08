//! Secret models and data structures
//!
//! This module defines data structures for secret information display and operations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Detailed secret information for the info command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretInfo {
    /// Secret name (sanitized)
    pub name: String,
    
    /// Original name (before sanitization)
    pub original_name: Option<String>,
    
    /// Secret identifier (full URI)
    pub id: String,
    
    /// Current version identifier
    pub version: Option<String>,
    
    /// Whether the secret is enabled
    pub enabled: bool,
    
    /// Creation timestamp
    pub created: Option<DateTime<Utc>>,
    
    /// Last updated timestamp
    pub updated: Option<DateTime<Utc>>,
    
    /// Expiration date (if set)
    pub expires: Option<DateTime<Utc>>,
    
    /// Not-before date (if set)
    pub not_before: Option<DateTime<Utc>>,
    
    /// Recovery level (e.g., "Recoverable+Purgeable")
    pub recovery_level: Option<String>,
    
    /// Content type (if specified)
    pub content_type: Option<String>,
    
    /// Tags associated with the secret
    pub tags: HashMap<String, String>,
    
    /// Groups extracted from tags
    pub groups: Vec<String>,
    
    /// Folder extracted from tags
    pub folder: Option<String>,
    
    /// Note extracted from tags
    pub note: Option<String>,
    
    /// Key Vault URI
    pub vault_uri: String,
    
    /// Number of versions (if available)
    pub version_count: Option<usize>,
}

impl SecretInfo {
    /// Extract groups from tags
    pub fn extract_groups(tags: &HashMap<String, String>) -> Vec<String> {
        tags.get("groups")
            .map(|g| g.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect())
            .unwrap_or_default()
    }
    
    /// Extract folder from tags
    pub fn extract_folder(tags: &HashMap<String, String>) -> Option<String> {
        tags.get("folder").cloned()
    }
    
    /// Extract note from tags
    pub fn extract_note(tags: &HashMap<String, String>) -> Option<String> {
        tags.get("note").cloned()
    }
    
    /// Extract original name from tags
    pub fn extract_original_name(tags: &HashMap<String, String>) -> Option<String> {
        tags.get("original_name").cloned()
    }
}

impl std::fmt::Display for SecretInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Secret Information:")?;
        writeln!(f, "  Name: {}", self.name)?;
        
        if let Some(original) = &self.original_name {
            if original != &self.name {
                writeln!(f, "  Original Name: {original}")?;
            }
        }
        
        writeln!(f, "  Vault: {}", self.vault_uri)?;
        writeln!(f, "  Enabled: {}", if self.enabled { "Yes" } else { "No" })?;
        
        if let Some(version) = &self.version {
            writeln!(f, "  Current Version: {version}")?;
        }
        
        if let Some(created) = self.created {
            writeln!(f, "  Created: {}", created.format("%Y-%m-%d %H:%M:%S UTC"))?;
        }
        
        if let Some(updated) = self.updated {
            writeln!(f, "  Last Updated: {}", updated.format("%Y-%m-%d %H:%M:%S UTC"))?;
        }
        
        if let Some(expires) = self.expires {
            writeln!(f, "  Expires: {}", expires.format("%Y-%m-%d %H:%M:%S UTC"))?;
        }
        
        if let Some(not_before) = self.not_before {
            writeln!(f, "  Not Before: {}", not_before.format("%Y-%m-%d %H:%M:%S UTC"))?;
        }
        
        if let Some(content_type) = &self.content_type {
            writeln!(f, "  Content Type: {content_type}")?;
        }
        
        if let Some(recovery_level) = &self.recovery_level {
            writeln!(f, "  Recovery Level: {recovery_level}")?;
        }
        
        if !self.groups.is_empty() {
            writeln!(f, "  Groups: {}", self.groups.join(", "))?;
        }
        
        if let Some(folder) = &self.folder {
            writeln!(f, "  Folder: {folder}")?;
        }
        
        if let Some(note) = &self.note {
            writeln!(f, "  Note: {note}")?;
        }
        
        if let Some(count) = self.version_count {
            writeln!(f, "  Total Versions: {count}")?;
        }
        
        // Display other tags (excluding system tags)
        let system_tags = ["groups", "folder", "note", "original_name", "created_by"];
        let other_tags: HashMap<_, _> = self.tags
            .iter()
            .filter(|(k, _)| !system_tags.contains(&k.as_str()))
            .collect();
        
        if !other_tags.is_empty() {
            writeln!(f, "  Additional Tags:")?;
            for (key, value) in other_tags {
                writeln!(f, "    {key}: {value}")?;
            }
        }
        
        Ok(())
    }
}