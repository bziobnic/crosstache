//! Config, context, cache, and environment command execution handlers.

use crate::cli::commands::{CacheCommands, CharsetType, ConfigCommands, ContextCommands, EnvCommands};
use crate::cli::helpers::format_cache_size;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::output;
use crate::vault::VaultManager;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use zeroize::Zeroizing;

// ── Config ───────────────────────────────────────────────────────────────────

pub(crate) async fn execute_config_command(command: ConfigCommands, config: Config) -> Result<()> {
    match command {
        ConfigCommands::Show => {
            execute_config_show(&config).await?;
        }
        ConfigCommands::Set { key, value } => {
            execute_config_set(&key, &value, config).await?;
        }
        ConfigCommands::Path => {
            execute_config_path().await?;
        }
    }
    Ok(())
}

async fn execute_config_show(config: &Config) -> Result<()> {
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    #[derive(Tabled)]
    struct ConfigItem {
        #[tabled(rename = "Setting")]
        key: String,
        #[tabled(rename = "Value")]
        value: String,
        #[tabled(rename = "Source")]
        source: String,
    }

    let items = vec![
        ConfigItem {
            key: "debug".to_string(),
            value: config.debug.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "subscription_id".to_string(),
            value: if config.subscription_id.is_empty() {
                "<not set>".to_string()
            } else {
                config.subscription_id.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_vault".to_string(),
            value: if config.default_vault.is_empty() {
                "<not set>".to_string()
            } else {
                config.default_vault.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_resource_group".to_string(),
            value: config.default_resource_group.clone(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_location".to_string(),
            value: config.default_location.clone(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "tenant_id".to_string(),
            value: if config.tenant_id.is_empty() {
                "<not set>".to_string()
            } else {
                config.tenant_id.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "function_app_url".to_string(),
            value: if config.function_app_url.is_empty() {
                "<not set>".to_string()
            } else {
                config.function_app_url.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "cache_enabled".to_string(),
            value: config.cache_enabled.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "cache_ttl_secs".to_string(),
            value: format!("{}s", config.cache_ttl_secs),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "output_json".to_string(),
            value: config.output_json.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "no_color".to_string(),
            value: config.no_color.to_string(),
            source: "config".to_string(),
        },
    ];

    // Add blob storage configuration items
    let mut items = items;
    let blob_config = config.get_blob_config();

    // Add credential priority
    items.push(ConfigItem {
        key: "azure_credential_priority".to_string(),
        value: config.azure_credential_priority.to_string(),
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "storage_account".to_string(),
        value: if blob_config.storage_account.is_empty() {
            "<not set>".to_string()
        } else {
            blob_config.storage_account
        },
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "storage_container".to_string(),
        value: blob_config.container_name,
        source: "config".to_string(),
    });

    if let Some(endpoint) = blob_config.endpoint {
        items.push(ConfigItem {
            key: "storage_endpoint".to_string(),
            value: endpoint,
            source: "config".to_string(),
        });
    }

    items.push(ConfigItem {
        key: "blob_chunk_size_mb".to_string(),
        value: blob_config.chunk_size_mb.to_string(),
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "blob_max_concurrent_uploads".to_string(),
        value: blob_config.max_concurrent_uploads.to_string(),
        source: "config".to_string(),
    });

    let items = items;

    if config.output_json {
        let json_output = serde_json::to_string_pretty(config).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize config: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let table = Table::new(&items);
        println!("{}", format_table(table, config.no_color));
    }

    Ok(())
}

async fn execute_config_path() -> Result<()> {
    let config_path = Config::get_config_path()?;
    println!("{}", config_path.display());
    Ok(())
}

async fn execute_config_set(key: &str, value: &str, mut config: Config) -> Result<()> {
    match key {
        "debug" => {
            config.debug = value.to_lowercase() == "true" || value == "1";
        }
        "subscription_id" => {
            config.subscription_id = value.to_string();
        }
        "default_vault" => {
            config.default_vault = value.to_string();
        }
        "default_resource_group" => {
            config.default_resource_group = value.to_string();
        }
        "default_location" => {
            config.default_location = value.to_string();
        }
        "tenant_id" => {
            config.tenant_id = value.to_string();
        }
        "function_app_url" => {
            config.function_app_url = value.to_string();
        }
        "cache_enabled" => {
            config.cache_enabled = value.to_lowercase() == "true" || value == "1";
        }
        "cache_ttl" | "cache_ttl_secs" => {
            let seconds = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for cache_ttl_secs: {value}"))
            })?;
            config.cache_ttl_secs = seconds;
        }
        "output_json" => {
            config.output_json = value.to_lowercase() == "true" || value == "1";
        }
        "no_color" => {
            config.no_color = value.to_lowercase() == "true" || value == "1";
        }
        "azure_credential_priority" => {
            use crate::config::settings::AzureCredentialType;
            use std::str::FromStr;
            config.azure_credential_priority =
                AzureCredentialType::from_str(value).map_err(CrosstacheError::config)?;
        }
        // Blob storage configuration
        "storage_account" => {
            let mut blob_config = config.get_blob_config();
            blob_config.storage_account = value.to_string();
            config.set_blob_config(blob_config);
        }
        "storage_container" => {
            let mut blob_config = config.get_blob_config();
            blob_config.container_name = value.to_string();
            config.set_blob_config(blob_config);
        }
        "storage_endpoint" => {
            let mut blob_config = config.get_blob_config();
            blob_config.endpoint = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            config.set_blob_config(blob_config);
        }
        "blob_chunk_size_mb" => {
            let chunk_size = value.parse::<usize>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for blob_chunk_size_mb: {value}"))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.chunk_size_mb = chunk_size;
            config.set_blob_config(blob_config);
        }
        "blob_max_concurrent_uploads" => {
            let max_uploads = value.parse::<usize>().map_err(|_| {
                CrosstacheError::config(format!(
                    "Invalid value for blob_max_concurrent_uploads: {value}"
                ))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.max_concurrent_uploads = max_uploads;
            config.set_blob_config(blob_config);
        }
        "clipboard_timeout" => {
            config.clipboard_timeout = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!(
                    "Invalid value for clipboard_timeout: {value} (expected seconds as integer, 0 to disable)"
                ))
            })?;
        }
        "gen_default_charset" => {
            let charset = value
                .parse::<CharsetType>()
                .map_err(CrosstacheError::config)?;
            config.gen_default_charset = Some(charset.to_string());
        }
        _ => {
            return Err(CrosstacheError::config(format!(
                "Unknown configuration key: {key}. Available keys: debug, subscription_id, default_vault, default_resource_group, default_location, tenant_id, function_app_url, cache_enabled, cache_ttl_secs, output_json, no_color, azure_credential_priority, storage_account, storage_container, storage_endpoint, blob_chunk_size_mb, blob_max_concurrent_uploads, clipboard_timeout, gen_default_charset"
            )));
        }
    }

    config.save().await?;
    output::success(&format!("Configuration updated: {key} = {value}"));

    Ok(())
}

// ── Cache ────────────────────────────────────────────────────────────────────

pub(crate) async fn execute_cache_command(command: CacheCommands, config: Config) -> Result<()> {
    use crate::cache::CacheManager;

    let cache_manager = CacheManager::from_config(&config);

    match command {
        CacheCommands::Clear { vault } => {
            let vault_ref = vault.as_deref();
            cache_manager.clear(vault_ref);
            match vault_ref {
                Some(name) => output::success(&format!("Cache cleared for vault '{name}'.")),
                None => output::success("Cache cleared."),
            }
        }
        CacheCommands::Status => {
            let status = cache_manager.status();
            println!("Cache directory : {}", status.cache_dir.display());
            println!("Enabled         : {}", status.enabled);
            println!("TTL             : {}s", status.ttl_secs);
            println!("Entries         : {}", status.entry_count);
            println!(
                "Total size      : {}",
                format_cache_size(status.total_size_bytes)
            );
            if !status.entries.is_empty() {
                println!("\nEntries:");
                for entry in &status.entries {
                    let freshness = if entry.is_stale { "stale" } else { "fresh" };
                    println!(
                        "  {} — created {} — expires {} [{}]",
                        entry.key,
                        entry.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
                        entry.expires_at.format("%Y-%m-%d %H:%M:%S UTC"),
                        freshness,
                    );
                }
            }
        }
        CacheCommands::Refresh { key } => {
            execute_cache_refresh(&key, config).await?;
        }
    }
    Ok(())
}

async fn execute_cache_refresh(key: &str, config: Config) -> Result<()> {
    use crate::cache::refresh::release_lock;
    use crate::cache::{CacheKey, CacheManager};

    let cache_key: CacheKey = key
        .parse()
        .map_err(CrosstacheError::invalid_argument)?;

    let cache_manager = CacheManager::from_config(&config);
    let lock_path = cache_key
        .to_path(cache_manager.cache_dir())
        .with_extension("lock");

    let result = match cache_key {
        CacheKey::SecretsList { ref vault_name } => {
            refresh_secrets_list(vault_name.clone(), config).await
        }
        CacheKey::VaultList => refresh_vault_list(config).await,
        CacheKey::FileList {
            ref vault_name,
            recursive,
        } => {
            #[cfg(feature = "file-ops")]
            {
                crate::cli::file_ops::refresh_file_list(vault_name.clone(), recursive, config).await
            }
            #[cfg(not(feature = "file-ops"))]
            {
                let _ = (vault_name, recursive);
                Ok(())
            }
        }
    };

    release_lock(&lock_path);
    result
}

async fn refresh_secrets_list(vault_name: String, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await?;

    let cache_manager = CacheManager::from_config(&config);
    let cache_key = CacheKey::SecretsList { vault_name };
    cache_manager.set(&cache_key, &secrets);

    Ok(())
}

async fn refresh_vault_list(config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
    use std::sync::Arc;

    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    let vault_manager = VaultManager::new(
        auth_provider,
        config.subscription_id.clone(),
        config.no_color,
    )?;

    let vaults = vault_manager
        .list_vaults_formatted(
            Some(&config.subscription_id),
            None,
            crate::utils::format::OutputFormat::Json,
            None,
        )
        .await?;

    let cache_manager = CacheManager::from_config(&config);
    cache_manager.set(&CacheKey::VaultList, &vaults);

    Ok(())
}

// ── Context ──────────────────────────────────────────────────────────────────

pub(crate) async fn execute_context_command(command: ContextCommands, config: Config) -> Result<()> {
    match command {
        ContextCommands::Show => {
            execute_context_show(&config).await?;
        }
        ContextCommands::Use {
            vault_name,
            resource_group,
            global,
            local,
        } => {
            execute_context_use(&vault_name, resource_group, global, local, &config).await?;
        }
        ContextCommands::List => {
            execute_context_list(&config).await?;
        }
        ContextCommands::Clear { global } => {
            execute_context_clear(global, &config).await?;
        }
    }
    Ok(())
}

async fn execute_context_show(config: &Config) -> Result<()> {
    use crate::config::ContextManager;

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if let Some(ref context) = context_manager.current {
        println!("Current Vault Context:");
        println!("  Vault: {}", context.vault_name);
        if let Some(ref rg) = context.resource_group {
            println!("  Resource Group: {rg}");
        }
        if let Some(ref sub) = context.subscription_id {
            println!("  Subscription: {sub}");
        }
        println!(
            "  Last Used: {}",
            context.last_used.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!("  Usage Count: {}", context.usage_count);

        // Show context source
        println!("  Scope: {}", context_manager.scope_description());
    } else {
        output::info("No vault context set");
        if !config.default_vault.is_empty() {
            println!("Using config default: {}", config.default_vault);
        } else {
            println!("Hint: Use 'xv context use <vault-name>' to set a context");
        }
    }

    Ok(())
}

async fn execute_context_use(
    vault_name: &str,
    resource_group: Option<String>,
    global: bool,
    local: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::{ContextManager, VaultContext};

    let mut context_manager = if local {
        // Create local context
        ContextManager::new_local()?
    } else if global {
        // Use global context
        ContextManager::new_global()?
    } else {
        // Load existing or create new (defaults to global)
        ContextManager::load()
            .await
            .unwrap_or_else(|_| ContextManager::new_global().unwrap_or_default())
    };

    // Create new context
    let new_context = VaultContext::new(
        vault_name.to_string(),
        resource_group.or_else(|| {
            if !config.default_resource_group.is_empty() {
                Some(config.default_resource_group.clone())
            } else {
                None
            }
        }),
        if !config.subscription_id.is_empty() {
            Some(config.subscription_id.clone())
        } else {
            None
        },
    );

    // Update context manager
    context_manager.set_context(new_context).await?;

    let scope = if local { "local" } else { "global" };
    output::success(&format!(
        "Switched to vault '{vault_name}' ({scope} context)"
    ));

    if let Some(ref rg) = context_manager.current_resource_group() {
        println!("   Resource Group: {rg}");
    }

    Ok(())
}

async fn execute_context_list(_config: &Config) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if context_manager.recent.is_empty() && context_manager.current.is_none() {
        output::info("No vault contexts found");
        println!("Hint: Use 'xv context use <vault-name>' to create a context");
        return Ok(());
    }

    #[derive(Tabled)]
    struct ContextItem {
        #[tabled(rename = "Status")]
        status: String,
        #[tabled(rename = "Vault")]
        vault: String,
        #[tabled(rename = "Resource Group")]
        resource_group: String,
        #[tabled(rename = "Last Used")]
        last_used: String,
        #[tabled(rename = "Usage Count")]
        usage_count: String,
    }

    let mut items = Vec::new();

    // Add current context
    if let Some(ref context) = context_manager.current {
        items.push(ContextItem {
            status: "● Current".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    // Add recent contexts
    for context in context_manager.list_recent() {
        // Skip if it's the current context
        if let Some(ref current) = context_manager.current {
            if current.vault_name == context.vault_name {
                continue;
            }
        }

        items.push(ContextItem {
            status: "  Recent".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    if !items.is_empty() {
        let table = Table::new(&items);
        println!("{}", format_table(table, false));

        println!("\nScope: {}", context_manager.scope_description());
        if ContextManager::local_context_exists() {
            println!("Note: Local context file found in current directory (.xv/context)");
        }
    }

    Ok(())
}

async fn execute_context_clear(global: bool, _config: &Config) -> Result<()> {
    use crate::config::ContextManager;

    let mut context_manager = if global {
        ContextManager::new_global()?
    } else {
        ContextManager::load().await.unwrap_or_default()
    };

    if context_manager.current.is_none() {
        output::info("No active context to clear");
        return Ok(());
    }

    let vault_name = context_manager
        .current_vault()
        .unwrap_or("unknown")
        .to_string();
    context_manager.clear_context().await?;

    let scope = if global {
        "global"
    } else {
        context_manager.scope_description()
    };
    output::success(&format!(
        "Cleared vault context for '{vault_name}' ({scope} scope)"
    ));

    Ok(())
}

// ── Environment Profiles ─────────────────────────────────────────────────────

/// Environment profile structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EnvironmentProfile {
    pub name: String,
    pub vault_name: String,
    pub resource_group: String,
    pub subscription_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used: Option<chrono::DateTime<chrono::Utc>>,
}

impl EnvironmentProfile {
    pub fn new(
        name: String,
        vault_name: String,
        resource_group: String,
        subscription_id: Option<String>,
    ) -> Self {
        Self {
            name,
            vault_name,
            resource_group,
            subscription_id,
            created_at: chrono::Utc::now(),
            last_used: None,
        }
    }

    pub fn update_usage(&mut self) {
        self.last_used = Some(chrono::Utc::now());
    }
}

/// Environment profile manager
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct EnvironmentProfileManager {
    pub profiles: std::collections::HashMap<String, EnvironmentProfile>,
    pub current_profile: Option<String>,
}

impl EnvironmentProfileManager {
    /// Load profiles from configuration file
    pub async fn load() -> Result<Self> {
        let profile_path = Self::get_profile_path()?;

        if !profile_path.exists() {
            return Ok(Self::default());
        }

        let content = tokio::fs::read_to_string(&profile_path).await?;
        let manager = serde_json::from_str(&content)?;
        Ok(manager)
    }

    /// Save profiles to configuration file
    pub async fn save(&self) -> Result<()> {
        let profile_path = Self::get_profile_path()?;

        // Create parent directories if they don't exist
        if let Some(parent) = profile_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let content = serde_json::to_string_pretty(self)?;
        crate::utils::helpers::write_sensitive_file_async(&profile_path, content.as_bytes())
            .await?;
        Ok(())
    }

    /// Get the profile configuration file path
    fn get_profile_path() -> Result<PathBuf> {
        // Check for local .xv.json file first
        let local_path = std::env::current_dir()?.join(".xv.json");
        if local_path.exists() {
            return Ok(local_path);
        }

        // Use global profile path
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
            Ok(config_dir.join("xv").join("profiles.json"))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let config_dir = dirs::config_dir()
                .ok_or_else(|| CrosstacheError::config("Unable to determine config directory"))?;
            Ok(config_dir.join("xv").join("profiles.json"))
        }
    }

    /// Add a new environment profile
    pub fn create_profile(&mut self, profile: EnvironmentProfile) -> Result<()> {
        if self.profiles.contains_key(&profile.name) {
            return Err(CrosstacheError::config(format!(
                "Environment profile '{}' already exists",
                profile.name
            )));
        }

        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }

    /// Delete an environment profile
    pub fn delete_profile(&mut self, name: &str) -> Result<()> {
        if !self.profiles.contains_key(name) {
            return Err(CrosstacheError::config(format!(
                "Environment profile '{}' not found",
                name
            )));
        }

        // Clear current profile if it's the one being deleted
        if self.current_profile.as_ref() == Some(&name.to_string()) {
            self.current_profile = None;
        }

        self.profiles.remove(name);
        Ok(())
    }

    /// Use an environment profile (set it as current)
    pub fn use_profile(&mut self, name: &str) -> Result<&EnvironmentProfile> {
        let profile = self.profiles.get_mut(name).ok_or_else(|| {
            CrosstacheError::config(format!("Environment profile '{}' not found", name))
        })?;

        profile.update_usage();
        self.current_profile = Some(name.to_string());
        Ok(profile)
    }
}

// ── Env Commands ─────────────────────────────────────────────────────────────

pub(crate) async fn execute_env_command(command: EnvCommands, config: Config) -> Result<()> {
    match command {
        EnvCommands::List => execute_env_list(&config).await,
        EnvCommands::Use { name } => execute_env_use(&name, &config).await,
        EnvCommands::Create {
            name,
            vault,
            group,
            subscription,
            global,
        } => execute_env_create(&name, &vault, &group, subscription, global, &config).await,
        EnvCommands::Delete { name, force } => execute_env_delete(&name, force, &config).await,
        EnvCommands::Show => execute_env_show(&config).await,
        EnvCommands::Pull {
            format,
            group,
            output,
        } => execute_env_pull(&format, group, output, &config).await,
        EnvCommands::Push { file, overwrite } => execute_env_push(file, overwrite, &config).await,
    }
}

async fn execute_env_list(_config: &Config) -> Result<()> {
    let manager = EnvironmentProfileManager::load().await?;

    if manager.profiles.is_empty() {
        output::info("No environment profiles found.");
        println!("Create one with: xv env create <name> --vault <vault> --group <group>");
        return Ok(());
    }

    println!("Environment Profiles:");
    println!("────────────────────");

    for (name, profile) in &manager.profiles {
        let current_marker = if manager.current_profile.as_ref() == Some(name) {
            "* "
        } else {
            "  "
        };

        println!(
            "{}{} → {} ({})",
            current_marker, name, profile.vault_name, profile.resource_group
        );

        if let Some(last_used) = profile.last_used {
            println!(
                "    Last used: {}",
                last_used.format("%Y-%m-%d %H:%M:%S UTC")
            );
        }
    }

    if let Some(current_name) = &manager.current_profile {
        println!("\nCurrent profile: {}", current_name);
    } else {
        println!("\nNo profile currently active");
    }

    Ok(())
}

async fn execute_env_use(name: &str, _config: &Config) -> Result<()> {
    let mut manager = EnvironmentProfileManager::load().await?;

    // Get profile data before using (to avoid borrow checker issues)
    let (vault_name, resource_group, subscription_id) = {
        let profile = manager.use_profile(name)?;
        (
            profile.vault_name.clone(),
            profile.resource_group.clone(),
            profile.subscription_id.clone(),
        )
    };

    // Update the vault context using the profile
    use crate::config::context::VaultContext;
    use crate::config::ContextManager;

    let vault_context = VaultContext::new(
        vault_name.clone(),
        Some(resource_group.clone()),
        subscription_id.clone(),
    );

    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    context_manager.set_context(vault_context).await?;

    // Save the profile manager
    manager.save().await?;

    output::success(&format!("Using environment profile: {}", name));
    println!("  Vault: {}", vault_name);
    println!("  Resource Group: {}", resource_group);
    if let Some(subscription) = &subscription_id {
        println!("  Subscription: {}", subscription);
    }

    Ok(())
}

async fn execute_env_create(
    name: &str,
    vault: &str,
    group: &str,
    subscription: Option<String>,
    global: bool,
    _config: &Config,
) -> Result<()> {
    let mut manager = EnvironmentProfileManager::load().await?;

    let profile = EnvironmentProfile::new(
        name.to_string(),
        vault.to_string(),
        group.to_string(),
        subscription.clone(),
    );

    manager.create_profile(profile.clone())?;

    if global {
        // Set as current profile
        manager.use_profile(name)?;

        // Update the vault context
        use crate::config::context::VaultContext;
        use crate::config::ContextManager;

        let vault_context = VaultContext::new(
            vault.to_string(),
            Some(group.to_string()),
            subscription.clone(),
        );

        let mut context_manager = ContextManager::load().await.unwrap_or_default();
        context_manager.set_context(vault_context).await?;
    }

    manager.save().await?;

    output::success(&format!("Created environment profile: {}", name));
    println!("  Vault: {}", vault);
    println!("  Resource Group: {}", group);
    if let Some(subscription) = &subscription {
        println!("  Subscription: {}", subscription);
    }

    if global {
        println!("  Set as current profile");
    }

    Ok(())
}

async fn execute_env_delete(name: &str, force: bool, _config: &Config) -> Result<()> {
    let mut manager = EnvironmentProfileManager::load().await?;

    if !manager.profiles.contains_key(name) {
        return Err(CrosstacheError::config(format!(
            "Environment profile '{}' not found",
            name
        )));
    }

    if !force {
        use crate::utils::interactive::InteractivePrompt;

        let prompt = InteractivePrompt::new();
        let confirmation_message = format!("Delete environment profile '{}'?", name);
        if !prompt.confirm(&confirmation_message, false)? {
            println!("Delete cancelled");
            return Ok(());
        }
    }

    manager.delete_profile(name)?;
    manager.save().await?;

    output::success(&format!("Deleted environment profile: {}", name));

    Ok(())
}

async fn execute_env_show(_config: &Config) -> Result<()> {
    let manager = EnvironmentProfileManager::load().await?;

    if let Some(current_name) = &manager.current_profile {
        if let Some(profile) = manager.profiles.get(current_name) {
            println!("Current Environment Profile: {}", current_name);
            println!("──────────────────────────");
            println!("Vault: {}", profile.vault_name);
            println!("Resource Group: {}", profile.resource_group);
            if let Some(subscription) = &profile.subscription_id {
                println!("Subscription: {}", subscription);
            }
            println!(
                "Created: {}",
                profile.created_at.format("%Y-%m-%d %H:%M:%S UTC")
            );
            if let Some(last_used) = profile.last_used {
                println!("Last Used: {}", last_used.format("%Y-%m-%d %H:%M:%S UTC"));
            }
        } else {
            println!(
                "Current profile '{}' not found (corrupted state)",
                current_name
            );
        }
    } else {
        output::info("No environment profile is currently active");
        println!("Use 'xv env list' to see available profiles");
        println!("Use 'xv env use <name>' to activate a profile");
    }

    Ok(())
}

async fn execute_env_pull(
    format: &crate::utils::format::OutputFormat,
    groups: Vec<String>,
    output: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use crate::utils::format::OutputFormat;
    use std::sync::Arc;

    // Create authentication provider and secret manager
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Determine vault name
    let vault_name = config.resolve_vault_name(None).await?;

    eprintln!(
        "Pulling secrets from vault '{}'...",
        vault_name
    );

    // Get all secrets or filtered by group
    let mut all_secrets = Vec::new();
    if groups.is_empty() {
        // Get all secrets
        let secrets = secret_manager
            .list_secrets_formatted(
                &vault_name,
                None,
                crate::utils::format::OutputFormat::Json, // We don't use the output, just need the list
                false,
                true,
            )
            .await?;
        for secret_summary in secrets {
            match secret_manager
                .get_secret_safe(&vault_name, &secret_summary.name, true, true)
                .await
            {
                Ok(secret) => all_secrets.push(secret),
                Err(e) => eprintln!(
                    "Warning: Failed to get secret '{}': {}",
                    secret_summary.name, e
                ),
            }
        }
    } else {
        // Get secrets filtered by groups
        for group in &groups {
            let secrets = secret_manager
                .list_secrets_formatted(
                    &vault_name,
                    Some(group),
                    crate::utils::format::OutputFormat::Json, // We don't use the output, just need the list
                    false,
                    true,
                )
                .await?;
            for secret_summary in secrets {
                match secret_manager
                    .get_secret_safe(&vault_name, &secret_summary.name, true, true)
                    .await
                {
                    Ok(secret) => all_secrets.push(secret),
                    Err(e) => eprintln!(
                        "Warning: Failed to get secret '{}': {}",
                        secret_summary.name, e
                    ),
                }
            }
        }
    }

    // Format the secrets based on the requested output format
    let content = match format.resolve_for_stdout() {
        OutputFormat::Json => {
            // Build a simple JSON array of {name, value} objects
            let entries: Vec<serde_json::Value> = all_secrets
                .iter()
                .filter_map(|s| {
                    s.value.as_ref().map(|v| {
                        serde_json::json!({ "name": s.original_name, "value": v.as_str() })
                    })
                })
                .collect();
            serde_json::to_string_pretty(&entries).map_err(|e| {
                CrosstacheError::serialization(format!("JSON serialization failed: {e}"))
            })?
        }
        OutputFormat::Yaml => {
            let entries: Vec<serde_json::Value> = all_secrets
                .iter()
                .filter_map(|s| {
                    s.value.as_ref().map(|v| {
                        serde_json::json!({ "name": s.original_name, "value": v.as_str() })
                    })
                })
                .collect();
            serde_yaml::to_string(&entries).map_err(|e| {
                CrosstacheError::serialization(format!("YAML serialization failed: {e}"))
            })?
        }
        OutputFormat::Csv => {
            let mut csv = String::from("name,value\n");
            for s in &all_secrets {
                if let Some(ref v) = s.value {
                    let escaped = v.replace('"', "\"\"");
                    csv.push_str(&format!("{},\"{}\"\n", s.original_name, escaped));
                }
            }
            csv
        }
        // Plain / Auto / Table / Template / Raw: use dotenv format
        _ => {
            let mut dotenv_content = String::new();
            for secret in &all_secrets {
                if let Some(ref value) = secret.value {
                    let key = &secret.original_name;
                    let escaped_value =
                        if value.contains('\n') || value.contains('"') || value.contains('\\') {
                            format!(
                                "\"{}\"",
                                value
                                    .replace('\\', "\\\\")
                                    .replace('"', "\\\"")
                                    .replace('\n', "\\n")
                            )
                        } else if value.contains(' ') || value.starts_with('#') {
                            format!("\"{}\"", value.as_str())
                        } else {
                            value.to_string()
                        };
                    dotenv_content.push_str(&format!("{}={}\n", key, escaped_value));
                }
            }
            dotenv_content
        }
    };

    // Output to file or stdout
    if let Some(output_path) = output {
        crate::utils::helpers::write_sensitive_file(
            std::path::Path::new(&output_path),
            content.as_bytes(),
        )?;
        output::success(&format!(
            "Successfully exported {} secret(s) to '{}' (permissions: owner-only)",
            all_secrets.len(),
            output_path
        ));
    } else {
        print!("{}", content);
    }

    Ok(())
}

async fn execute_env_push(file: Option<String>, overwrite: bool, config: &Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use crate::secret::manager::SecretRequest;
    use std::collections::HashMap;
    use std::io::Read;
    use std::sync::Arc;

    // Create authentication provider and secret manager
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Determine vault name
    let vault_name = config.resolve_vault_name(None).await?;

    // Read .env content from file or stdin
    let env_content = if let Some(file_path) = file {
        println!("Reading .env file from '{}'...", file_path);
        std::fs::read_to_string(&file_path)?
    } else {
        println!("Reading .env content from stdin...");
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    };

    // Parse .env content
    let mut secrets = HashMap::new();
    for (line_num, line) in env_content.lines().enumerate() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse KEY=VALUE format
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let value = line[eq_pos + 1..].trim();

            // Handle quoted values
            let processed_value =
                if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                    let unquoted = &value[1..value.len() - 1];
                    // Unescape quoted content
                    unquoted
                        .replace("\\\"", "\"")
                        .replace("\\n", "\n")
                        .replace("\\\\", "\\")
                } else {
                    value.to_string()
                };

            if key.is_empty() {
                eprintln!("Warning: Empty key on line {} - skipping", line_num + 1);
                continue;
            }

            secrets.insert(key.to_string(), processed_value);
        } else {
            eprintln!(
                "Warning: Invalid format on line {} - skipping: {}",
                line_num + 1,
                line
            );
        }
    }

    if secrets.is_empty() {
        println!("No valid key=value pairs found in input");
        return Ok(());
    }

    println!(
        "Pushing {} secret(s) to vault '{}'...",
        secrets.len(),
        vault_name
    );

    // Check for existing secrets if not overwriting
    if !overwrite {
        let mut existing_secrets = Vec::new();
        for key in secrets.keys() {
            if secret_manager
                .get_secret_safe(&vault_name, key, false, false)
                .await
                .is_ok()
            {
                existing_secrets.push(key);
            }
        }

        if !existing_secrets.is_empty() {
            return Err(CrosstacheError::config(format!(
                "The following secret(s) already exist: {}. Use --overwrite to replace them.",
                existing_secrets
                    .into_iter()
                    .map(|s| format!("'{}'", s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // Set each secret
    let mut success_count = 0;
    let mut error_count = 0;

    for (key, value) in secrets {
        let secret_request = SecretRequest {
            name: key.clone(),
            value: Zeroizing::new(value.clone()),
            content_type: Some("text/plain".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(HashMap::new()),
            groups: None,
            note: None,
            folder: None,
        };

        match secret_manager
            .set_secret_safe(&vault_name, &key, &value, Some(secret_request))
            .await
        {
            Ok(_) => {
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &format!("Set '{}'", key),
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                output::error(&format!("  Failed to set '{}': {}", key, e));
                error_count += 1;
            }
        }
    }

    if error_count > 0 {
        println!(
            "Completed with {} successful and {} failed operations",
            success_count, error_count
        );
    } else {
        output::success(&format!(
            "Successfully pushed {} secret(s) to vault '{}'",
            success_count, vault_name
        ));
    }

    Ok(())
}
