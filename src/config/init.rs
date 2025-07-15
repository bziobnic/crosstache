//! Configuration initialization logic for interactive setup
//!
//! This module handles the step-by-step initialization process for new users,
//! including Azure environment detection, configuration building, and vault creation.

use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
use crate::config::settings::Config;
use crate::error::{crosstacheError, Result};
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
        self.prompt.step(1, 5, "Detecting Azure Environment")?;
        let azure_env = self.detect_azure_environment().await?;

        // Step 2: Configure subscription
        self.prompt.step(2, 5, "Configuring Subscription")?;
        let subscription = self.configure_subscription(&azure_env).await?;

        // Step 3: Configure resource group
        self.prompt.step(3, 5, "Configuring Resource Group")?;
        let resource_group = self.configure_resource_group(&subscription).await?;

        // Step 4: Configure location
        self.prompt.step(4, 5, "Configuring Default Location")?;
        let location = self.configure_location(&subscription).await?;

        // Step 5: Optional vault creation
        self.prompt.step(5, 5, "Optional Test Vault Creation")?;
        let vault_config = self.configure_vault_creation(&subscription, &resource_group, &location).await?;

        // Build the final configuration
        let init_config = InitConfig {
            subscription_id: subscription.id,
            tenant_id: subscription.tenant_id,
            default_resource_group: resource_group,
            default_location: location,
            default_vault: vault_config.clone(),
            create_test_vault: vault_config.is_some(),
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
                    println!("  • {}", instruction);
                }
                return Err(crosstacheError::config(
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
                &format!("Resource group '{}' doesn't exist. Create it?", resource_group_name),
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

        progress.finish_success(&format!("Created vault '{}'", vault_name));
        Ok(())
    }

    /// Build the final configuration
    async fn build_config(&self, init_config: InitConfig) -> Result<Config> {
        use std::time::Duration;
        
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
        })
    }

    /// Save configuration to file
    async fn save_config(&self, config: &Config) -> Result<()> {
        let progress = ProgressIndicator::new("Saving configuration...");
        
        // Use the same config path as the settings module for consistency
        let config_file = Config::get_config_path()?;
        
        // Create parent directories if they don't exist
        if let Some(parent) = config_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crosstacheError::config(format!(
                    "Failed to create config directory: {}", e
                )))?;
        }

        // Save configuration file
        let config_content = toml::to_string_pretty(config)
            .map_err(|e| crosstacheError::serialization(format!(
                "Failed to serialize config: {}", e
            )))?;

        std::fs::write(&config_file, config_content)
            .map_err(|e| crosstacheError::config(format!(
                "Failed to write config file: {}", e
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
        };

        assert_eq!(init_config.subscription_id, "test-sub");
        assert_eq!(init_config.default_location, "eastus");
        assert!(init_config.create_test_vault);
        assert!(init_config.default_vault.is_some());
    }
}