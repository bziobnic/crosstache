//! Vault command execution handlers.

use crate::backend::{Backend, BackendKind, BackendRegistry};
use crate::cli::commands::{VaultCommands, VaultShareCommands};
use crate::cli::helpers::{share_unsupported_error, use_vault_trait_path};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::output;
use crate::vault::VaultCreateRequest;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Materialize the active (or, when the registry failed to build, the
/// config-requested) backend as an `Arc<dyn Backend>`, so vault/secret
/// operations can run through the trait layer whether or not startup
/// managed to build the shared [`BackendRegistry`].
///
/// When `registry` is `None` (a non-fatal backend init failure at startup) it
/// constructs the requested backend on demand, so the command fails with a
/// clear construction error instead of silently dropping to a second code path.
pub(crate) async fn active_or_construct_backend(
    registry: Option<&BackendRegistry>,
    config: &Config,
) -> Result<Arc<dyn Backend>> {
    if let Some(reg) = registry {
        return Ok(reg.active_arc());
    }
    let kind = crate::cli::helpers::requested_backend_kind(config).unwrap_or(BackendKind::Azure);
    BackendRegistry::create_for_kind(kind, config)
        .await
        .map_err(|e| CrosstacheError::config(e.to_string()))
}

/// Resolve the current vault name through the unified workspace seam (the
/// degenerate workspace-of-one's default entry), replacing legacy
/// `config.resolve_vault_name(None)` call sites. `resolve_workspace` never
/// yields `None` (it synthesizes a degenerate workspace-of-one) and preserves
/// the Azure no-vault hard error, so this returns the exact same vault the
/// legacy resolver produced.
pub(crate) async fn resolve_current_vault(config: &Config) -> Result<String> {
    let ws = crate::workspace::resolve_workspace(config)
        .await?
        .ok_or_else(|| {
            CrosstacheError::config(
                "internal error: resolve_workspace returned None; the degenerate \
                 workspace-of-one must always yield Some or Err",
            )
        })?;
    Ok(ws.default_entry()?.vault.clone())
}

/// Borrow the vault sub-trait from a materialized backend, or produce the
/// standard "backend does not support vault operations" error.
fn vaults_of(backend: &dyn Backend) -> Result<&dyn crate::backend::vault::VaultBackend> {
    backend.vaults().ok_or_else(|| {
        CrosstacheError::InvalidArgument(format!(
            "The {} backend does not support vault operations.",
            backend.name()
        ))
    })
}

pub(crate) async fn execute_vault_command(
    command: VaultCommands,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Non-Azure trait path ───────────────────────────────────────────
    // Local/AWS resolve the core CRUD verbs (create/list/delete/info) here
    // through `VaultBackend`; the section below covers Azure plus the verbs
    // this branch doesn't implement (restore/purge/export/import/update/share,
    // resource-group filtering) — also through the trait.
    if use_vault_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");

        // Capability check: `vault share` needs RBAC, not vault CRUD, so it
        // is answered before resolving general vault support.
        if let VaultCommands::Share { .. } = command {
            let active = reg.active();
            if !active.capabilities().has_rbac {
                return Err(share_unsupported_error(
                    active.kind(),
                    active.name(),
                    "vault sharing",
                ));
            }
        }

        let vaults_backend = vaults_of(reg.active())?;

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
                names_only,
                page,
                page_size,
                pager,
                ..
            } => {
                use crate::utils::pagination::Pagination;

                let pager = pager
                    .map(crate::cli::commands::PagerWhen::wants_pager)
                    .unwrap_or(false);
                let vaults = vaults_backend.list_vaults(None).await?;
                let output_format = config.runtime_output_format;
                let pagination = Pagination::from_args(page, page_size)?;

                render_vault_list(
                    &vaults,
                    output_format,
                    pagination,
                    pager,
                    names_only,
                    &config,
                )?;
            }
            VaultCommands::Delete { name, force, .. } => {
                if !crate::cli::helpers::confirm_destructive(
                    force,
                    &format!("Delete vault '{name}'?"),
                )? {
                    output::info("Aborted; vault not deleted.");
                    return Ok(());
                }
                vaults_backend.delete_vault(&name, None).await?;
                output::success(&format!("Successfully deleted vault '{name}'"));
            }
            VaultCommands::Info { name, .. } => {
                let vault = vaults_backend.get_vault(&name, None).await?;
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
                        config.runtime_columns.clone(),
                    );
                    let table = formatter.format_table(&[vault])?;
                    println!("{table}");
                }
            }
            _other => {
                // Commands not yet supported on non-Azure backends
                // (Restore, Purge, Export, Import, Update; Share is answered
                // by the RBAC capability check above)
                return Err(CrosstacheError::InvalidArgument(format!(
                    "The {} backend does not support this vault command yet.",
                    reg.active().name(),
                )));
            }
        }
        return Ok(());
    }

    // Materialize the active/requested backend once. Every vault verb now runs
    // through the trait layer: create/restore/purge/list/delete/info/update via
    // `VaultBackend`, export/import via `SecretBackend`, share via `VaultBackend`
    // RBAC. Azure-specific inputs (resource-group override, safe-delete warnings,
    // vault-property display) are threaded through the trait or reproduced
    // CLI-side.
    let backend = active_or_construct_backend(registry, &config).await?;

    let vault_cache_manager = crate::cache::CacheManager::from_config(&config);

    match command {
        VaultCommands::Create {
            name,
            resource_group,
            location,
        } => {
            execute_vault_create(
                vaults_of(backend.as_ref())?,
                &name,
                resource_group,
                location,
                &config,
            )
            .await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::List {
            resource_group,
            names_only,
            no_cache,
            page,
            page_size,
            pager,
        } => {
            execute_vault_list(
                vaults_of(backend.as_ref())?,
                resource_group,
                names_only,
                no_cache,
                page,
                page_size,
                pager
                    .map(crate::cli::commands::PagerWhen::wants_pager)
                    .unwrap_or(false),
                &config,
            )
            .await?;
        }
        VaultCommands::Delete {
            name,
            resource_group,
            force,
        } => {
            execute_vault_delete(
                vaults_of(backend.as_ref())?,
                &name,
                resource_group,
                force,
                &config,
            )
            .await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::Info {
            name,
            resource_group,
        } => {
            execute_vault_info(vaults_of(backend.as_ref())?, &name, resource_group, &config)
                .await?;
        }
        VaultCommands::Restore { name, location } => {
            execute_vault_restore(vaults_of(backend.as_ref())?, &name, &location, &config).await?;
            vault_cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
        }
        VaultCommands::Purge {
            name,
            location,
            force,
        } => {
            execute_vault_purge(
                vaults_of(backend.as_ref())?,
                &name,
                &location,
                force,
                &config,
            )
            .await?;
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
                backend.as_ref(),
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
                backend.as_ref(),
                &name,
                resource_group,
                input,
                &format,
                overwrite,
                dry_run,
                &config,
            )
            .await?;
            // Invalidate the secrets list for the target vault (secrets were
            // written). Import is an Azure-legacy-only path (see
            // `use_vault_trait_path`'s doc comment above), so
            // `effective_backend_name()` is guaranteed "azure" here — used
            // rather than a hardcoded literal to keep every
            // `CacheKey::SecretsList` producer on one convention.
            vault_cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
                backend: config.effective_backend_name().to_string(),
                vault_name: name,
            });
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
                vaults_of(backend.as_ref())?,
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
            // Capability gate: vault sharing requires RBAC support. The gate is
            // `has_rbac` on the resolved backend (constructed from config above
            // even when startup init failed); `kind` only selects the message
            // text (e.g. AWS's IAM guidance) once the gate fails.
            if !backend.capabilities().has_rbac {
                return Err(share_unsupported_error(
                    backend.kind(),
                    backend.name(),
                    "vault sharing",
                ));
            }
            execute_vault_share(vaults_of(backend.as_ref())?, command, &config).await?;
        }
    }
    Ok(())
}

async fn execute_vault_create(
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
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

    // Config-derived request. The Azure adapter fills the current user as an
    // admin access policy and applies purge-protection defaults inside
    // `create_vault`; non-Azure backends ignore the Azure-only scalar fields.
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
        purge_protection: None, // Let the backend set safe defaults
        tags: Some(std::collections::HashMap::from([
            ("created_by".to_string(), "crosstache".to_string()),
            (
                "created_at".to_string(),
                chrono::Utc::now().format("%Y-%m-%d").to_string(),
            ),
        ])),
        access_policies: None, // Will be set automatically by the backend
    };

    let vault = vaults_backend.create_vault(create_request).await?;

    output::success(&format!("Successfully created vault '{}'", vault.name));
    println!("   Resource Group: {}", vault.resource_group);
    println!("   Location: {}", vault.location);
    println!("   URI: {}", vault.uri);

    output::hint(&format!(
        "Start using it with 'xv cx use {}' or 'xv set <name> <value>'",
        vault.name
    ));

    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Shared rendering for `vault list`'s cached and fresh branches: names-only
/// output, empty-state messaging (stderr for humans, valid-empty JSON/etc. on
/// stdout for machine formats), pagination, and the standard count label.
fn render_vault_list(
    vaults: &[crate::vault::models::VaultSummary],
    output_format: crate::utils::format::OutputFormat,
    pagination: crate::utils::pagination::Pagination,
    pager: bool,
    names_only: bool,
    config: &Config,
) -> Result<()> {
    use crate::utils::format::{OutputFormat, TableFormatter};
    use crate::utils::list_output::{count_label, empty_state_message};
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};

    if names_only {
        for v in vaults {
            println!("{}", v.name);
        }
        return Ok(());
    }

    let human_table_like = matches!(
        output_format,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = TableFormatter::new(
        output_format,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if vaults.is_empty() {
        if human_table_like {
            formatter.validate_columns::<crate::vault::models::VaultSummary>()?;
            crate::utils::output::info(&empty_state_message("vaults", None));
        } else {
            println!("{}", formatter.format_table(vaults)?);
        }
        return Ok(());
    }

    let page = paginate_slice(vaults, pagination);
    let mut output = formatter.format_table(&page.items)?;
    if human_table_like {
        output.push('\n');
        output.push_str(&count_label(
            page.items.len(),
            page.total_items,
            "vault",
            "vaults",
            None,
            page.page_size.is_some(),
        ));
    }
    if let Some(footer) = pagination_footer_text(&page, "vault", "vaults", output_format) {
        output.push('\n');
        output.push_str(&footer);
    }
    crate::utils::pager::print_output(&output, pager)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_list(
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
    resource_group: Option<String>,
    names_only: bool,
    no_cache: bool,
    page: Option<usize>,
    page_size: Option<usize>,
    pager: bool,
    config: &Config,
) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    use crate::utils::pagination::Pagination;
    use crate::vault::models::VaultSummary;

    let cache_manager = CacheManager::from_config(config);
    let cache_key = CacheKey::VaultList;
    let use_cache = cache_manager.is_enabled() && !no_cache;
    let output_format = config.runtime_output_format;
    let pagination = Pagination::from_args(page, page_size)?;

    if use_cache && resource_group.is_none() {
        if let Some(cached) = cache_manager.get::<Vec<VaultSummary>>(&cache_key) {
            return render_vault_list(
                &cached,
                output_format,
                pagination,
                pager,
                names_only,
                config,
            );
        }
    }

    let vaults = vaults_backend
        .list_vaults(resource_group.as_deref())
        .await?;

    if use_cache && resource_group.is_none() {
        cache_manager.set(&cache_key, &vaults);
    }

    render_vault_list(
        &vaults,
        output_format,
        pagination,
        pager,
        names_only,
        config,
    )
}

async fn execute_vault_delete(
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
    name: &str,
    resource_group: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default (kept for the
    // warning text; the trait `unwrap_or`s the same default internally).
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    // Reproduce the retired `VaultManager::delete_vault_safe` UX (soft-delete
    // warnings gated on the vault's purge-protection/retention) CLI-side, since
    // that is presentation policy rather than a backend concern.
    let vault = vaults_backend
        .get_vault(name, Some(&resource_group))
        .await?;
    if !force {
        output::warn(&format!(
            "This will soft-delete vault '{name}' in resource group '{resource_group}'"
        ));
        if vault.has_purge_protection() {
            output::warn(
                "This vault has purge protection enabled - it cannot be permanently deleted.",
            );
        } else {
            output::warn(&format!(
                "The vault will be recoverable for {} days after deletion.",
                vault.get_retention_days()
            ));
        }
    }

    vaults_backend
        .delete_vault(name, Some(&resource_group))
        .await?;

    output::success(&format!(
        "Successfully deleted vault '{name}' (soft delete)"
    ));

    Ok(())
}

/// Best-effort suggestion: if the error is `VaultNotFound`, list vaults in the
/// same resource group and find the closest name match, then return an enriched
/// error. Failures in the list call are swallowed — they must NOT change the
/// original error path.
async fn attach_vault_suggestion(
    err: CrosstacheError,
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
    resource_group: &str,
) -> CrosstacheError {
    if let CrosstacheError::VaultNotFound { name: missing, .. } = err {
        let suggestion = match vaults_backend.list_vaults(Some(resource_group)).await {
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
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
    name: &str,
    resource_group: Option<String>,
    config: &Config,
) -> Result<()> {
    // Use provided resource group or fall back to config default.
    let resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    let vault = match vaults_backend.get_vault(name, Some(&resource_group)).await {
        Ok(v) => v,
        Err(e) => {
            return Err(attach_vault_suggestion(
                CrosstacheError::from(e),
                vaults_backend,
                &resource_group,
            )
            .await)
        }
    };

    if config.output_json {
        let json_output = serde_json::to_string_pretty(&vault).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize vault info: {e}"))
        })?;
        println!("{json_output}");
    } else {
        display_vault_details(&vault, config.no_color)?;
    }

    Ok(())
}

/// Human-readable vault-properties display, relocated CLI-side from the retired
/// `VaultManager::display_vault_details` (presentation, not a backend concern).
fn display_vault_details(
    vault: &crate::vault::models::VaultProperties,
    no_color: bool,
) -> Result<()> {
    use crate::utils::format::{DisplayUtils, OutputFormat, TableFormatter};

    let du = DisplayUtils::new(no_color);
    du.print_header(&format!("Vault: {}", vault.name))?;

    let vault_uri = vault.get_vault_uri();
    let retention_days = format!("{} days", vault.soft_delete_retention_in_days);

    let details = vec![
        ("Resource ID", vault.id.as_str()),
        ("Location", vault.location.as_str()),
        ("Resource Group", vault.resource_group.as_str()),
        ("Subscription", vault.subscription_id.as_str()),
        ("Vault URI", vault_uri.as_str()),
        ("SKU", vault.sku.as_str()),
        ("Soft Delete Retention", retention_days.as_str()),
        (
            "Purge Protection",
            if vault.purge_protection {
                "Enabled"
            } else {
                "Disabled"
            },
        ),
        (
            "Deployment Access",
            if vault.enabled_for_deployment {
                "Enabled"
            } else {
                "Disabled"
            },
        ),
        (
            "Disk Encryption Access",
            if vault.enabled_for_disk_encryption {
                "Enabled"
            } else {
                "Disabled"
            },
        ),
        (
            "Template Access",
            if vault.enabled_for_template_deployment {
                "Enabled"
            } else {
                "Disabled"
            },
        ),
    ];

    let formatted_details = du.format_key_value_pairs(&details);
    println!("{formatted_details}");

    if !vault.access_policies.is_empty() {
        du.print_separator()?;
        du.print_header("Access Policies")?;

        let formatter = TableFormatter::new(OutputFormat::Table, no_color, None, None);
        let table_output = formatter.format_table(&vault.access_policies)?;
        println!("{table_output}");
    }

    if !vault.tags.is_empty() {
        du.print_separator()?;
        du.print_header("Tags")?;

        let tag_pairs: Vec<(&str, &str)> = vault
            .tags
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let formatted_tags = du.format_key_value_pairs(&tag_pairs);
        println!("{formatted_tags}");
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
    let backend = active_or_construct_backend(registry, config).await?;
    execute_vault_info(
        vaults_of(backend.as_ref())?,
        vault_name,
        resource_group,
        config,
    )
    .await
}

async fn execute_vault_restore(
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
    name: &str,
    location: &str,
    _config: &Config,
) -> Result<()> {
    // The CLI `--location` (required for vault restore/purge) identifies the
    // region the vault was soft-deleted in; thread it through so Azure targets
    // the correct region instead of the config default.
    output::info(&format!("Restoring soft-deleted vault '{name}'..."));
    vaults_backend.restore_vault(name, Some(location)).await?;
    output::success(&format!("Successfully restored vault '{name}'"));
    Ok(())
}

async fn execute_vault_purge(
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
    name: &str,
    location: &str,
    force: bool,
    _config: &Config,
) -> Result<()> {
    if !force {
        output::warn(&format!(
            "This will PERMANENTLY delete vault '{name}' and all its contents!"
        ));
        output::warn("This action cannot be undone.");
    }
    // Thread the CLI `--location` through (see restore above).
    vaults_backend.purge_vault(name, Some(location)).await?;
    output::success(&format!(
        "Successfully purged vault '{name}' (permanent deletion)"
    ));
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
    backend: &dyn Backend,
    name: &str,
    resource_group: Option<String>,
    output: Option<String>,
    format: &str,
    include_values: bool,
    group: Option<String>,
    config: &Config,
) -> Result<()> {
    let _resource_group = resource_group.unwrap_or_else(|| config.default_resource_group.clone());

    let secrets_backend = backend.secrets();

    // Get all secrets from vault (including disabled ones for export). The
    // trait `list_secrets` returns the unfiltered list, matching the legacy
    // `show_all = true` export behavior.
    let secrets = secrets_backend
        .list_secrets(name, group.as_deref())
        .await
        .map_err(CrosstacheError::from)?;

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
                    match secrets_backend
                        .get_secret(name, &secret.original_name, true)
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
                    match secrets_backend
                        .get_secret(name, &secret.original_name, true)
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
                    match secrets_backend
                        .get_secret(name, &secret.original_name, true)
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
    backend: &dyn Backend,
    name: &str,
    resource_group: Option<String>,
    input: Option<String>,
    format: &str,
    overwrite: bool,
    dry_run: bool,
    config: &Config,
) -> Result<()> {
    use crate::secret::manager::SecretRequest;
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

    // Import secrets through the active backend's secret trait.
    let secrets_backend = backend.secrets();

    let mut imported_count = 0;
    let mut skipped_count = 0;
    let mut failed_count = 0;

    for secret_request in secrets_to_import {
        let secret_name = secret_request.name.clone();

        // Check if secret exists if not overwriting
        if !overwrite {
            match secrets_backend.secret_exists(name, &secret_name).await {
                Ok(true) => {
                    output::hint(&format!("Skipping existing secret: {secret_name}"));
                    skipped_count += 1;
                    continue;
                }
                Ok(false) => {
                    // Secret doesn't exist, proceed with import
                }
                Err(_) => {
                    // Existence probe failed; fall through and let the write
                    // surface a definitive error below.
                }
            }
        }

        match secrets_backend.set_secret(name, secret_request).await {
            Ok(_) => {
                output::success(&format!("Imported secret: {secret_name}"));
                imported_count += 1;
            }
            Err(e) => {
                output::error(&format!("Failed to import secret '{secret_name}': {e}"));
                failed_count += 1;
            }
        }
    }

    // Don't dress a partial failure up as success: use a warning summary when
    // any secret failed (the non-zero exit is returned below), and reserve the
    // `[ok]` success line for a fully clean import.
    let summary =
        format!("Import completed: {imported_count} imported, {skipped_count} skipped, {failed_count} failed");
    if failed_count > 0 {
        output::warn(&summary);
    } else {
        output::success(&summary);
    }

    // Invalidate the secrets list cache for the target vault. Import is an
    // Azure-legacy-only path (see `use_vault_trait_path`'s doc comment at
    // the top of this file), so `effective_backend_name()` is guaranteed
    // "azure" here — used rather than a hardcoded literal to keep every
    // `CacheKey::SecretsList` producer on one convention.
    if imported_count > 0 {
        let cache_manager = crate::cache::CacheManager::from_config(config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
            backend: config.effective_backend_name().to_string(),
            vault_name: name.to_string(),
        });
    }

    // Any failed secret import must surface as a non-zero exit so scripted
    // imports don't silently drop secrets.
    if failed_count > 0 {
        return Err(CrosstacheError::unknown(format!(
            "vault import: {failed_count} secret(s) failed to import into vault '{name}'"
        )));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_vault_update(
    vaults_backend: &dyn crate::backend::vault::VaultBackend,
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

    let vault = vaults_backend
        .update_vault(name, Some(&resource_group), update_request)
        .await?;

    println!("Successfully updated vault '{}'", vault.name);

    Ok(())
}

/// Check that a vault is in RBAC authorization mode before performing share operations.
/// Human display string for an access level, matching the retired
/// `VaultManager` output (note `Admin` renders as "Administrator").
fn access_level_display(level: &crate::vault::models::AccessLevel) -> &'static str {
    use crate::vault::models::AccessLevel;
    match level {
        AccessLevel::Reader => "Reader",
        AccessLevel::Contributor => "Contributor",
        AccessLevel::Admin => "Administrator",
    }
}

async fn check_vault_rbac_mode(
    vault_backend: &dyn crate::backend::vault::VaultBackend,
    vault_name: &str,
    resource_group: Option<&str>,
) -> Result<()> {
    if !vault_backend
        .vault_uses_rbac(vault_name, resource_group)
        .await?
    {
        return Err(CrosstacheError::invalid_argument(format!(
            "Vault '{vault_name}' uses access policy authorization mode. \
             Vault sharing (RBAC role assignments) requires RBAC authorization mode. \
             Enable it with: az keyvault update --name {vault_name} --enable-rbac-authorization true"
        )));
    }
    Ok(())
}

async fn execute_vault_share(
    vault_backend: &dyn crate::backend::vault::VaultBackend,
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
            // `resource_group.as_deref()` overrides the backend's configured
            // default when the user passed `--resource-group`; `None` lets the
            // Azure impl fall back to the config default (behavior-preserving).
            let resource_group = resource_group.as_deref();

            check_vault_rbac_mode(vault_backend, &vault_name, resource_group).await?;

            let object_id = vault_backend.resolve_principal(&user).await?;
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

            // Output parity with the retired `VaultManager::grant_vault_access`
            // (which framed the trait call with these info/success lines).
            let access_level_str = access_level_display(&access_level);
            output::info(&format!(
                "Granting {access_level_str} access to vault '{vault_name}' for user '{user}'..."
            ));
            vault_backend
                .grant_access(&vault_name, resource_group, &object_id, access_level)
                .await?;
            output::success(&format!(
                "Successfully granted {access_level_str} access to vault '{vault_name}' for user '{user}'"
            ));
        }
        VaultShareCommands::Revoke {
            vault_name,
            user,
            resource_group,
        } => {
            let resource_group = resource_group.as_deref();

            check_vault_rbac_mode(vault_backend, &vault_name, resource_group).await?;

            let object_id = vault_backend.resolve_principal(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            output::info(&format!(
                "Revoking access to vault '{vault_name}' for user '{user}'..."
            ));
            vault_backend
                .revoke_access(&vault_name, resource_group, &object_id)
                .await?;
            output::success(&format!(
                "Successfully revoked access to vault '{vault_name}' for user '{user}'"
            ));
        }
        VaultShareCommands::List {
            vault_name,
            resource_group,
            all,
            page,
            page_size,
            pager,
        } => {
            use crate::utils::pagination::{paginate_slice, pagination_footer_text, Pagination};
            use std::fmt::Write as _;

            let pager = pager
                .map(crate::cli::commands::PagerWhen::wants_pager)
                .unwrap_or(false);
            let resource_group = resource_group.as_deref();

            check_vault_rbac_mode(vault_backend, &vault_name, resource_group).await?;

            let mut roles = vault_backend
                .list_access(&vault_name, resource_group)
                .await?;

            crate::cli::helpers::enrich_and_filter_roles(vault_backend, &mut roles, all).await;

            let fmt = config.runtime_output_format;
            let human_table_like = matches!(
                fmt,
                crate::utils::format::OutputFormat::Table
                    | crate::utils::format::OutputFormat::Plain
                    | crate::utils::format::OutputFormat::Raw
            );

            let pagination = Pagination::from_args(page, page_size)?;
            let paged = paginate_slice(&roles, pagination);

            let formatter = crate::utils::format::TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );

            if roles.is_empty() {
                if human_table_like {
                    formatter.validate_columns::<crate::vault::models::VaultRole>()?;
                    output::info(&format!(
                        "No access assignments found for vault '{vault_name}'"
                    ));
                } else {
                    println!("{}", formatter.format_table(&paged.items)?);
                }
            } else {
                let table_output = formatter.format_table(&paged.items)?;
                let mut output = String::new();
                if human_table_like {
                    let _ = writeln!(output, "Access assignments for vault '{vault_name}':");
                }
                output.push_str(&table_output);
                if human_table_like {
                    output.push('\n');
                    output.push_str(&crate::utils::list_output::count_label(
                        paged.items.len(),
                        paged.total_items,
                        "assignment",
                        "assignments",
                        None,
                        paged.page_size.is_some(),
                    ));
                }
                if let Some(footer) =
                    pagination_footer_text(&paged, "assignment", "assignments", fmt)
                {
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
