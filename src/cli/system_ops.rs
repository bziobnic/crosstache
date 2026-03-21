//! Audit, system, and utility command execution logic.
//!
//! Extracted from `commands.rs` — pure mechanical move, no logic changes.

use crate::cli::helpers::{
    copy_to_clipboard, extract_claims_from_token, generate_random_value, schedule_clipboard_clear,
};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::output;
use clap::CommandFactory;
use clap_complete::Shell;
use serde::{Deserialize, Serialize};

use super::commands::{CharsetType, Cli, ResourceType};

/// Azure Activity Log entry for audit purposes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AuditLogEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub operation: String,
    pub resource_name: String,
    pub resource_type: String,
    pub caller: String,
    pub status: String,
    pub correlation_id: String,
    pub vault_name: Option<String>,
    pub subscription_id: String,
    pub resource_group: String,
    pub properties: serde_json::Value,
}

impl std::fmt::Display for AuditLogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} | {} | {} | {} | {}",
            self.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
            self.operation,
            self.resource_name,
            self.caller,
            self.status
        )
    }
}

/// Azure Activity Log client for fetching audit data
pub struct AzureActivityLogClient {
    auth_provider: std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
}

impl AzureActivityLogClient {
    pub fn new(
        auth_provider: std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
    ) -> Self {
        Self { auth_provider }
    }

    /// Fetch audit logs for a specific vault
    pub async fn get_vault_audit_logs(
        &self,
        subscription_id: &str,
        resource_group: &str,
        vault_name: &str,
        days: u32,
    ) -> Result<Vec<AuditLogEntry>> {
        let end_time = chrono::Utc::now();
        let start_time = end_time - chrono::Duration::days(days as i64);

        let start_time_str = start_time.format("%Y-%m-%dT%H:%M:%S.%3fZ");
        let end_time_str = end_time.format("%Y-%m-%dT%H:%M:%S.%3fZ");

        // Build the Azure Activity Log API URL
        let activity_url = format!(
            "https://management.azure.com/subscriptions/{}/providers/microsoft.insights/eventtypes/management/values?api-version=2015-04-01&$filter=eventTimestamp ge '{}' and eventTimestamp le '{}' and resourceUri eq '/subscriptions/{}/resourceGroups/{}/providers/Microsoft.KeyVault/vaults/{}'",
            subscription_id, start_time_str, end_time_str, subscription_id, resource_group, vault_name
        );

        // Get access token from auth provider
        let token = self
            .auth_provider
            .get_token(&["https://management.azure.com/.default"])
            .await?;

        // Make HTTP request
        let client = reqwest::Client::new();
        let response = client
            .get(&activity_url)
            .header("Authorization", format!("Bearer {}", token.token.secret()))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| {
                CrosstacheError::network(format!("Failed to fetch activity logs: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(CrosstacheError::azure_api(format!(
                "Activity Log API returned {}: {}",
                status, error_text
            )));
        }

        let activity_response: serde_json::Value = response.json().await.map_err(|e| {
            CrosstacheError::serialization(format!("Failed to parse activity logs: {}", e))
        })?;

        // Parse the Azure Activity Log response
        self.parse_activity_log_response(activity_response, vault_name)
    }

    /// Fetch audit logs for a specific secret
    pub async fn get_secret_audit_logs(
        &self,
        subscription_id: &str,
        resource_group: &str,
        vault_name: &str,
        secret_name: &str,
        days: u32,
    ) -> Result<Vec<AuditLogEntry>> {
        // Get all vault logs and filter for the specific secret
        let vault_logs = self
            .get_vault_audit_logs(subscription_id, resource_group, vault_name, days)
            .await?;

        let secret_logs: Vec<AuditLogEntry> = vault_logs
            .into_iter()
            .filter(|log| {
                log.resource_name.contains(secret_name)
                    || log.properties.get("secretName").and_then(|v| v.as_str())
                        == Some(secret_name)
            })
            .collect();

        Ok(secret_logs)
    }

    /// Parse Azure Activity Log API response
    fn parse_activity_log_response(
        &self,
        response: serde_json::Value,
        vault_name: &str,
    ) -> Result<Vec<AuditLogEntry>> {
        let mut entries = Vec::new();

        if let Some(value) = response.get("value").and_then(|v| v.as_array()) {
            for event in value {
                if let Ok(entry) = self.parse_activity_log_entry(event, vault_name) {
                    entries.push(entry);
                }
            }
        }

        // Sort by timestamp (newest first)
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok(entries)
    }

    /// Parse individual activity log entry
    fn parse_activity_log_entry(
        &self,
        event: &serde_json::Value,
        vault_name: &str,
    ) -> Result<AuditLogEntry> {
        let timestamp = event
            .get("eventTimestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .ok_or_else(|| CrosstacheError::serialization("Invalid timestamp in activity log"))?;

        let operation = event
            .get("operationName")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let resource_name = event
            .get("resourceId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let resource_type = event
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("Microsoft.KeyVault/vaults")
            .to_string();

        let caller = event
            .get("caller")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let status = event
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or(
                event
                    .get("subStatus")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
            )
            .to_string();

        let correlation_id = event
            .get("correlationId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let subscription_id = event
            .get("subscriptionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let resource_group = event
            .get("resourceGroupName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let properties = event
            .get("properties")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        Ok(AuditLogEntry {
            timestamp,
            operation,
            resource_name,
            resource_type,
            caller,
            status,
            correlation_id,
            vault_name: Some(vault_name.to_string()),
            subscription_id,
            resource_group,
            properties,
        })
    }
}

pub(crate) async fn execute_audit_command(
    name: Option<String>,
    vault: Option<String>,
    days: u32,
    operation: Option<String>,
    resource_group_override: Option<String>,
    raw: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use std::sync::Arc;

    // Create authentication provider
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {}", e))
        })?,
    );

    // Create audit log client
    let audit_client = AzureActivityLogClient::new(auth_provider);

    // Determine vault and context
    let (vault_name, resource_group, subscription_id) = if let Some(vault_name) = vault {
        // Use specified vault, need to get resource group and subscription
        let rg = resource_group_override
            .clone()
            .unwrap_or_else(|| config.default_resource_group.clone());
        let sub = config.subscription_id.clone();

        if rg.is_empty() {
            return Err(CrosstacheError::config(
                "No resource group specified. Use --resource-group or 'xv init' to configure",
            ));
        }

        (vault_name, rg, sub)
    } else {
        // Use current vault context
        let vault_name = config.resolve_vault_name(None).await?;
        let rg = resource_group_override.unwrap_or_else(|| config.default_resource_group.clone());
        let sub = config.subscription_id.clone();

        if rg.is_empty() {
            return Err(CrosstacheError::config(
                "No resource group specified. Use --resource-group or 'xv init' to configure",
            ));
        }

        (vault_name, rg, sub)
    };

    output::step(&format!("Fetching audit logs for {} days...", days));

    // Fetch audit logs
    let mut logs = if let Some(secret_name) = name {
        println!("  Secret: {}", secret_name);
        println!("  Vault: {}", vault_name);
        audit_client
            .get_secret_audit_logs(
                &subscription_id,
                &resource_group,
                &vault_name,
                &secret_name,
                days,
            )
            .await?
    } else {
        println!("  Vault: {}", vault_name);
        audit_client
            .get_vault_audit_logs(&subscription_id, &resource_group, &vault_name, days)
            .await?
    };

    // Filter by operation if specified
    if let Some(op_filter) = operation {
        logs.retain(|log| {
            log.operation
                .to_lowercase()
                .contains(&op_filter.to_lowercase())
        });
    }

    if logs.is_empty() {
        output::info("No audit log entries found for the specified criteria");
        return Ok(());
    }

    println!();
    output::info(&format!("Found {} audit log entries:\n", logs.len()));

    if raw {
        // Show raw JSON output
        for log in logs {
            let json_output = serde_json::to_string_pretty(&log).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize log entry: {}", e))
            })?;
            println!("{}", json_output);
            println!("---");
        }
    } else {
        // Show formatted output
        println!(
            "{:<20} | {:<25} | {:<20} | {:<30} | {:<10}",
            "Timestamp", "Operation", "Resource", "Caller", "Status"
        );
        println!("{}", "-".repeat(120));

        for log in logs {
            // Extract resource name (last part after /)
            let resource_display = log
                .resource_name
                .split('/')
                .next_back()
                .unwrap_or(&log.resource_name);

            // Truncate long strings for better display
            let operation = if log.operation.len() > 25 {
                format!("{}...", &log.operation[..22])
            } else {
                log.operation.clone()
            };

            let caller = if log.caller.len() > 30 {
                format!("{}...", &log.caller[..27])
            } else {
                log.caller.clone()
            };

            let resource = if resource_display.len() > 20 {
                format!("{}...", &resource_display[..17])
            } else {
                resource_display.to_string()
            };

            println!(
                "{:<20} | {:<25} | {:<20} | {:<30} | {:<10}",
                log.timestamp.format("%m-%d %H:%M:%S"),
                operation,
                resource,
                caller,
                log.status
            );
        }

        println!();
        output::hint(
            "Use --raw to see full details, or --operation <type> to filter by operation type",
        );
    }

    Ok(())
}

pub(crate) async fn execute_init_command(_config: Config) -> Result<()> {
    use crate::config::init::ConfigInitializer;
    use crate::config::settings::Config as SettingsConfig;

    // Warn if config already exists
    if let Ok(config_path) = SettingsConfig::get_config_path() {
        if config_path.exists() {
            output::warn(&format!(
                "Configuration already exists at {}",
                config_path.display()
            ));
            output::hint("This will overwrite your existing configuration.");
            let prompt = crate::utils::interactive::InteractivePrompt::new();
            if !prompt.confirm("Continue with re-initialization?", false)? {
                output::info("Init cancelled. Existing configuration preserved.");
                return Ok(());
            }
        }
    }

    // Create the initializer and run the interactive setup
    let initializer = ConfigInitializer::new();
    let new_config = initializer.run_interactive_setup().await?;

    // Show setup summary
    initializer.show_setup_summary(&new_config)?;

    Ok(())
}

pub(crate) async fn execute_info_command(
    resource: String,
    resource_type: Option<ResourceType>,
    resource_group: Option<String>,
    subscription: Option<String>,
    config: Config,
) -> Result<()> {
    use crate::utils::resource_detector::ResourceDetector;

    // Detect the resource type
    let detected_type =
        ResourceDetector::detect_resource_type(&resource, resource_type, resource_group.is_some());

    // If auto-detected and verbose, show why we detected it
    if resource_type.is_none() && config.debug {
        let reason = ResourceDetector::get_detection_reason(
            &resource,
            detected_type,
            resource_group.is_some(),
        );
        eprintln!("Auto-detected resource type: {detected_type} ({reason})");
    }

    // Route to the appropriate handler
    match detected_type {
        ResourceType::Vault => {
            crate::cli::vault_ops::execute_vault_info_from_root(&resource, resource_group, subscription, &config).await
        }
        ResourceType::Secret => execute_secret_info_from_root(&resource, &config).await,
        #[cfg(feature = "file-ops")]
        ResourceType::File => {
            crate::cli::file_ops::execute_file_info_from_root(&resource, &config).await
        }
    }
}

/// Execute secret info from root info command
async fn execute_secret_info_from_root(secret_name: &str, config: &Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    // Check if we have a vault context
    let vault_name = if !config.default_vault.is_empty() {
        &config.default_vault
    } else {
        return Err(CrosstacheError::config(
            "No vault context set. Use 'xv context set <vault>' to set a default vault",
        ));
    };

    // Create authentication provider
    let auth_provider = Arc::new(DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )?);

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Get secret info
    let secret_info = secret_manager
        .get_secret_info(vault_name, secret_name)
        .await?;

    // Display based on output format
    if config.output_json {
        let json_output = serde_json::to_string_pretty(&secret_info).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize secret info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        println!("{secret_info}");
    }

    Ok(())
}

pub(crate) async fn execute_version_command() -> Result<()> {
    let build_info = super::commands::get_build_info();

    println!("crosstache Rust CLI");
    println!("===================");
    println!("Version:      {}", build_info.version);
    println!("Git Hash:     {}", build_info.git_hash);
    println!("Git Branch:   {}", build_info.git_branch);

    Ok(())
}

pub(crate) async fn execute_completion_command(shell: Shell) -> Result<()> {
    use clap_complete::generate;
    use std::io;

    let mut cmd = Cli::command();
    let name = "xv";

    generate(shell, &mut cmd, name, &mut io::stdout());

    Ok(())
}

pub(crate) async fn execute_whoami_command(config: Config) -> Result<()> {
    use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
    use crate::config::ContextManager;

    output::step("Checking authentication and context...\n");

    // Create authentication provider
    let auth_provider = DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )
    .map_err(|e| CrosstacheError::authentication(format!("Failed to create auth provider: {e}")))?;

    // Get access token to validate authentication
    let token = match auth_provider
        .get_token(&["https://vault.azure.net/.default"])
        .await
    {
        Ok(token) => token,
        Err(e) => {
            output::error(&format!("Authentication failed: {}", e));
            return Ok(());
        }
    };

    output::success("Authentication successful\n");

    // Try to get tenant and subscription information
    let management_token = auth_provider
        .get_token(&["https://management.azure.com/.default"])
        .await
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to get management token: {e}"))
        })?;

    // Parse token to get identity info (from JWT)
    let token_claims = extract_claims_from_token(token.token.secret())?;

    output::step("Identity Information:");
    if let Some(ref name) = token_claims.name {
        println!("   Name: {}", name);
    }
    if let Some(ref email) = token_claims.email {
        println!("   Email: {}", email);
    }
    if token_claims.name.is_none() && token_claims.email.is_none() {
        if let Some(ref oid) = token_claims.object_id {
            println!("   Object ID: {}", oid);
        }
    }

    // Get tenant name
    if let Some(ref tid) = token_claims.tenant_id {
        let tenant_display = match get_tenant_name(management_token.token.secret(), tid).await {
            Ok(name) => format!("{} ({})", name, tid),
            Err(_) => tid.clone(),
        };
        println!("   Tenant: {}", tenant_display);
    }

    // Get subscription information with name
    match get_current_subscription_details(management_token.token.secret()).await {
        Ok((sub_id, sub_name)) => {
            println!("   Subscription: {} ({})", sub_name, sub_id);
        }
        Err(_) => {
            println!("   Subscription: Unable to determine");
        }
    }

    // Show current context information
    println!();
    output::info("Context Information:");

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if let Some(current_vault) = context_manager.current_vault() {
        println!("   Current Vault: {}", current_vault);
    } else {
        println!("   Current Vault: None set");
    }

    if let Some(current_sub) = context_manager.current_subscription_id() {
        println!("   Current Subscription: {}", current_sub);
    } else {
        println!("   Current Subscription: None set");
    }

    // Show recent vaults
    let recent_contexts = context_manager.list_recent();
    if !recent_contexts.is_empty() {
        println!();
        output::info("Recent Vaults:");
        for context in recent_contexts.iter().take(5) {
            println!(
                "   {} (last used: {})",
                context.vault_name,
                context.last_used.format("%Y-%m-%d %H:%M:%S")
            );
        }
    }

    println!();
    output::step("Configuration:");
    println!("   Configured Default Vault: {}", config.default_vault);
    println!("   Default subscription: {}", config.subscription_id);
    println!("   No color mode: {}", config.no_color);
    println!(
        "   Credential priority: {:?}",
        config.azure_credential_priority
    );

    Ok(())
}

/// Resolve a tenant ID to its display name via the Azure management API.
async fn get_tenant_name(token: &str, _tenant_id: &str) -> Result<String> {
    use crate::utils::network::{create_http_client, NetworkConfig};

    let network_config = NetworkConfig::default();
    let http_client = create_http_client(&network_config)?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", token)
            .parse()
            .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
    );

    let response = http_client
        .get("https://management.azure.com/tenants?api-version=2020-01-01")
        .headers(headers)
        .send()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to get tenants: {e}")))?;

    if !response.status().is_success() {
        return Err(CrosstacheError::azure_api(
            "Failed to get tenant information",
        ));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to parse response: {e}")))?;

    if let Some(tenants) = json["value"].as_array() {
        for tenant in tenants {
            if tenant["tenantId"].as_str() == Some(_tenant_id) {
                if let Some(name) = tenant["displayName"].as_str() {
                    return Ok(name.to_string());
                }
            }
        }
    }

    Err(CrosstacheError::azure_api("Tenant name not found"))
}

/// Get the current subscription ID and display name.
async fn get_current_subscription_details(token: &str) -> Result<(String, String)> {
    use crate::utils::network::{create_http_client, NetworkConfig};

    let network_config = NetworkConfig::default();
    let http_client = create_http_client(&network_config)?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", token)
            .parse()
            .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
    );

    let response = http_client
        .get("https://management.azure.com/subscriptions?api-version=2020-01-01")
        .headers(headers)
        .send()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to get subscriptions: {e}")))?;

    if !response.status().is_success() {
        return Err(CrosstacheError::azure_api(
            "Failed to get subscription information",
        ));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to parse response: {e}")))?;

    if let Some(subscriptions) = json["value"].as_array() {
        if let Some(first_sub) = subscriptions.first() {
            let sub_id = first_sub["subscriptionId"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let sub_name = first_sub["displayName"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            return Ok((sub_id, sub_name));
        }
    }

    Err(CrosstacheError::azure_api("No subscriptions found"))
}

pub(crate) async fn execute_gen_command(
    length: usize,
    charset: Option<CharsetType>,
    save: Option<String>,
    vault: Option<String>,
    raw: bool,
    config: Config,
) -> Result<()> {
    // Validate length
    if !(6..=100).contains(&length) {
        return Err(CrosstacheError::invalid_argument(
            "Length must be between 6 and 100",
        ));
    }

    // Resolve charset: CLI flag → config default → Alphanumeric
    let resolved_charset = if let Some(c) = charset {
        c
    } else if let Some(ref s) = config.gen_default_charset {
        s.parse::<CharsetType>().map_err(|e| {
            CrosstacheError::config(format!(
                "Invalid value for config key 'gen_default_charset': {e}"
            ))
        })?
    } else {
        CharsetType::Alphanumeric
    };

    // Generate the password
    let password = generate_random_value(length, resolved_charset, None)?;

    // Handle --save
    if let Some(ref name) = save {
        use crate::auth::provider::DefaultAzureCredentialProvider;
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
        let vault_name = config.resolve_vault_name(vault).await?;

        // Print raw password first so it appears before set_secret_safe's info messages
        if raw {
            println!("{}", password.as_str());
        }

        match secret_manager
            .set_secret_safe(&vault_name, name, password.as_str(), None)
            .await
        {
            Ok(_) => {
                if raw {
                    // Password already printed above
                } else {
                    match copy_to_clipboard(password.as_str()) {
                        Ok(()) => {
                            let timeout = config.clipboard_timeout;
                            if timeout > 0 {
                                output::success(&format!(
                                    "Secret '{name}' saved and copied to clipboard (auto-clears in {timeout}s)"
                                ));
                                schedule_clipboard_clear(timeout);
                            } else {
                                output::success(&format!(
                                    "Secret '{name}' saved and copied to clipboard"
                                ));
                            }
                        }
                        Err(e) => {
                            output::warn(&format!("Failed to copy to clipboard: {e}"));
                            println!("{}", password.as_str());
                        }
                    }
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to save secret '{name}': {e}"));
                output::warn("Generated password (save this now):");
                println!("{}", password.as_str());
            }
        }
        return Ok(());
    }

    // No --save — just output
    if raw {
        println!("{}", password.as_str());
    } else {
        match copy_to_clipboard(password.as_str()) {
            Ok(()) => {
                let timeout = config.clipboard_timeout;
                if timeout > 0 {
                    output::success(&format!(
                        "Password copied to clipboard (auto-clears in {timeout}s)"
                    ));
                    schedule_clipboard_clear(timeout);
                } else {
                    output::success("Password copied to clipboard");
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to copy to clipboard: {e}"));
                output::hint("Use --raw to print the value to stdout instead.");
                println!("{}", password.as_str());
            }
        }
    }

    Ok(())
}
