//! Configuration initialization logic for interactive setup
//!
//! This module handles the step-by-step initialization process for new users,
//! including Azure environment detection, configuration building, and vault creation.

use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
use crate::config::settings::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::azure_detect::{AzureDetector, AzureEnvironment, AzureSubscription};
use crate::utils::interactive::{InteractivePrompt, ProgressIndicator, SetupHelper};
use crate::vault::manager::VaultManager;
use crate::vault::models::VaultCreateRequest;
use std::sync::Arc;

/// Interactive configuration initialization
pub struct ConfigInitializer {
    prompt: InteractivePrompt,
}

/// Configuration data collected during initialization
#[derive(Debug, Clone)]
pub struct InitConfig {
    pub subscription_id: String,
    pub tenant_id: String,
    pub default_resource_group: String,
    pub default_location: String,
    pub default_vault: Option<String>,
    pub create_test_vault: bool,
    pub storage_account_name: String,
    pub blob_container_name: String,
    pub create_storage_account: bool,
}

impl ConfigInitializer {
    /// Create a new configuration initializer
    pub fn new() -> Self {
        Self {
            prompt: InteractivePrompt::new(),
        }
    }

    /// Run the complete interactive initialization process
    pub async fn run_interactive_setup(&self) -> Result<Config> {
        self.prompt.welcome()?;

        // Step 1: Detect Azure environment
        self.prompt.step(1, 6, "Detecting Azure Environment")?;
        let azure_env = self.detect_azure_environment().await?;

        // Step 2: Configure subscription
        self.prompt.step(2, 6, "Configuring Subscription")?;
        let subscription = self.configure_subscription(&azure_env).await?;

        // Step 3: Configure resource group
        self.prompt.step(3, 6, "Configuring Resource Group")?;
        let resource_group = self.configure_resource_group(&subscription).await?;

        // Step 4: Configure location
        self.prompt.step(4, 6, "Configuring Default Location")?;
        let location = self.configure_location(&subscription).await?;

        // Step 5: Configure blob storage
        self.prompt.step(5, 6, "Configuring Blob Storage")?;
        let (storage_account, container_name, blob_storage_configured) = self.configure_blob_storage(&subscription, &resource_group, &location).await?;

        // Step 6: Optional vault creation
        self.prompt.step(6, 6, "Optional Test Vault Creation")?;
        let vault_config = self.configure_vault_creation(&subscription, &resource_group, &location).await?;

        // Build the final configuration
        let init_config = InitConfig {
            subscription_id: subscription.id,
            tenant_id: subscription.tenant_id,
            default_resource_group: resource_group,
            default_location: location,
            default_vault: vault_config.clone(),
            create_test_vault: vault_config.is_some(),
            storage_account_name: storage_account,
            blob_container_name: container_name,
            create_storage_account: blob_storage_configured,
        };

        // Create and save the configuration
        let config = self.build_config(init_config).await?;
        self.save_config(&config).await?;

        self.prompt.success("Setup completed successfully!")?;
        self.prompt.info("You can now start using crosstache with your configured defaults.")?;

        Ok(config)
    }

    /// Detect Azure environment and handle issues
    async fn detect_azure_environment(&self) -> Result<AzureEnvironment> {
        let progress = ProgressIndicator::new("Detecting Azure CLI and environment...");
        
        let azure_env = AzureDetector::detect_environment().await?;
        
        if !azure_env.is_ready() {
            progress.finish_error("Azure environment not ready");
            self.prompt.error(&azure_env.get_status_message())?;
            
            let instructions = azure_env.get_setup_instructions();
            if !instructions.is_empty() {
                self.prompt.info("Please complete the following steps:")?;
                for instruction in instructions {
                    println!("  • {instruction}");
                }
                return Err(CrosstacheError::config(
                    "Azure environment not ready. Please complete the setup steps above and run 'xv init' again."
                ));
            }
        }

        progress.finish_success(&format!(
            "Found Azure CLI v{} with {} subscription(s)",
            azure_env.cli_version.as_deref().unwrap_or("unknown"),
            azure_env.subscriptions.len()
        ));

        if let Some(current) = &azure_env.current_subscription {
            self.prompt.info(&format!("Current subscription: {} ({})", current.name, current.id))?;
        }

        Ok(azure_env)
    }

    /// Configure Azure subscription
    async fn configure_subscription(&self, azure_env: &AzureEnvironment) -> Result<AzureSubscription> {
        if azure_env.subscriptions.len() == 1 {
            let subscription = &azure_env.subscriptions[0];
            let use_default = self.prompt.confirm(
                &format!("Use subscription '{}' ({})?", subscription.name, subscription.id),
                true,
            )?;

            if use_default {
                return Ok(subscription.clone());
            }
        }

        if azure_env.subscriptions.len() > 1 {
            self.prompt.info("Multiple subscriptions available:")?;
            
            let subscription_options: Vec<String> = azure_env.subscriptions
                .iter()
                .map(|s| format!("{} ({})", s.name, s.id))
                .collect();

            let default_index = azure_env.current_subscription.as_ref()
                .and_then(|current| {
                    azure_env.subscriptions.iter().position(|s| s.id == current.id)
                });

            let selected_index = self.prompt.select(
                "Select a subscription",
                &subscription_options,
                default_index,
            )?;

            return Ok(azure_env.subscriptions[selected_index].clone());
        }

        // Manual entry if needed
        let subscription_id = self.prompt.input_text_validated(
            "Enter subscription ID",
            None,
            SetupHelper::validate_subscription_id,
        )?;

        // Create a basic subscription object
        Ok(AzureSubscription {
            id: subscription_id,
            name: "Manual Entry".to_string(),
            tenant_id: azure_env.tenant_info.as_ref()
                .map(|t| t.id.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            is_default: false,
            state: "Unknown".to_string(),
        })
    }

    /// Configure resource group
    async fn configure_resource_group(&self, subscription: &AzureSubscription) -> Result<String> {
        let progress = ProgressIndicator::new("Loading resource groups...");
        
        // Try to get existing resource groups
        let existing_groups = AzureDetector::get_resource_groups(&subscription.id).await
            .unwrap_or_default();
        
        progress.finish_clear();

        if !existing_groups.is_empty() {
            self.prompt.info(&format!("Found {} existing resource group(s)", existing_groups.len()))?;
            
            let use_existing = self.prompt.confirm(
                "Use an existing resource group?",
                true,
            )?;

            if use_existing {
                let selected_index = self.prompt.select(
                    "Select a resource group",
                    &existing_groups,
                    None,
                )?;
                return Ok(existing_groups[selected_index].clone());
            }
        }

        // Create new resource group
        let default_name = SetupHelper::generate_default_resource_group();
        let resource_group_name = self.prompt.input_text_validated(
            "Enter resource group name",
            Some(&default_name),
            SetupHelper::validate_resource_group_name,
        )?;

        // Check if it exists
        let exists = AzureDetector::resource_group_exists(&subscription.id, &resource_group_name).await
            .unwrap_or(false);

        if !exists {
            let create_rg = self.prompt.confirm(
                &format!("Resource group '{resource_group_name}' doesn't exist. Create it?"),
                true,
            )?;

            if create_rg {
                // We'll create it when we know the location
                self.prompt.info("Resource group will be created with the selected location.")?;
            }
        }

        Ok(resource_group_name)
    }

    /// Configure default location
    async fn configure_location(&self, subscription: &AzureSubscription) -> Result<String> {
        let progress = ProgressIndicator::new("Loading available locations...");
        
        let locations = AzureDetector::get_locations(&subscription.id).await
            .unwrap_or_else(|_| vec![
                "eastus".to_string(),
                "westus2".to_string(),
                "centralus".to_string(),
                "northeurope".to_string(),
                "westeurope".to_string(),
            ]);
        
        progress.finish_clear();

        // Suggest a good default location
        let default_location = locations.iter()
            .find(|&loc| loc == "eastus" || loc == "westus2")
            .unwrap_or(&locations[0]);

        let default_index = locations.iter().position(|loc| loc == default_location);

        let selected_index = self.prompt.select(
            "Select default location",
            &locations,
            default_index,
        )?;

        Ok(locations[selected_index].clone())
    }

    /// Configure blob storage during initialization
    async fn configure_blob_storage(
        &self,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<(String, String, bool)> {
        let create_storage = self.prompt.confirm(
            "Configure blob storage for file operations?",
            true,
        )?;

        if !create_storage {
            return Ok((String::new(), String::new(), false));
        }

        let progress = ProgressIndicator::new("Loading existing storage accounts...");
        
        // Try to get existing storage accounts in the resource group
        let existing_accounts = AzureDetector::get_storage_accounts(&subscription.id, resource_group).await
            .unwrap_or_default();
        
        progress.finish_clear();

        let (storage_name, create_new_storage) = if !existing_accounts.is_empty() {
            self.prompt.info(&format!("Found {} existing storage account(s) in resource group '{}'", existing_accounts.len(), resource_group))?;
            
            let use_existing = self.prompt.confirm(
                "Use an existing storage account?",
                true,
            )?;

            if use_existing {
                let selected_index = self.prompt.select(
                    "Select a storage account",
                    &existing_accounts,
                    None,
                )?;
                (existing_accounts[selected_index].clone(), false)
            } else {
                // Create new storage account
                let default_storage_name = SetupHelper::generate_storage_account_name();
                let storage_name = self.prompt.input_text_validated(
                    "Enter new storage account name",
                    Some(&default_storage_name),
                    SetupHelper::validate_storage_account_name,
                )?;
                (storage_name, true)
            }
        } else {
            // No existing accounts, create new one
            let default_storage_name = SetupHelper::generate_storage_account_name();
            let storage_name = self.prompt.input_text_validated(
                "Enter storage account name",
                Some(&default_storage_name),
                SetupHelper::validate_storage_account_name,
            )?;
            (storage_name, true)
        };

        let container_name = self.prompt.input_text_validated(
            "Enter container name for files",
            Some("crosstache-files"),
            SetupHelper::validate_container_name,
        )?;

        // Create storage account if needed
        if create_new_storage {
            self.create_storage_account(&storage_name, subscription, resource_group, location).await?;
        } else {
            // If using existing storage account, just create the container
            self.create_blob_container(&storage_name, &container_name, subscription).await?;
        }

        Ok((storage_name, container_name, true))
    }

    /// Create storage account and container
    async fn create_storage_account(
        &self,
        storage_name: &str,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<()> {
        let progress = ProgressIndicator::new("Creating storage account...");

        // For now, we'll use Azure CLI to create the storage account
        // TODO: Implement proper Azure Management API integration
        progress.set_message("Creating storage account...");
        
        // Create storage account using Azure CLI with timeout
        let create_storage_cmd = tokio::time::timeout(
            std::time::Duration::from_secs(180), // 3 minute timeout for storage account creation
            tokio::process::Command::new("az")
                .args([
                    "storage", "account", "create",
                    "--name", storage_name,
                    "--resource-group", resource_group,
                    "--location", location,
                    "--sku", "Standard_LRS",
                    "--kind", "StorageV2",
                    "--access-tier", "Hot",
                    "--allow-blob-public-access", "false",
                    "--min-tls-version", "TLS1_2",
                    "--subscription", &subscription.id,
                ])
                .output()
        ).await;

        let create_storage_cmd = match create_storage_cmd {
            Ok(result) => result?,
            Err(_) => {
                return Err(CrosstacheError::azure_api(
                    "Storage account creation timed out after 3 minutes. Please check your Azure CLI authentication and network connection.".to_string()
                ));
            }
        };

        if !create_storage_cmd.status.success() {
            let error_msg = String::from_utf8_lossy(&create_storage_cmd.stderr);
            return Err(CrosstacheError::azure_api(format!(
                "Failed to create storage account: {error_msg}"
            )));
        }

        progress.set_message("Waiting for storage account to be ready...");
        
        // Wait for storage account to propagate before creating container
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        progress.set_message("Creating blob container...");

        // Create blob container with timeout to prevent hanging
        let create_container_cmd = tokio::time::timeout(
            std::time::Duration::from_secs(120), // 2 minute timeout
            tokio::process::Command::new("az")
                .args([
                    "storage", "container", "create",
                    "--name", "crosstache-files",
                    "--account-name", storage_name,
                    "--subscription", &subscription.id,
                ])
                .output()
        ).await;

        // Check if container creation command completed
        let command_succeeded = match &create_container_cmd {
            Ok(result) => {
                match result {
                    Ok(output) => output.status.success(),
                    Err(_) => false,
                }
            }
            Err(_) => {
                // Command timed out, but container might still have been created
                progress.set_message("Container creation timed out, verifying...");
                false
            }
        };

        // Always verify if the container actually exists, regardless of command result
        progress.set_message("Verifying container creation...");
        let container_exists = AzureDetector::container_exists(&subscription.id, storage_name, "crosstache-files").await
            .unwrap_or(false);

        if !container_exists {
            // Container doesn't exist, check for specific errors only if command failed
            if !command_succeeded {
                if let Ok(Ok(output)) = create_container_cmd {
                    let error_msg = String::from_utf8_lossy(&output.stderr);
                    
                    // Check for specific authentication errors
                    if error_msg.contains("authentication") || error_msg.contains("login") || error_msg.contains("Please run 'az login'") {
                        return Err(CrosstacheError::authentication(
                            "Failed to authenticate with Azure Storage. Please ensure you're logged in with 'az login' and have proper permissions.".to_string()
                        ));
                    }
                    
                    // Check for permission errors
                    if error_msg.contains("authorization") || error_msg.contains("permission") || error_msg.contains("forbidden") {
                        return Err(CrosstacheError::permission_denied(
                            "Insufficient permissions to create blob container. Please ensure you have Storage Blob Data Contributor role.".to_string()
                        ));
                    }
                    
                    return Err(CrosstacheError::azure_api(format!(
                        "Failed to create blob container: {error_msg}"
                    )));
                }
            }
            
            return Err(CrosstacheError::azure_api(
                "Container creation failed or timed out and container does not exist. Please check your Azure CLI authentication and network connection.".to_string()
            ));
        }

        progress.finish_success(&format!("Created storage account '{storage_name}'"));
        Ok(())
    }

    /// Create blob container in existing storage account
    async fn create_blob_container(
        &self,
        storage_name: &str,
        container_name: &str,
        subscription: &AzureSubscription,
    ) -> Result<()> {
        let progress = ProgressIndicator::new("Creating blob container...");

        // Check if container already exists
        let container_exists = AzureDetector::container_exists(&subscription.id, storage_name, container_name).await
            .unwrap_or(false);

        if container_exists {
            progress.finish_success(&format!("Container '{container_name}' already exists in storage account '{storage_name}'"));
            return Ok(());
        }

        // Create blob container with timeout
        let create_container_cmd = tokio::time::timeout(
            std::time::Duration::from_secs(120), // 2 minute timeout
            tokio::process::Command::new("az")
                .args([
                    "storage", "container", "create",
                    "--name", container_name,
                    "--account-name", storage_name,
                    "--subscription", &subscription.id,
                ])
                .output()
        ).await;

        // Check if container creation command completed
        let command_succeeded = match &create_container_cmd {
            Ok(result) => {
                match result {
                    Ok(output) => output.status.success(),
                    Err(_) => false,
                }
            }
            Err(_) => {
                // Command timed out, but container might still have been created
                progress.set_message("Container creation timed out, verifying...");
                false
            }
        };

        // Always verify if the container actually exists, regardless of command result
        progress.set_message("Verifying container creation...");
        let container_exists = AzureDetector::container_exists(&subscription.id, storage_name, container_name).await
            .unwrap_or(false);

        if !container_exists {
            // Container doesn't exist, check for specific errors only if command failed
            if !command_succeeded {
                if let Ok(Ok(output)) = create_container_cmd {
                    let error_msg = String::from_utf8_lossy(&output.stderr);
                    
                    // Check for specific authentication errors
                    if error_msg.contains("authentication") || error_msg.contains("login") || error_msg.contains("Please run 'az login'") {
                        return Err(CrosstacheError::authentication(
                            "Failed to authenticate with Azure Storage. Please ensure you're logged in with 'az login' and have proper permissions.".to_string()
                        ));
                    }
                    
                    // Check for permission errors
                    if error_msg.contains("authorization") || error_msg.contains("permission") || error_msg.contains("forbidden") {
                        return Err(CrosstacheError::permission_denied(
                            "Insufficient permissions to create blob container. Please ensure you have Storage Blob Data Contributor role.".to_string()
                        ));
                    }
                    
                    return Err(CrosstacheError::azure_api(format!(
                        "Failed to create blob container: {error_msg}"
                    )));
                }
            }
            
            return Err(CrosstacheError::azure_api(
                "Container creation failed or timed out and container does not exist. Please check your Azure CLI authentication and network connection.".to_string()
            ));
        }

        progress.finish_success(&format!("Created container '{container_name}' in storage account '{storage_name}'"));
        Ok(())
    }

    /// Configure optional vault creation
    async fn configure_vault_creation(
        &self,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<Option<String>> {
        let create_vault = self.prompt.confirm(
            "Create a test vault to get started?",
            true,
        )?;

        if !create_vault {
            return Ok(None);
        }

        let default_vault_name = SetupHelper::generate_default_vault_name();
        let vault_name = self.prompt.input_text_validated(
            "Enter vault name",
            Some(&default_vault_name),
            SetupHelper::validate_vault_name,
        )?;

        // Create the vault
        self.create_test_vault(&vault_name, subscription, resource_group, location).await?;

        Ok(Some(vault_name))
    }

    /// Create a test vault
    async fn create_test_vault(
        &self,
        vault_name: &str,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<()> {
        let progress = ProgressIndicator::new("Creating test vault...");

        // First, ensure resource group exists
        let rg_exists = AzureDetector::resource_group_exists(&subscription.id, resource_group).await
            .unwrap_or(false);

        if !rg_exists {
            progress.set_message("Creating resource group...");
            AzureDetector::create_resource_group(&subscription.id, resource_group, location).await?;
        }

        // Create authentication provider
        let auth_provider = Arc::new(DefaultAzureCredentialProvider::new()?) as Arc<dyn AzureAuthProvider>;
        
        // Create vault manager
        let vault_manager = VaultManager::new(
            auth_provider,
            subscription.id.clone(),
            false, // no_color = false
        )?;

        // Create vault request
        let vault_request = VaultCreateRequest {
            name: vault_name.to_string(),
            location: location.to_string(),
            resource_group: resource_group.to_string(),
            subscription_id: subscription.id.clone(),
            sku: Some("standard".to_string()),
            enabled_for_deployment: Some(false),
            enabled_for_disk_encryption: Some(false),
            enabled_for_template_deployment: Some(false),
            soft_delete_retention_in_days: Some(90),
            purge_protection: Some(true),
            tags: None,
            access_policies: None,
        };

        progress.set_message("Creating vault...");
        let vault_name = vault_request.name.clone();
        let vault_location = vault_request.location.clone();
        let vault_resource_group = vault_request.resource_group.clone();
        
        vault_manager.create_vault_with_setup(
            &vault_name,
            &vault_location,
            &vault_resource_group,
            Some(vault_request),
        ).await?;

        progress.finish_success(&format!("Created vault '{vault_name}'"));
        Ok(())
    }

    /// Build the final configuration
    async fn build_config(&self, init_config: InitConfig) -> Result<Config> {
        use std::time::Duration;
        use crate::config::settings::BlobConfig;
        
        // Create blob config if storage account was configured
        let blob_config = if !init_config.storage_account_name.is_empty() {
            Some(BlobConfig {
                storage_account: init_config.storage_account_name,
                container_name: init_config.blob_container_name,
                endpoint: None, // Will be auto-generated
                enable_large_file_support: true,
                chunk_size_mb: 4,
                max_concurrent_uploads: 3,
            })
        } else {
            None
        };

        Ok(Config {
            subscription_id: init_config.subscription_id,
            tenant_id: init_config.tenant_id,
            default_vault: init_config.default_vault.unwrap_or_default(),
            default_resource_group: init_config.default_resource_group,
            default_location: init_config.default_location,
            output_json: false,
            no_color: false,
            debug: false,
            cache_ttl: Duration::from_secs(300),
            function_app_url: String::new(),
            blob_config,
            azure_credential_priority: crate::config::settings::AzureCredentialType::Default,
        })
    }

    /// Save configuration to file
    async fn save_config(&self, config: &Config) -> Result<()> {
        let progress = ProgressIndicator::new("Saving configuration...");
        
        // Use the same config path as the settings module for consistency
        let config_file = Config::get_config_path()?;
        
        // Create parent directories if they don't exist
        if let Some(parent) = config_file.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| CrosstacheError::config(format!(
                    "Failed to create config directory: {e}"
                )))?;
        }

        // Save configuration file
        let config_content = toml::to_string_pretty(config)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to serialize config: {e}"
            )))?;

        tokio::fs::write(&config_file, config_content).await
            .map_err(|e| CrosstacheError::config(format!(
                "Failed to write config file: {e}"
            )))?;

        progress.finish_success(&format!("Configuration saved to {}", config_file.display()));
        Ok(())
    }

    /// Show setup summary
    pub fn show_setup_summary(&self, config: &Config) -> Result<()> {
        println!();
        self.prompt.success("Setup Summary")?;
        println!("┌─────────────────────────────────────────────────────────────┐");
        println!("│ Configuration                                               │");
        println!("├─────────────────────────────────────────────────────────────┤");
        println!("│ Subscription ID: {:<39} │", config.subscription_id);
        println!("│ Resource Group:  {:<39} │", config.default_resource_group);
        println!("│ Default Location: {:<38} │", config.default_location);
        
        if !config.default_vault.is_empty() {
            println!("│ Default Vault:   {:<40} │", config.default_vault);
        }
        
        // Show blob storage configuration if present
        if let Some(blob_config) = &config.blob_config {
            if !blob_config.storage_account.is_empty() {
                println!("│ Storage Account: {:<40} │", blob_config.storage_account);
                println!("│ Blob Container:  {:<40} │", blob_config.container_name);
            }
        }
        
        println!("└─────────────────────────────────────────────────────────────┘");
        println!();
        
        self.prompt.info("Next steps:")?;
        println!("  • List your vaults: xv vault list");
        println!("  • Set a secret: xv set my-secret");
        println!("  • Get help: xv --help");
        
        Ok(())
    }
}

impl Default for ConfigInitializer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_initializer_creation() {
        let initializer = ConfigInitializer::new();
        // Just test that we can create the initializer
        assert!(std::ptr::addr_of!(initializer).is_aligned());
    }

    #[test]
    fn test_init_config_structure() {
        let init_config = InitConfig {
            subscription_id: "test-sub".to_string(),
            tenant_id: "test-tenant".to_string(),
            default_resource_group: "test-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: Some("test-vault".to_string()),
            create_test_vault: true,
            storage_account_name: "teststorage".to_string(),
            blob_container_name: "test-container".to_string(),
            create_storage_account: true,
        };

        assert_eq!(init_config.subscription_id, "test-sub");
        assert_eq!(init_config.default_location, "eastus");
        assert!(init_config.create_test_vault);
        assert!(init_config.default_vault.is_some());
    }
}