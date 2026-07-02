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
            subscription_id,
            start_time_str,
            end_time_str,
            subscription_id,
            resource_group,
            vault_name
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
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));

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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_audit_command(
    name: Option<String>,
    vault: Option<String>,
    days: u32,
    operation: Option<String>,
    resource_group_override: Option<String>,
    raw: bool,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use std::sync::Arc;

    // Capability check: audit requires audit log support
    if let Some(registry) = registry {
        let caps = registry.active().capabilities();
        if !caps.has_audit {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support audit logs.",
                registry.active().name()
            )));
        }
        // Backends that implement the audit trait are dispatched generically.
        // For Azure specifically, keep the legacy Activity Log path when the
        // caller passes an explicit `--resource-group` override or when no
        // default resource group is configured (the legacy path produces a
        // clear ConfigError for that).  Non-Azure backends always use the
        // trait path — `--resource-group` is an Azure-only concept.
        let use_legacy_azure_path = registry.active().kind() == crate::backend::BackendKind::Azure
            && (resource_group_override.is_some() || config.default_resource_group.is_empty());

        if !use_legacy_azure_path {
            if let Some(auditor) = registry.active().audit() {
                return execute_backend_audit(auditor, name, vault, days, operation, raw, config)
                    .await;
            }
        }
    }

    // Create authentication provider — reuse from registry when available
    let auth_provider: Arc<dyn crate::auth::provider::AzureAuthProvider> =
        crate::cli::helpers::get_azure_auth_provider(registry, &config)?;

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
        output::info(&crate::utils::list_output::empty_state_message(
            "audit log entries",
            None,
        ));
        return Ok(());
    }

    println!();
    output::info(&format!(
        "{}:\n",
        crate::utils::list_output::count_label(
            logs.len(),
            logs.len(),
            "audit log entry",
            "audit log entries",
            None,
            false
        )
    ));

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

/// Render audit logs fetched through the backend-agnostic [`AuditBackend`]
/// trait, mirroring the Azure Activity Log output shapes (table and `--raw`
/// JSON).
async fn execute_backend_audit(
    auditor: &dyn crate::backend::AuditBackend,
    name: Option<String>,
    vault: Option<String>,
    days: u32,
    operation: Option<String>,
    raw: bool,
    config: Config,
) -> Result<()> {
    let vault_name = config.resolve_vault_name(vault).await?;

    output::step(&format!("Fetching audit logs for {} days...", days));

    let mut events: Vec<crate::backend::AuditEvent> = if let Some(secret_name) = name {
        println!("  Secret: {}", secret_name);
        println!("  Vault: {}", vault_name);
        auditor
            .get_secret_events(&vault_name, &secret_name, days)
            .await?
    } else {
        println!("  Vault: {}", vault_name);
        auditor.get_vault_events(&vault_name, days).await?
    };

    // Filter by operation if specified
    if let Some(op_filter) = operation {
        events.retain(|event| {
            event
                .operation
                .to_lowercase()
                .contains(&op_filter.to_lowercase())
        });
    }

    if events.is_empty() {
        output::info(&crate::utils::list_output::empty_state_message(
            "audit log entries",
            None,
        ));
        return Ok(());
    }

    println!();
    output::info(&format!(
        "{}:\n",
        crate::utils::list_output::count_label(
            events.len(),
            events.len(),
            "audit log entry",
            "audit log entries",
            None,
            false
        )
    ));

    if raw {
        // Show raw JSON output
        for event in events {
            let json_output = serde_json::to_string_pretty(&event).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize log entry: {}", e))
            })?;
            println!("{}", json_output);
            println!("---");
        }
    } else {
        // Show formatted output (same shape as the Azure table)
        println!(
            "{:<20} | {:<25} | {:<20} | {:<30} | {:<10}",
            "Timestamp", "Operation", "Resource", "Caller", "Status"
        );
        println!("{}", "-".repeat(120));

        for event in events {
            let operation = truncate_column(&event.operation, 25);
            let resource = truncate_column(&event.resource_name, 20);
            let caller = truncate_column(&event.caller, 30);

            println!(
                "{:<20} | {:<25} | {:<20} | {:<30} | {:<10}",
                event.timestamp.format("%m-%d %H:%M:%S"),
                operation,
                resource,
                caller,
                event.status
            );
        }

        println!();
        output::hint(
            "Use --raw to see full details, or --operation <type> to filter by operation type",
        );
    }

    Ok(())
}

/// Truncate a table cell to `max` characters, appending `...` when cut.
fn truncate_column(value: &str, max: usize) -> String {
    if value.chars().count() > max {
        let cut: String = value.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    } else {
        value.to_string()
    }
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
    registry: Option<&crate::backend::BackendRegistry>,
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
            crate::cli::vault_ops::execute_vault_info_from_root(
                &resource,
                resource_group,
                subscription,
                &config,
                registry,
            )
            .await
        }
        ResourceType::Secret => execute_secret_info_from_root(&resource, &config, registry).await,
        #[cfg(feature = "file-ops")]
        ResourceType::File => {
            crate::cli::file_ops::execute_file_info_from_root(&resource, &config).await
        }
    }
}

/// Execute secret info from root info command
async fn execute_secret_info_from_root(
    secret_name: &str,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::secret::manager::SecretManager;

    // Check if we have a vault context
    let vault_name = if !config.default_vault.is_empty() {
        &config.default_vault
    } else {
        return Err(CrosstacheError::config(
            "No vault context set. Use 'xv context set <vault>' to set a default vault",
        ));
    };

    // Create authentication provider
    let auth_provider = crate::cli::helpers::get_azure_auth_provider(registry, config)?;

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

    // P0.3: List compiled-in backends so users can see whether aws is available.
    let mut backends = vec!["azure", "local"];
    if cfg!(feature = "aws") {
        backends.push("aws");
    }

    println!("crosstache Rust CLI");
    println!("===================");
    println!("Version:      {}", build_info.version);
    println!("Git Hash:     {}", build_info.git_hash);
    println!("Git Ref:      {}", build_info.git_ref);
    println!("Backends:     {}", backends.join(", "));

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

pub(crate) async fn execute_whoami_command(
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::config::ContextManager;

    output::step("Checking authentication and context...\n");

    // Create authentication provider — reuse from registry when available
    let auth_provider = crate::cli::helpers::get_azure_auth_provider(registry, &config)?;

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

    let cwd = std::env::current_dir()?;
    if let Ok(Some((path, cfg))) = crate::config::project::find_project_config(&cwd).await {
        if let Ok((name, _)) = crate::config::project::resolve_env(&cfg, config.env_flag.as_deref())
        {
            println!();
            println!("   Active env: {name} (from {})", path.display());
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_gen_command(
    length: usize,
    charset: Option<CharsetType>,
    save: Option<String>,
    vault: Option<String>,
    raw: bool,
    meta: crate::cli::commands::SecretWriteArgs,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    // Validate length
    if !(6..=100).contains(&length) {
        return Err(CrosstacheError::invalid_argument(
            "Length must be between 6 and 100",
        ));
    }

    // Metadata flags only make sense when saving — there is nothing to attach
    // them to otherwise. Reject early with a clear message instead of silently
    // ignoring them.
    if save.is_none() && meta.has_any() {
        return Err(CrosstacheError::invalid_argument(
            "--group/--note/--folder/--expires/--not-before require --save (there is no \
             secret to attach metadata to without it)",
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
        // Print raw password first so it appears before any info messages
        if raw {
            println!("{}", password.as_str());
        }

        let save_result =
            save_generated_secret(name, password.as_str(), vault, &meta, &config, registry).await;

        match save_result {
            Ok(()) => {
                if !raw {
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
                if !raw {
                    output::warn("Generated password (save this now):");
                    println!("{}", password.as_str());
                }
                // `gen --save` was asked to persist the secret; the save failed,
                // so exit non-zero (after printing the value for recovery) rather
                // than reporting success.
                return Err(CrosstacheError::unknown(format!(
                    "failed to save generated secret '{name}': {e}"
                )));
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

/// Persist a generated secret with full write-time metadata, routing through
/// the same code paths `xv set` uses:
///   - trait backend (local/aws and Azure-via-registry) → `set_secret`
///   - legacy Azure fallback (no registry) → `set_secret_safe` with options
///
/// This is what makes `gen --save` a true superset of `set`: groups, note,
/// folder, expires, and not-before are all carried through, instead of the
/// old behavior that dropped every piece of metadata.
async fn save_generated_secret(
    name: &str,
    value: &str,
    vault: Option<String>,
    meta: &crate::cli::commands::SecretWriteArgs,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::cli::helpers::{resolve_vault_for_trait, use_trait_path};

    let request = meta.to_secret_request(name, zeroize::Zeroizing::new(value.to_string()))?;

    // Trait path: any backend exposed through the registry (local, aws, and
    // Azure once it has a registry). A --vault flag overrides the resolved
    // context/config vault.
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = match vault {
            Some(v) => v,
            None => resolve_vault_for_trait(config, registry).await?,
        };
        reg.active()
            .secrets()
            .set_secret(&vault_name, request)
            .await?;
        crate::cli::secret_ops::invalidate_trait_secret_cache(config, &vault_name);
        return Ok(());
    }

    // Legacy Azure fallback (registry construction failed / skipped): create
    // an auth provider on demand and use set_secret_safe with the metadata
    // options so groups/note/folder/expiry still apply.
    use crate::secret::manager::SecretManager;
    let auth_provider = crate::cli::helpers::get_azure_auth_provider(registry, config)?;
    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    let vault_name = config.resolve_vault_name(vault).await?;
    secret_manager
        .set_secret_safe(&vault_name, name, value, Some(request))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use async_trait::async_trait;

    use super::*;
    use crate::backend::{
        AuditBackend, AuditEvent, Backend, BackendCapabilities, BackendError, BackendKind,
        BackendRegistry, NameCharset, SecretBackend,
    };
    use crate::secret::manager::{
        SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
    };

    struct StubSecretBackend;

    #[async_trait]
    impl SecretBackend for StubSecretBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!("audit routing must not call secret operations")
        }

        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!("audit routing must not call secret operations")
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!("audit routing must not call secret operations")
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<SecretSummary>, BackendError> {
            unreachable!("audit routing must not call secret operations")
        }

        async fn delete_secret(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            unreachable!("audit routing must not call secret operations")
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            unreachable!("audit routing must not call secret operations")
        }
    }

    #[derive(Clone, Default)]
    struct StubAuditCalls {
        vault_events: Arc<AtomicUsize>,
        secret_events: Arc<AtomicUsize>,
    }

    struct StubAuditBackend {
        calls: StubAuditCalls,
    }

    #[async_trait]
    impl AuditBackend for StubAuditBackend {
        async fn get_vault_events(
            &self,
            vault: &str,
            days: u32,
        ) -> std::result::Result<Vec<AuditEvent>, BackendError> {
            assert_eq!(vault, "test-vault");
            assert_eq!(days, 7);
            self.calls.vault_events.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }

        async fn get_secret_events(
            &self,
            _vault: &str,
            _secret_name: &str,
            _days: u32,
        ) -> std::result::Result<Vec<AuditEvent>, BackendError> {
            self.calls.secret_events.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    struct StubAuditedBackend {
        secret_backend: StubSecretBackend,
        audit_backend: StubAuditBackend,
    }

    impl StubAuditedBackend {
        fn new(calls: StubAuditCalls) -> Self {
            Self {
                secret_backend: StubSecretBackend,
                audit_backend: StubAuditBackend { calls },
            }
        }
    }

    #[async_trait]
    impl Backend for StubAuditedBackend {
        fn name(&self) -> &'static str {
            "aws"
        }

        fn kind(&self) -> BackendKind {
            BackendKind::Aws
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                has_audit: true,
                name_charset: NameCharset::AwsRelaxed,
                ..BackendCapabilities::default()
            }
        }

        fn secrets(&self) -> &dyn SecretBackend {
            &self.secret_backend
        }

        fn audit(&self) -> Option<&dyn AuditBackend> {
            Some(&self.audit_backend)
        }

        async fn health_check(&self) -> std::result::Result<(), BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn non_azure_audit_with_resource_group_uses_backend_trait() {
        let calls = StubAuditCalls::default();
        let registry = BackendRegistry::new(Arc::new(StubAuditedBackend::new(calls.clone())));
        let config = Config {
            default_vault: "test-vault".to_string(),
            default_resource_group: "default-rg".to_string(),
            ..Config::default()
        };

        execute_audit_command(
            None,
            Some("test-vault".to_string()),
            7,
            None,
            Some("ignored-for-non-azure".to_string()),
            false,
            config,
            Some(&registry),
        )
        .await
        .expect("non-Azure auditors must not fall through to Azure Activity Log routing");

        assert_eq!(calls.vault_events.load(Ordering::SeqCst), 1);
        assert_eq!(calls.secret_events.load(Ordering::SeqCst), 0);
    }
}
