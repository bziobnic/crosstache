//! CLI commands and argument parsing
//! 
//! This module defines the command-line interface structure using clap,
//! including all commands, subcommands, and their arguments.

use clap::{Parser, Subcommand};
use crate::config::Config;
use crate::error::{crosstacheError, Result};
use crate::vault::{VaultManager, VaultCreateRequest};
use crate::utils::format::OutputFormat;

/// Get the full version string with build information
fn get_version() -> &'static str {
    env!("VERSION_WITH_GIT")
}

/// Get build information for display
pub fn get_build_info() -> BuildInfo {
    BuildInfo {
        version: env!("FINAL_VERSION"),
        build_number: env!("BUILD_NUMBER"),
        git_hash: env!("GIT_HASH"),
        git_branch: env!("GIT_BRANCH"),
        build_time: env!("BUILD_TIME"),
        full_version: env!("FINAL_VERSION"),
    }
}

#[derive(Debug)]
pub struct BuildInfo {
    pub version: &'static str,
    pub build_number: &'static str,
    pub git_hash: &'static str,
    pub git_branch: &'static str,
    pub build_time: &'static str,
    pub full_version: &'static str,
}

#[derive(Parser)]
#[command(name = "xv")]
#[command(about = "A comprehensive tool for managing Azure Key Vaults")]
#[command(version = get_version(), author)]
pub struct Cli {
    /// Enable debug logging
    #[arg(long, global = true)]
    pub debug: bool,
    
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Vault management commands
    Vault {
        #[command(subcommand)]
        command: VaultCommands,
    },
    /// Secret management commands
    Secret {
        #[command(subcommand)]
        command: SecretCommands,
    },
    /// Configuration management commands
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Vault context management
    Context {
        #[command(subcommand)]
        command: ContextCommands,
    },
    /// Initialize default configuration
    Init,
    /// Show vault information (alias for vault info)
    Info {
        /// Vault name
        vault_name: Option<String>,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Subscription ID
        #[arg(short, long)]
        subscription: Option<String>,
    },
    /// Show detailed version and build information
    Version,
}

#[derive(Subcommand)]
pub enum VaultCommands {
    /// Create a new vault
    Create {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Location
        #[arg(short, long)]
        location: Option<String>,
    },
    /// List vaults
    List {
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
    },
    /// Delete a vault
    Delete {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Show vault information
    Info {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
    },
    /// Restore a soft-deleted vault
    Restore {
        /// Vault name
        name: String,
        /// Location (region) where the vault was deleted
        #[arg(short, long)]
        location: String,
    },
    /// Permanently purge a soft-deleted vault
    Purge {
        /// Vault name
        name: String,
        /// Location (region) where the vault was deleted
        #[arg(short, long)]
        location: String,
        /// Force purge without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Export vault secrets to a file
    Export {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Export format (json, env, txt)
        #[arg(short, long, default_value = "json")]
        format: String,
        /// Include secret values (requires appropriate permissions)
        #[arg(long)]
        include_values: bool,
        /// Filter by secret group
        #[arg(short, long)]
        group: Option<String>,
    },
    /// Import secrets from a file
    Import {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Input file path (default: stdin)
        #[arg(short, long)]
        input: Option<String>,
        /// Import format (json, env, txt)
        #[arg(short, long, default_value = "json")]
        format: String,
        /// Overwrite existing secrets
        #[arg(long)]
        overwrite: bool,
        /// Dry run (show what would be imported)
        #[arg(long)]
        dry_run: bool,
    },
    /// Update vault properties and tags
    Update {
        /// Vault name
        name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Add or update tags (key=value format)
        #[arg(long, value_parser = parse_key_val::<String, String>)]
        tag: Vec<(String, String)>,
        /// Enable vault for deployment
        #[arg(long)]
        enable_deployment: Option<bool>,
        /// Enable vault for disk encryption
        #[arg(long)]
        enable_disk_encryption: Option<bool>,
        /// Enable vault for template deployment
        #[arg(long)]
        enable_template_deployment: Option<bool>,
        /// Enable purge protection
        #[arg(long)]
        enable_purge_protection: Option<bool>,
        /// Soft delete retention in days (7-90)
        #[arg(long)]
        retention_days: Option<i32>,
    },
    /// Vault-level access management
    Share {
        #[command(subcommand)]
        command: VaultShareCommands,
    }
}

#[derive(Subcommand)]
pub enum VaultShareCommands {
    /// Grant access to a vault
    Grant {
        /// Vault name
        vault_name: String,
        /// User email or service principal ID
        user: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Access level (reader, contributor, admin)
        #[arg(short, long, default_value = "reader")]
        level: String,
    },
    /// Revoke access to a vault
    Revoke {
        /// Vault name
        vault_name: String,
        /// User email or service principal ID
        user: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
    },
    /// List vault access assignments
    List {
        /// Vault name
        vault_name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: String,
    },
}

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Set a secret
    Set {
        /// Secret name
        name: String,
        /// Vault name
        vault: Option<String>,
        /// Read value from stdin
        #[arg(long)]
        stdin: bool,
        /// Note to attach to the secret
        #[arg(long)]
        note: Option<String>,
        /// Folder path for the secret (e.g., 'app/database', 'config/dev')
        #[arg(long)]
        folder: Option<String>,
    },
    /// Get a secret
    Get {
        /// Secret name
        name: String,
        /// Vault name
        vault: Option<String>,
        /// Raw output (print value instead of copying to clipboard)
        #[arg(short, long)]
        raw: bool,
    },
    /// List secrets
    List {
        /// Vault name
        vault: Option<String>,
        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,
        /// Show all secrets including disabled ones
        #[arg(long)]
        all: bool,
    },
    /// Delete a secret
    Delete {
        /// Secret name
        name: String,
        /// Vault name
        vault: Option<String>,
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Update secret properties
    Update {
        /// Secret name
        name: String,
        /// Vault name
        vault: Option<String>,
        /// New value (if not provided, will prompt)
        value: Option<String>,
        /// Read value from stdin
        #[arg(long)]
        stdin: bool,
        /// Tags for the secret in key=value format
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        tags: Vec<(String, String)>,
        /// Groups for the secret (can be specified multiple times)
        #[arg(short, long)]
        group: Vec<String>,
        /// New name for the secret (rename operation)
        #[arg(long)]
        rename: Option<String>,
        /// Note to attach to the secret
        #[arg(long)]
        note: Option<String>,
        /// Folder path for the secret (e.g., 'app/database', 'config/dev')
        #[arg(long)]
        folder: Option<String>,
        /// Replace existing tags instead of merging
        #[arg(long)]
        replace_tags: bool,
        /// Replace existing groups instead of merging
        #[arg(long)]
        replace_groups: bool,
    },
    /// Permanently delete (purge) a secret
    Purge {
        /// Secret name
        name: String,
        /// Vault name
        vault: Option<String>,
        /// Force purge without confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Restore a deleted secret
    Restore {
        /// Secret name
        name: String,
        /// Vault name
        vault: Option<String>,
    },
    /// Parse connection strings
    Parse {
        /// Connection string to parse
        connection_string: String,
        /// Output format
        #[arg(short, long, default_value = "table")]
        format: String,
    },
    /// Secret-level access management
    Share {
        #[command(subcommand)]
        command: ShareCommands,
    }
}

#[derive(Subcommand)]
pub enum ShareCommands {
    /// Grant access to a secret
    Grant {
        /// Secret name
        secret_name: String,
        /// User email or service principal ID
        user: String,
        /// Vault name
        vault: Option<String>,
        /// Access level (read, write, admin)
        #[arg(short, long, default_value = "read")]
        level: String,
    },
    /// Revoke access to a secret
    Revoke {
        /// Secret name
        secret_name: String,
        /// User email or service principal ID
        user: String,
        /// Vault name
        vault: Option<String>,
    },
    /// List access permissions for a secret
    List {
        /// Secret name
        secret_name: String,
        /// Vault name
        vault: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Setting name
        key: String,
        /// Setting value
        value: String,
    },
    /// Show configuration file path
    Path,
}

#[derive(Subcommand)]
pub enum ContextCommands {
    /// Show current vault context
    Show,
    /// Switch to a vault context
    Use {
        /// Vault name
        vault_name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Make this the global default
        #[arg(long)]
        global: bool,
        /// Set for current directory only
        #[arg(long)]
        local: bool,
    },
    /// List recent vault contexts
    List,
    /// Clear current context
    Clear {
        /// Clear global context
        #[arg(long)]
        global: bool,
    },
}

impl Cli {
    pub async fn execute(self, config: Config) -> Result<()> {
        match self.command {
            Commands::Vault { command } => {
                execute_vault_command(command, config).await
            }
            Commands::Secret { command } => {
                execute_secret_command(command, config).await
            }
            Commands::Config { command } => {
                execute_config_command(command, config).await
            }
            Commands::Context { command } => {
                execute_context_command(command, config).await
            }
            Commands::Init => {
                execute_init_command(config).await
            }
            Commands::Info { vault_name, resource_group, subscription } => {
                execute_info_command(vault_name, resource_group, subscription, config).await
            }
            Commands::Version => {
                execute_version_command().await
            }
        }
    }
}

async fn execute_vault_command(command: VaultCommands, config: Config) -> Result<()> {
    use std::sync::Arc;
    use crate::auth::provider::DefaultAzureCredentialProvider;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::new()
            .map_err(|e| crosstacheError::authentication(format!("Failed to create auth provider: {}", e)))?
    );

    // Create vault manager
    let vault_manager = VaultManager::new(
        auth_provider,
        config.subscription_id.clone(),
        config.no_color,
    );

    match command {
        VaultCommands::Create { name, resource_group, location } => {
            execute_vault_create(&vault_manager, &name, resource_group, location, &config).await?;
        }
        VaultCommands::List { resource_group } => {
            execute_vault_list(&vault_manager, resource_group, &config).await?;
        }
        VaultCommands::Delete { name, resource_group, force } => {
            execute_vault_delete(&vault_manager, &name, resource_group, force, &config).await?;
        }
        VaultCommands::Info { name, resource_group } => {
            execute_vault_info(&vault_manager, &name, resource_group, &config).await?;
        }
        VaultCommands::Restore { name, location } => {
            execute_vault_restore(&vault_manager, &name, &location, &config).await?;
        }
        VaultCommands::Purge { name, location, force } => {
            execute_vault_purge(&vault_manager, &name, &location, force, &config).await?;
        }
        VaultCommands::Export { 
            name, 
            resource_group, 
            output, 
            format, 
            include_values, 
            group 
        } => {
            execute_vault_export(&vault_manager, &name, resource_group, output, &format, include_values, group, &config).await?;
        }
        VaultCommands::Import { 
            name, 
            resource_group, 
            input, 
            format, 
            overwrite, 
            dry_run 
        } => {
            execute_vault_import(&vault_manager, &name, resource_group, input, &format, overwrite, dry_run, &config).await?;
        }
        VaultCommands::Update { 
            name, 
            resource_group, 
            tag, 
            enable_deployment,
            enable_disk_encryption,
            enable_template_deployment,
            enable_purge_protection,
            retention_days
        } => {
            execute_vault_update(&vault_manager, &name, resource_group, tag, enable_deployment, enable_disk_encryption, enable_template_deployment, enable_purge_protection, retention_days, &config).await?;
        }
        VaultCommands::Share { command } => {
            execute_vault_share(&vault_manager, command, &config).await?;
        }
    }
    Ok(())
}

async fn execute_vault_create(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    location: Option<String>,
    config: &Config,
) -> Result<()> {
    // Use defaults from config if not provided
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    let location = location.unwrap_or_else(|| config.default_location.clone());

    println!("Creating vault '{}' in resource group '{}' at location '{}'...", name, resource_group, location);

    let create_request = VaultCreateRequest {
        name: name.to_string(),
        location: location.clone(),
        resource_group: resource_group.clone(),
        subscription_id: config.subscription_id.clone(),
        sku: Some("standard".to_string()),
        enabled_for_deployment: Some(false),
        enabled_for_disk_encryption: Some(false),
        enabled_for_template_deployment: Some(false),
        soft_delete_retention_in_days: Some(90),
        purge_protection: Some(false),
        tags: Some(std::collections::HashMap::from([
            ("created_by".to_string(), "crosstache".to_string()),
            ("created_at".to_string(), chrono::Utc::now().format("%Y-%m-%d").to_string()),
        ])),
        access_policies: None, // Will be set automatically by the manager
    };

    let vault = vault_manager.create_vault_with_setup(
        &name,
        &location,
        &resource_group,
        Some(create_request),
    ).await?;
    
    println!("✅ Successfully created vault '{}'", vault.name);
    println!("   Resource Group: {}", vault.resource_group);
    println!("   Location: {}", vault.location);
    println!("   URI: {}", vault.uri);
    
    Ok(())
}

async fn execute_vault_list(
    vault_manager: &VaultManager,
    resource_group: Option<String>,
    config: &Config,
) -> Result<()> {
    let output_format = if config.output_json {
        OutputFormat::Json
    } else {
        OutputFormat::Table
    };

    vault_manager.list_vaults_formatted(
        Some(&config.subscription_id),
        resource_group.as_deref(),
        output_format,
    ).await?;
    
    Ok(())
}

async fn execute_vault_delete(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    
    vault_manager.delete_vault_safe(name, &resource_group, force).await?;
    
    Ok(())
}

async fn execute_vault_info(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    
    if config.output_json {
        let vault = vault_manager.get_vault_properties(name, &resource_group).await?;
        let json_output = serde_json::to_string_pretty(&vault)
            .map_err(|e| crosstacheError::serialization(format!("Failed to serialize vault info: {}", e)))?;
        println!("{}", json_output);
    } else {
        let _vault = vault_manager.get_vault_info(name, &resource_group).await?;
        // Display will be handled by the vault manager
    }
    
    Ok(())
}

async fn execute_secret_command(command: SecretCommands, config: Config) -> Result<()> {
    use std::sync::Arc;
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::new()
            .map_err(|e| crosstacheError::authentication(format!("Failed to create auth provider: {}", e)))?
    );

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    match command {
        SecretCommands::Set { name, vault, stdin, note, folder } => {
            execute_secret_set(&secret_manager, &name, vault, stdin, note, folder, &config).await?;
        }
        SecretCommands::Get { name, vault, raw } => {
            execute_secret_get(&secret_manager, &name, vault, raw, &config).await?;
        }
        SecretCommands::List { vault, group, all } => {
            execute_secret_list(&secret_manager, vault, group, all, &config).await?;
        }
        SecretCommands::Delete { name, vault, force } => {
            execute_secret_delete(&secret_manager, &name, vault, force, &config).await?;
        }
        SecretCommands::Update { 
            name, 
            vault, 
            value, 
            stdin, 
            tags, 
            group, 
            rename, 
            note, 
            folder,
            replace_tags, 
            replace_groups 
        } => {
            execute_secret_update(&secret_manager, &name, vault, value, stdin, tags, group, rename, note, folder, replace_tags, replace_groups, &config).await?;
        }
        SecretCommands::Purge { name, vault, force } => {
            execute_secret_purge(&secret_manager, &name, vault, force, &config).await?;
        }
        SecretCommands::Restore { name, vault } => {
            execute_secret_restore(&secret_manager, &name, vault, &config).await?;
        }
        SecretCommands::Parse { connection_string, format } => {
            execute_secret_parse(&secret_manager, &connection_string, &format, &config).await?;
        }
        SecretCommands::Share { command } => {
            execute_secret_share(&secret_manager, command, &config).await?;
        }
    }
    Ok(())
}

async fn execute_config_command(command: ConfigCommands, config: Config) -> Result<()> {
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

async fn execute_context_command(command: ContextCommands, config: Config) -> Result<()> {
    match command {
        ContextCommands::Show => {
            execute_context_show(&config).await?;
        }
        ContextCommands::Use { vault_name, resource_group, global, local } => {
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

async fn execute_init_command(config: Config) -> Result<()> {
    println!("TODO: Initialize default configuration");
    Ok(())
}

async fn execute_info_command(
    vault_name: Option<String>,
    resource_group: Option<String>,
    subscription: Option<String>,
    config: Config,
) -> Result<()> {
    println!("TODO: Show info for vault {:?}", vault_name);
    Ok(())
}

async fn execute_version_command() -> Result<()> {
    let build_info = get_build_info();
    
    println!("crosstache Rust CLI");
    println!("===================");
    println!("Version:      {}", build_info.version);
    println!("Git Hash:     {}", build_info.git_hash);
    println!("Git Branch:   {}", build_info.git_branch);
    println!("Built:        {}", build_info.build_time);
    
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
            key: "cache_ttl".to_string(),
            value: format!("{}s", config.cache_ttl.as_secs()),
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
    
    if config.output_json {
        let json_output = serde_json::to_string_pretty(config)
            .map_err(|e| crosstacheError::serialization(format!("Failed to serialize config: {}", e)))?;
        println!("{}", json_output);
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
        "cache_ttl" => {
            let seconds = value.parse::<u64>()
                .map_err(|_| crosstacheError::config(format!("Invalid value for cache_ttl: {}", value)))?;
            config.cache_ttl = std::time::Duration::from_secs(seconds);
        }
        "output_json" => {
            config.output_json = value.to_lowercase() == "true" || value == "1";
        }
        "no_color" => {
            config.no_color = value.to_lowercase() == "true" || value == "1";
        }
        _ => {
            return Err(crosstacheError::config(format!("Unknown configuration key: {}", key)));
        }
    }
    
    config.save().await?;
    println!("✅ Configuration updated: {} = {}", key, value);
    
    Ok(())
}

async fn execute_secret_set(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    config: &Config,
) -> Result<()> {
    use std::io::{self, Read};
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get secret value
    let value = if stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer.trim().to_string()
    } else {
        // Use rpassword for secure input
        rpassword::prompt_password(format!("Enter value for secret '{}': ", name))?
    };

    if value.is_empty() {
        return Err(crosstacheError::config("Secret value cannot be empty"));
    }

    // Create secret request with note and/or folder if provided
    let secret_request = if note.is_some() || folder.is_some() {
        Some(crate::secret::manager::SecretRequest {
            name: name.to_string(),
            value: value.clone(),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note,
            folder,
        })
    } else {
        None
    };

    // Set the secret
    let secret = secret_manager.set_secret_safe(&vault_name, name, &value, secret_request).await?;
    
    println!("✅ Successfully set secret '{}'", secret.original_name);
    println!("   Vault: {}", vault_name);
    println!("   Version: {}", secret.version);
    
    Ok(())
}

async fn execute_secret_get(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    raw: bool,
    config: &Config,
) -> Result<()> {
    use clipboard::{ClipboardProvider, ClipboardContext};
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get the secret
    let secret = secret_manager.get_secret_safe(&vault_name, name, true, true).await?;
    
    if raw {
        // Raw output - print the value
        if let Some(value) = secret.value {
            print!("{}", value);
        }
    } else {
        // Default behavior - copy to clipboard
        if let Some(ref value) = secret.value {
            match ClipboardContext::new() {
                Ok(mut ctx) => {
                    match ctx.set_contents(value.clone()) {
                        Ok(_) => {
                            println!("✅ Secret '{}' copied to clipboard", name);
                        }
                        Err(e) => {
                            eprintln!("⚠️  Failed to copy to clipboard: {}", e);
                            eprintln!("Secret value: {}", value);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("⚠️  Failed to access clipboard: {}", e);
                    eprintln!("Secret value: {}", value);
                }
            }
        } else {
            println!("⚠️  Secret '{}' has no value", name);
        }
    }
    
    Ok(())
}

async fn execute_secret_list(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    group: Option<String>,
    show_all: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    let output_format = if config.output_json {
        crate::utils::format::OutputFormat::Json
    } else {
        crate::utils::format::OutputFormat::Table
    };

    secret_manager.list_secrets_formatted(&vault_name, group.as_deref(), output_format, false, show_all).await?;
    
    Ok(())
}

async fn execute_secret_delete(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Confirmation unless forced
    if !force {
        let confirm = rpassword::prompt_password(format!(
            "Are you sure you want to delete secret '{}' from vault '{}'? (y/N): ",
            name, vault_name
        ))?;
        
        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Delete operation cancelled.");
            return Ok(());
        }
    }

    secret_manager.delete_secret_safe(&vault_name, name, force).await?;
    println!("✅ Successfully deleted secret '{}'", name);
    
    Ok(())
}

async fn execute_secret_update(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    value: Option<String>,
    stdin: bool,
    tags: Vec<(String, String)>,
    groups: Vec<String>,
    rename: Option<String>,
    note: Option<String>,
    folder: Option<String>,
    replace_tags: bool,
    replace_groups: bool,
    config: &Config,
) -> Result<()> {
    use std::io::{self, Read};
    use std::collections::HashMap;
    use crate::secret::manager::SecretUpdateRequest;
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get new value if explicitly provided (but don't prompt)
    let new_value = if let Some(v) = value {
        // Validate provided value
        if v.is_empty() {
            return Err(crosstacheError::config("Secret value cannot be empty"));
        }
        Some(v)
    } else if stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        let trimmed = buffer.trim().to_string();
        if trimmed.is_empty() {
            return Err(crosstacheError::config("Secret value cannot be empty"));
        }
        Some(trimmed)
    } else {
        None // Don't update value, just metadata
    };

    // Ensure at least one update is specified
    if new_value.is_none() && tags.is_empty() && groups.is_empty() && rename.is_none() && note.is_none() && folder.is_none() {
        return Err(crosstacheError::invalid_argument(
            "No updates specified. Use 'secret update' to modify metadata (groups, tags, folder, note) or rename secrets. Use 'secret set' to update secret values."
        ));
    }

    // Convert tags vector to HashMap
    let tags_map = if !tags.is_empty() {
        Some(tags.into_iter().collect::<HashMap<String, String>>())
    } else {
        None
    };

    // Convert groups vector to Option
    let groups_vec = if !groups.is_empty() {
        Some(groups)
    } else {
        None
    };

    // Validate rename if provided
    if let Some(ref new_name) = rename {
        if new_name.is_empty() {
            return Err(crosstacheError::invalid_argument("New secret name cannot be empty"));
        }
        if new_name == name {
            return Err(crosstacheError::invalid_argument("New secret name must be different from current name"));
        }
    }

    // Create update request with enhanced parameters
    let update_request = SecretUpdateRequest {
        name: name.to_string(),
        new_name: rename.clone(),
        value: new_value.clone(),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: tags_map,
        groups: groups_vec,
        note: note.clone(),
        folder: folder.clone(),
        replace_tags,
        replace_groups,
    };

    // Show update summary
    println!("Updating secret '{}'...", name);
    if let Some(ref new_name) = rename {
        println!("  → Renaming to: {}", new_name);
    }
    if new_value.is_some() {
        println!("  → Updating value");
    }
    if !update_request.tags.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
        let action = if replace_tags { "Replacing" } else { "Merging" };
        println!("  → {} tags: {}", action, update_request.tags.as_ref().unwrap().len());
    }
    if !update_request.groups.as_ref().map(|g| g.is_empty()).unwrap_or(true) {
        let action = if replace_groups { "Replacing" } else { "Adding to" };
        println!("  → {} groups: {:?}", action, update_request.groups.as_ref().unwrap());
    }
    if let Some(ref note_text) = note {
        println!("  → Adding note: {}", note_text);
    }
    if let Some(ref folder_path) = folder {
        println!("  → Setting folder: {}", folder_path);
    }

    // Perform enhanced secret update
    let secret = secret_manager.update_secret_enhanced(&vault_name, &update_request).await?;
    
    println!("✅ Successfully updated secret '{}'", secret.original_name);
    println!("   Vault: {}", vault_name);
    println!("   Version: {}", secret.version);
    
    if let Some(ref new_name) = rename {
        println!("   New Name: {}", new_name);
    }
    
    Ok(())
}

async fn execute_secret_purge(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Confirmation unless forced
    if !force {
        let confirm = rpassword::prompt_password(format!(
            "Are you sure you want to PERMANENTLY DELETE secret '{}' from vault '{}'? This cannot be undone! (y/N): ",
            name, vault_name
        ))?;
        
        if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
            println!("Purge operation cancelled.");
            return Ok(());
        }
    }

    // Permanently purge the secret using the secret manager
    secret_manager.purge_secret_safe(&vault_name, name, force).await?;
    println!("✅ Successfully purged secret '{}'", name);
    
    Ok(())
}

async fn execute_secret_restore(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    println!("Restoring deleted secret '{}'...", name);
    
    // Restore the secret using the secret manager
    let restored_secret = secret_manager.restore_secret_safe(&vault_name, name).await?;
    
    println!("✅ Successfully restored secret '{}'", restored_secret.original_name);
    println!("   Vault: {}", vault_name);
    println!("   Version: {}", restored_secret.version);
    println!("   Enabled: {}", restored_secret.enabled);
    println!("   Created: {}", restored_secret.created_on);
    println!("   Updated: {}", restored_secret.updated_on);
    
    if !restored_secret.tags.is_empty() {
        println!("   Tags: {}", restored_secret.tags.len());
    }
    
    Ok(())
}

async fn execute_secret_parse(
    secret_manager: &crate::secret::manager::SecretManager,
    connection_string: &str,
    format: &str,
    config: &Config,
) -> Result<()> {
    let components = secret_manager.parse_connection_string(connection_string).await?;
    
    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = serde_json::to_string_pretty(&components)
                .map_err(|e| crosstacheError::serialization(format!("Failed to serialize components: {}", e)))?;
            println!("{}", json_output);
        }
        "table" | _ => {
            if components.is_empty() {
                println!("No components found in connection string");
            } else {
                use crate::utils::format::format_table;
                use tabled::Table;
                
                let table = Table::new(&components);
                println!("{}", format_table(table, config.no_color));
            }
        }
    }
    
    Ok(())
}

async fn execute_secret_share(
    secret_manager: &crate::secret::manager::SecretManager,
    command: ShareCommands,
    config: &Config,
) -> Result<()> {
    match command {
        ShareCommands::Grant { secret_name, user, vault, level } => {
            let vault_name = vault.unwrap_or_else(|| config.default_vault.clone());
            if vault_name.is_empty() {
                return Err(crosstacheError::config("Vault name is required. Set default_vault in config or provide --vault"));
            }
            
            println!("TODO: Grant {} access to secret '{}' for user '{}' in vault '{}'", 
                     level, secret_name, user, vault_name);
        }
        ShareCommands::Revoke { secret_name, user, vault } => {
            let vault_name = vault.unwrap_or_else(|| config.default_vault.clone());
            if vault_name.is_empty() {
                return Err(crosstacheError::config("Vault name is required. Set default_vault in config or provide --vault"));
            }
            
            println!("TODO: Revoke access to secret '{}' for user '{}' in vault '{}'", 
                     secret_name, user, vault_name);
        }
        ShareCommands::List { secret_name, vault } => {
            let vault_name = vault.unwrap_or_else(|| config.default_vault.clone());
            if vault_name.is_empty() {
                return Err(crosstacheError::config("Vault name is required. Set default_vault in config or provide --vault"));
            }
            
            println!("TODO: List access permissions for secret '{}' in vault '{}'", 
                     secret_name, vault_name);
        }
    }
    
    Ok(())
}

async fn execute_vault_restore(
    vault_manager: &VaultManager,
    name: &str,
    location: &str,
    config: &Config,
) -> Result<()> {
    vault_manager.restore_vault(name, location).await?;
    Ok(())
}

async fn execute_vault_purge(
    vault_manager: &VaultManager,
    name: &str,
    location: &str,
    force: bool,
    config: &Config,
) -> Result<()> {
    vault_manager.purge_vault_permanent(name, location, force).await?;
    Ok(())
}

async fn execute_vault_export(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    output: Option<String>,
    format: &str,
    include_values: bool,
    group: Option<String>,
    config: &Config,
) -> Result<()> {
    use std::fs::File;
    use std::io::Write;
    use crate::secret::manager::SecretManager;
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use std::sync::Arc;

    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    
    // Create secret manager to get secrets from vault
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::new()
            .map_err(|e| crosstacheError::authentication(format!("Failed to create auth provider: {}", e)))?
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    
    // Get all secrets from vault (including disabled ones for export)
    let secrets = secret_manager.list_secrets_formatted(
        name,
        group.as_deref(),
        OutputFormat::Json,
        false,
        true, // show_all = true for export
    ).await?;
    
    // Prepare export data based on format
    let export_data = match format.to_lowercase().as_str() {
        "json" => {
            let mut export_json = serde_json::Map::new();
            export_json.insert("vault".to_string(), serde_json::Value::String(name.to_string()));
            export_json.insert("exported_at".to_string(), serde_json::Value::String(chrono::Utc::now().to_rfc3339()));
            
            let mut secrets_json = Vec::new();
            for secret in &secrets {
                let mut secret_data = serde_json::Map::new();
                secret_data.insert("name".to_string(), serde_json::Value::String(secret.original_name.clone()));
                secret_data.insert("enabled".to_string(), serde_json::Value::Bool(secret.enabled));
                secret_data.insert("content_type".to_string(), serde_json::Value::String(secret.content_type.clone()));
                
                if include_values {
                    // Get actual secret value
                    match secret_manager.get_secret_safe(name, &secret.original_name, true, true).await {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                secret_data.insert("value".to_string(), serde_json::Value::String(value));
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: Failed to get value for secret '{}': {}", secret.original_name, e);
                        }
                    }
                }
                
                secrets_json.push(serde_json::Value::Object(secret_data));
            }
            export_json.insert("secrets".to_string(), serde_json::Value::Array(secrets_json));
            
            serde_json::to_string_pretty(&export_json)
                .map_err(|e| crosstacheError::serialization(format!("Failed to serialize export data: {}", e)))?
        }
        "env" => {
            let mut env_lines = Vec::new();
            env_lines.push(format!("# Exported from vault '{}' on {}", name, chrono::Utc::now().to_rfc3339()));
            
            for secret in &secrets {
                if include_values {
                    match secret_manager.get_secret_safe(name, &secret.original_name, true, true).await {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                let env_name = secret.original_name.to_uppercase().replace("-", "_").replace(".", "_");
                                env_lines.push(format!("{}={}", env_name, value));
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: Failed to get value for secret '{}': {}", secret.original_name, e);
                        }
                    }
                } else {
                    let env_name = secret.original_name.to_uppercase().replace("-", "_").replace(".", "_");
                    env_lines.push(format!("# {}", env_name));
                }
            }
            
            env_lines.join("\n")
        }
        "txt" => {
            let mut txt_lines = Vec::new();
            txt_lines.push(format!("Vault: {}", name));
            txt_lines.push(format!("Exported: {}", chrono::Utc::now().to_rfc3339()));
            txt_lines.push("".to_string());
            
            for secret in &secrets {
                txt_lines.push(format!("Secret: {}", secret.original_name));
                txt_lines.push(format!("  Enabled: {}", secret.enabled));
                txt_lines.push(format!("  Content Type: {}", secret.content_type));
                txt_lines.push(format!("  Updated: {}", secret.updated_on));
                
                if include_values {
                    match secret_manager.get_secret_safe(name, &secret.original_name, true, true).await {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                txt_lines.push(format!("  Value: {}", value));
                            }
                        }
                        Err(e) => {
                            eprintln!("Warning: Failed to get value for secret '{}': {}", secret.original_name, e);
                        }
                    }
                }
                txt_lines.push("".to_string());
            }
            
            txt_lines.join("\n")
        }
        _ => {
            return Err(crosstacheError::invalid_argument(format!("Unsupported export format: {}", format)));
        }
    };
    
    // Write to output
    match output {
        Some(file_path) => {
            let mut file = File::create(&file_path)
                .map_err(|e| crosstacheError::unknown(format!("Failed to create output file: {}", e)))?;
            file.write_all(export_data.as_bytes())
                .map_err(|e| crosstacheError::unknown(format!("Failed to write to output file: {}", e)))?;
            println!("Exported {} secrets to {}", secrets.len(), file_path);
        }
        None => {
            println!("{}", export_data);
        }
    }
    
    Ok(())
}

async fn execute_vault_import(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    input: Option<String>,
    format: &str,
    overwrite: bool,
    dry_run: bool,
    config: &Config,
) -> Result<()> {
    use std::fs;
    use std::io::{self, Read};
    use crate::secret::manager::{SecretManager, SecretRequest};
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use std::sync::Arc;

    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    
    // Read import data
    let import_data = match input {
        Some(file_path) => {
            fs::read_to_string(file_path)
                .map_err(|e| crosstacheError::unknown(format!("Failed to read input file: {}", e)))?
        }
        None => {
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)
                .map_err(|e| crosstacheError::unknown(format!("Failed to read from stdin: {}", e)))?;
            buffer
        }
    };
    
    // Parse import data based on format
    let secrets_to_import = match format.to_lowercase().as_str() {
        "json" => {
            let json_data: serde_json::Value = serde_json::from_str(&import_data)
                .map_err(|e| crosstacheError::serialization(format!("Failed to parse JSON: {}", e)))?;
            
            let secrets_array = json_data.get("secrets")
                .and_then(|s| s.as_array())
                .ok_or_else(|| crosstacheError::serialization("Missing 'secrets' array in JSON"))?;
            
            let mut secrets = Vec::new();
            for secret_value in secrets_array {
                let secret_obj = secret_value.as_object()
                    .ok_or_else(|| crosstacheError::serialization("Invalid secret object in JSON"))?;
                
                let name = secret_obj.get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| crosstacheError::serialization("Missing secret name"))?;
                
                let value = secret_obj.get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| crosstacheError::serialization("Missing secret value"))?;
                
                let content_type = secret_obj.get("content_type")
                    .and_then(|ct| ct.as_str())
                    .map(|s| s.to_string());
                
                let enabled = secret_obj.get("enabled")
                    .and_then(|e| e.as_bool());
                
                secrets.push(SecretRequest {
                    name: name.to_string(),
                    value: value.to_string(),
                    content_type,
                    enabled,
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: None,
                    folder: None,
                });
            }
            
            secrets
        }
        "env" => {
            let mut secrets = Vec::new();
            for line in import_data.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                
                if let Some(pos) = line.find('=') {
                    let key = line[..pos].trim().to_lowercase().replace("_", "-");
                    let value = line[pos + 1..].trim();
                    
                    secrets.push(SecretRequest {
                        name: key,
                        value: value.to_string(),
                        content_type: Some("text/plain".to_string()),
                        enabled: Some(true),
                        expires_on: None,
                        not_before: None,
                        tags: None,
                        groups: None,
                        note: None,
                        folder: None,
                    });
                }
            }
            
            secrets
        }
        _ => {
            return Err(crosstacheError::invalid_argument(format!("Unsupported import format: {}", format)));
        }
    };
    
    if dry_run {
        println!("Dry run: Would import {} secrets to vault '{}':", secrets_to_import.len(), name);
        for secret in &secrets_to_import {
            println!("  - {}", secret.name);
        }
        return Ok(());
    }
    
    // Create secret manager to import secrets
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::new()
            .map_err(|e| crosstacheError::authentication(format!("Failed to create auth provider: {}", e)))?
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    
    let mut imported_count = 0;
    let mut skipped_count = 0;
    
    for secret_request in secrets_to_import {
        let secret_name = secret_request.name.clone();
        let secret_value = secret_request.value.clone();
        
        // Check if secret exists if not overwriting
        if !overwrite {
            match secret_manager.get_secret_safe(name, &secret_name, false, true).await {
                Ok(_) => {
                    println!("Skipping existing secret: {}", secret_name);
                    skipped_count += 1;
                    continue;
                }
                Err(_) => {
                    // Secret doesn't exist, proceed with import
                }
            }
        }
        
        match secret_manager.set_secret_safe(name, &secret_name, &secret_value, Some(secret_request)).await {
            Ok(_) => {
                println!("Imported secret: {}", secret_name);
                imported_count += 1;
            }
            Err(e) => {
                eprintln!("Failed to import secret '{}': {}", secret_name, e);
            }
        }
    }
    
    println!("Import completed: {} imported, {} skipped", imported_count, skipped_count);
    
    Ok(())
}

async fn execute_vault_update(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    tags: Vec<(String, String)>,
    enable_deployment: Option<bool>,
    enable_disk_encryption: Option<bool>,
    enable_template_deployment: Option<bool>,
    enable_purge_protection: Option<bool>,
    retention_days: Option<i32>,
    config: &Config,
) -> Result<()> {
    use std::collections::HashMap;
    use crate::vault::models::VaultUpdateRequest;

    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
    
    // Convert tags vector to HashMap
    let tags_map = if !tags.is_empty() {
        Some(tags.into_iter().collect::<HashMap<String, String>>())
    } else {
        None
    };
    
    let update_request = VaultUpdateRequest {
        enabled_for_deployment: enable_deployment,
        enabled_for_disk_encryption: enable_disk_encryption,
        enabled_for_template_deployment: enable_template_deployment,
        soft_delete_retention_in_days: retention_days,
        purge_protection: enable_purge_protection,
        tags: tags_map,
        access_policies: None, // Don't modify access policies in update
    };
    
    // Note: This would need proper implementation in vault manager
    println!("Updating vault '{}' in resource group '{}'...", name, resource_group);
    println!("Update request: {:?}", update_request);
    println!("TODO: Implement vault update functionality");
    
    Ok(())
}

async fn execute_vault_share(
    vault_manager: &VaultManager,
    command: VaultShareCommands,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::AccessLevel;

    match command {
        VaultShareCommands::Grant { vault_name, user, resource_group, level } => {
            let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
            
            let access_level = match level.to_lowercase().as_str() {
                "reader" | "read" => AccessLevel::Reader,
                "contributor" | "write" => AccessLevel::Contributor,
                "admin" | "administrator" => AccessLevel::Admin,
                _ => return Err(crosstacheError::invalid_argument(format!("Invalid access level: {}", level))),
            };
            
            vault_manager.grant_vault_access(&vault_name, &resource_group, &user, access_level, Some(&user)).await?;
        }
        VaultShareCommands::Revoke { vault_name, user, resource_group } => {
            let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
            
            vault_manager.revoke_vault_access(&vault_name, &resource_group, &user, Some(&user)).await?;
        }
        VaultShareCommands::List { vault_name, resource_group, format } => {
            let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());
            
            let output_format = match format.to_lowercase().as_str() {
                "json" => OutputFormat::Json,
                "table" | _ => OutputFormat::Table,
            };
            
            vault_manager.list_vault_access(&vault_name, &resource_group, output_format).await?;
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
            println!("  Resource Group: {}", rg);
        }
        if let Some(ref sub) = context.subscription_id {
            println!("  Subscription: {}", sub);
        }
        println!("  Last Used: {}", context.last_used.format("%Y-%m-%d %H:%M:%S UTC"));
        println!("  Usage Count: {}", context.usage_count);
        
        // Show context source
        println!("  Scope: {}", context_manager.scope_description());
    } else {
        println!("No vault context set");
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
        ContextManager::load().await.unwrap_or_else(|_| {
            ContextManager::new_global().unwrap()
        })
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
    println!("✅ Switched to vault '{}' ({} context)", vault_name, scope);
    
    if let Some(ref rg) = context_manager.current_resource_group() {
        println!("   Resource Group: {}", rg);
    }
    
    Ok(())
}

async fn execute_context_list(_config: &Config) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};
    
    let context_manager = ContextManager::load().await.unwrap_or_default();
    
    if context_manager.recent.is_empty() && context_manager.current.is_none() {
        println!("No vault contexts found");
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
        println!("No active context to clear");
        return Ok(());
    }
    
    let vault_name = context_manager.current_vault().unwrap().to_string();
    context_manager.clear_context().await?;
    
    let scope = if global { "global" } else { context_manager.scope_description() };
    println!("✅ Cleared vault context for '{}' ({} scope)", vault_name, scope);
    
    Ok(())
}

/// Parse a single key-value pair
fn parse_key_val<T, U>(s: &str) -> std::result::Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}