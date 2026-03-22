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
            .map(|g| {
                g.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
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

// Exists only to give tests a way to build a minimal SecretInfo without filling every field.
#[cfg(test)]
impl SecretInfo {
    fn test_minimal(name: &str, vault_uri: &str) -> Self {
        SecretInfo {
            name: name.to_string(),
            original_name: None,
            id: format!("{vault_uri}secrets/{name}"),
            version: None,
            enabled: true,
            created: None,
            updated: None,
            expires: None,
            not_before: None,
            recovery_level: None,
            content_type: None,
            tags: HashMap::new(),
            groups: Vec::new(),
            folder: None,
            note: None,
            vault_uri: vault_uri.to_string(),
            version_count: None,
        }
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
            writeln!(
                f,
                "  Last Updated: {}",
                updated.format("%Y-%m-%d %H:%M:%S UTC")
            )?;
        }

        if let Some(expires) = self.expires {
            writeln!(f, "  Expires: {}", expires.format("%Y-%m-%d %H:%M:%S UTC"))?;
        }

        if let Some(not_before) = self.not_before {
            writeln!(
                f,
                "  Not Before: {}",
                not_before.format("%Y-%m-%d %H:%M:%S UTC")
            )?;
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
        let other_tags: HashMap<_, _> = self
            .tags
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- extract_groups ---

    #[test]
    fn test_extract_groups_empty_tags() {
        let tags = HashMap::new();
        assert_eq!(SecretInfo::extract_groups(&tags), Vec::<String>::new());
    }

    #[test]
    fn test_extract_groups_single_group() {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), "production".to_string());
        assert_eq!(SecretInfo::extract_groups(&tags), vec!["production"]);
    }

    #[test]
    fn test_extract_groups_multiple_groups() {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), "prod,staging,dev".to_string());
        let groups = SecretInfo::extract_groups(&tags);
        assert_eq!(groups, vec!["prod", "staging", "dev"]);
    }

    #[test]
    fn test_extract_groups_trims_whitespace() {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), " prod , staging , dev ".to_string());
        let groups = SecretInfo::extract_groups(&tags);
        assert_eq!(groups, vec!["prod", "staging", "dev"]);
    }

    #[test]
    fn test_extract_groups_filters_empty_entries() {
        let mut tags = HashMap::new();
        // e.g. trailing comma leaves an empty segment
        tags.insert("groups".to_string(), "prod,,dev,".to_string());
        let groups = SecretInfo::extract_groups(&tags);
        assert_eq!(groups, vec!["prod", "dev"]);
    }

    // --- extract_folder ---

    #[test]
    fn test_extract_folder_missing() {
        let tags = HashMap::new();
        assert_eq!(SecretInfo::extract_folder(&tags), None);
    }

    #[test]
    fn test_extract_folder_present() {
        let mut tags = HashMap::new();
        tags.insert("folder".to_string(), "infra/db".to_string());
        assert_eq!(
            SecretInfo::extract_folder(&tags),
            Some("infra/db".to_string())
        );
    }

    // --- extract_note ---

    #[test]
    fn test_extract_note_missing() {
        let tags = HashMap::new();
        assert_eq!(SecretInfo::extract_note(&tags), None);
    }

    #[test]
    fn test_extract_note_present() {
        let mut tags = HashMap::new();
        tags.insert("note".to_string(), "rotate monthly".to_string());
        assert_eq!(
            SecretInfo::extract_note(&tags),
            Some("rotate monthly".to_string())
        );
    }

    // --- extract_original_name ---

    #[test]
    fn test_extract_original_name_missing() {
        let tags = HashMap::new();
        assert_eq!(SecretInfo::extract_original_name(&tags), None);
    }

    #[test]
    fn test_extract_original_name_present() {
        let mut tags = HashMap::new();
        tags.insert(
            "original_name".to_string(),
            "my very long secret name".to_string(),
        );
        assert_eq!(
            SecretInfo::extract_original_name(&tags),
            Some("my very long secret name".to_string())
        );
    }

    // --- Display ---

    #[test]
    fn test_display_contains_name_and_vault() {
        let info = SecretInfo::test_minimal("api-key", "https://myvault.vault.azure.net/");
        let output = info.to_string();
        assert!(output.contains("api-key"), "output: {output}");
        assert!(
            output.contains("https://myvault.vault.azure.net/"),
            "output: {output}"
        );
    }

    #[test]
    fn test_display_shows_enabled_yes() {
        let mut info =
            SecretInfo::test_minimal("my-secret", "https://myvault.vault.azure.net/");
        info.enabled = true;
        let output = info.to_string();
        assert!(output.contains("Enabled: Yes"), "output: {output}");
    }

    #[test]
    fn test_display_shows_enabled_no() {
        let mut info =
            SecretInfo::test_minimal("my-secret", "https://myvault.vault.azure.net/");
        info.enabled = false;
        let output = info.to_string();
        assert!(output.contains("Enabled: No"), "output: {output}");
    }

    #[test]
    fn test_display_shows_original_name_when_different() {
        let mut info =
            SecretInfo::test_minimal("hashed-name-abc123", "https://myvault.vault.azure.net/");
        info.original_name = Some("my very long original secret name".to_string());
        let output = info.to_string();
        assert!(
            output.contains("Original Name: my very long original secret name"),
            "output: {output}"
        );
    }

    #[test]
    fn test_display_hides_original_name_when_same() {
        let mut info =
            SecretInfo::test_minimal("api-key", "https://myvault.vault.azure.net/");
        info.original_name = Some("api-key".to_string());
        let output = info.to_string();
        assert!(
            !output.contains("Original Name"),
            "should not show original name when same: {output}"
        );
    }

    #[test]
    fn test_display_shows_groups() {
        let mut info =
            SecretInfo::test_minimal("my-secret", "https://myvault.vault.azure.net/");
        info.groups = vec!["prod".to_string(), "infra".to_string()];
        let output = info.to_string();
        assert!(output.contains("Groups: prod, infra"), "output: {output}");
    }

    #[test]
    fn test_display_shows_additional_tags() {
        let mut info =
            SecretInfo::test_minimal("my-secret", "https://myvault.vault.azure.net/");
        info.tags
            .insert("owner".to_string(), "team-platform".to_string());
        let output = info.to_string();
        assert!(output.contains("Additional Tags"), "output: {output}");
        assert!(output.contains("owner"), "output: {output}");
        assert!(output.contains("team-platform"), "output: {output}");
    }

    #[test]
    fn test_display_excludes_system_tags_from_additional() {
        let mut info =
            SecretInfo::test_minimal("my-secret", "https://myvault.vault.azure.net/");
        // System tags should not appear in the "Additional Tags" section
        info.tags.insert("groups".to_string(), "prod".to_string());
        info.tags
            .insert("created_by".to_string(), "xv-cli".to_string());
        // No non-system tags, so "Additional Tags" header should not appear
        let output = info.to_string();
        assert!(
            !output.contains("Additional Tags"),
            "system tags should not appear in additional: {output}"
        );
    }
}
