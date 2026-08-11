//! Configuration initialization logic for interactive setup
//!
//! This module handles the step-by-step initialization process for new users,
//! including Azure environment detection, configuration building, and vault creation.

use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
use crate::config::backend_ops::{add_backend, configured_backends, BackendType};
use crate::config::settings::{load_config_file_only, AzureConfig, Config};
use crate::config::setup::{atomic_save_config, build_setup_config, SetupRequest};
use crate::error::{CrosstacheError, Result};
use crate::utils::azure_detect::{AzureDetector, AzureEnvironment, AzureSubscription};
use crate::utils::interactive::{InteractivePrompt, ProgressIndicator, Prompter, SetupHelper};
use crate::utils::output;
use crate::vault::manager::VaultManager;
use crate::vault::models::VaultCreateRequest;
use std::sync::Arc;

/// Interactive configuration initialization
pub struct ConfigInitializer {
    prompt: Box<dyn Prompter>,
    /// Concrete prompt used only by helpers that need
    /// `input_text_validated`, which is not on the `Prompter` trait (not
    /// object-safe). Currently only the Azure provisioning helpers
    /// (subscription/resource-group/location/storage/vault prompts) use it;
    /// they are not driven through `Prompter` by tests in this task.
    validated_prompt: InteractivePrompt,
}

/// Configuration data collected during initialization
#[derive(Debug, Clone)]
pub struct InitConfig {
    pub subscription_id: String,
    pub tenant_id: String,
    pub default_resource_group: String,
    pub default_location: String,
    pub default_vault: Option<String>,
    #[allow(dead_code)]
    pub create_test_vault: bool,
    pub storage_account_name: String,
    pub blob_container_name: String,
    #[allow(dead_code)]
    pub create_storage_account: bool,
}

impl ConfigInitializer {
    /// Create a new configuration initializer
    pub fn new() -> Self {
        Self {
            prompt: Box::new(InteractivePrompt::new()),
            validated_prompt: InteractivePrompt::new(),
        }
    }

    /// Create a configuration initializer driven by a scripted `Prompter`,
    /// for tests. See the `validated_prompt` field docs above for what this
    /// does and does not cover.
    #[cfg(test)]
    pub fn with_prompter(prompter: Box<dyn Prompter>) -> Self {
        Self {
            prompt: prompter,
            validated_prompt: InteractivePrompt::new(),
        }
    }

    /// Run the complete interactive initialization process
    pub async fn run_interactive_setup(&self) -> Result<Config> {
        // `welcome` is a concrete-type convenience (not on `Prompter`, and
        // not meaningful to script), so it goes through `validated_prompt`.
        self.validated_prompt.welcome()?;

        // Step 0: Choose backend
        println!();
        output::step("Backend Selection");
        let backend_options = vec![
            "Azure Key Vault (cloud-based, requires Azure subscription)".to_string(),
            "Local (age-encrypted files, offline, no cloud account needed)".to_string(),
            "AWS Secrets Manager (cloud-based, requires AWS account)".to_string(),
        ];
        let backend_index = self.prompt.select(
            "Which secrets backend would you like to use?",
            &backend_options,
            Some(0),
        )?;

        let chosen = match backend_index {
            1 => BackendType::Local,
            2 => BackendType::Aws,
            _ => BackendType::Azure,
        };

        // Loading the existing config means init no longer silently discards
        // other backends — but it can still replace the selected one, so ask.
        // `load_config_file_only` is used deliberately, on two counts:
        //   * it skips validation — a full `Config::load()` validates only the
        //     *active* backend, so an incomplete active backend (e.g. Azure
        //     with no subscription_id) would make `load()` fail and
        //     `unwrap_or_default()` would then discard every other configured
        //     block;
        //   * it skips environment-variable overrides, unlike
        //     `load_config_no_validation`. This value is the base of a config
        //     that gets *written to disk*, and `add_backend` does not clear the
        //     top-level Azure fields, so an env load would persist ambient
        //     `AZURE_SUBSCRIPTION_ID` / `AZURE_TENANT_ID` / `DEBUG` / ... into
        //     xv.conf for a local or AWS setup. Same reasoning as
        //     `xv backend add`/`rm` (see `cli::backend_ops`).
        let existing = load_config_file_only().await.unwrap_or_default();
        if configured_backends(&existing).contains(&chosen) {
            let proceed = self.prompt.confirm(
                &format!(
                    "Backend '{chosen}' is already configured; reconfigure it? \
                     Its existing settings will be replaced."
                ),
                false,
            )?;
            if !proceed {
                output::info("Aborted; no changes made.");
                return Ok(existing);
            }
        }

        if backend_index == 1 {
            return self.run_local_setup(existing).await;
        }

        if backend_index == 2 {
            return self.run_aws_setup(existing).await;
        }

        // Azure flow (unchanged)

        // Step 1: Detect Azure environment
        println!();
        output::step("Step 1/6: Detecting Azure Environment");
        let azure_env = self.detect_azure_environment().await?;

        // Step 2: Configure subscription
        println!();
        output::step("Step 2/6: Configuring Subscription");
        let subscription = self.configure_subscription(&azure_env).await?;

        // Step 3: Configure resource group
        println!();
        output::step("Step 3/6: Configuring Resource Group");
        let resource_group = self.configure_resource_group(&subscription).await?;

        // Step 4: Configure location
        println!();
        output::step("Step 4/6: Configuring Default Location");
        let location = self.configure_location(&subscription).await?;

        // Create resource group now that we have the location. This ensures the
        // group exists even if the user skips optional vault creation in step 6.
        let rg_exists = crate::utils::azure_detect::AzureDetector::resource_group_exists(
            &subscription.id,
            &resource_group,
        )
        .await
        .unwrap_or(false);
        if !rg_exists {
            let progress =
                crate::utils::interactive::ProgressIndicator::new("Creating resource group...");
            crate::utils::azure_detect::AzureDetector::create_resource_group(
                &subscription.id,
                &resource_group,
                &location,
            )
            .await?;
            progress.finish_success(&format!("Created resource group '{resource_group}'"));
        }

        // Step 5: Configure blob storage
        println!();
        output::step("Step 5/6: Configuring Blob Storage");
        let (storage_account, container_name, blob_storage_configured) = self
            .configure_blob_storage(&subscription, &resource_group, &location)
            .await?;

        // Step 6: Optional vault creation
        println!();
        output::step("Step 6/6: Optional Test Vault Creation");
        let vault_config = self
            .configure_vault_creation(&subscription, &resource_group, &location)
            .await?;

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

        // Create and save the configuration. `existing` (loaded above) is the
        // base, so any already-configured `local`/`aws` blocks and unrelated
        // settings survive choosing Azure, same as the local/aws branches.
        let config = self.build_config(init_config, existing).await?;
        self.save_config(&config).await?;

        output::success("Setup completed successfully!");
        output::info("You can now start using crosstache with your configured defaults.");

        Ok(config)
    }

    /// Collect the interactive answers for one backend, producing a
    /// `SetupRequest`. Pure prompting: no files are written and no backend is
    /// contacted, so the caller decides whether to apply the result.
    pub async fn collect_backend_request(&self, backend: BackendType) -> Result<SetupRequest> {
        match backend {
            BackendType::Local => self.collect_local_request(),
            BackendType::Aws => self.collect_aws_request().await,
            BackendType::Azure => self.collect_azure_request().await,
        }
    }

    /// Prompt for the local backend's settings. No files are created.
    fn collect_local_request(&self) -> Result<SetupRequest> {
        // Step 1: Store path
        println!();
        output::step("Step 1/3: Store Location");
        let default_store = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".xv")
            .join("store");
        let store_path = self.prompt.input_text(
            "Store path for encrypted secrets",
            Some(&default_store.to_string_lossy()),
        )?;

        // Step 2: Key file path
        println!();
        output::step("Step 2/3: Key File");
        let default_key = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".xv")
            .join("key.txt");
        let key_file = self
            .prompt
            .input_text("Age key file path", Some(&default_key.to_string_lossy()))?;

        // Step 3: Default vault name
        println!();
        output::step("Step 3/3: Default Vault");
        let default_vault = self
            .prompt
            .input_text("Default vault name", Some("default"))?;

        Ok(SetupRequest::Local {
            store_path: store_path.into(),
            key_file: key_file.into(),
            vault: default_vault,
        })
    }

    /// Collect AWS-specific settings from the user. No credentials are
    /// contacted; region/profile default from the environment.
    async fn collect_aws_request(&self) -> Result<SetupRequest> {
        use dialoguer::Input;

        println!();
        output::step("Step 1/3: AWS Region");
        let region: String = Input::new()
            .with_prompt("AWS region")
            .default(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string()))
            .interact_text()
            .map_err(|e| CrosstacheError::config(format!("Region prompt failed: {e}")))?;

        println!();
        output::step("Step 2/3: AWS Profile");
        let profile: String = Input::new()
            .with_prompt("AWS profile")
            .default(std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".to_string()))
            .interact_text()
            .map_err(|e| CrosstacheError::config(format!("Profile prompt failed: {e}")))?;

        println!();
        output::step("Step 3/3: Default Vault");
        let default_vault: String = Input::new()
            .with_prompt("Default vault (prefix)")
            .default("default".to_string())
            .interact_text()
            .map_err(|e| CrosstacheError::config(format!("Vault prompt failed: {e}")))?;

        Ok(SetupRequest::Aws {
            region,
            profile: Some(profile),
            vault_prefix: default_vault,
        })
    }

    /// Collect Azure-specific settings from the user for `xv backend add
    /// azure`, reusing the same environment-detection, subscription,
    /// resource-group, and location helpers `xv init`'s Azure flow uses.
    ///
    /// Unlike `xv init`'s Azure flow, this always requires a vault — there is
    /// no legacy "skip vault creation" compatibility concern here, since
    /// `SetupRequest::Azure` (and therefore `add_backend`) requires one. Blob
    /// storage is intentionally NOT collected here: it is an `xv init`-only
    /// convenience unrelated to which secrets backend is configured, and
    /// `SetupRequest::Azure` has no field for it.
    async fn collect_azure_request(&self) -> Result<SetupRequest> {
        println!();
        output::step("Step 1/4: Detecting Azure Environment");
        let azure_env = self.detect_azure_environment().await?;

        println!();
        output::step("Step 2/4: Configuring Subscription");
        let subscription = self.configure_subscription(&azure_env).await?;

        println!();
        output::step("Step 3/4: Configuring Resource Group");
        let resource_group = self.configure_resource_group(&subscription).await?;

        println!();
        output::step("Step 4/4: Configuring Default Location");
        let location = self.configure_location(&subscription).await?;

        let rg_exists = AzureDetector::resource_group_exists(&subscription.id, &resource_group)
            .await
            .unwrap_or(false);
        if !rg_exists {
            let progress = ProgressIndicator::new("Creating resource group...");
            AzureDetector::create_resource_group(&subscription.id, &resource_group, &location)
                .await?;
            progress.finish_success(&format!("Created resource group '{resource_group}'"));
        }

        let vault = loop {
            match self
                .configure_vault_creation(&subscription, &resource_group, &location)
                .await?
            {
                Some(vault) => break vault,
                None => output::error(
                    "A vault is required to add the Azure backend; please create one.",
                ),
            }
        };

        Ok(SetupRequest::Azure {
            subscription_id: subscription.id,
            tenant_id: subscription.tenant_id,
            vault,
            resource_group,
            location,
        })
    }

    /// Run the simplified local backend setup (3 steps).
    ///
    /// `base` is the caller's already-loaded existing config (via
    /// `load_config_file_only`), so other configured backends survive.
    async fn run_local_setup(&self, base: Config) -> Result<Config> {
        let request = self.collect_local_request()?;
        let (store_path, key_file, default_vault) = match &request {
            SetupRequest::Local {
                store_path,
                key_file,
                vault,
            } => (
                store_path.to_string_lossy().to_string(),
                key_file.to_string_lossy().to_string(),
                vault.clone(),
            ),
            _ => unreachable!("collect_local_request always returns SetupRequest::Local"),
        };

        // Validate and build the candidate before the backend creates keys or
        // directories.
        let progress = ProgressIndicator::new("Setting up local backend...");
        let config = add_backend(&request, base, true).await?;
        progress.finish_success("Local backend initialized");
        let local_config = config.local.as_ref().ok_or_else(|| {
            CrosstacheError::config("Local setup did not produce a local configuration")
        })?;

        // Read the public key for the summary
        let resolved =
            crate::backend::local::config::ResolvedLocalConfig::from_raw(Some(local_config));
        let public_key = if resolved.recipients_file.exists() {
            std::fs::read_to_string(&resolved.recipients_file)
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            String::new()
        };

        self.save_config(&config).await?;

        output::success("Local backend setup completed!");
        println!();
        println!("  Store path:    {store_path}");
        println!("  Key file:      {key_file}");
        println!("  Default vault: {default_vault}");
        if !public_key.is_empty() {
            println!("  Public key:    {public_key}");
        }

        println!();
        output::warn(
            "Secret values are encrypted, but metadata (notes, tags, folders) and secret \
             NAMES are stored in plaintext on disk by default.\n  To encrypt metadata at rest, \
             set `encrypt_metadata = true` under [local] in your config, then run \
             `xv local encrypt-metadata`.\n  (Secret names remain visible as filenames \
             regardless.)",
        );

        Ok(config)
    }

    /// Run the simplified AWS backend setup.
    ///
    /// `base` is the caller's already-loaded existing config (via
    /// `load_config_file_only`), so other configured backends survive.
    async fn run_aws_setup(&self, base: Config) -> Result<Config> {
        let request = self.collect_aws_request().await?;
        let (region, profile, default_vault) = match &request {
            SetupRequest::Aws {
                region,
                profile,
                vault_prefix,
            } => (region.clone(), profile.clone(), vault_prefix.clone()),
            _ => unreachable!("collect_aws_request always returns SetupRequest::Aws"),
        };

        let config = add_backend(&request, base, true).await?;

        self.save_config(&config).await?;

        output::success("AWS backend setup completed!");
        println!();
        println!("  Region:        {region}");
        println!(
            "  Profile:       {}",
            profile.as_deref().unwrap_or("default")
        );
        println!("  Default vault: {default_vault}");

        Ok(config)
    }

    /// Detect Azure environment and handle issues
    async fn detect_azure_environment(&self) -> Result<AzureEnvironment> {
        let progress = ProgressIndicator::new("Detecting Azure CLI and environment...");

        let azure_env = AzureDetector::detect_environment().await?;

        if !azure_env.is_ready() {
            progress.finish_error("Azure environment not ready");
            output::error(&azure_env.get_status_message());

            let instructions = azure_env.get_setup_instructions();
            if !instructions.is_empty() {
                output::info("Please complete the following steps:");
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
            output::info(&format!(
                "Current subscription: {} ({})",
                current.name, current.id
            ));
        }

        Ok(azure_env)
    }

    /// Configure Azure subscription
    async fn configure_subscription(
        &self,
        azure_env: &AzureEnvironment,
    ) -> Result<AzureSubscription> {
        if azure_env.subscriptions.len() == 1 {
            let subscription = &azure_env.subscriptions[0];
            let use_default = self.prompt.confirm(
                &format!(
                    "Use subscription '{}' ({})?",
                    subscription.name, subscription.id
                ),
                true,
            )?;

            if use_default {
                return Ok(subscription.clone());
            }
        }

        if azure_env.subscriptions.len() > 1 {
            output::info("Multiple subscriptions available:");

            let subscription_options: Vec<String> = azure_env
                .subscriptions
                .iter()
                .map(|s| format!("{} ({})", s.name, s.id))
                .collect();

            let default_index = azure_env.current_subscription.as_ref().and_then(|current| {
                azure_env
                    .subscriptions
                    .iter()
                    .position(|s| s.id == current.id)
            });

            let selected_index = self.prompt.select(
                "Select a subscription",
                &subscription_options,
                default_index,
            )?;

            return Ok(azure_env.subscriptions[selected_index].clone());
        }

        // Manual entry if needed
        let subscription_id = self.validated_prompt.input_text_validated(
            "Enter subscription ID",
            None,
            SetupHelper::validate_subscription_id,
        )?;

        // Create a basic subscription object
        Ok(AzureSubscription {
            id: subscription_id,
            name: "Manual Entry".to_string(),
            tenant_id: azure_env
                .tenant_info
                .as_ref()
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
        let existing_groups = AzureDetector::get_resource_groups(&subscription.id)
            .await
            .unwrap_or_default();

        progress.finish_clear();

        if !existing_groups.is_empty() {
            output::info(&format!(
                "Found {} existing resource group(s)",
                existing_groups.len()
            ));

            let use_existing = self
                .prompt
                .confirm("Use an existing resource group?", true)?;

            if use_existing {
                let selected_index =
                    self.prompt
                        .select("Select a resource group", &existing_groups, None)?;
                return Ok(existing_groups[selected_index].clone());
            }
        }

        // Create new resource group
        let default_name = SetupHelper::generate_default_resource_group();
        let resource_group_name = self.validated_prompt.input_text_validated(
            "Enter resource group name",
            Some(&default_name),
            SetupHelper::validate_resource_group_name,
        )?;

        // Check if it exists
        let exists = AzureDetector::resource_group_exists(&subscription.id, &resource_group_name)
            .await
            .unwrap_or(false);

        if !exists {
            let create_rg = self.prompt.confirm(
                &format!("Resource group '{resource_group_name}' doesn't exist. Create it?"),
                true,
            )?;

            if create_rg {
                // We'll create it when we know the location
                output::info("Resource group will be created with the selected location.");
            }
        }

        Ok(resource_group_name)
    }

    /// Configure default location
    async fn configure_location(&self, subscription: &AzureSubscription) -> Result<String> {
        let progress = ProgressIndicator::new("Loading available locations...");

        let locations = AzureDetector::get_locations(&subscription.id)
            .await
            .unwrap_or_else(|_| {
                vec![
                    "eastus".to_string(),
                    "westus2".to_string(),
                    "centralus".to_string(),
                    "northeurope".to_string(),
                    "westeurope".to_string(),
                ]
            });

        progress.finish_clear();

        // Suggest a good default location
        let default_location = locations
            .iter()
            .find(|&loc| loc == "eastus" || loc == "westus2")
            .unwrap_or(&locations[0]);

        let default_index = locations.iter().position(|loc| loc == default_location);

        let selected_index =
            self.prompt
                .select("Select default location", &locations, default_index)?;

        Ok(locations[selected_index].clone())
    }

    /// Configure blob storage during initialization
    async fn configure_blob_storage(
        &self,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<(String, String, bool)> {
        let create_storage = self
            .prompt
            .confirm("Configure blob storage for file operations?", true)?;

        if !create_storage {
            return Ok((String::new(), String::new(), false));
        }

        let progress = ProgressIndicator::new("Loading existing storage accounts...");

        // Try to get existing storage accounts in the resource group
        let existing_accounts =
            AzureDetector::get_storage_accounts(&subscription.id, resource_group)
                .await
                .unwrap_or_default();

        progress.finish_clear();

        let (storage_name, create_new_storage) = if !existing_accounts.is_empty() {
            output::info(&format!(
                "Found {} existing storage account(s) in resource group '{}'",
                existing_accounts.len(),
                resource_group
            ));

            let use_existing = self
                .prompt
                .confirm("Use an existing storage account?", true)?;

            if use_existing {
                let selected_index =
                    self.prompt
                        .select("Select a storage account", &existing_accounts, None)?;
                (existing_accounts[selected_index].clone(), false)
            } else {
                // Create new storage account
                let default_storage_name = SetupHelper::generate_storage_account_name();
                let storage_name = self.validated_prompt.input_text_validated(
                    "Enter new storage account name",
                    Some(&default_storage_name),
                    SetupHelper::validate_storage_account_name,
                )?;
                (storage_name, true)
            }
        } else {
            // No existing accounts, create new one
            let default_storage_name = SetupHelper::generate_storage_account_name();
            let storage_name = self.validated_prompt.input_text_validated(
                "Enter storage account name",
                Some(&default_storage_name),
                SetupHelper::validate_storage_account_name,
            )?;
            (storage_name, true)
        };

        let container_name = self.validated_prompt.input_text_validated(
            "Enter container name for files",
            Some("crosstache-files"),
            SetupHelper::validate_container_name,
        )?;

        // Create storage account if needed
        if create_new_storage {
            self.create_storage_account(&storage_name, subscription, resource_group, location)
                .await?;
        } else {
            // If using existing storage account, just create the container
            self.create_blob_container(&storage_name, &container_name, subscription)
                .await?;
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
            tokio::process::Command::new(crate::backend::azure::az_program())
                .args([
                    "storage",
                    "account",
                    "create",
                    "--name",
                    storage_name,
                    "--resource-group",
                    resource_group,
                    "--location",
                    location,
                    "--sku",
                    "Standard_LRS",
                    "--kind",
                    "StorageV2",
                    "--access-tier",
                    "Hot",
                    "--allow-blob-public-access",
                    "false",
                    "--min-tls-version",
                    "TLS1_2",
                    "--subscription",
                    &subscription.id,
                ])
                .output(),
        )
        .await;

        let create_storage_cmd = match create_storage_cmd {
            Ok(result) => match result {
                Ok(output) => output,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(CrosstacheError::azure_api(
                        "Azure CLI ('az') is not installed or not found in PATH. \
                         Storage account creation requires Azure CLI. \
                         Install it from https://docs.microsoft.com/cli/azure/install-azure-cli \
                         or create the storage account manually and set AZURE_STORAGE_ACCOUNT."
                            .to_string(),
                    ));
                }
                Err(e) => return Err(CrosstacheError::IoError(e)),
            },
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
            tokio::process::Command::new(crate::backend::azure::az_program())
                .args([
                    "storage",
                    "container",
                    "create",
                    "--name",
                    "crosstache-files",
                    "--account-name",
                    storage_name,
                    "--subscription",
                    &subscription.id,
                ])
                .output(),
        )
        .await;

        // Check if container creation command completed
        let command_succeeded = match &create_container_cmd {
            Ok(result) => match result {
                Ok(output) => output.status.success(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(CrosstacheError::azure_api(
                        "Azure CLI ('az') is not installed or not found in PATH. \
                         Install it from https://docs.microsoft.com/cli/azure/install-azure-cli"
                            .to_string(),
                    ));
                }
                Err(_) => false,
            },
            Err(_) => {
                // Command timed out, but container might still have been created
                progress.set_message("Container creation timed out, verifying...");
                false
            }
        };

        // Always verify if the container actually exists, regardless of command result
        progress.set_message("Verifying container creation...");
        let container_exists =
            AzureDetector::container_exists(&subscription.id, storage_name, "crosstache-files")
                .await
                .unwrap_or(false);

        if !container_exists {
            // Container doesn't exist, check for specific errors only if command failed
            if !command_succeeded {
                if let Ok(Ok(output)) = create_container_cmd {
                    let error_msg = String::from_utf8_lossy(&output.stderr);

                    // Check for specific authentication errors
                    if error_msg.contains("authentication")
                        || error_msg.contains("login")
                        || error_msg.contains("Please run 'az login'")
                    {
                        return Err(CrosstacheError::authentication(
                            "Failed to authenticate with Azure Storage. Please ensure you're logged in with 'az login' and have proper permissions.".to_string()
                        ));
                    }

                    // Check for permission errors
                    if error_msg.contains("authorization")
                        || error_msg.contains("permission")
                        || error_msg.contains("forbidden")
                    {
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
        let container_exists =
            AzureDetector::container_exists(&subscription.id, storage_name, container_name)
                .await
                .unwrap_or(false);

        if container_exists {
            progress.finish_success(&format!(
                "Container '{container_name}' already exists in storage account '{storage_name}'"
            ));
            return Ok(());
        }

        // Create blob container with timeout
        let create_container_cmd = tokio::time::timeout(
            std::time::Duration::from_secs(120), // 2 minute timeout
            tokio::process::Command::new(crate::backend::azure::az_program())
                .args([
                    "storage",
                    "container",
                    "create",
                    "--name",
                    container_name,
                    "--account-name",
                    storage_name,
                    "--subscription",
                    &subscription.id,
                ])
                .output(),
        )
        .await;

        // Check if container creation command completed
        let command_succeeded = match &create_container_cmd {
            Ok(result) => match result {
                Ok(output) => output.status.success(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(CrosstacheError::azure_api(
                        "Azure CLI ('az') is not installed or not found in PATH. \
                         Install it from https://docs.microsoft.com/cli/azure/install-azure-cli"
                            .to_string(),
                    ));
                }
                Err(_) => false,
            },
            Err(_) => {
                // Command timed out, but container might still have been created
                progress.set_message("Container creation timed out, verifying...");
                false
            }
        };

        // Always verify if the container actually exists, regardless of command result
        progress.set_message("Verifying container creation...");
        let container_exists =
            AzureDetector::container_exists(&subscription.id, storage_name, container_name)
                .await
                .unwrap_or(false);

        if !container_exists {
            // Container doesn't exist, check for specific errors only if command failed
            if !command_succeeded {
                if let Ok(Ok(output)) = create_container_cmd {
                    let error_msg = String::from_utf8_lossy(&output.stderr);

                    // Check for specific authentication errors
                    if error_msg.contains("authentication")
                        || error_msg.contains("login")
                        || error_msg.contains("Please run 'az login'")
                    {
                        return Err(CrosstacheError::authentication(
                            "Failed to authenticate with Azure Storage. Please ensure you're logged in with 'az login' and have proper permissions.".to_string()
                        ));
                    }

                    // Check for permission errors
                    if error_msg.contains("authorization")
                        || error_msg.contains("permission")
                        || error_msg.contains("forbidden")
                    {
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

        progress.finish_success(&format!(
            "Created container '{container_name}' in storage account '{storage_name}'"
        ));
        Ok(())
    }

    /// Configure optional vault creation
    async fn configure_vault_creation(
        &self,
        subscription: &AzureSubscription,
        resource_group: &str,
        location: &str,
    ) -> Result<Option<String>> {
        let create_vault = self
            .prompt
            .confirm("Create a test vault to get started?", true)?;

        if !create_vault {
            return Ok(None);
        }

        let default_vault_name = SetupHelper::generate_default_vault_name();
        let vault_name = self.validated_prompt.input_text_validated(
            "Enter vault name",
            Some(&default_vault_name),
            SetupHelper::validate_vault_name,
        )?;

        // Create the vault
        self.create_test_vault(&vault_name, subscription, resource_group, location)
            .await?;

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
        let rg_exists = AzureDetector::resource_group_exists(&subscription.id, resource_group)
            .await
            .unwrap_or(false);

        if !rg_exists {
            progress.set_message("Creating resource group...");
            AzureDetector::create_resource_group(&subscription.id, resource_group, location)
                .await?;
        }

        // Create authentication provider
        let auth_provider =
            Arc::new(DefaultAzureCredentialProvider::new()?) as Arc<dyn AzureAuthProvider>;

        // Create vault manager
        let vault_manager = VaultManager::new(auth_provider, subscription.id.clone())?;

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

        vault_manager
            .create_vault_with_setup(
                &vault_name,
                &vault_location,
                &vault_resource_group,
                Some(vault_request),
            )
            .await?;

        progress.finish_success(&format!("Created vault '{vault_name}'"));
        Ok(())
    }

    /// Build the final configuration.
    ///
    /// `base` is the caller's already-loaded existing config (via
    /// `load_config_file_only` in `run_interactive_setup`, or
    /// `Config::default()` from the unit tests below), so choosing Azure
    /// preserves other configured backends and unrelated settings the same
    /// way the local/aws branches do. Kept pure — no disk access here — so
    /// the tests stay host-independent; callers load and pass `base` in.
    async fn build_config(&self, init_config: InitConfig, base: Config) -> Result<Config> {
        use crate::config::settings::BlobConfig;

        let InitConfig {
            subscription_id,
            tenant_id,
            default_resource_group,
            default_location,
            default_vault,
            storage_account_name,
            blob_container_name,
            ..
        } = init_config;

        // Create blob config if storage account was configured
        let blob_config = if !storage_account_name.is_empty() {
            Some(BlobConfig {
                storage_account: storage_account_name,
                container_name: blob_container_name,
                endpoint: None, // Will be auto-generated
                enable_large_file_support: true,
                chunk_size_mb: 4,
                max_concurrent_uploads: 3,
                progress_threshold_mb: 5,
            })
        } else {
            None
        };

        let mut candidate = base;
        candidate.blob_config = blob_config;

        if let Some(vault) = default_vault {
            // `build_setup_config` clears `local`/`aws` (and `azure`) to
            // enforce the exclusive-backend invariant its other callers
            // (`xv backend add`'s exclusive paths) need, but Azure `init`
            // must not destroy an already-configured local/aws backend.
            // Capture them and restore after: `build_setup_config` validates
            // only the newly-active Azure backend, so restoring afterward
            // does not affect that validation.
            let existing_local = candidate.local.clone();
            let existing_aws = candidate.aws.clone();
            let mut config = build_setup_config(
                &SetupRequest::Azure {
                    subscription_id,
                    tenant_id,
                    vault,
                    resource_group: default_resource_group,
                    location: default_location,
                },
                candidate,
            )?;
            config.local = existing_local;
            config.aws = existing_aws;
            return Ok(config);
        }

        // Compatibility-only branch for the existing interactive
        // "Create a test vault? = no" flow. Shared non-interactive Azure
        // setup remains strict and always requires a vault. This branch
        // never calls `build_setup_config` (so never clears `local`/`aws`),
        // and `candidate` already carries the caller's `base`, so other
        // configured backends and unrelated settings survive untouched.
        candidate.backend = None;
        candidate.subscription_id = subscription_id.clone();
        candidate.tenant_id = tenant_id.clone();
        candidate.default_vault.clear();
        candidate.default_resource_group = default_resource_group.clone();
        candidate.default_location = default_location.clone();
        // Write a fresh `[azure]` block from the values just collected
        // rather than leaving whatever `base` carried: `azure_settings()`
        // prefers the block over the legacy top-level fields, so a stale
        // pre-existing block would otherwise keep reporting old
        // subscription/tenant/vault values after this reconfigure. This is
        // precisely the "user declined to create a vault" case, so the
        // block's `default_vault` is `None` (not carried over from any
        // prior block) so `azure_settings()` correctly reports no vault.
        candidate.azure = Some(AzureConfig {
            subscription_id: Some(subscription_id),
            tenant_id: Some(tenant_id),
            default_vault: None,
            resource_group: Some(default_resource_group),
            location: Some(default_location),
        });
        candidate.validate()?;
        Ok(candidate)
    }

    /// Save configuration to file
    async fn save_config(&self, config: &Config) -> Result<()> {
        let progress = ProgressIndicator::new("Saving configuration...");

        // Use the same config path as the settings module for consistency
        let config_file = Config::get_config_path()?;
        atomic_save_config(config, &config_file)
            .await
            .map_err(|e| CrosstacheError::config(format!("Failed to write config file: {e}")))?;

        progress.finish_success(&format!("Configuration saved to {}", config_file.display()));
        Ok(())
    }

    /// Show setup summary
    pub fn show_setup_summary(&self, config: &Config) -> Result<()> {
        // Local backend shows its own summary in run_local_setup()
        if config.backend.as_deref() == Some("local") {
            println!();
            output::info("Next steps:");
            output::hint("Set a secret: xv set my-secret");
            output::hint("Get a secret: xv get my-secret --raw");
            output::hint("List secrets: xv list");
            output::hint("Get help: xv --help");
            return Ok(());
        }

        println!();
        output::success("Setup Summary");
        println!();
        println!("  Subscription ID:  {}", config.subscription_id);
        println!("  Resource Group:   {}", config.default_resource_group);
        println!("  Default Location: {}", config.default_location);

        if !config.default_vault.is_empty() {
            println!("  Default Vault:    {}", config.default_vault);
        }

        // Show blob storage configuration if present
        if let Some(blob_config) = &config.blob_config {
            if !blob_config.storage_account.is_empty() {
                println!("  Storage Account:  {}", blob_config.storage_account);
                println!("  Blob Container:   {}", blob_config.container_name);
            }
        }

        println!();

        output::info("Next steps:");
        output::hint("List your vaults: xv vault list");
        output::hint("Set a secret: xv set my-secret");
        output::hint("Get help: xv --help");

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
    use crate::utils::interactive::{Answer, ScriptedPrompter};

    /// This is the justification for the `Prompter` seam: drive
    /// `collect_local_request` through scripted answers instead of a real
    /// terminal, and assert the resulting `SetupRequest` carries them.
    #[test]
    fn collect_local_request_carries_the_scripted_answers() {
        let initializer = ConfigInitializer::with_prompter(Box::new(ScriptedPrompter::new(vec![
            Answer::Text("/tmp/my-store".into()),
            Answer::Text("/tmp/my-key.txt".into()),
            Answer::Text("my-vault".into()),
        ])));

        let request = initializer.collect_local_request().unwrap();

        match request {
            SetupRequest::Local {
                store_path,
                key_file,
                vault,
            } => {
                assert_eq!(store_path, std::path::PathBuf::from("/tmp/my-store"));
                assert_eq!(key_file, std::path::PathBuf::from("/tmp/my-key.txt"));
                assert_eq!(vault, "my-vault");
            }
            other => panic!("expected SetupRequest::Local, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn collect_backend_request_dispatches_local_through_the_same_collector() {
        let initializer = ConfigInitializer::with_prompter(Box::new(ScriptedPrompter::new(vec![
            Answer::Text("/tmp/my-store".into()),
            Answer::Text("/tmp/my-key.txt".into()),
            Answer::Text("my-vault".into()),
        ])));

        let request = initializer
            .collect_backend_request(BackendType::Local)
            .await
            .unwrap();

        assert!(matches!(request, SetupRequest::Local { .. }));
    }

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

    #[tokio::test]
    async fn azure_without_vault_keeps_the_legacy_cli_shape_only() {
        let initializer = ConfigInitializer::new();
        let init_config = InitConfig {
            subscription_id: "test-sub".to_string(),
            tenant_id: "test-tenant".to_string(),
            default_resource_group: "test-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: None,
            create_test_vault: false,
            storage_account_name: String::new(),
            blob_container_name: String::new(),
            create_storage_account: false,
        };

        let config = initializer
            .build_config(init_config, Config::default())
            .await
            .unwrap();
        assert_eq!(config.backend, None);
        assert_eq!(config.default_vault, "");
        assert_eq!(config.subscription_id, "test-sub");
        assert_eq!(config.tenant_id, "test-tenant");

        let strict_request = SetupRequest::Azure {
            subscription_id: "test-sub".into(),
            tenant_id: "test-tenant".into(),
            vault: String::new(),
            resource_group: "test-rg".into(),
            location: "eastus".into(),
        };
        assert!(build_setup_config(&strict_request, Config::default()).is_err());
    }

    #[tokio::test]
    async fn azure_with_vault_matches_the_shared_setup_builder() {
        let initializer = ConfigInitializer::new();
        let init_config = InitConfig {
            subscription_id: "test-sub".to_string(),
            tenant_id: "test-tenant".to_string(),
            default_resource_group: "test-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: Some("test-vault".to_string()),
            create_test_vault: true,
            storage_account_name: String::new(),
            blob_container_name: String::new(),
            create_storage_account: false,
        };
        let expected = build_setup_config(
            &SetupRequest::Azure {
                subscription_id: "test-sub".into(),
                tenant_id: "test-tenant".into(),
                vault: "test-vault".into(),
                resource_group: "test-rg".into(),
                location: "eastus".into(),
            },
            Config::default(),
        )
        .unwrap();

        let actual = initializer
            .build_config(init_config, Config::default())
            .await
            .unwrap();
        assert_eq!(
            toml::to_string(&actual).unwrap(),
            toml::to_string(&expected).unwrap()
        );
    }

    /// Regression test for the Bugbot finding: choosing Azure in `xv init`
    /// must not silently destroy an already-configured `[local]` block or
    /// unrelated top-level settings, matching the guarantee the local/aws
    /// init branches already have via `add_backend`. Covers both the
    /// with-vault (`build_setup_config`) and no-vault (legacy CLI shape)
    /// branches of `build_config`.
    #[tokio::test]
    async fn azure_init_preserves_a_preexisting_local_backend_and_unrelated_settings() {
        use crate::config::settings::LocalConfig;

        let initializer = ConfigInitializer::new();
        let base = Config {
            local: Some(LocalConfig {
                store_path: Some("/tmp/existing-store".into()),
                key_file: Some("/tmp/existing-key.txt".into()),
                default_vault: Some("existing".into()),
                ..Default::default()
            }),
            clipboard_timeout: 999,
            ..Config::default()
        };

        // With-vault branch.
        let init_config = InitConfig {
            subscription_id: "test-sub".to_string(),
            tenant_id: "test-tenant".to_string(),
            default_resource_group: "test-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: Some("test-vault".to_string()),
            create_test_vault: true,
            storage_account_name: String::new(),
            blob_container_name: String::new(),
            create_storage_account: false,
        };
        let config = initializer
            .build_config(init_config, base.clone())
            .await
            .unwrap();
        assert!(
            config.local.is_some(),
            "the with-vault Azure branch must preserve the existing [local] block"
        );
        assert_eq!(config.clipboard_timeout, 999);

        // No-vault (legacy) branch.
        let init_config = InitConfig {
            subscription_id: "test-sub".to_string(),
            tenant_id: "test-tenant".to_string(),
            default_resource_group: "test-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: None,
            create_test_vault: false,
            storage_account_name: String::new(),
            blob_container_name: String::new(),
            create_storage_account: false,
        };
        let config = initializer.build_config(init_config, base).await.unwrap();
        assert!(
            config.local.is_some(),
            "the no-vault legacy Azure branch must preserve the existing [local] block"
        );
        assert_eq!(config.clipboard_timeout, 999);
    }

    /// Regression test for the Bugbot finding on top of the previous fix:
    /// the no-vault branch used to leave a pre-existing `[azure]` block
    /// untouched while overwriting only the legacy top-level fields, so
    /// `azure_settings()` (which prefers the block) kept reporting stale
    /// subscription/tenant/vault values after a reconfigure.
    #[tokio::test]
    async fn azure_no_vault_reconfigure_overwrites_a_stale_azure_block() {
        use crate::config::settings::AzureConfig;

        let initializer = ConfigInitializer::new();
        let base = Config {
            azure: Some(AzureConfig {
                subscription_id: Some("stale-sub".into()),
                tenant_id: Some("stale-tenant".into()),
                default_vault: Some("stale-vault".into()),
                resource_group: Some("stale-rg".into()),
                location: Some("stale-location".into()),
            }),
            ..Config::default()
        };

        let init_config = InitConfig {
            subscription_id: "new-sub".to_string(),
            tenant_id: "new-tenant".to_string(),
            default_resource_group: "new-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: None,
            create_test_vault: false,
            storage_account_name: String::new(),
            blob_container_name: String::new(),
            create_storage_account: false,
        };

        let config = initializer.build_config(init_config, base).await.unwrap();

        let azure = config
            .azure
            .as_ref()
            .expect("no-vault branch must write a fresh [azure] block");
        assert_eq!(azure.subscription_id.as_deref(), Some("new-sub"));
        assert_eq!(azure.tenant_id.as_deref(), Some("new-tenant"));
        assert_eq!(azure.resource_group.as_deref(), Some("new-rg"));
        assert_eq!(azure.location.as_deref(), Some("eastus"));
        assert_eq!(
            azure.default_vault, None,
            "the no-vault branch declined a vault, so the block must not carry the stale one"
        );

        let settings = config.azure_settings();
        assert_eq!(settings.subscription_id.as_deref(), Some("new-sub"));
        assert_eq!(settings.tenant_id.as_deref(), Some("new-tenant"));
        assert_eq!(settings.default_vault, None);
    }

    /// Companion regression: a no-vault init with NO pre-existing `[azure]`
    /// block must still write one, so `configured_backends()` (which tests
    /// `config.azure.is_some()`) reports Azure as configured even though
    /// this path never calls `build_setup_config`/`apply_backend`.
    #[tokio::test]
    async fn azure_no_vault_init_writes_a_fresh_azure_block_when_none_existed() {
        use crate::config::backend_ops::{configured_backends, BackendType};

        let initializer = ConfigInitializer::new();
        let init_config = InitConfig {
            subscription_id: "test-sub".to_string(),
            tenant_id: "test-tenant".to_string(),
            default_resource_group: "test-rg".to_string(),
            default_location: "eastus".to_string(),
            default_vault: None,
            create_test_vault: false,
            storage_account_name: String::new(),
            blob_container_name: String::new(),
            create_storage_account: false,
        };

        let config = initializer
            .build_config(init_config, Config::default())
            .await
            .unwrap();

        assert!(
            config.azure.is_some(),
            "a fresh [azure] block must be written"
        );
        assert!(
            configured_backends(&config).contains(&BackendType::Azure),
            "azure must show up as a configured backend"
        );
    }
}
