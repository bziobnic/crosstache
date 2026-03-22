//! Vault command execution handlers.

use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
use crate::cli::commands::{VaultCommands, VaultShareCommands};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use crate::vault::{VaultCreateRequest, VaultManager};
use std::sync::Arc;
use zeroize::Zeroizing;

pub(crate) async fn execute_vault_command(command: VaultCommands, config: Config) -> Result<()> {
    // Create authentication provider with credential priority from config
    let auth_provider: Arc<dyn AzureAuthProvider> = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

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
        } => {
            execute_vault_list(&vault_manager, resource_group, format, no_cache, &config).await?;
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

async fn execute_vault_list(
    vault_manager: &VaultManager,
    resource_group: Option<String>,
    format: OutputFormat,
    no_cache: bool,
    config: &Config,
) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    use crate::utils::format::TableFormatter;
    use crate::vault::models::VaultSummary;

    let cache_manager = CacheManager::from_config(config);
    let cache_key = CacheKey::VaultList;
    let use_cache = cache_manager.is_enabled() && !no_cache;
    let output_format = format.resolve_for_stdout();

    if use_cache && resource_group.is_none() {
        if let Some(cached) = cache_manager.get::<Vec<VaultSummary>>(&cache_key) {
            if cached.is_empty() {
                output::info("No vaults found.");
            } else {
                let formatter = TableFormatter::new(output_format, config.no_color, config.template.clone());
                println!("{}", formatter.format_table(&cached)?);
            }
            return Ok(());
        }
    }

    let vaults = vault_manager
        .list_vaults_formatted(
            Some(&config.subscription_id),
            resource_group.as_deref(),
            output_format,
            config.template.clone(),
        )
        .await?;

    if use_cache && resource_group.is_none() {
        cache_manager.set(&cache_key, &vaults);
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

async fn execute_vault_info(
    vault_manager: &VaultManager,
    name: &str,
    resource_group: Option<String>,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    if config.output_json {
        let vault = vault_manager
            .get_vault_properties(name, &resource_group)
            .await?;
        let json_output = serde_json::to_string_pretty(&vault).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize vault info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let _vault = vault_manager.get_vault_info(name, &resource_group).await?;
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
) -> Result<()> {
    // Create authentication provider
    let auth_provider = Arc::new(DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )?);

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
) -> Result<()> {
    use crate::secret::manager::SecretManager;

    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Create secret manager to get secrets from vault
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
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
                                env_lines.push(format!("{env_name}={}", value.as_str()));
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
                    (line[..pos].trim().to_lowercase().replace("_", "-"), line[pos + 1..].trim())
                } else if let Some(pos) = line.find(':') {
                    // KEY: VALUE format
                    (line[..pos].trim().to_lowercase().replace("_", "-"), line[pos + 1..].trim())
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
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
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
                    )))
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
        } => {
            let resource_group =
                resource_group.unwrap_or_else(|| config.default_resource_group.clone());

            check_vault_rbac_mode(vault_manager, &vault_name, &resource_group).await?;

            let mut roles = vault_manager
                .list_vault_access_raw(&vault_name, &resource_group)
                .await?;

            vault_manager
                .resolve_and_filter_roles(&mut roles, all)
                .await?;

            if roles.is_empty() {
                output::info(&format!(
                    "No access assignments found for vault '{vault_name}'"
                ));
            } else {
                if format.resolve_for_stdout() == crate::utils::format::OutputFormat::Table {
                    println!("Access assignments for vault '{vault_name}':");
                }
                let formatter =
                    crate::utils::format::TableFormatter::new(format, config.no_color, config.template.clone());
                let table_output = formatter.format_table(&roles)?;
                println!("{table_output}");
            }
        }
    }

    Ok(())
}
