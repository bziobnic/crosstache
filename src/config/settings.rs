//! Configuration settings management
//!
//! This module handles loading configuration from multiple sources,
//! validation, and persistence.

use crate::error::{crosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tabled::Tabled;
use crate::utils::format::FormattableOutput;

#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct Config {
    #[tabled(rename = "Debug")]
    pub debug: bool,
    #[tabled(rename = "Subscription ID")]
    pub subscription_id: String,
    #[tabled(rename = "Default Vault")]
    pub default_vault: String,
    #[tabled(rename = "Default Resource Group")]
    pub default_resource_group: String,
    #[tabled(rename = "Default Location")]
    pub default_location: String,
    #[tabled(skip)]
    pub tenant_id: String,
    #[tabled(skip)]
    pub function_app_url: String,
    #[tabled(skip)]
    pub cache_ttl: Duration,
    #[tabled(rename = "JSON Output")]
    pub output_json: bool,
    #[tabled(rename = "No Color")]
    pub no_color: bool,
}

impl FormattableOutput for Config {}

impl Default for Config {
    fn default() -> Self {
        Self {
            debug: false,
            subscription_id: String::new(),
            default_vault: String::new(),
            default_resource_group: "Vaults".to_string(),
            default_location: "eastus".to_string(),
            tenant_id: String::new(),
            function_app_url: String::new(),
            cache_ttl: Duration::from_secs(300), // 5 minutes
            output_json: false,
            no_color: false,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        if self.subscription_id.is_empty() {
            return Err(crosstacheError::config("Subscription ID is required"));
        }

        if self.tenant_id.is_empty() {
            return Err(crosstacheError::config("Tenant ID is required"));
        }

        Ok(())
    }

    pub fn get_config_path() -> Result<PathBuf> {
        // Use platform-appropriate config directory
        let config_dir = dirs::config_dir()
            .ok_or_else(|| crosstacheError::config("Unable to determine config directory"))?;

        Ok(config_dir.join("xv").join("xv.conf"))
    }

    pub async fn load() -> Result<Self> {
        load_config().await
    }

    pub async fn save(&self) -> Result<()> {
        save_config(self).await
    }

    /// Resolve vault name with context awareness
    /// Priority: CLI argument > context > config default
    pub async fn resolve_vault_name(&self, vault_arg: Option<String>) -> Result<String> {
        use crate::config::ContextManager;

        // 1. Command line argument takes precedence
        if let Some(vault) = vault_arg {
            return Ok(vault);
        }

        // 2. Check local/global context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(vault_name) = context_manager.current_vault() {
            return Ok(vault_name.to_string());
        }

        // 3. Fall back to config default
        if !self.default_vault.is_empty() {
            return Ok(self.default_vault.clone());
        }

        Err(crosstacheError::config(
            "No vault specified. Use --vault, set context with 'xv context use', or configure default_vault"
        ))
    }

    /// Resolve resource group with context awareness
    /// Priority: CLI argument > context > config default
    pub async fn resolve_resource_group(&self, rg_arg: Option<String>) -> Result<String> {
        use crate::config::ContextManager;

        // 1. Command line argument takes precedence
        if let Some(rg) = rg_arg {
            return Ok(rg);
        }

        // 2. Check context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(rg) = context_manager.current_resource_group() {
            return Ok(rg.to_string());
        }

        // 3. Fall back to config default
        if !self.default_resource_group.is_empty() {
            return Ok(self.default_resource_group.clone());
        }

        Err(crosstacheError::config("No resource group specified"))
    }

    /// Resolve subscription ID with context awareness
    /// Priority: CLI argument > context > config default
    pub async fn resolve_subscription_id(&self, sub_arg: Option<String>) -> Result<String> {
        use crate::config::ContextManager;

        // 1. Command line argument takes precedence
        if let Some(sub) = sub_arg {
            return Ok(sub);
        }

        // 2. Check context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(sub) = context_manager.current_subscription_id() {
            return Ok(sub.to_string());
        }

        // 3. Fall back to config default
        if !self.subscription_id.is_empty() {
            return Ok(self.subscription_id.clone());
        }

        Err(crosstacheError::config("No subscription ID specified"))
    }
}

/// Load configuration from multiple sources with priority order:
/// 1. Command-line flags (handled by clap)
/// 2. Environment variables
/// 3. Configuration file
/// 4. Default values
pub async fn load_config() -> Result<Config> {
    let config = load_config_no_validation().await?;
    
    // Validate configuration
    config.validate()?;
    
    Ok(config)
}

/// Load configuration without validation (for init and config commands)
pub async fn load_config_no_validation() -> Result<Config> {
    let mut config = Config::default();

    // Load from configuration file if it exists
    let config_path = Config::get_config_path()?;
    if config_path.exists() {
        config = load_from_file(&config_path).await?;
    }

    // Override with environment variables
    load_from_env(&mut config);

    Ok(config)
}

async fn load_from_file(path: &PathBuf) -> Result<Config> {
    let contents = tokio::fs::read_to_string(path).await?;

    // Try to parse as TOML first, then JSON as fallback
    if let Ok(config) = toml::from_str::<Config>(&contents) {
        return Ok(config);
    }

    let config = serde_json::from_str::<Config>(&contents)?;
    Ok(config)
}

fn load_from_env(config: &mut Config) {
    if let Ok(value) = std::env::var("DEBUG") {
        config.debug = value.to_lowercase() == "true" || value == "1";
    }

    if let Ok(value) = std::env::var("AZURE_SUBSCRIPTION_ID") {
        config.subscription_id = value;
    }

    if let Ok(value) = std::env::var("DEFAULT_VAULT") {
        config.default_vault = value;
    }

    if let Ok(value) = std::env::var("DEFAULT_RESOURCE_GROUP") {
        config.default_resource_group = value;
    }

    if let Ok(value) = std::env::var("AZURE_TENANT_ID") {
        config.tenant_id = value;
    }

    if let Ok(value) = std::env::var("FUNCTION_APP_URL") {
        config.function_app_url = value;
    }

    if let Ok(value) = std::env::var("CACHE_TTL") {
        if let Ok(seconds) = value.parse::<u64>() {
            config.cache_ttl = Duration::from_secs(seconds);
        }
    }
}

pub async fn save_config(config: &Config) -> Result<()> {
    let config_path = Config::get_config_path()?;

    // Create parent directories if they don't exist
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Serialize to TOML format
    let contents = toml::to_string_pretty(config)
        .map_err(|e| crosstacheError::serialization(e.to_string()))?;

    tokio::fs::write(&config_path, contents).await?;

    Ok(())
}

pub async fn init_default_config() -> Result<()> {
    let config_path = Config::get_config_path()?;

    // Don't overwrite existing configuration
    if config_path.exists() {
        return Ok(());
    }

    let config = Config::default();
    save_config(&config).await?;

    Ok(())
}
