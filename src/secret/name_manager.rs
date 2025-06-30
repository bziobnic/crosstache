//! Secret name management and mapping
//! 
//! This module provides centralized secret name mapping services
//! for handling user-friendly to sanitized name conversions.
//! Supports persistent storage of name mappings and migration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use tabled::Tabled;

use crate::error::{CrossvaultError, Result};
use crate::utils::sanitizer::{sanitize_secret_name, get_secret_name_info, SecretNameInfo};

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

/// Name mapping storage format
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NameMappingStorage {
    version: String,
    mappings: HashMap<String, NameMapping>, // Key: "vault_name:sanitized_name"
    metadata: HashMap<String, String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Name manager for secret name mapping and persistence
pub struct NameManager {
    storage: Arc<RwLock<NameMappingStorage>>,
    storage_path: PathBuf,
    auto_save: bool,
}

impl NameManager {
    /// Create a new name manager with default storage location
    pub fn new() -> Result<Self> {
        let storage_path = Self::default_storage_path()?;
        Self::with_storage_path(storage_path)
    }

    /// Create a new name manager with custom storage path
    pub fn with_storage_path<P: AsRef<Path>>(storage_path: P) -> Result<Self> {
        let storage_path = storage_path.as_ref().to_path_buf();
        let storage = if storage_path.exists() {
            Self::load_storage(&storage_path)?
        } else {
            NameMappingStorage {
                version: "1.0".to_string(),
                mappings: HashMap::new(),
                metadata: HashMap::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }
        };

        Ok(Self {
            storage: Arc::new(RwLock::new(storage)),
            storage_path,
            auto_save: true,
        })
    }

    /// Create an in-memory only name manager (for testing)
    pub fn in_memory() -> Self {
        let storage = NameMappingStorage {
            version: "1.0".to_string(),
            mappings: HashMap::new(),
            metadata: HashMap::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        Self {
            storage: Arc::new(RwLock::new(storage)),
            storage_path: PathBuf::from("/tmp/in-memory"),
            auto_save: false,
        }
    }

    /// Add or update a name mapping
    pub async fn add_mapping(
        &self,
        vault_name: &str,
        original_name: &str,
        group: Option<&str>,
    ) -> Result<NameMapping> {
        let name_info = get_secret_name_info(original_name)?;
        let group = group.unwrap_or("").to_string();
        let key = format!("{}:{}", vault_name, name_info.sanitized_name);
        
        let mapping = NameMapping {
            original_name: original_name.to_string(),
            sanitized_name: name_info.sanitized_name.clone(),
            vault_name: vault_name.to_string(),
            group,
            is_hashed: name_info.is_hashed,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            metadata: HashMap::new(),
        };

        {
            let mut storage = self.storage.write().await;
            if let Some(existing) = storage.mappings.get(&key) {
                // Update existing mapping
                let mut updated_mapping = existing.clone();
                updated_mapping.updated_at = Utc::now();
                updated_mapping.group = mapping.group.clone();
                storage.mappings.insert(key, updated_mapping.clone());
                storage.updated_at = Utc::now();
                
                if self.auto_save {
                    drop(storage);
                    self.save().await?;
                }
                
                return Ok(updated_mapping);
            }
            
            storage.mappings.insert(key, mapping.clone());
            storage.updated_at = Utc::now();
        }

        if self.auto_save {
            self.save().await?;
        }

        Ok(mapping)
    }

    /// Get mapping by original name
    pub async fn get_mapping_by_original(
        &self,
        vault_name: &str,
        original_name: &str,
    ) -> Result<Option<NameMapping>> {
        let sanitized_name = sanitize_secret_name(original_name)?;
        let key = format!("{}:{}", vault_name, sanitized_name);
        
        let storage = self.storage.read().await;
        Ok(storage.mappings.get(&key).cloned())
    }

    /// Get mapping by sanitized name
    pub async fn get_mapping_by_sanitized(
        &self,
        vault_name: &str,
        sanitized_name: &str,
    ) -> Result<Option<NameMapping>> {
        let key = format!("{}:{}", vault_name, sanitized_name);
        
        let storage = self.storage.read().await;
        Ok(storage.mappings.get(&key).cloned())
    }

    /// Resolve original name from sanitized name
    pub async fn resolve_original_name(
        &self,
        vault_name: &str,
        sanitized_name: &str,
    ) -> Result<String> {
        if let Some(mapping) = self.get_mapping_by_sanitized(vault_name, sanitized_name).await? {
            Ok(mapping.original_name)
        } else {
            // If no mapping found, return the sanitized name as fallback
            Ok(sanitized_name.to_string())
        }
    }

    /// Resolve sanitized name from original name
    pub async fn resolve_sanitized_name(
        &self,
        vault_name: &str,
        original_name: &str,
    ) -> Result<String> {
        if let Some(mapping) = self.get_mapping_by_original(vault_name, original_name).await? {
            Ok(mapping.sanitized_name)
        } else {
            // Create new mapping if needed
            let mapping = self.add_mapping(vault_name, original_name, None).await?;
            Ok(mapping.sanitized_name)
        }
    }

    /// List all mappings for a vault
    pub async fn list_mappings_for_vault(&self, vault_name: &str) -> Result<Vec<NameMapping>> {
        let storage = self.storage.read().await;
        let mut mappings = Vec::new();
        
        for mapping in storage.mappings.values() {
            if mapping.vault_name == vault_name {
                mappings.push(mapping.clone());
            }
        }
        
        // Sort by original name
        mappings.sort_by(|a, b| a.original_name.cmp(&b.original_name));
        Ok(mappings)
    }

    /// List all mappings for a group
    pub async fn list_mappings_for_group(
        &self,
        vault_name: &str,
        group: &str,
    ) -> Result<Vec<NameMapping>> {
        let storage = self.storage.read().await;
        let mut mappings = Vec::new();
        
        for mapping in storage.mappings.values() {
            if mapping.vault_name == vault_name && mapping.group == group {
                mappings.push(mapping.clone());
            }
        }
        
        // Sort by original name
        mappings.sort_by(|a, b| a.original_name.cmp(&b.original_name));
        Ok(mappings)
    }

    /// Get all unique groups for a vault
    pub async fn get_groups_for_vault(&self, vault_name: &str) -> Result<Vec<String>> {
        let storage = self.storage.read().await;
        let mut groups = std::collections::HashSet::new();
        
        for mapping in storage.mappings.values() {
            if mapping.vault_name == vault_name {
                groups.insert(mapping.group.clone());
            }
        }
        
        let mut sorted_groups: Vec<String> = groups.into_iter().collect();
        sorted_groups.sort();
        Ok(sorted_groups)
    }

    /// Remove a mapping
    pub async fn remove_mapping(
        &self,
        vault_name: &str,
        original_name: &str,
    ) -> Result<bool> {
        let sanitized_name = sanitize_secret_name(original_name)?;
        let key = format!("{}:{}", vault_name, sanitized_name);
        
        let removed = {
            let mut storage = self.storage.write().await;
            let removed = storage.mappings.remove(&key).is_some();
            if removed {
                storage.updated_at = Utc::now();
            }
            removed
        };

        if removed && self.auto_save {
            self.save().await?;
        }

        Ok(removed)
    }

    /// Get mapping statistics
    pub async fn get_statistics(&self) -> Result<NameMappingStats> {
        let storage = self.storage.read().await;
        let mut vaults = std::collections::HashSet::new();
        let mut groups = std::collections::HashSet::new();
        let mut hashed_count = 0;
        
        for mapping in storage.mappings.values() {
            vaults.insert(mapping.vault_name.clone());
            groups.insert(mapping.group.clone());
            if mapping.is_hashed {
                hashed_count += 1;
            }
        }
        
        Ok(NameMappingStats {
            total_mappings: storage.mappings.len(),
            hashed_names: hashed_count,
            simple_names: storage.mappings.len() - hashed_count,
            unique_vaults: vaults.len(),
            unique_groups: groups.len(),
            last_updated: storage.updated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        })
    }

    /// Migrate mappings from Azure Key Vault tags
    pub async fn migrate_from_tags(
        &self,
        vault_mappings: Vec<(String, String, HashMap<String, String>)>, // (vault, secret_name, tags)
    ) -> Result<usize> {
        let mut migrated_count = 0;
        
        for (vault_name, sanitized_name, tags) in vault_mappings {
            if let Some(original_name) = tags.get("original_name") {
                let group = tags.get("groups")
                    .and_then(|groups| groups.split(',').next())
                    .map(|g| g.trim().to_string())
                    .unwrap_or_else(|| "".to_string());
                
                // Check if mapping already exists
                let key = format!("{}:{}", vault_name, sanitized_name);
                let exists = {
                    let storage = self.storage.read().await;
                    storage.mappings.contains_key(&key)
                };
                
                if !exists {
                    let mapping = NameMapping {
                        original_name: original_name.clone(),
                        sanitized_name: sanitized_name.clone(),
                        vault_name: vault_name.clone(),
                        group,
                        is_hashed: sanitized_name.len() == 64 && sanitized_name.chars().all(|c| c.is_ascii_hexdigit()),
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                        metadata: tags.clone(),
                    };
                    
                    let mut storage = self.storage.write().await;
                    storage.mappings.insert(key, mapping);
                    storage.updated_at = Utc::now();
                    migrated_count += 1;
                }
            }
        }
        
        if migrated_count > 0 && self.auto_save {
            self.save().await?;
        }
        
        Ok(migrated_count)
    }

    /// Clear all mappings (with confirmation)
    pub async fn clear_all(&self) -> Result<usize> {
        let count = {
            let mut storage = self.storage.write().await;
            let count = storage.mappings.len();
            storage.mappings.clear();
            storage.updated_at = Utc::now();
            count
        };
        
        if self.auto_save {
            self.save().await?;
        }
        
        Ok(count)
    }

    /// Save mappings to storage
    pub async fn save(&self) -> Result<()> {
        let storage = self.storage.read().await;
        let json_data = serde_json::to_string_pretty(&*storage)
            .map_err(|e| CrossvaultError::serialization(format!("Failed to serialize mappings: {}", e)))?;
        
        // Ensure parent directory exists
        if let Some(parent) = self.storage_path.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| CrossvaultError::config(format!("Failed to create storage directory: {}", e)))?;
        }
        
        tokio::fs::write(&self.storage_path, json_data).await
            .map_err(|e| CrossvaultError::config(format!("Failed to save name mappings: {}", e)))?;
        
        Ok(())
    }

    /// Load mappings from storage
    pub async fn load(&self) -> Result<()> {
        let storage = Self::load_storage(&self.storage_path)?;
        let mut current_storage = self.storage.write().await;
        *current_storage = storage;
        Ok(())
    }

    /// Get storage file path
    pub fn get_storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Set auto-save behavior
    pub fn set_auto_save(&mut self, auto_save: bool) {
        self.auto_save = auto_save;
    }

    /// Check if storage file exists
    pub fn storage_exists(&self) -> bool {
        self.storage_path.exists()
    }


    /// Get default storage path
    fn default_storage_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| CrossvaultError::config("Unable to determine config directory"))?;
        
        Ok(config_dir.join("xv").join("name_mappings.json"))
    }

    /// Load storage from file
    fn load_storage(path: &Path) -> Result<NameMappingStorage> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| CrossvaultError::config(format!("Failed to read name mappings file: {}", e)))?;
        
        let storage: NameMappingStorage = serde_json::from_str(&content)
            .map_err(|e| CrossvaultError::serialization(format!("Failed to parse name mappings: {}", e)))?;
        
        Ok(storage)
    }
}

/// Name manager builder for flexible construction
pub struct NameManagerBuilder {
    storage_path: Option<PathBuf>,
    auto_save: bool,
    in_memory: bool,
}

impl NameManagerBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            storage_path: None,
            auto_save: true,
            in_memory: false,
        }
    }

    /// Set custom storage path
    pub fn with_storage_path<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.storage_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Disable auto-save
    pub fn without_auto_save(mut self) -> Self {
        self.auto_save = false;
        self
    }

    /// Use in-memory storage (for testing)
    pub fn in_memory(mut self) -> Self {
        self.in_memory = true;
        self
    }

    /// Build the name manager
    pub fn build(self) -> Result<NameManager> {
        if self.in_memory {
            return Ok(NameManager::in_memory());
        }

        let mut manager = if let Some(path) = self.storage_path {
            NameManager::with_storage_path(path)?
        } else {
            NameManager::new()?
        };

        manager.set_auto_save(self.auto_save);
        Ok(manager)
    }
}

impl Default for NameManagerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_name_manager_basic_operations() {
        let manager = NameManager::in_memory();
        
        // Add a mapping
        let mapping = manager.add_mapping("test-vault", "app/database/connection", None).await.unwrap();
        assert_eq!(mapping.original_name, "app/database/connection");
        assert_eq!(mapping.group, "app/database");
        
        // Resolve names
        let original = manager.resolve_original_name("test-vault", &mapping.sanitized_name).await.unwrap();
        assert_eq!(original, "app/database/connection");
        
        let sanitized = manager.resolve_sanitized_name("test-vault", "app/database/connection").await.unwrap();
        assert_eq!(sanitized, mapping.sanitized_name);
    }

    #[tokio::test]
    async fn test_name_manager_persistence() {
        let temp_dir = tempdir().unwrap();
        let storage_path = temp_dir.path().join("test_mappings.json");
        
        // Create manager and add mapping
        {
            let manager = NameManager::with_storage_path(&storage_path).unwrap();
            manager.add_mapping("test-vault", "test-secret", Some("test-group")).await.unwrap();
        }
        
        // Create new manager and verify persistence
        {
            let manager = NameManager::with_storage_path(&storage_path).unwrap();
            let mapping = manager.get_mapping_by_original("test-vault", "test-secret").await.unwrap();
            assert!(mapping.is_some());
            assert_eq!(mapping.unwrap().group, "test-group");
        }
    }

}