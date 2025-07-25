//! Vault context management for crosstache
//!
//! This module provides smart vault context detection, allowing users to work
//! with vaults without repeatedly specifying --vault flags.

use crate::error::{crosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

/// Represents a vault context with associated metadata
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultContext {
    /// Current vault name
    pub vault_name: String,
    /// Resource group for the vault
    pub resource_group: Option<String>,
    /// Subscription ID (optional override)
    pub subscription_id: Option<String>,
    /// Storage container name for blob operations (optional override)
    pub storage_container: Option<String>,
    /// Last used timestamp
    pub last_used: chrono::DateTime<chrono::Utc>,
    /// Usage count for prioritization
    #[serde(default)]
    pub usage_count: u32,
}

impl VaultContext {
    /// Create a new vault context
    pub fn new(
        vault_name: String,
        resource_group: Option<String>,
        subscription_id: Option<String>,
    ) -> Self {
        Self {
            vault_name,
            resource_group,
            subscription_id,
            storage_container: None,
            last_used: chrono::Utc::now(),
            usage_count: 1,
        }
    }

    /// Create a new vault context with storage container
    pub fn with_storage_container(
        vault_name: String,
        resource_group: Option<String>,
        subscription_id: Option<String>,
        storage_container: Option<String>,
    ) -> Self {
        Self {
            vault_name,
            resource_group,
            subscription_id,
            storage_container,
            last_used: chrono::Utc::now(),
            usage_count: 1,
        }
    }

    /// Update usage timestamp and increment count
    pub fn update_usage(&mut self) {
        self.last_used = chrono::Utc::now();
        self.usage_count = self.usage_count.saturating_add(1);
    }

    /// Check if this context matches the given vault name
    pub fn matches_vault(&self, vault_name: &str) -> bool {
        self.vault_name == vault_name
    }
}

/// Manages vault contexts with local and global persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextManager {
    /// Current active context
    pub current: Option<VaultContext>,
    /// Recently used contexts (max 10)
    pub recent: Vec<VaultContext>,
    /// Context file path
    #[serde(skip)]
    pub context_file: Option<PathBuf>,
    /// Whether this is a local context (directory-specific)
    #[serde(skip)]
    pub is_local: bool,
}

impl Default for ContextManager {
    fn default() -> Self {
        Self {
            current: None,
            recent: Vec::new(),
            context_file: None,
            is_local: false,
        }
    }
}

impl ContextManager {
    /// Load context from local directory or global config
    pub async fn load() -> Result<Self> {
        // 1. Check for .xv/context in current directory
        if let Ok(local_context) = Self::load_local_context().await {
            debug!("Loaded local vault context");
            return Ok(local_context);
        }

        // 2. Fall back to global context
        debug!("No local context found, loading global context");
        Self::load_global_context().await
    }

    /// Load context from current directory (.xv/context)
    async fn load_local_context() -> Result<Self> {
        let context_path = std::env::current_dir()?.join(".xv").join("context");
        if !context_path.exists() {
            return Err(crosstacheError::config("No local context found"));
        }

        let content = tokio::fs::read_to_string(&context_path).await?;
        let mut context: ContextManager = serde_json::from_str(&content)?;
        context.context_file = Some(context_path);
        context.is_local = true;

        debug!(
            "Loaded local context from: {}",
            context.context_file.as_ref().unwrap().display()
        );
        Ok(context)
    }

    /// Load context from global config directory
    async fn load_global_context() -> Result<Self> {
        let context_path = Self::global_context_path()?;
        if !context_path.exists() {
            debug!("No global context file found, using default");
            return Ok(Self {
                context_file: Some(context_path),
                is_local: false,
                ..Default::default()
            });
        }

        let content = tokio::fs::read_to_string(&context_path).await?;
        let mut context: ContextManager = serde_json::from_str(&content)?;
        context.context_file = Some(context_path);
        context.is_local = false;

        debug!(
            "Loaded global context from: {}",
            context.context_file.as_ref().unwrap().display()
        );
        Ok(context)
    }

    /// Create a new local context manager
    pub fn new_local() -> Result<Self> {
        let context_path = std::env::current_dir()?.join(".xv").join("context");
        Ok(Self {
            context_file: Some(context_path),
            is_local: true,
            ..Default::default()
        })
    }

    /// Create a new global context manager
    pub fn new_global() -> Result<Self> {
        Ok(Self {
            context_file: Some(Self::global_context_path()?),
            is_local: false,
            ..Default::default()
        })
    }

    /// Save current context
    pub async fn save(&self) -> Result<()> {
        if let Some(ref path) = self.context_file {
            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            let content = serde_json::to_string_pretty(self)?;
            tokio::fs::write(path, content).await?;

            debug!("Saved context to: {}", path.display());
        }
        Ok(())
    }

    /// Set current context and update recent list
    pub async fn set_context(&mut self, context: VaultContext) -> Result<()> {
        let vault_name = context.vault_name.clone();

        // Update recent contexts - remove existing entry for this vault
        self.recent.retain(|c| c.vault_name != vault_name);

        // Add current context to recent if we're changing contexts
        if let Some(ref current) = self.current {
            if current.vault_name != vault_name {
                self.recent.insert(0, current.clone());
            }
        }

        // Keep only 10 recent contexts
        self.recent.truncate(10);

        self.current = Some(context);
        self.save().await?;

        info!("Set vault context to: {}", vault_name);
        Ok(())
    }

    /// Update usage timestamp for current context
    pub async fn update_usage(&mut self, vault_name: &str) -> Result<()> {
        let mut updated = false;

        if let Some(ref mut context) = self.current {
            if context.vault_name == vault_name {
                context.update_usage();
                updated = true;
            }
        }

        if updated {
            self.save().await?;
            debug!("Updated usage for vault: {}", vault_name);
        }

        Ok(())
    }

    /// Clear current context
    pub async fn clear_context(&mut self) -> Result<()> {
        if let Some(ref context) = self.current {
            info!("Cleared vault context: {}", context.vault_name);

            // Move current context to recent
            self.recent.insert(0, context.clone());
            self.recent.truncate(10);
        }

        self.current = None;
        self.save().await?;
        Ok(())
    }

    /// Get current vault name from context
    pub fn current_vault(&self) -> Option<&str> {
        self.current.as_ref().map(|c| c.vault_name.as_str())
    }

    /// Get current resource group from context
    pub fn current_resource_group(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.resource_group.as_deref())
    }

    /// Get current subscription ID from context
    pub fn current_subscription_id(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.subscription_id.as_deref())
    }

    /// Get current storage container from context
    pub fn current_storage_container(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.storage_container.as_deref())
    }

    /// List recent contexts sorted by usage
    pub fn list_recent(&self) -> Vec<&VaultContext> {
        let mut recent = self.recent.iter().collect::<Vec<_>>();
        recent.sort_by(|a, b| {
            // Sort by usage count (descending), then by last used (descending)
            b.usage_count
                .cmp(&a.usage_count)
                .then_with(|| b.last_used.cmp(&a.last_used))
        });
        recent
    }

    /// Find context by vault name in recent list
    pub fn find_recent_context(&self, vault_name: &str) -> Option<&VaultContext> {
        self.recent.iter().find(|c| c.vault_name == vault_name)
    }

    /// Get global context file path
    fn global_context_path() -> Result<PathBuf> {
        // Use XDG Base Directory specification on Linux and macOS
        // On Windows, use the platform-appropriate config directory
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::env;
            let config_dir = if let Ok(xdg_config_home) = env::var("XDG_CONFIG_HOME") {
                PathBuf::from(xdg_config_home)
            } else {
                let home_dir = env::var("HOME")
                    .map_err(|_| crosstacheError::config("HOME environment variable not set"))?;
                PathBuf::from(home_dir).join(".config")
            };
            Ok(config_dir.join("xv").join("context"))
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Use platform-appropriate config directory for other platforms
            let config_dir = dirs::config_dir()
                .ok_or_else(|| crosstacheError::config("Could not determine config directory"))?;
            Ok(config_dir.join("xv").join("context"))
        }
    }

    /// Check if local context directory exists
    pub fn local_context_exists() -> bool {
        std::env::current_dir()
            .map(|dir| dir.join(".xv").join("context").exists())
            .unwrap_or(false)
    }

    /// Initialize local context directory
    pub async fn init_local_context() -> Result<PathBuf> {
        let context_dir = std::env::current_dir()?.join(".xv");
        tokio::fs::create_dir_all(&context_dir).await?;

        let context_path = context_dir.join("context");

        // Create empty context file if it doesn't exist
        if !context_path.exists() {
            let empty_context = ContextManager::default();
            let content = serde_json::to_string_pretty(&empty_context)?;
            tokio::fs::write(&context_path, content).await?;
        }

        Ok(context_path)
    }

    /// Migrate from old configuration format
    pub async fn migrate_from_config(
        default_vault: &str,
        default_resource_group: Option<&str>,
        subscription_id: Option<&str>,
    ) -> Result<Self> {
        if default_vault.is_empty() {
            return Ok(Self::new_global()?);
        }

        let context = VaultContext::new(
            default_vault.to_string(),
            default_resource_group.map(|s| s.to_string()),
            subscription_id.map(|s| s.to_string()),
        );

        let mut context_manager = Self::new_global()?;
        context_manager.set_context(context).await?;

        info!(
            "Migrated default vault '{}' to context system",
            default_vault
        );
        Ok(context_manager)
    }

    /// Get context scope description for display
    pub fn scope_description(&self) -> &'static str {
        if self.is_local {
            "Local (current directory)"
        } else {
            "Global"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio;

    #[tokio::test]
    async fn test_vault_context_creation() {
        let context = VaultContext::new(
            "test-vault".to_string(),
            Some("test-rg".to_string()),
            Some("test-sub".to_string()),
        );

        assert_eq!(context.vault_name, "test-vault");
        assert_eq!(context.resource_group, Some("test-rg".to_string()));
        assert_eq!(context.subscription_id, Some("test-sub".to_string()));
        assert_eq!(context.usage_count, 1);
    }

    #[tokio::test]
    async fn test_context_update_usage() {
        let mut context = VaultContext::new("test-vault".to_string(), None, None);

        let initial_count = context.usage_count;
        let initial_time = context.last_used;

        // Small delay to ensure timestamp changes
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

        context.update_usage();

        assert_eq!(context.usage_count, initial_count + 1);
        assert!(context.last_used > initial_time);
    }

    #[tokio::test]
    async fn test_context_manager_set_and_clear() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().join("context");

        let mut manager = ContextManager {
            context_file: Some(context_path.clone()),
            ..Default::default()
        };

        let context = VaultContext::new("test-vault".to_string(), None, None);

        // Set context
        manager.set_context(context.clone()).await.unwrap();
        assert_eq!(manager.current_vault(), Some("test-vault"));
        assert!(context_path.exists());

        // Clear context
        manager.clear_context().await.unwrap();
        assert_eq!(manager.current_vault(), None);
        assert_eq!(manager.recent.len(), 1);
        assert_eq!(manager.recent[0].vault_name, "test-vault");
    }

    #[tokio::test]
    async fn test_recent_contexts_limit() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().join("context");

        let mut manager = ContextManager {
            context_file: Some(context_path),
            ..Default::default()
        };

        // Add 12 contexts (more than the limit of 10)
        for i in 0..12 {
            let context = VaultContext::new(format!("vault-{i}"), None, None);
            manager.set_context(context).await.unwrap();
        }

        // Should only keep 10 recent contexts
        assert!(manager.recent.len() <= 10);
    }

    #[test]
    fn test_context_matching() {
        let context = VaultContext::new("my-vault".to_string(), None, None);

        assert!(context.matches_vault("my-vault"));
        assert!(!context.matches_vault("other-vault"));
    }

    #[tokio::test]
    async fn test_vault_context_with_storage_container() {
        let context = VaultContext::with_storage_container(
            "test-vault".to_string(),
            Some("test-rg".to_string()),
            Some("test-sub".to_string()),
            Some("my-container".to_string()),
        );

        assert_eq!(context.vault_name, "test-vault");
        assert_eq!(context.resource_group, Some("test-rg".to_string()));
        assert_eq!(context.subscription_id, Some("test-sub".to_string()));
        assert_eq!(context.storage_container, Some("my-container".to_string()));
        assert_eq!(context.usage_count, 1);
    }

    #[tokio::test]
    async fn test_current_storage_container() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().join("context");

        let mut manager = ContextManager {
            context_file: Some(context_path),
            ..Default::default()
        };

        // Test with no context
        assert_eq!(manager.current_storage_container(), None);

        // Test with context without storage container
        let context1 = VaultContext::new("test-vault".to_string(), None, None);
        manager.set_context(context1).await.unwrap();
        assert_eq!(manager.current_storage_container(), None);

        // Test with context with storage container
        let context2 = VaultContext::with_storage_container(
            "test-vault-2".to_string(),
            None,
            None,
            Some("my-container".to_string()),
        );
        manager.set_context(context2).await.unwrap();
        assert_eq!(manager.current_storage_container(), Some("my-container"));
    }
}
