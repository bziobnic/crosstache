//! Vault command execution handlers.

use crate::auth::provider::AzureAuthProvider;
use crate::backend::BackendRegistry;
use crate::cli::commands::{VaultCommands, VaultShareCommands};
use crate::cli::helpers::{get_azure_auth_provider, use_vault_trait_path};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use crate::vault::{VaultCreateRequest, VaultManager};
use std::sync::Arc;
use zeroize::Zeroizing;

pub(crate) async fn execute_vault_command(
    command: VaultCommands,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends only) ─────────────────────
    // Azure vault operations are not yet ported to the trait layer — they use
    // the legacy VaultManager path below.
    if use_vault_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vaults_backend = reg.active().vaults().ok_or_else(|| {
            CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support vault operations.",
                reg.active().name()
            ))
        })?;

        match command {
            VaultCommands::Create { name, .. } => {
                let request = crate::vault::models::VaultCreateRequest {
                    name: name.clone(),
                    location: String::new(),
                    resource_group: String::new(),
                    subscription_id: String::new(),
                    sku: None,
                    enabled_for_deployment: None,
                    enabled_for_disk_encryption: None,
                    enabled_for_template_deployment: None,
                    soft_delete_retention_in_days: None,
                    purge_protection: None,
                    tags: None,
                    access_policies: None,
                };
                let vault = vaults_backend.create_vault(request).await?;
                output::success(&format!("Successfully created vault '{}'", vault.name));
            }
            VaultCommands::List {
                format,
                page,
                page_size,
                pager,
                ..
            } => {
                use crate::utils::format::TableFormatter;
                use crate::utils::pagination::{
                    paginate_slice, pagination_footer_text, Pagination,
                };

                let vaults = vaults_backend.list_vaults().await?;
                let output_format = format.resolve_for_stdout();
                let pagination = Pagination::from_args(page, page_size)?;

                if vaults.is_empty() {
                    output::info("No vaults found.");
                } else {
                    let page_data = paginate_slice(&vaults, pagination);
                    let formatter = TableFormatter::new(
                        output_format,
                        config.no_color,
                        config.template.clone(),
                    );
                    let mut out = formatter.format_table(&page_data.items)?;
                    if let Some(footer) = pagination_footer_text(&page_data, "vault", output_format)
                    {
                        out.push('\n');
                        out.push_str(&footer);
                    }
                    crate::utils::pager::print_output(&out, pager)?;
                }
            }
            VaultCommands::Delete { name, force, .. } => {
                if !force {
                    output::warn(&format!(
                        "About to delete vault '{name}'. Use --force to confirm."
                    ));
                    return Ok(());
                }
                vaults_backend.delete_vault(&name).await?;
                output::success(&format!("Successfully deleted vault '{name}'"));
            }
            VaultCommands::Info { name, .. } => {
                let vault = vaults_backend.get_vault(&name).await?;
                if config.output_json {
                    let json = serde_json::to_string_pretty(&vault).map_err(|e| {
                        CrosstacheError::serialization(format!(
                            "Failed to serialize vault info: {e}"
                        ))
                    })?;
                    println!("{json}");
                } else {
                    use crate::utils::format::TableFormatter;
                    let formatter = TableFormatter::new(
                        config.runtime_output_format,
                        config.no_color,
                        config.template.clone(),
                    );
                    let table = formatter.format_table(&[vault])?;
                    println!("{table}");
                }
            }
            _other => {
                // Commands not yet supported on non-Azure backends
                // (Restore, Purge, Export, Import, Update, Share)
                return Err(CrosstacheError::InvalidArgument(format!(
                    "The {} backend does not support this vault command yet.",
                    reg.active().name(),
                )));
            }
        }
        return Ok(());
    }

    // ── Azure legacy path (unchanged) ─────────────────────────────────
    // Create authentication provider — reuse from registry when available
    let auth_provider: Arc<dyn AzureAuthProvider> = get_azure_auth_provider(registry, &config)?;

    // Create vault manager
    let vault_manager = VaultManager::new(
        auth_provider.clone(),
        config.subscription_id.clone(),
        config.no_color,
    )?;

    let vault_cache_manager = crate::cache::CacheManager::from_config(&config);

    match command {
        VaultCommands::Create {
            name,
            resource_group,
            location,
        } => {
            execute_vault_create(&vault_manager, &name, resource_group, location, &config).await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::List {
            resource_group,
            format,
            no_cache,
            page,
            page_size,
            pager,
        } => {
            execute_vault_list(
                &vault_manager,
                resource_group,
                format,
                no_cache,
                page,
                page_size,
                pager,
                &config,
            )
            .await?;
        }
        VaultCommands::Delete {
            name,
            resource_group,
            force,
        } => {
            execute_vault_delete(&vault_manager, &name, resource_group, force, &config).await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::Info {
            name,
            resource_group,
        } => {
            execute_vault_info(&vault_manager, &name, resource_group, &config).await?;
        }
        VaultCommands::Restore { name, location } => {
            execute_vault_restore(&vault_manager, &name, &location, &config).await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::Purge {
            name,
            location,
            force,
        } => {
            execute_vault_purge(&vault_manager, &name, &location, force, &config).await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::Export {
            name,
            resource_group,
            output,
            format,
            include_values,
            group,
        } => {
            execute_vault_export(
                &vault_manager,
                &name,
                resource_group,
                output,
                &format,
                include_values,
                group,
                &config,
                registry,
            )
            .await?;
        }
        VaultCommands::Import {
            name,
            resource_group,
            input,
            format,
            overwrite,
            dry_run,
        } => {
            execute_vault_import(
                &vault_manager,
                &name,
                resource_group,
                input,
                &format,
                overwrite,
                dry_run,
                &config,
                registry,
            )
            .await?;
            // Invalidate the secrets list for the target vault (secrets were written)
            vault_cache_manager
                .invalidate(&crate::cache::CacheKey::SecretsList { vault_name: name });
        }
        VaultCommands::Update {
            name,
            resource_group,
            tag,
            enable_deployment,
            enable_disk_encryption,
            enable_template_deployment,
            enable_purge_protection,
            retention_days,
        } => {
            execute_vault_update(
                &vault_manager,
                &name,
                resource_group,
                tag,
                enable_deployment,
                enable_disk_encryption,
                enable_template_deployment,
                enable_purge_protection,
                retention_days,
                &config,
            )
            .await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::Share { command } => {
            // Capability check: vault sharing requires RBAC support
            if let Some(registry) = registry {
                let caps = registry.active().capabilities();
                if !caps.has_rbac {
                    return Err(CrosstacheError::InvalidArgument(format!(
                        "The {} backend does not support vault sharing. Use the azure backend for RBAC.",
                        registry.active().name()
                    )));
                }
            }
            execute_vault_share(&vault_manager, &auth_provider, command, &config).await?;
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

    println!(
        "Creating vault '{name}' in resource group '{resource_group}' at location '{location}'..."
    );

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
        purge_protection: None, // Let the manager set safe defaults
        tags: Some(std::collections::HashMap::from([
            ("created_by".to_string(), "crosstache".to_string()),
            (
                "created_at".to_string(),
                chrono::Utc::now().format("%Y-%m-%d").to_string(),
            ),
        ])),
        access_policies: None, // Will be set automatically by the manager
    };

    let vault = vault_manager
        .create_vault_with_setup(name, &location, &resource_group, Some(create_request))
        .await?;

    output::success(&format!("Successfully created vault '{}'", vault.name));
    println!("   Resource Group: {}", vault.resource_group);
    println!("   Location: {}", vault.location);
    println!("   URI: {}", vault.uri);

    output::hint(&format!(
        "Start using it with 'xv use {}' or 'xv set <name> <value>'",
        vault.name
    ));

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_list(
    vault_manager: &VaultManager,
    resource_group: Option<String>,
    format: OutputFormat,
    no_cache: bool,
    page: Option<usize>,
    page_size: Option<usize>,
    pager: bool,
    config: &Config,
) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    use crate::utils::format::TableFormatter;
    use crate::utils::pagination::{paginate_slice, pagination_footer_text, Pagination};
    use crate::vault::models::VaultSummary;

    let cache_manager = CacheManager::from_config(config);
    let cache_key = CacheKey::VaultList;
    let use_cache = cache_manager.is_enabled() && !no_cache;
    let output_format = format.resolve_for_stdout();
    let pagination = Pagination::from_args(page, page_size)?;

    if use_cache && resource_group.is_none() {
        if let Some(cached) = cache_manager.get::<Vec<VaultSummary>>(&cache_key) {
            if cached.is_empty() {
                output::info("No vaults found.");
            } else {
                let page = paginate_slice(&cached, pagination);
                let formatter =
                    TableFormatter::new(output_format, config.no_color, config.template.clone());
                let mut output = formatter.format_table(&page.items)?;
                if let Some(footer) = pagination_footer_text(&page, "vault", output_format) {
                    output.push('\n');
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
            return Ok(());
        }
    }

    let vaults = vault_manager
        .list_vaults(Some(&config.subscription_id), resource_group.as_deref())
        .await?;

    if use_cache && resource_group.is_none() {
        cache_manager.set(&cache_key, &vaults);
    }

    if vaults.is_empty() {
        output::info("No vaults found.");
    } else {
        let page = paginate_slice(&vaults, pagination);
        let formatter =
            TableFormatter::new(output_format, config.no_color, config.template.clone());
        let mut output = formatter.format_table(&page.items)?;
        if let Some(footer) = pagination_footer_text(&page, "vault", output_format) {
            output.push('\n');
            output.push_str(&footer);
        }
        crate::utils::pager::print_output(&output, pager)?;
    }

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

    vault_manager
        .delete_vault_safe(name, &resource_group, force)
        .await?;

    Ok(())
}

/// Best-effort suggestion: if the error is `VaultNotFound`, list vaults in the
/// same resource group and find the closest name match, then return an enriched
/// error. Failures in the list call are swallowed — they must NOT change the
/// original error path.
async fn attach_vault_suggestion(
    err: CrosstacheError,
    vault_manager: &VaultManager,
    resource_group: &str,
) -> CrosstacheError {
    if let CrosstacheError::VaultNotFound { name: missing, .. } = err {
        let suggestion = match vault_manager
            .vault_ops()
            .list_vaults(None, Some(resource_group))
            .await
        {
            Ok(summaries) => {
                let candidates: Vec<String> = summaries.into_iter().map(|s| s.name).collect();
                crate::utils::suggestions::closest_match(&missing, &candidates)
                    .map(|s| s.to_string())
            }
            Err(e) => {
                tracing::debug!("suggestion list-call failed: {e}");
                None
            }
        };
        CrosstacheError::vault_not_found(missing).with_suggestion(suggestion)
    } else {
        err
    }
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
        let vault = match vault_manager
            .get_vault_properties(name, &resource_group)
            .await
        {
            Ok(v) => v,
            Err(e) => return Err(attach_vault_suggestion(e, vault_manager, &resource_group).await),
        };
        let json_output = serde_json::to_string_pretty(&vault).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize vault info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let _vault = match vault_manager.get_vault_info(name, &resource_group).await {
            Ok(v) => v,
            Err(e) => return Err(attach_vault_suggestion(e, vault_manager, &resource_group).await),
        };
        // Display will be handled by the vault manager
    }

    Ok(())
}

/// Execute vault info from root info command
pub(crate) async fn execute_vault_info_from_root(
    vault_name: &str,
    resource_group: Option<String>,
    _subscription: Option<String>,
    config: &Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Create authentication provider — reuse from registry when available
    let auth_provider = get_azure_auth_provider(registry, config)?;

    // Create vault manager
    let vault_manager = VaultManager::new(
        auth_provider,
        config.subscription_id.clone(),
        config.no_color,
    )?;

    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Call the existing vault info function
    execute_vault_info(&vault_manager, vault_name, Some(resource_group), config).await
}

async fn execute_vault_restore(
    vault_manager: &VaultManager,
    name: &str,
    location: &str,
    _config: &Config,
) -> Result<()> {
    vault_manager.restore_vault(name, location).await?;
    Ok(())
}

async fn execute_vault_purge(
    vault_manager: &VaultManager,
    name: &str,
    location: &str,
    force: bool,
    _config: &Config,
) -> Result<()> {
    vault_manager
        .purge_vault_permanent(name, location, force)
        .await?;
    Ok(())
}

/// Quote a value for POSIX shell single-quoting: wrap in `'...'` with
/// embedded single quotes escaped as `'\''`. Round-trips byte-for-byte
/// through `sh` `source`/`eval`.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A valid shell identifier: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn format_env_line(key: &str, value: &str) -> String {
    format!("{key}={}", shell_single_quote(value))
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_export(
    _vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    output: Option<String>,
    format: &str,
    include_values: bool,
    group: Option<String>,
    config: &Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::secret::manager::SecretManager;

    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Create secret manager to get secrets from vault
    let auth_provider = get_azure_auth_provider(registry, config)?;
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Get all secrets from vault (including disabled ones for export)
    let secrets = secret_manager
        .list_secrets_formatted(
            name,
            group.as_deref(),
            OutputFormat::Json,
            false,
            true, // show_all = true for export
        )
        .await?;

    // Prepare export data based on format
    let export_data = match format.to_lowercase().as_str() {
        "json" => {
            let mut export_json = serde_json::Map::new();
            export_json.insert(
                "vault".to_string(),
                serde_json::Value::String(name.to_string()),
            );
            export_json.insert(
                "exported_at".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );

            let mut secrets_json = Vec::new();
            for secret in &secrets {
                let mut secret_data = serde_json::Map::new();
                secret_data.insert(
                    "name".to_string(),
                    serde_json::Value::String(secret.original_name.clone()),
                );
                secret_data.insert(
                    "enabled".to_string(),
                    serde_json::Value::Bool(secret.enabled),
                );
                secret_data.insert(
                    "content_type".to_string(),
                    serde_json::Value::String(secret.content_type.clone()),
                );

                if include_values {
                    // Get actual secret value
                    match secret_manager
                        .get_secret_safe(name, &secret.original_name, true, true)
                        .await
                    {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                secret_data.insert(
                                    "value".to_string(),
                                    serde_json::Value::String(value.to_string()),
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get value for secret '{}': {}",
                                secret.original_name, e
                            );
                        }
                    }
                }

                secrets_json.push(serde_json::Value::Object(secret_data));
            }
            export_json.insert(
                "secrets".to_string(),
                serde_json::Value::Array(secrets_json),
            );

            serde_json::to_string_pretty(&export_json).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize export data: {e}"))
            })?
        }
        "env" => {
            let mut env_lines = Vec::new();
            env_lines.push(format!(
                "# Exported from vault '{}' on {}",
                name,
                chrono::Utc::now().to_rfc3339()
            ));

            for secret in &secrets {
                if include_values {
                    match secret_manager
                        .get_secret_safe(name, &secret.original_name, true, true)
                        .await
                    {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                let env_name = secret
                                    .original_name
                                    .to_uppercase()
                                    .replace("-", "_")
                                    .replace(".", "_");
                                if is_valid_env_key(&env_name) {
                                    env_lines.push(format_env_line(&env_name, value.as_str()));
                                } else {
                                    eprintln!(
                                        "Warning: Skipping secret '{}' — derived env name '{}' is not a valid shell identifier",
                                        secret.original_name, env_name
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get value for secret '{}': {}",
                                secret.original_name, e
                            );
                        }
                    }
                } else {
                    let env_name = secret
                        .original_name
                        .to_uppercase()
                        .replace("-", "_")
                        .replace(".", "_");
                    env_lines.push(format!("# {env_name}"));
                }
            }

            env_lines.join("\n")
        }
        "txt" => {
            let mut txt_lines = Vec::new();
            txt_lines.push(format!("Vault: {name}"));
            txt_lines.push(format!("Exported: {}", chrono::Utc::now().to_rfc3339()));
            txt_lines.push("".to_string());

            for secret in &secrets {
                txt_lines.push(format!("Secret: {}", secret.original_name));
                txt_lines.push(format!("  Enabled: {}", secret.enabled));
                txt_lines.push(format!("  Content Type: {}", secret.content_type));
                txt_lines.push(format!("  Updated: {}", secret.updated_on));

                if include_values {
                    match secret_manager
                        .get_secret_safe(name, &secret.original_name, true, true)
                        .await
                    {
                        Ok(secret_props) => {
                            if let Some(value) = secret_props.value {
                                txt_lines.push(format!("  Value: {}", value.as_str()));
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get value for secret '{}': {}",
                                secret.original_name, e
                            );
                        }
                    }
                }
                txt_lines.push("".to_string());
            }

            txt_lines.join("\n")
        }
        _ => {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported export format: {format}"
            )));
        }
    };

    // Write to output
    match output {
        Some(file_path) => {
            crate::utils::helpers::write_sensitive_file(
                std::path::Path::new(&file_path),
                export_data.as_bytes(),
            )
            .map_err(|e| {
                CrosstacheError::unknown(format!("Failed to write to output file: {e}"))
            })?;
            println!(
                "Exported {} secrets to {} (permissions: owner-only)",
                secrets.len(),
                file_path
            );
        }
        None => {
            println!("{export_data}");
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_import(
    _vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    input: Option<String>,
    format: &str,
    overwrite: bool,
    dry_run: bool,
    config: &Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::secret::manager::{SecretManager, SecretRequest};
    use std::fs;
    use std::io::{self, Read};

    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Read import data
    let import_data = match input {
        Some(file_path) => fs::read_to_string(file_path)
            .map_err(|e| CrosstacheError::unknown(format!("Failed to read input file: {e}")))?,
        None => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| CrosstacheError::unknown(format!("Failed to read from stdin: {e}")))?;
            buffer
        }
    };

    // Parse import data based on format
    let secrets_to_import = match format.to_lowercase().as_str() {
        "json" => {
            let json_data: serde_json::Value = serde_json::from_str(&import_data).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to parse JSON: {e}"))
            })?;

            let secrets_array = json_data
                .get("secrets")
                .and_then(|s| s.as_array())
                .ok_or_else(|| CrosstacheError::serialization("Missing 'secrets' array in JSON"))?;

            let mut secrets = Vec::new();
            for secret_value in secrets_array {
                let secret_obj = secret_value.as_object().ok_or_else(|| {
                    CrosstacheError::serialization("Invalid secret object in JSON")
                })?;

                let name = secret_obj
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| CrosstacheError::serialization("Missing secret name"))?;

                let value = match secret_obj.get("value").and_then(|v| v.as_str()) {
                    Some(v) => v,
                    None => {
                        eprintln!(
                            "Warning: Skipping secret '{}' — no value in export (was it exported with --include-values?)",
                            name
                        );
                        continue;
                    }
                };

                let content_type = secret_obj
                    .get("content_type")
                    .and_then(|ct| ct.as_str())
                    .map(|s| s.to_string());

                let enabled = secret_obj.get("enabled").and_then(|e| e.as_bool());

                secrets.push(SecretRequest {
                    name: name.to_string(),
                    value: Zeroizing::new(value.to_string()),
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
                        value: Zeroizing::new(value.to_string()),
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
        "txt" => {
            // txt format: one KEY=VALUE or KEY: VALUE per line, comments (#) and blank lines ignored
            let mut secrets = Vec::new();
            for line in import_data.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                let (key, value) = if let Some(pos) = line.find('=') {
                    // KEY=VALUE format
                    (
                        line[..pos].trim().to_lowercase().replace("_", "-"),
                        line[pos + 1..].trim(),
                    )
                } else if let Some(pos) = line.find(':') {
                    // KEY: VALUE format
                    (
                        line[..pos].trim().to_lowercase().replace("_", "-"),
                        line[pos + 1..].trim(),
                    )
                } else {
                    continue; // Skip lines without a separator
                };

                if key.is_empty() || value.is_empty() {
                    continue;
                }

                secrets.push(SecretRequest {
                    name: key,
                    value: Zeroizing::new(value.to_string()),
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

            secrets
        }
        _ => {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported import format: '{format}'. Supported formats: json, env, txt"
            )));
        }
    };

    if dry_run {
        output::info(&format!(
            "Dry run: Would import {} secrets to vault '{}':",
            secrets_to_import.len(),
            name
        ));
        for secret in &secrets_to_import {
            println!("  - {}", secret.name);
        }
        return Ok(());
    }

    // Create secret manager to import secrets
    let auth_provider = get_azure_auth_provider(registry, config)?;
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    let mut imported_count = 0;
    let mut skipped_count = 0;

    for secret_request in secrets_to_import {
        let secret_name = secret_request.name.clone();
        let secret_value = secret_request.value.clone();

        // Check if secret exists if not overwriting
        if !overwrite {
            match secret_manager
                .get_secret_safe(name, &secret_name, false, true)
                .await
            {
                Ok(_) => {
                    output::hint(&format!("Skipping existing secret: {secret_name}"));
                    skipped_count += 1;
                    continue;
                }
                Err(_) => {
                    // Secret doesn't exist, proceed with import
                }
            }
        }

        match secret_manager
            .set_secret_safe(name, &secret_name, &secret_value, Some(secret_request))
            .await
        {
            Ok(_) => {
                output::success(&format!("Imported secret: {secret_name}"));
                imported_count += 1;
            }
            Err(e) => {
                output::error(&format!("Failed to import secret '{secret_name}': {e}"));
            }
        }
    }

    output::success(&format!(
        "Import completed: {imported_count} imported, {skipped_count} skipped"
    ));

    // Invalidate the secrets list cache for the target vault
    if imported_count > 0 {
        let cache_manager = crate::cache::CacheManager::from_config(config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
            vault_name: name.to_string(),
        });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    use crate::vault::models::VaultUpdateRequest;
    use std::collections::HashMap;

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

    let vault = vault_manager
        .update_vault(name, &resource_group, &update_request)
        .await?;

    println!("Successfully updated vault '{}'", vault.name);

    Ok(())
}

/// Check that a vault is in RBAC authorization mode before performing share operations.
async fn check_vault_rbac_mode(
    vault_manager: &VaultManager,
    vault_name: &str,
    resource_group: &str,
) -> Result<()> {
    let props = vault_manager
        .get_vault_properties(vault_name, resource_group)
        .await?;
    if props.enable_rbac_authorization != Some(true) {
        return Err(CrosstacheError::invalid_argument(format!(
            "Vault '{vault_name}' uses access policy authorization mode. \
             Vault sharing (RBAC role assignments) requires RBAC authorization mode. \
             Enable it with: az keyvault update --name {vault_name} --enable-rbac-authorization true"
        )));
    }
    Ok(())
}

async fn execute_vault_share(
    vault_manager: &VaultManager,
    auth_provider: &Arc<dyn AzureAuthProvider>,
    command: VaultShareCommands,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::AccessLevel;

    match command {
        VaultShareCommands::Grant {
            vault_name,
            user,
            resource_group,
            level,
        } => {
            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            check_vault_rbac_mode(vault_manager, &vault_name, &resource_group).await?;

            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            let access_level = match level.to_lowercase().as_str() {
                "reader" | "read" => AccessLevel::Reader,
                "contributor" | "write" => AccessLevel::Contributor,
                "admin" | "administrator" => AccessLevel::Admin,
                _ => {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "Invalid access level: {level}"
                    )));
                }
            };

            vault_manager
                .grant_vault_access(
                    &vault_name,
                    &resource_group,
                    &object_id,
                    access_level,
                    Some(&user),
                )
                .await?;
        }
        VaultShareCommands::Revoke {
            vault_name,
            user,
            resource_group,
        } => {
            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            check_vault_rbac_mode(vault_manager, &vault_name, &resource_group).await?;

            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            vault_manager
                .revoke_vault_access(&vault_name, &resource_group, &object_id, Some(&user))
                .await?;
        }
        VaultShareCommands::List {
            vault_name,
            resource_group,
            format,
            all,
            page,
            page_size,
            pager,
        } => {
            use crate::utils::pagination::{paginate_slice, pagination_footer_text, Pagination};
            use std::fmt::Write as _;

            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            check_vault_rbac_mode(vault_manager, &vault_name, &resource_group).await?;

            let mut roles = vault_manager
                .list_vault_access_raw(&vault_name, &resource_group)
                .await?;

            vault_manager
                .resolve_and_filter_roles(&mut roles, all)
                .await?;

            let pagination = Pagination::from_args(page, page_size)?;
            let paged = paginate_slice(&roles, pagination);

            if roles.is_empty() {
                output::info(&format!(
                    "No access assignments found for vault '{vault_name}'"
                ));
            } else {
                let formatter = crate::utils::format::TableFormatter::new(
                    format,
                    config.no_color,
                    config.template.clone(),
                );
                let table_output = formatter.format_table(&paged.items)?;
                let mut output = String::new();
                if format.resolve_for_stdout() == crate::utils::format::OutputFormat::Table {
                    let _ = writeln!(output, "Access assignments for vault '{vault_name}':");
                }
                output.push_str(&table_output);
                if let Some(footer) = pagination_footer_text(&paged, "assignment", format) {
                    output.push('\n');
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{format_env_line, is_valid_env_key, shell_single_quote};

    fn adversarial_values() -> Vec<(&'static str, &'static str)> {
        vec![
            ("V_NEWLINE", "line1\nline2"),
            ("V_HASH", "abc#def"),
            ("V_DOLLAR", "$HOME and ${PATH}"),
            ("V_SINGLE_QUOTE", "it's a 'test'"),
            ("V_DOUBLE_QUOTE", "say \"hello\""),
            ("V_BACKSLASH", "back\\slash\\\\double"),
            ("V_SPACES", "  leading and trailing  "),
            ("V_EMPTY", ""),
            ("V_BACKTICK", "`whoami`"),
            ("V_MIXED", "a'b\"c$d`e\\f\ng#h"),
        ]
    }

    #[test]
    fn quote_newline() {
        assert_eq!(shell_single_quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn quote_hash() {
        assert_eq!(shell_single_quote("a#b"), "'a#b'");
    }

    #[test]
    fn quote_dollar() {
        assert_eq!(shell_single_quote("$HOME"), "'$HOME'");
    }

    #[test]
    fn quote_single_quote() {
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_double_quote() {
        assert_eq!(shell_single_quote("a\"b"), "'a\"b'");
    }

    #[test]
    fn quote_backslash() {
        assert_eq!(shell_single_quote("a\\b"), "'a\\b'");
    }

    #[test]
    fn quote_spaces() {
        assert_eq!(shell_single_quote(" a b "), "' a b '");
    }

    #[test]
    fn quote_empty() {
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn format_env_line_quotes_value() {
        assert_eq!(format_env_line("KEY", "v'al"), "KEY='v'\\''al'");
    }

    #[test]
    fn valid_env_keys() {
        assert!(is_valid_env_key("FOO"));
        assert!(is_valid_env_key("_FOO"));
        assert!(is_valid_env_key("FOO_BAR_2"));
        assert!(is_valid_env_key("f"));
    }

    #[test]
    fn invalid_env_keys() {
        assert!(!is_valid_env_key(""));
        assert!(!is_valid_env_key("2FOO"));
        assert!(!is_valid_env_key("FOO-BAR"));
        assert!(!is_valid_env_key("FOO BAR"));
        assert!(!is_valid_env_key("FOO.BAR"));
        assert!(!is_valid_env_key("FOO$"));
    }

    #[cfg(unix)]
    #[test]
    fn round_trip_through_sh() {
        use std::process::Command;

        for (key, value) in adversarial_values() {
            let script = format!("{}\nprintf %s \"${key}\"", format_env_line(key, value));
            let output = Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("failed to run sh");
            assert!(
                output.status.success(),
                "sh failed for {key}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.stdout,
                value.as_bytes(),
                "round-trip mismatch for {key}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn round_trip_whole_file_via_dot_source() {
        use std::io::Write;
        use std::process::Command;

        let values = adversarial_values();
        let mut file_body = String::from("# Exported from vault 'test' on 2026-01-01\n");
        for (key, value) in &values {
            file_body.push_str(&format_env_line(key, value));
            file_body.push('\n');
        }

        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(file_body.as_bytes()).expect("write");

        for (key, value) in &values {
            let script = format!(". {}\nprintf %s \"${key}\"", tmp.path().display());
            let output = Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("failed to run sh");
            assert!(
                output.status.success(),
                "sourcing failed for {key}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                output.stdout,
                value.as_bytes(),
                "round-trip mismatch for {key}"
            );
        }
    }
}
