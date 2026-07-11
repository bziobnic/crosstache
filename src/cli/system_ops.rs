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

use super::commands::{CharsetType, Cli, ResourceType};

/// One audit event as rendered by every output format. Machine formats emit
/// exactly these five fields (the pre-unification `--raw` per-entry documents
/// with `---` separators are gone — changelog-documented breaking change).
#[derive(tabled::Tabled, serde::Serialize)]
struct AuditRow {
    #[tabled(rename = "Timestamp")]
    timestamp: String,
    #[tabled(rename = "Operation")]
    operation: String,
    #[tabled(rename = "Resource")]
    resource: String,
    #[tabled(rename = "Caller")]
    caller: String,
    #[tabled(rename = "Status")]
    status: String,
}

/// Render audit rows through the shared TableFormatter: global `--format`
/// honored (JSON = array of rows), `--columns`/`--no-color` inherited, valid
/// empty machine output, human count/empty on stderr.
fn render_audit_rows(rows: &[AuditRow], config: &Config) -> Result<()> {
    use crate::utils::format::{OutputFormat, TableFormatter};

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if rows.is_empty() {
        if human_table_like {
            formatter.validate_columns::<AuditRow>()?;
            output::info(&crate::utils::list_output::empty_state_message(
                "audit log entries",
                None,
            ));
        } else {
            // Valid-empty machine output on stdout (e.g. `[]` for JSON).
            println!("{}", formatter.format_table(rows)?);
        }
        return Ok(());
    }

    if human_table_like {
        output::info(&format!(
            "{}:",
            crate::utils::list_output::count_label(
                rows.len(),
                rows.len(),
                "audit log entry",
                "audit log entries",
                None,
                false
            )
        ));
    }
    println!("{}", formatter.format_table(rows)?);
    if human_table_like {
        output::hint("Use --operation <type> to filter by operation type");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_audit_command(
    name: Option<String>,
    vault: Option<String>,
    days: u32,
    operation: Option<String>,
    resource_group_override: Option<String>,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    // Resolve the backend + vault the audit targets, PAIRED. An explicit
    // `--vault` uses the active/requested backend; otherwise the workspace
    // default ENTRY supplies BOTH the vault and its backend (they can differ
    // from the process-active backend in a multi-vault workspace, so the
    // auditor must come from the same entry as the vault — Bugbot PR #346).
    // Every backend — including Azure — surfaces its audit trail through the
    // `AuditBackend` trait; the Azure adapter honors an explicit
    // `--resource-group` override.
    let (backend, vault_name) = match vault {
        Some(v) => (
            crate::cli::vault_ops::active_or_construct_backend(registry, &config).await?,
            v,
        ),
        None => {
            let (backend, _backend_name, vault) =
                crate::cli::vault_ops::resolve_current_vault(&config, registry).await?;
            (backend, vault)
        }
    };

    // Capability check on the RESOLVED backend.
    if !backend.capabilities().has_audit {
        return Err(CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support audit logs.",
            backend.name()
        )));
    }
    let auditor = backend.audit().ok_or_else(|| {
        CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support audit logs.",
            backend.name()
        ))
    })?;

    execute_backend_audit(
        auditor,
        name,
        vault_name,
        days,
        operation,
        resource_group_override.as_deref(),
        &config,
    )
    .await
}

/// Render audit logs fetched through the backend-agnostic [`AuditBackend`]
/// trait via the shared `AuditRow` renderer (global `--format` honored).
async fn execute_backend_audit(
    auditor: &dyn crate::backend::AuditBackend,
    name: Option<String>,
    vault_name: String,
    days: u32,
    operation: Option<String>,
    resource_group: Option<&str>,
    config: &Config,
) -> Result<()> {
    // `vault_name` and `auditor` are resolved together by the caller, so the
    // audit query always runs against the backend that owns the vault.
    output::step(&format!("Fetching audit logs for {} days...", days));

    let mut events: Vec<crate::backend::AuditEvent> = if let Some(secret_name) = name {
        output::info(&format!("  Secret: {}", secret_name));
        output::info(&format!("  Vault: {}", vault_name));
        auditor
            .get_secret_events(&vault_name, &secret_name, resource_group, days)
            .await?
    } else {
        output::info(&format!("  Vault: {}", vault_name));
        auditor
            .get_vault_events(&vault_name, resource_group, days)
            .await?
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

    let rows: Vec<AuditRow> = events
        .iter()
        .map(|event| AuditRow {
            timestamp: event.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            operation: event.operation.clone(),
            resource: event.resource_name.clone(),
            caller: event.caller.clone(),
            status: event.status.clone(),
        })
        .collect();

    render_audit_rows(&rows, config)
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
    use crate::secret::models::SecretInfo;
    use chrono::{DateTime, Utc};

    // Check if we have a vault context
    let vault_name = if !config.default_vault.is_empty() {
        config.default_vault.clone()
    } else {
        return Err(CrosstacheError::config(
            "No vault context set. Use 'xv context set <vault>' to set a default vault",
        ));
    };

    // Fetch through the active backend's secret trait and reconstruct the
    // display record from the returned properties (the same tag-derived
    // groups/folder/note extraction the legacy `get_secret_info` did).
    let backend = crate::cli::vault_ops::active_or_construct_backend(registry, config).await?;
    let props = backend
        .secrets()
        .get_secret(&vault_name, secret_name, false)
        .await
        .map_err(CrosstacheError::from)?;

    let tags = props.tags.clone();
    let parse_ts = |s: &str| -> Option<DateTime<Utc>> {
        (!s.is_empty())
            .then(|| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .flatten()
    };
    let secret_info = SecretInfo {
        name: props.name.clone(),
        original_name: SecretInfo::extract_original_name(&tags)
            .or_else(|| (!props.original_name.is_empty()).then(|| props.original_name.clone())),
        id: format!("{}/secrets/{}/{}", vault_name, props.name, props.version),
        version: (!props.version.is_empty()).then(|| props.version.clone()),
        enabled: props.enabled,
        created: parse_ts(&props.created_on),
        updated: parse_ts(&props.updated_on),
        expires: props.expires_on,
        not_before: props.not_before,
        recovery_level: props.recovery_level.clone(),
        content_type: (!props.content_type.is_empty()).then(|| props.content_type.clone()),
        groups: SecretInfo::extract_groups(&tags),
        folder: SecretInfo::extract_folder(&tags),
        note: SecretInfo::extract_note(&tags),
        vault_uri: vault_name.clone(),
        version_count: None,
        tags,
    };

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

    // `whoami` is an Azure-identity command: it validates auth by acquiring an
    // Azure token and reading the JWT claims — a direct auth-provider use, not
    // a legacy manager. Reuse the provider the registry built at startup when
    // present, else build one from config with the same credential priority.
    let auth_provider = match registry.and_then(|r| r.azure_auth_provider()) {
        Some(provider) => provider,
        None => {
            use crate::auth::provider::DefaultAzureCredentialProvider;
            std::sync::Arc::new(
                DefaultAzureCredentialProvider::with_credential_priority(
                    config.azure_credential_priority.clone(),
                )
                .map_err(|e| {
                    CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
                })?,
            )
        }
    };

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

    let context_manager = ContextManager::load().await?;

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
    if let Some((path, cfg)) = crate::config::project::find_project_config(&cwd).await? {
        if let Some((name, _)) =
            crate::config::project::resolve_env(&cfg, config.env_flag.as_deref())?
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

    // Apply env-profile `group`/`folder` write-time defaults when the
    // caller didn't pass an explicit `--group`/`--folder`, exactly like
    // `xv set` — see `apply_profile_write_defaults` for the shared logic.
    let mut meta = meta.clone();
    crate::cli::secret_ops::apply_profile_write_defaults(&mut meta, config).await?;

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
        crate::cli::secret_ops::invalidate_trait_secret_cache(
            config,
            config.effective_backend_name(),
            &vault_name,
        );
        return Ok(());
    }

    // Fallback when the registry could not be built at startup: construct the
    // requested backend on demand and write through the same secret trait, so
    // the metadata (groups/note/folder/expiry) carried in `request` still
    // applies. `resolve_vault_for_trait` keeps the Azure no-vault hard error.
    let backend = crate::cli::vault_ops::active_or_construct_backend(registry, config).await?;
    let vault_name = match vault {
        Some(v) => v,
        None => resolve_vault_for_trait(config, registry).await?,
    };
    backend
        .secrets()
        .set_secret(&vault_name, request)
        .await
        .map_err(CrosstacheError::from)?;
    crate::cli::secret_ops::invalidate_trait_secret_cache(
        config,
        config.effective_backend_name(),
        &vault_name,
    );
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
            _resource_group: Option<&str>,
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
            _resource_group: Option<&str>,
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
            config,
            Some(&registry),
        )
        .await
        .expect("non-Azure auditors must not fall through to Azure Activity Log routing");

        assert_eq!(calls.vault_events.load(Ordering::SeqCst), 1);
        assert_eq!(calls.secret_events.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn render_audit_rows_table_format_includes_headers_and_cells() {
        use crate::utils::format::{OutputFormat, TableFormatter};

        let rows = vec![
            AuditRow {
                timestamp: "2024-01-15 10:30:45".to_string(),
                operation: "SecretRead".to_string(),
                resource: "mysecret".to_string(),
                caller: "user@example.com".to_string(),
                status: "Success".to_string(),
            },
            AuditRow {
                timestamp: "2024-01-15 10:31:12".to_string(),
                operation: "SecretWrite".to_string(),
                resource: "anothersecret".to_string(),
                caller: "admin@example.com".to_string(),
                status: "Success".to_string(),
            },
        ];

        let formatter = TableFormatter::new(OutputFormat::Table, true, None, None);
        let output = formatter
            .format_table(&rows)
            .expect("Table formatting should succeed");

        // Verify header is present
        assert!(
            output.contains("Timestamp"),
            "Table output should contain 'Timestamp' header"
        );

        // Verify at least one cell value is present
        assert!(
            output.contains("SecretRead") || output.contains("mysecret"),
            "Table output should contain audit row data"
        );
    }

    #[test]
    fn render_audit_rows_json_format_produces_valid_json_array() {
        use crate::utils::format::{OutputFormat, TableFormatter};

        let rows = vec![AuditRow {
            timestamp: "2024-01-15 10:30:45".to_string(),
            operation: "SecretDelete".to_string(),
            resource: "oldsecret".to_string(),
            caller: "operator@example.com".to_string(),
            status: "Success".to_string(),
        }];

        let formatter = TableFormatter::new(OutputFormat::Json, true, None, None);
        let output = formatter
            .format_table(&rows)
            .expect("JSON formatting should succeed");

        // Parse as JSON and verify structure
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("Output should be valid JSON");

        let array = parsed.as_array().expect("JSON output should be an array");
        assert_eq!(
            array.len(),
            1,
            "JSON array should have exactly 1 element matching input rows"
        );

        // Verify the object contains expected fields
        let first_obj = &array[0];
        assert_eq!(
            first_obj.get("timestamp").and_then(|v| v.as_str()),
            Some("2024-01-15 10:30:45"),
            "JSON object should contain timestamp field with correct value"
        );
        assert_eq!(
            first_obj.get("operation").and_then(|v| v.as_str()),
            Some("SecretDelete"),
            "JSON object should contain operation field with correct value"
        );
    }
}
