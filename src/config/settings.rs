//! Configuration settings management
//!
//! This module handles loading configuration from multiple sources,
//! validation, and persistence.

use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tabled::Tabled;
use std::fmt;

/// Azure credential type priority for authentication
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AzureCredentialType {
    /// Use Azure CLI credentials first
    Cli,
    /// Use Managed Identity credentials first
    ManagedIdentity,
    /// Use environment variable credentials first
    Environment,
    /// Use the default credential chain order
    #[default]
    Default,
}

impl fmt::Display for AzureCredentialType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => write!(f, "cli"),
            Self::ManagedIdentity => write!(f, "managed_identity"),
            Self::Environment => write!(f, "environment"),
            Self::Default => write!(f, "default"),
        }
    }
}

impl std::str::FromStr for AzureCredentialType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cli" | "azure-cli" | "az" => Ok(Self::Cli),
            "managed_identity" | "managed-identity" | "msi" => Ok(Self::ManagedIdentity),
            "environment" | "env" => Ok(Self::Environment),
            "default" => Ok(Self::Default),
            _ => Err(format!("Invalid credential type: {s}. Valid options: cli, managed_identity, environment, default")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobConfig {
    pub storage_account: String,
    pub container_name: String,
    pub endpoint: Option<String>,
    pub enable_large_file_support: bool,
    pub chunk_size_mb: usize,
    pub max_concurrent_uploads: usize,
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            storage_account: String::new(),
            container_name: "crosstache-files".to_string(),
            endpoint: None,
            enable_large_file_support: true,
            chunk_size_mb: 4,
            max_concurrent_uploads: 3,
        }
    }
}

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
    #[tabled(skip)]
    pub blob_config: Option<BlobConfig>,
    /// Azure credential type to use first for authentication
    /// Controls the order in which credentials are attempted
    #[tabled(rename = "Credential Priority")]
    #[serde(default)]
    pub azure_credential_priority: AzureCredentialType,
}

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
            blob_config: None,
            azure_credential_priority: AzureCredentialType::Default,
        }
    }
}

impl Config {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        if self.subscription_id.is_empty() {
            return Err(CrosstacheError::config("Subscription ID is required"));
        }

        if self.tenant_id.is_empty() {
            return Err(CrosstacheError::config("Tenant ID is required"));
        }

        Ok(())
    }

    pub fn get_config_path() -> Result<PathBuf> {
        // Use XDG Base Directory specification on Linux and macOS
        // On Windows, use the platform-appropriate config directory
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::env;
            let config_dir = if let Ok(xdg_config_home) = env::var("XDG_CONFIG_HOME") {
                PathBuf::from(xdg_config_home)
            } else {
                let home_dir = env::var("HOME")
                    .map_err(|_| CrosstacheError::config("HOME environment variable not set"))?;
                PathBuf::from(home_dir).join(".config")
            };
            Ok(config_dir.join("xv").join("xv.conf"))
        }
        
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Use platform-appropriate config directory for other platforms
            let config_dir = dirs::config_dir()
                .ok_or_else(|| CrosstacheError::config("Unable to determine config directory"))?;
            Ok(config_dir.join("xv").join("xv.conf"))
        }
    }

    #[allow(dead_code)]
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

        Err(CrosstacheError::config(
            "No vault specified. Use --vault, set context with 'xv context use', or configure default_vault"
        ))
    }

    /// Resolve resource group with context awareness
    /// Priority: CLI argument > context > config default
    #[allow(dead_code)]
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

        Err(CrosstacheError::config("No resource group specified"))
    }

    /// Resolve subscription ID with context awareness
    /// Priority: CLI argument > context > config default
    #[allow(dead_code)]
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

        Err(CrosstacheError::config("No subscription ID specified"))
    }

    /// Get blob storage configuration, creating default if not present
    pub fn get_blob_config(&self) -> BlobConfig {
        self.blob_config.clone().unwrap_or_default()
    }

    /// Set blob storage configuration
    pub fn set_blob_config(&mut self, blob_config: BlobConfig) {
        self.blob_config = Some(blob_config);
    }

    /// Check if blob storage is configured
    #[allow(dead_code)]
    pub fn is_blob_storage_configured(&self) -> bool {
        self.blob_config.as_ref()
            .map(|config| !config.storage_account.is_empty())
            .unwrap_or(false)
    }

    /// Get storage account endpoint URL
    #[allow(dead_code)]
    pub fn get_storage_endpoint(&self) -> Option<String> {
        self.blob_config.as_ref()
            .and_then(|config| {
                config.endpoint.clone().or_else(|| {
                    if !config.storage_account.is_empty() {
                        Some(format!("https://{}.blob.core.windows.net", config.storage_account))
                    } else {
                        None
                    }
                })
            })
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

    // Load Azure credential priority from environment variable
    if let Ok(value) = std::env::var("AZURE_CREDENTIAL_PRIORITY") {
        if let Ok(cred_type) = value.parse::<AzureCredentialType>() {
            config.azure_credential_priority = cred_type;
        }
    }

    // Load blob storage configuration from environment variables
    let mut blob_config = config.blob_config.clone().unwrap_or_default();
    let mut blob_config_updated = false;

    // Check if we have existing config from file
    let had_existing_config = config.blob_config.is_some();

    if let Ok(value) = std::env::var("AZURE_STORAGE_ACCOUNT") {
        blob_config.storage_account = value;
        blob_config_updated = true;
    }

    if let Ok(value) = std::env::var("AZURE_STORAGE_CONTAINER") {
        blob_config.container_name = value;
        blob_config_updated = true;
    }

    if let Ok(value) = std::env::var("AZURE_STORAGE_ENDPOINT") {
        blob_config.endpoint = Some(value);
        blob_config_updated = true;
    }

    if let Ok(value) = std::env::var("BLOB_CHUNK_SIZE_MB") {
        if let Ok(chunk_size) = value.parse::<usize>() {
            blob_config.chunk_size_mb = chunk_size;
            blob_config_updated = true;
        }
    }

    if let Ok(value) = std::env::var("BLOB_MAX_CONCURRENT_UPLOADS") {
        if let Ok(max_uploads) = value.parse::<usize>() {
            blob_config.max_concurrent_uploads = max_uploads;
            blob_config_updated = true;
        }
    }

    // Set blob_config if we have existing config OR if updated by env vars
    if had_existing_config || blob_config_updated {
        config.blob_config = Some(blob_config);
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
        .map_err(|e| CrosstacheError::serialization(e.to_string()))?;

    tokio::fs::write(&config_path, contents).await?;

    Ok(())
}

#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_azure_credential_type_from_str() {
        use std::str::FromStr;

        // Test valid credential types
        assert_eq!(AzureCredentialType::from_str("cli").unwrap(), AzureCredentialType::Cli);
        assert_eq!(AzureCredentialType::from_str("CLI").unwrap(), AzureCredentialType::Cli);
        assert_eq!(AzureCredentialType::from_str("azure-cli").unwrap(), AzureCredentialType::Cli);
        assert_eq!(AzureCredentialType::from_str("az").unwrap(), AzureCredentialType::Cli);
        
        assert_eq!(AzureCredentialType::from_str("managed_identity").unwrap(), AzureCredentialType::ManagedIdentity);
        assert_eq!(AzureCredentialType::from_str("managed-identity").unwrap(), AzureCredentialType::ManagedIdentity);
        assert_eq!(AzureCredentialType::from_str("msi").unwrap(), AzureCredentialType::ManagedIdentity);
        
        assert_eq!(AzureCredentialType::from_str("environment").unwrap(), AzureCredentialType::Environment);
        assert_eq!(AzureCredentialType::from_str("env").unwrap(), AzureCredentialType::Environment);
        
        assert_eq!(AzureCredentialType::from_str("default").unwrap(), AzureCredentialType::Default);

        // Test invalid credential types
        assert!(AzureCredentialType::from_str("invalid").is_err());
        assert!(AzureCredentialType::from_str("unknown").is_err());
        assert!(AzureCredentialType::from_str("").is_err());
    }

    #[test]
    fn test_azure_credential_type_display() {
        assert_eq!(AzureCredentialType::Cli.to_string(), "cli");
        assert_eq!(AzureCredentialType::ManagedIdentity.to_string(), "managed_identity");
        assert_eq!(AzureCredentialType::Environment.to_string(), "environment");
        assert_eq!(AzureCredentialType::Default.to_string(), "default");
    }

    #[test]
    fn test_azure_credential_type_default() {
        assert_eq!(AzureCredentialType::default(), AzureCredentialType::Default);
    }

    #[test]
    fn test_config_with_credential_priority() {
        let mut config = Config::default();
        assert_eq!(config.azure_credential_priority, AzureCredentialType::Default);
        
        config.azure_credential_priority = AzureCredentialType::Cli;
        assert_eq!(config.azure_credential_priority, AzureCredentialType::Cli);
    }
}
