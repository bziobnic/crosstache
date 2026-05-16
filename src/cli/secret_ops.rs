//! Secret command execution handlers.

use crate::backend::{BackendKind, BackendRef, BackendRegistry};
use crate::cli::commands::{CharsetType, ShareCommands};
use crate::cli::helpers::{
    copy_to_clipboard, generate_random_value, get_azure_auth_provider, mask_secrets,
    resolve_vault_for_trait, schedule_clipboard_clear, use_trait_path,
};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::SecretManager;
use crate::utils::format::OutputFormat;
use crate::utils::output;
use crate::utils::pagination::Pagination;
use std::sync::Arc;
use zeroize::Zeroizing;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_set_direct(
    args: Vec<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    expires: Option<String>,
    not_before: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;

        // Parse expiry dates if provided
        let expires_on = if let Some(expires_str) = expires.as_deref() {
            use crate::utils::datetime::parse_datetime_or_duration;
            Some(parse_datetime_or_duration(expires_str)?)
        } else {
            None
        };
        let not_before_on = if let Some(not_before_str) = not_before.as_deref() {
            use crate::utils::datetime::parse_datetime_or_duration;
            Some(parse_datetime_or_duration(not_before_str)?)
        } else {
            None
        };

        if args.len() == 1 && !args[0].contains('=') {
            // Single secret set
            let name = &args[0];
            let value = if stdin {
                let mut buffer = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
                buffer.trim().to_string()
            } else {
                rpassword::prompt_password(format!("Enter value for secret '{name}': "))?
            };
            if value.is_empty() {
                return Err(CrosstacheError::config("Secret value cannot be empty"));
            }
            let request = crate::secret::manager::SecretRequest {
                name: name.to_string(),
                value: Zeroizing::new(value),
                content_type: None,
                enabled: Some(true),
                expires_on,
                not_before: not_before_on,
                tags: None,
                groups: None,
                note,
                folder,
            };
            let props = reg
                .active()
                .secrets()
                .set_secret(&vault_name, request)
                .await?;
            output::success(&format!(
                "Successfully set secret '{}'",
                props.original_name
            ));
            println!("   Vault: {vault_name}");
            println!("   Version: {}", props.version);
            output::hint(&format!("Verify with 'xv get {}'", props.original_name));
        } else {
            // Bulk set
            if stdin {
                return Err(CrosstacheError::invalid_argument(
                    "--stdin cannot be used with bulk set operation",
                ));
            }
            if expires.is_some() || not_before.is_some() {
                return Err(CrosstacheError::invalid_argument(
                    "--expires and --not-before cannot be used with bulk set operation",
                ));
            }
            let pairs = parse_bulk_set_args(args)?;
            output::step(&format!(
                "Setting {} secret(s) in vault '{}'...",
                pairs.len(),
                vault_name
            ));
            let mut success_count = 0usize;
            let mut error_count = 0usize;
            for (key, value) in pairs {
                let request = crate::secret::manager::SecretRequest {
                    name: key.clone(),
                    value: Zeroizing::new(value),
                    content_type: None,
                    enabled: Some(true),
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: note.clone(),
                    folder: folder.clone(),
                };
                match reg
                    .active()
                    .secrets()
                    .set_secret(&vault_name, request)
                    .await
                {
                    Ok(props) => {
                        output::success(&format!("  ✓ {}", props.original_name));
                        success_count += 1;
                    }
                    Err(e) => {
                        output::warn(&format!("  ✗ {key}: {e}"));
                        error_count += 1;
                    }
                }
            }
            println!();
            output::success(&format!(
                "Bulk set complete: {success_count} succeeded, {error_count} failed"
            ));
        }

        // Invalidate the secrets list cache for the resolved vault
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_get_direct(
    name: &str,
    raw: bool,
    version: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;

        let secret = if let Some(ref ver) = version {
            reg.active()
                .secrets()
                .get_secret_version(&vault_name, name, ver, true)
                .await?
        } else {
            reg.active()
                .secrets()
                .get_secret(&vault_name, name, true)
                .await?
        };

        if raw {
            if let Some(value) = secret.value {
                print!("{}", value.as_str());
            }
        } else if let Some(ref value) = secret.value {
            match copy_to_clipboard(value) {
                Ok(()) => {
                    let timeout = config.clipboard_timeout;
                    if timeout > 0 {
                        output::success(&format!(
                            "Secret '{name}' copied to clipboard (auto-clears in {timeout}s)"
                        ));
                        schedule_clipboard_clear(timeout);
                    } else {
                        output::success(&format!("Secret '{name}' copied to clipboard"));
                    }
                }
                Err(e) => {
                    output::warn(&format!("Failed to copy to clipboard: {e}"));
                    eprintln!("Use 'xv get {name} --raw' to print the value to stdout instead.");
                }
            }
        } else {
            output::warn(&format!("Secret '{name}' has no value"));
        }
        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

fn secret_summary_matches_group(
    secret: &crate::secret::manager::SecretSummary,
    group: &str,
) -> bool {
    secret
        .groups
        .as_ref()
        .map(|groups| groups.split(',').any(|grp| grp.trim() == group))
        .unwrap_or(false)
}

fn trait_secret_cache_key(vault_name: &str) -> crate::cache::CacheKey {
    crate::cache::CacheKey::SecretsList {
        vault_name: vault_name.to_string(),
    }
}

fn invalidate_trait_secret_cache(config: &Config, vault_name: &str) {
    let cache_manager = crate::cache::CacheManager::from_config(config);
    cache_manager.invalidate(&trait_secret_cache_key(vault_name));
}

fn filter_secret_summaries_for_display(
    mut secrets: Vec<crate::secret::manager::SecretSummary>,
    group: Option<&str>,
    all: bool,
) -> Vec<crate::secret::manager::SecretSummary> {
    if !all {
        secrets.retain(|s| s.enabled);
    }
    if let Some(g) = group {
        secrets.retain(|s| secret_summary_matches_group(s, g));
    }
    secrets
}

fn secret_count_label(
    displayed: usize,
    total: usize,
    qualifier: Option<&str>,
    paginated: bool,
) -> String {
    let noun = match qualifier {
        Some(q) => format!("{q} secret(s)"),
        None => "secret(s)".to_string(),
    };

    if paginated {
        format!("Showing {displayed} of {total} {noun}")
    } else {
        format!("{displayed} {noun}")
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn display_cached_secret_list(
    secrets: Vec<crate::secret::manager::SecretSummary>,
    group: Option<String>,
    all: bool,
    pagination: Pagination,
    pager: bool,
    vault_name: &str,
    config: &Config,
    names_only: bool,
) -> Result<()> {
    use crate::utils::format::TableFormatter;
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};
    use std::fmt::Write as _;

    let filtered = filter_secret_summaries_for_display(secrets, group.as_deref(), all);

    // Early exit for names-only mode (no pagination for pipe-friendly output)
    if names_only {
        for s in &filtered {
            let display = if s.original_name.is_empty() {
                &s.name
            } else {
                &s.original_name
            };
            println!("{display}");
        }
        return Ok(());
    }

    let page = paginate_slice(&filtered, pagination);

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    if human_table_like {
        let mut output = String::new();
        output.push('\n');
        // Color only for styled table; plain/raw must not emit ANSI escapes
        if !config.no_color && fmt == OutputFormat::Table {
            let _ = writeln!(output, "\x1b[36mVault: {}\x1b[0m", vault_name);
        } else {
            let _ = writeln!(output, "Vault: {}", vault_name);
        }
        output.push('\n');

        if page.total_items == 0 {
            let msg = if all {
                "No secrets found in vault."
            } else {
                "No enabled secrets found in vault. Use --all to show disabled secrets."
            };
            output.push_str(&output::format_line(
                output::Level::Info,
                msg,
                output::should_use_rich_stdout(),
            ));
            crate::utils::pager::print_output(&output, pager)?;
            return Ok(());
        }

        let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
        output.push_str(&formatter.format_table(&page.items)?);
        output.push('\n');
        let _ = writeln!(
            output,
            "{} in vault '{}'",
            secret_count_label(
                page.items.len(),
                page.total_items,
                None,
                page.page_size.is_some(),
            ),
            vault_name
        );
        if let Some(footer) = pagination_footer_text(&page, "secret", fmt) {
            output.push('\n');
            output.push_str(&footer);
        }
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
    let output = formatter.format_table(&page.items)?;
    crate::utils::pager::print_output(&output, pager)?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_list_direct(
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    no_cache: bool,
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (all backends) ───────────────────────────────
    if use_trait_path(registry) {
        use crate::cache::CacheManager;

        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        let cache_manager = CacheManager::from_config(&config);
        let cache_key = trait_secret_cache_key(&vault_name);
        let use_cache = cache_manager.is_enabled() && !no_cache;

        // Try cache (skip for expiry filters — they need per-secret API calls)
        if use_cache && expiring.is_none() && !expired {
            if let Some(cached) =
                cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
            {
                return display_cached_secret_list(
                    cached,
                    group,
                    all,
                    pagination,
                    pager,
                    &vault_name,
                    &config,
                    names_only,
                );
            }
        }

        // Fetch the full unfiltered list for the cache. For expiry filters,
        // derive the display set from this cached dataset after applying the
        // cheap group/enabled filters so we only call get_secret for rows that
        // can actually be displayed.
        let all_secrets = reg
            .active()
            .secrets()
            .list_secrets(&vault_name, None)
            .await?;

        // Cache the unfiltered list so subsequent calls see the full dataset.
        if use_cache {
            cache_manager.set(&cache_key, &all_secrets);
        }

        // Apply expiry filtering if requested (requires per-secret trait calls)
        let secrets = if expired || expiring.is_some() {
            use crate::utils::datetime::{is_expired, is_expiring_within};

            let display_candidates =
                filter_secret_summaries_for_display(all_secrets, group.as_deref(), all);
            let mut filtered_secrets = Vec::new();
            for secret_summary in display_candidates {
                match reg
                    .active()
                    .secrets()
                    .get_secret(&vault_name, &secret_summary.name, false)
                    .await
                {
                    Ok(secret_props) => {
                        let should_include = if expired {
                            is_expired(secret_props.expires_on)
                        } else if let Some(ref duration) = expiring {
                            match is_expiring_within(secret_props.expires_on, duration) {
                                Ok(is_exp) => is_exp,
                                Err(e) => {
                                    eprintln!("Warning: Invalid duration '{}': {}", duration, e);
                                    false
                                }
                            }
                        } else {
                            true
                        };
                        if should_include {
                            filtered_secrets.push(secret_summary);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to get details for secret '{}': {}",
                            secret_summary.name, e
                        );
                    }
                }
            }
            filtered_secrets
        } else {
            all_secrets
        };

        return display_cached_secret_list(
            secrets,
            if expired || expiring.is_some() {
                None
            } else {
                group
            },
            if expired || expiring.is_some() {
                true
            } else {
                all
            },
            pagination,
            pager,
            &vault_name,
            &config,
            names_only,
        );
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_delete_direct(
    name: Option<String>,
    group: Option<String>,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;

        if let Some(group_name) = group {
            // Group delete: list, filter by group, delete matching
            let secrets = reg
                .active()
                .secrets()
                .list_secrets(&vault_name, Some(&group_name))
                .await?;
            if secrets.is_empty() {
                output::info(&format!("No secrets found in group '{group_name}'"));
                return Ok(());
            }
            if !force {
                output::warn(&format!(
                    "About to delete {} secret(s) in group '{group_name}'. Use --force to confirm.",
                    secrets.len()
                ));
                return Ok(());
            }
            for s in &secrets {
                reg.active()
                    .secrets()
                    .delete_secret(&vault_name, &s.name)
                    .await?;
                output::success(&format!("Deleted '{}'", s.name));
            }
        } else if let Some(secret_name) = name {
            if !force {
                output::warn(&format!(
                    "About to delete secret '{secret_name}'. Use --force to confirm."
                ));
                return Ok(());
            }
            reg.active()
                .secrets()
                .delete_secret(&vault_name, &secret_name)
                .await?;
            output::success(&format!("Successfully deleted secret '{secret_name}'"));
        } else {
            return Err(CrosstacheError::invalid_argument(
                "Either secret name or --group must be specified",
            ));
        }

        // Invalidate the secrets list cache for the resolved vault
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_history_direct(
    name: &str,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Capability check: history requires versioning support
    if let Some(registry) = registry {
        let caps = registry.active().capabilities();
        if !caps.has_versioning {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support version history.",
                registry.active().name()
            )));
        }
    }

    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        let versions = reg
            .active()
            .secrets()
            .list_versions(&vault_name, name)
            .await?;
        if versions.is_empty() {
            output::info(&format!("No version history for '{name}'"));
        } else {
            use crate::utils::format::TableFormatter;
            let formatter = TableFormatter::new(
                config.runtime_output_format,
                config.no_color,
                config.template.clone(),
            );
            let table = formatter.format_table(&versions)?;
            println!("{table}");
            output::info(&format!("{} version(s) of '{name}'", versions.len()));
        }
        return Ok(());
    }

    Err(CrosstacheError::config(
        "No backend registry available. Run 'xv config show' to check your configuration.",
    ))
}

pub(crate) async fn execute_secret_rollback_direct(
    name: &str,
    version: &str,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Capability check: rollback requires versioning support
    if let Some(registry) = registry {
        let caps = registry.active().capabilities();
        if !caps.has_versioning {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support version rollback.",
                registry.active().name()
            )));
        }
    }

    // ── Trait-based path ───────────────────────────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        if reg.active().kind() == crate::backend::BackendKind::Azure {
            return execute_secret_rollback_legacy(name, version, force, config, registry).await;
        }
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        if !force {
            output::warn(&format!(
                "About to roll back secret '{name}' to version {version}. Use --force to confirm."
            ));
            return Ok(());
        }
        let props = reg
            .active()
            .secrets()
            .rollback(&vault_name, name, version)
            .await?;
        output::success(&format!(
            "Successfully rolled back '{}' to version {version}",
            props.original_name
        ));
        // Invalidate the secrets list cache for the resolved vault
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
    }

    // ── Azure legacy path (unchanged) ─────────────────────────────────
    execute_secret_rollback_legacy(name, version, force, config, registry).await
}

async fn execute_secret_rollback_legacy(
    name: &str,
    version: &str,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_rollback(&secret_manager, name, None, version, force, &config).await?;

    // Invalidate the secrets list cache for the resolved vault
    if let Ok(vault_name) = config.resolve_vault_name(None).await {
        let cache_manager = crate::cache::CacheManager::from_config(&config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList { vault_name });
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_rotate_direct(
    name: &str,
    length: usize,
    charset: CharsetType,
    generator: Option<String>,
    show_value: bool,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_rotate(
        &secret_manager,
        name,
        None,
        length,
        charset,
        generator,
        show_value,
        force,
        &config,
    )
    .await?;

    // Invalidate the secrets list cache for the resolved vault
    if let Ok(vault_name) = config.resolve_vault_name(None).await {
        let cache_manager = crate::cache::CacheManager::from_config(&config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList { vault_name });
    }

    Ok(())
}

pub(crate) async fn execute_secret_run_direct(
    group: Vec<String>,
    no_masking: bool,
    inherit_env: bool,
    command: Vec<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_run(
        &secret_manager,
        None,
        group,
        no_masking,
        inherit_env,
        command,
        &config,
    )
    .await
}

pub(crate) async fn execute_secret_inject_direct(
    template: Option<String>,
    out: Option<String>,
    group: Vec<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_inject(&secret_manager, None, template, out, group, &config).await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_update_direct(
    name: &str,
    value: Option<String>,
    stdin: bool,
    tags: Vec<(String, String)>,
    groups: Vec<String>,
    rename: Option<String>,
    note: Option<String>,
    folder: Option<String>,
    replace_tags: bool,
    replace_groups: bool,
    expires: Option<String>,
    not_before: Option<String>,
    clear_expires: bool,
    clear_not_before: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;

        // Parse value from stdin if requested
        let resolved_value = if stdin {
            let mut buffer = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
            Some(Zeroizing::new(buffer.trim().to_string()))
        } else {
            value.map(Zeroizing::new)
        };

        // Parse expiry dates
        let expires_on_dt = if clear_expires {
            None // The backend will handle clearing
        } else if let Some(ref es) = expires {
            use crate::utils::datetime::parse_datetime_or_duration;
            Some(parse_datetime_or_duration(es)?)
        } else {
            None
        };
        let not_before_dt = if clear_not_before {
            None
        } else if let Some(ref nb) = not_before {
            use crate::utils::datetime::parse_datetime_or_duration;
            Some(parse_datetime_or_duration(nb)?)
        } else {
            None
        };

        let merged_tags = if tags.is_empty() {
            None
        } else {
            Some(
                tags.into_iter()
                    .collect::<std::collections::HashMap<_, _>>(),
            )
        };
        let merged_groups = if groups.is_empty() {
            None
        } else {
            Some(groups)
        };

        let request = crate::secret::manager::SecretUpdateRequest {
            name: name.to_string(),
            new_name: rename,
            value: resolved_value,
            content_type: None,
            enabled: None,
            expires_on: expires_on_dt,
            not_before: not_before_dt,
            tags: merged_tags,
            groups: merged_groups,
            note,
            folder,
            replace_tags,
            replace_groups,
        };
        let props = reg
            .active()
            .secrets()
            .update_secret(&vault_name, name, request)
            .await?;
        output::success(&format!(
            "Successfully updated secret '{}'",
            props.original_name
        ));
        // Invalidate the secrets list cache for metadata, value, rename, or enablement changes.
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
    }

    // ── Azure legacy path (unchanged) ─────────────────────────────────
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_update(
        &secret_manager,
        name,
        None,
        value,
        stdin,
        tags,
        groups,
        rename,
        note,
        folder,
        replace_tags,
        replace_groups,
        expires,
        not_before,
        clear_expires,
        clear_not_before,
        &config,
    )
    .await?;

    // Invalidate the secrets list cache for the resolved vault
    if let Ok(vault_name) = config.resolve_vault_name(None).await {
        let cache_manager = crate::cache::CacheManager::from_config(&config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList { vault_name });
    }

    Ok(())
}

pub(crate) async fn execute_secret_purge_direct(
    name: &str,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Capability check: purge requires soft-delete support
    if let Some(registry) = registry {
        let caps = registry.active().capabilities();
        if !caps.has_soft_delete {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support purge (soft-delete not available).",
                registry.active().name()
            )));
        }
    }

    // ── Trait-based path ───────────────────────────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        if reg.active().kind() == crate::backend::BackendKind::Azure {
            return execute_secret_purge_legacy(name, force, config, registry).await;
        }
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        if !force {
            output::warn(&format!(
                "About to PERMANENTLY DELETE secret '{name}'. This cannot be undone. Use --force to confirm."
            ));
            return Ok(());
        }
        reg.active()
            .secrets()
            .purge_secret(&vault_name, name)
            .await?;
        output::success(&format!("Successfully purged secret '{name}'"));
        // Invalidate the secrets list cache for the resolved vault
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
    }

    // ── Azure legacy path (unchanged) ─────────────────────────────────
    execute_secret_purge_legacy(name, force, config, registry).await
}

async fn execute_secret_purge_legacy(
    name: &str,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_purge(&secret_manager, name, None, force, &config).await?;

    // Invalidate the secrets list cache for the resolved vault
    if let Ok(vault_name) = config.resolve_vault_name(None).await {
        let cache_manager = crate::cache::CacheManager::from_config(&config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList { vault_name });
    }

    Ok(())
}

pub(crate) async fn execute_secret_restore_direct(
    name: &str,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    // Capability check: restore requires soft-delete support
    if let Some(registry) = registry {
        let caps = registry.active().capabilities();
        if !caps.has_soft_delete {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support restore (soft-delete not available).",
                registry.active().name()
            )));
        }
    }

    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        let vault_name = resolve_vault_for_trait(&config, registry).await?;
        let props = reg
            .active()
            .secrets()
            .restore_secret(&vault_name, name)
            .await?;
        output::success(&format!(
            "Successfully restored secret '{}'",
            props.original_name
        ));
        // Invalidate the secrets list cache for the resolved vault
        invalidate_trait_secret_cache(&config, &vault_name);
        return Ok(());
    }

    // ── Azure legacy path (unchanged) ─────────────────────────────────
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_restore(&secret_manager, name, None, &config).await?;

    // Invalidate the secrets list cache for the resolved vault
    if let Ok(vault_name) = config.resolve_vault_name(None).await {
        let cache_manager = crate::cache::CacheManager::from_config(&config);
        cache_manager.invalidate(&crate::cache::CacheKey::SecretsList { vault_name });
    }

    Ok(())
}

pub(crate) async fn execute_diff_command(
    vault1: &str,
    vault2: &str,
    show_values: bool,
    group: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use std::collections::BTreeSet;

    let auth_provider = get_azure_auth_provider(registry, &config)?;

    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // List secrets from both vaults
    let secrets_a = secret_manager
        .list_secrets_formatted(
            vault1,
            group.as_deref(),
            crate::utils::format::OutputFormat::Json,
            false,
            true,
        )
        .await?;

    let secrets_b = secret_manager
        .list_secrets_formatted(
            vault2,
            group.as_deref(),
            crate::utils::format::OutputFormat::Json,
            false,
            true,
        )
        .await?;

    // Build name sets
    let names_a: BTreeSet<String> = secrets_a.iter().map(|s| s.name.clone()).collect();
    let names_b: BTreeSet<String> = secrets_b.iter().map(|s| s.name.clone()).collect();
    let all_names: BTreeSet<String> = names_a.union(&names_b).cloned().collect();

    // Fetch values from both vaults for comparison
    let mut values_a = std::collections::HashMap::new();
    let mut values_b = std::collections::HashMap::new();

    for name in &names_a {
        match secret_manager
            .get_secret_safe(vault1, name, true, true)
            .await
        {
            Ok(props) => {
                if let Some(val) = props.value {
                    values_a.insert(name.clone(), val);
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to get '{}' from {}: {}", name, vault1, e));
            }
        }
    }

    for name in &names_b {
        match secret_manager
            .get_secret_safe(vault2, name, true, true)
            .await
        {
            Ok(props) => {
                if let Some(val) = props.value {
                    values_b.insert(name.clone(), val);
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to get '{}' from {}: {}", name, vault2, e));
            }
        }
    }

    // Compare and output
    println!("Comparing {} → {}", vault1, vault2);
    println!();

    let mut added = 0u32;
    let mut removed = 0u32;
    let mut changed = 0u32;
    let mut identical = 0u32;

    // Find max name length for alignment
    let max_len = all_names.iter().map(|n| n.len()).max().unwrap_or(0);

    for name in &all_names {
        let in_a = names_a.contains(name);
        let in_b = names_b.contains(name);

        match (in_a, in_b) {
            (false, true) => {
                println!(
                    "  + {:<width$}  (only in {})",
                    name,
                    vault2,
                    width = max_len
                );
                added += 1;
            }
            (true, false) => {
                println!(
                    "  - {:<width$}  (only in {})",
                    name,
                    vault1,
                    width = max_len
                );
                removed += 1;
            }
            (true, true) => {
                let val_a = values_a.get(name);
                let val_b = values_b.get(name);
                if val_a == val_b {
                    println!("  = {:<width$}  (identical)", name, width = max_len);
                    identical += 1;
                } else {
                    println!("  ~ {:<width$}  (value differs)", name, width = max_len);
                    if show_values {
                        let a_str = val_a.map(|v| v.as_str()).unwrap_or("<empty>");
                        let b_str = val_b.map(|v| v.as_str()).unwrap_or("<empty>");
                        println!("      {} : {}", vault1, a_str);
                        println!("      {} : {}", vault2, b_str);
                    }
                    changed += 1;
                }
            }
            (false, false) => unreachable!(),
        }
    }

    println!();
    println!(
        "Summary: {} added, {} removed, {} changed, {} identical",
        added, removed, changed, identical
    );

    Ok(())
}

pub(crate) async fn execute_secret_copy_direct(
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_copy(
        &secret_manager,
        name,
        from_vault,
        to_vault,
        new_name,
        &config,
    )
    .await?;

    // Invalidate the secrets list cache for both source and destination vaults
    let cache_manager = crate::cache::CacheManager::from_config(&config);
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        vault_name: from_vault.to_string(),
    });
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        vault_name: to_vault.to_string(),
    });

    Ok(())
}

pub(crate) async fn execute_secret_move_direct(
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_move(
        &secret_manager,
        name,
        from_vault,
        to_vault,
        new_name,
        force,
        &config,
    )
    .await?;

    // Invalidate the secrets list cache for both source and destination vaults
    let cache_manager = crate::cache::CacheManager::from_config(&config);
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        vault_name: from_vault.to_string(),
    });
    cache_manager.invalidate(&crate::cache::CacheKey::SecretsList {
        vault_name: to_vault.to_string(),
    });

    Ok(())
}

pub(crate) async fn execute_secret_parse_direct(
    connection_string: &str,
    format: &str,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    let auth_provider = get_azure_auth_provider(registry, &config)?;

    // Create secret manager
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    execute_secret_parse(&secret_manager, connection_string, format, &config).await
}

pub(crate) async fn execute_secret_share_direct(
    command: ShareCommands,
    config: Config,
    registry: Option<&BackendRegistry>,
) -> Result<()> {
    use crate::auth::provider::AzureAuthProvider;
    use crate::vault::manager::VaultManager;

    // Capability check: sharing requires RBAC support
    if let Some(registry) = registry {
        let caps = registry.active().capabilities();
        if !caps.has_rbac {
            return Err(CrosstacheError::InvalidArgument(format!(
                "The {} backend does not support access sharing. Use the azure backend for RBAC.",
                registry.active().name()
            )));
        }
    }

    let auth_provider: Arc<dyn AzureAuthProvider> = get_azure_auth_provider(registry, &config)?;

    // Create vault manager for secret-level RBAC
    let vault_manager = VaultManager::new(
        auth_provider.clone(),
        config.subscription_id.clone(),
        config.no_color,
    )?;

    execute_secret_share(&vault_manager, &auth_provider, command, &config).await
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_set(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    expires: Option<String>,
    not_before: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use std::io::{self, Read};

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
        rpassword::prompt_password(format!("Enter value for secret '{name}': "))?
    };

    if value.is_empty() {
        return Err(CrosstacheError::config("Secret value cannot be empty"));
    }

    // Parse expiry dates if provided
    let expires_on = if let Some(expires_str) = expires.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(expires_str)?)
    } else {
        None
    };

    let not_before_on = if let Some(not_before_str) = not_before.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(not_before_str)?)
    } else {
        None
    };

    // Create secret request with note, folder, and/or expiry dates if provided
    let secret_request =
        if note.is_some() || folder.is_some() || expires_on.is_some() || not_before_on.is_some() {
            Some(crate::secret::manager::SecretRequest {
                name: name.to_string(),
                value: Zeroizing::new(value.clone()),
                content_type: None,
                enabled: Some(true),
                expires_on,
                not_before: not_before_on,
                tags: None,
                groups: None,
                note,
                folder,
            })
        } else {
            None
        };

    // Set the secret
    let secret = secret_manager
        .set_secret_safe(&vault_name, name, &value, secret_request)
        .await?;

    output::success(&format!(
        "Successfully set secret '{}'",
        secret.original_name
    ));
    println!("   Vault: {vault_name}");
    println!("   Version: {}", secret.version);

    output::hint(&format!("Verify with 'xv get {}'", secret.original_name));

    Ok(())
}

async fn execute_secret_get(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    raw: bool,
    version: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Resolve user-friendly version (e.g. "v6") to Azure GUID
    let resolved_version = match &version {
        Some(ver) => {
            let (guid, _display) =
                resolve_version_to_guid(secret_manager, &vault_name, name, ver).await?;
            Some(guid)
        }
        None => None,
    };

    // Get the secret (specific version or current)
    let secret = match secret_manager
        .get_secret_with_version(&vault_name, name, resolved_version.as_deref(), true, true)
        .await
    {
        Ok(s) => s,
        Err(CrosstacheError::SecretNotFound { name: missing, .. }) => {
            // Best-effort suggestion: list secrets and find a close match.
            // Failures here must NOT change the original error path.
            let suggestion = match secret_manager
                .secret_ops()
                .list_secrets(&vault_name, None)
                .await
            {
                Ok(summaries) => {
                    let candidates: Vec<String> = summaries
                        .into_iter()
                        .map(|s| {
                            if s.original_name.is_empty() {
                                s.name
                            } else {
                                s.original_name
                            }
                        })
                        .collect();
                    crate::utils::suggestions::closest_match(&missing, &candidates)
                        .map(|s| s.to_string())
                }
                Err(e) => {
                    tracing::debug!("suggestion list-call failed: {e}");
                    None
                }
            };
            return Err(CrosstacheError::secret_not_found(missing).with_suggestion(suggestion));
        }
        Err(e) => return Err(e),
    };

    if raw {
        // Raw output - print the value
        if let Some(value) = secret.value {
            print!("{}", value.as_str());
        }
    } else {
        // Default behavior - copy to clipboard
        if let Some(ref value) = secret.value {
            match copy_to_clipboard(value) {
                Ok(()) => {
                    let timeout = config.clipboard_timeout;
                    if timeout > 0 {
                        output::success(&format!(
                            "Secret '{name}' copied to clipboard (auto-clears in {timeout}s)"
                        ));
                        schedule_clipboard_clear(timeout);
                    } else {
                        output::success(&format!("Secret '{name}' copied to clipboard"));
                    }
                }
                Err(e) => {
                    output::warn(&format!("Failed to copy to clipboard: {e}"));
                    eprintln!("Use 'xv get {name} --raw' to print the value to stdout instead.");
                }
            }
        } else {
            output::warn(&format!("Secret '{name}' has no value"));
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_secret_find_direct(
    pattern: Option<String>,
    in_fields: Vec<String>,
    limit: usize,
    min_score: f32,
    all_vaults: bool,
    names_only: bool,
    format: crate::utils::format::OutputFormat,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    // ── Trait-based path (non-Azure backends) ──────────────────────────
    if use_trait_path(registry) {
        let reg = registry.expect("use_trait_path guarantees Some");
        use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};

        // Parse --in fields
        let mut fields: Vec<FuzzyField> = vec![FuzzyField::Name];
        for raw in &in_fields {
            let parsed = match raw.to_ascii_lowercase().as_str() {
                "name" => FuzzyField::Name,
                "folder" => FuzzyField::Folder,
                "groups" => FuzzyField::Groups,
                "note" => FuzzyField::Note,
                "tags" => FuzzyField::Tags,
                other => {
                    return Err(CrosstacheError::invalid_argument(format!(
                        "unknown --in field: '{other}' (allowed: name, folder, groups, note, tags)"
                    )));
                }
            };
            if !fields.contains(&parsed) {
                fields.push(parsed);
            }
        }

        let items: Vec<CandidateItem> = if all_vaults {
            // List all vaults and collect secrets
            let mut combined = Vec::new();
            if let Some(vaults_backend) = reg.active().vaults() {
                let vaults = vaults_backend.list_vaults().await?;
                for v in &vaults {
                    match reg.active().secrets().list_secrets(&v.name, None).await {
                        Ok(secrets) => {
                            for s in &secrets {
                                let mut item = CandidateItem::from_secret_summary(s);
                                item.name = format!("{}/{}", v.name, item.name);
                                combined.push(item);
                            }
                        }
                        Err(e) => {
                            tracing::debug!("list_secrets failed for vault {}: {e}", v.name);
                        }
                    }
                }
            }
            combined
        } else {
            let vault_name = resolve_vault_for_trait(&config, registry).await?;
            let all_secrets = reg
                .active()
                .secrets()
                .list_secrets(&vault_name, None)
                .await?;
            all_secrets
                .iter()
                .map(CandidateItem::from_secret_summary)
                .collect()
        };

        let pattern_str = pattern.as_deref().unwrap_or("");
        let mut matches = score_matches(pattern_str, &items, &fields);

        if !pattern_str.is_empty() && !matches.is_empty() {
            let top = matches[0].score as f32;
            if top > 0.0 {
                let cutoff = (top * min_score).ceil() as u32;
                matches.retain(|m| m.score >= cutoff);
            }
        }
        matches.truncate(limit);

        if names_only {
            for m in &matches {
                println!("{}", m.item.name);
            }
            return Ok(());
        }

        let resolved = format.resolve_for_stdout();
        if matches!(resolved, OutputFormat::Json | OutputFormat::Yaml) {
            let envelope: Vec<serde_json::Value> = matches
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "name": m.item.name,
                        "score": m.score,
                        "folder": m.item.folder,
                        "groups": m.item.groups,
                    })
                })
                .collect();
            let rendered = match resolved {
                OutputFormat::Json => serde_json::to_string_pretty(&envelope).unwrap_or_default(),
                OutputFormat::Yaml => serde_yaml::to_string(&envelope).unwrap_or_default(),
                _ => unreachable!(),
            };
            println!("{rendered}");
        } else if matches.is_empty() {
            output::info("No matching secrets found");
        } else {
            for m in &matches {
                println!("{}", m.item.name);
            }
        }
        return Ok(());
    }

    // ── Azure legacy path (unchanged) ─────────────────────────────────
    let auth_provider = crate::cli::helpers::get_azure_auth_provider(registry, &config)?;
    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    execute_secret_find(
        &secret_manager,
        pattern.as_deref(),
        in_fields,
        limit,
        min_score,
        all_vaults,
        names_only,
        format,
        &config,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_find(
    secret_manager: &crate::secret::manager::SecretManager,
    pattern: Option<&str>,
    in_fields: Vec<String>,
    limit: usize,
    min_score: f32,
    all_vaults: bool,
    names_only: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};

    // Parse --in fields first so argument errors fire before vault resolution.
    let mut fields: Vec<FuzzyField> = vec![FuzzyField::Name];
    for raw in &in_fields {
        let parsed = match raw.to_ascii_lowercase().as_str() {
            "name" => FuzzyField::Name,
            "folder" => FuzzyField::Folder,
            "groups" => FuzzyField::Groups,
            "note" => FuzzyField::Note,
            "tags" => FuzzyField::Tags,
            other => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "unknown --in field: '{other}' (allowed: name, folder, groups, note, tags)"
                )));
            }
        };
        if !fields.contains(&parsed) {
            fields.push(parsed);
        }
    }

    // Single-vault mode needs a resolved vault; `--all-vaults` lists every
    // vault and must not require default vault context (see flags doc).
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let single_vault = if all_vaults {
        None
    } else {
        let vn = config.resolve_vault_name(None).await?;
        let _ = context_manager.update_usage(&vn).await;
        Some(vn)
    };

    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::vault::manager::VaultManager;

    let items: Vec<CandidateItem> = if all_vaults {
        // Build a VaultManager from the same credential priority used
        // by the secret manager. (Re-using auth context is cheap;
        // tokens cache underneath.)
        let auth_provider = std::sync::Arc::new(
            DefaultAzureCredentialProvider::with_credential_priority(
                config.azure_credential_priority.clone(),
            )
            .map_err(|e| {
                CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
            })?,
        );
        let vault_manager = VaultManager::new(
            auth_provider,
            config.subscription_id.clone(),
            config.no_color,
        )?;

        let vaults = vault_manager
            .vault_ops()
            .list_vaults(Some(&config.subscription_id), None)
            .await?;

        let progress = crate::utils::interactive::ProgressIndicator::new(&format!(
            "Searching {} vaults...",
            vaults.len()
        ));
        let mut combined: Vec<CandidateItem> = Vec::new();
        for v in &vaults {
            // Per-vault list — failures here are non-fatal; log + skip.
            match secret_manager
                .secret_ops()
                .list_secrets(&v.name, None)
                .await
            {
                Ok(secrets) => {
                    for s in &secrets {
                        let mut item = CandidateItem::from_secret_summary(s);
                        // Prefix the vault name into the displayed name so
                        // results are unambiguous: e.g. "myvault/SECRET".
                        item.name = format!("{}/{}", v.name, item.name);
                        combined.push(item);
                    }
                }
                Err(e) => {
                    tracing::debug!("list_secrets failed for vault {}: {e}", v.name);
                }
            }
        }
        progress.finish_clear();
        combined
    } else {
        // Single-vault path (existing logic from Task 5).
        let vault_name = single_vault.as_ref().ok_or_else(|| {
            CrosstacheError::config("vault name not resolved for single-vault search".to_string())
        })?;
        let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
        let all_secrets = secret_manager
            .secret_ops()
            .list_secrets(vault_name, None)
            .await;
        progress.finish_clear();
        let all_secrets = all_secrets?;
        all_secrets
            .iter()
            .map(CandidateItem::from_secret_summary)
            .collect()
    };
    let pattern_str = pattern.unwrap_or("");
    let mut matches = score_matches(pattern_str, &items, &fields);

    // Apply min_score (relative to the top score, so 0.3 means 30% of
    // top). Empty pattern → every score is 0; skip filtering.
    if !pattern_str.is_empty() && !matches.is_empty() {
        let top = matches[0].score as f32;
        if top > 0.0 {
            let cutoff = (top * min_score).ceil() as u32;
            matches.retain(|m| m.score >= cutoff);
        }
    }

    // Apply limit.
    matches.truncate(limit);

    // Render: --names-only beats everything (pipe-friendly).
    if names_only {
        for m in &matches {
            println!("{}", m.item.name);
        }
        return Ok(());
    }

    // Format-aware rendering.
    let resolved = format.resolve_for_stdout();
    use crate::utils::format::OutputFormat;
    if matches!(resolved, OutputFormat::Json | OutputFormat::Yaml) {
        let envelope: Vec<serde_json::Value> = matches
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.item.name,
                    "score": m.score,
                    "folder": m.item.folder,
                    "groups": m.item.groups,
                })
            })
            .collect();
        let rendered = match resolved {
            OutputFormat::Json => serde_json::to_string_pretty(&envelope).unwrap_or_default(),
            OutputFormat::Yaml => serde_yaml::to_string(&envelope).unwrap_or_default(),
            _ => unreachable!(),
        };
        println!("{rendered}");
        return Ok(());
    }

    // Plain/table fallback (Task 7 polishes the score-bar column).
    if matches.is_empty() {
        if all_vaults {
            if let Some(p) = pattern {
                output::info(&format!("No secrets match '{p}' across all vaults"));
            } else {
                output::info("No secrets found across all vaults");
            }
        } else {
            let vault_name = single_vault.as_ref().ok_or_else(|| {
                CrosstacheError::config(
                    "vault name not resolved for single-vault search".to_string(),
                )
            })?;
            if let Some(p) = pattern {
                output::info(&format!("No secrets match '{p}' in vault '{vault_name}'"));
            } else {
                output::info(&format!("No secrets in vault '{vault_name}'"));
            }
        }
        return Ok(());
    }
    use crate::utils::fuzzy::score_bar;
    let top = matches.iter().map(|m| m.score).max().unwrap_or(1).max(1) as f32;
    println!("{:<40}  {:<10}  {:<24}  GROUPS", "NAME", "SCORE", "FOLDER");
    for m in &matches {
        let folder = m.item.folder.as_deref().unwrap_or("");
        let groups = m.item.groups.as_deref().unwrap_or("");
        let bar = score_bar(m.score as f32 / top);
        println!("{:<40}  {bar}  {:<24}  {}", m.item.name, folder, groups);
    }
    Ok(())
}

#[allow(dead_code)] // called from src/main.rs::run_complete_secrets (binary-only path)
pub(crate) async fn execute_complete_secrets(config: Config) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};

    let vault_name = config.resolve_vault_name(None).await?;

    // Cache-only path. If cache is cold, exit silently — the user got
    // no completions, which is the right UX for a Tab press (no Azure
    // round-trip on every keystroke).
    let cache_manager = CacheManager::from_config(&config);
    if !cache_manager.is_enabled() {
        return Ok(());
    }
    let cache_key = CacheKey::SecretsList {
        vault_name: vault_name.clone(),
    };
    if let Some(cached) =
        cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key)
    {
        for s in &cached {
            let display = if s.original_name.is_empty() {
                &s.name
            } else {
                &s.original_name
            };
            println!("{display}");
        }
    }
    Ok(())
}

async fn execute_secret_history(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get secret versions using the secret operations
    let versions = secret_manager
        .secret_ops()
        .get_secret_versions(&vault_name, name)
        .await?;

    if versions.is_empty() {
        output::info(&format!("No versions found for secret '{name}'"));
        return Ok(());
    }

    // Display versions in a table
    #[derive(Tabled)]
    struct VersionInfo {
        #[tabled(rename = "Version")]
        version: String,
        #[tabled(rename = "Created")]
        created: String,
        #[tabled(rename = "Updated")]
        updated: String,
        #[tabled(rename = "Enabled")]
        enabled: String,
    }

    let version_infos: Vec<VersionInfo> = versions
        .into_iter()
        .map(|v| VersionInfo {
            version: v
                .version_number
                .map(|n| format!("v{n}"))
                .unwrap_or_else(|| v.version.chars().take(8).collect::<String>() + "..."),
            created: v.created_on,
            updated: v.updated_on,
            enabled: if v.enabled { "Yes" } else { "No" }.to_string(),
        })
        .collect();

    let table = Table::new(&version_infos);
    println!("Version history for secret '{name}' in vault '{vault_name}':");
    println!();
    println!("{}", format_table(table, config.no_color));

    Ok(())
}

/// Resolve a user-friendly version identifier (e.g. "v6", "6") to the Azure Key Vault hex GUID.
/// If the version string is already a hex GUID, it is returned as-is.
async fn resolve_version_to_guid(
    secret_manager: &crate::secret::manager::SecretManager,
    vault_name: &str,
    secret_name: &str,
    version: &str,
) -> Result<(String, String)> {
    if let Ok(version_num) = version.trim_start_matches('v').parse::<u32>() {
        if version_num == 0 {
            return Err(crate::error::CrosstacheError::invalid_argument(
                "Version number must be 1 or greater (v1 is the oldest version)",
            ));
        }
        let versions_list = secret_manager
            .secret_ops()
            .get_secret_versions(vault_name, secret_name)
            .await?;
        let max_version = versions_list
            .iter()
            .filter_map(|v| v.version_number)
            .max()
            .unwrap_or(0);
        let matched = versions_list
            .into_iter()
            .find(|v| v.version_number == Some(version_num));
        match matched {
            Some(v) => Ok((v.version, format!("v{version_num}"))),
            None => Err(crate::error::CrosstacheError::invalid_argument(format!(
                "Version v{version_num} not found for secret '{secret_name}'. \
                 Available versions: v1–v{max_version} (use 'xv history {secret_name}' to list them)"
            ))),
        }
    } else {
        // Assume it's already a GUID
        Ok((version.to_string(), version.to_string()))
    }
}

async fn execute_secret_rollback(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    version: &str,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::interactive::InteractivePrompt;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Resolve user-friendly version (e.g. "v6") to Azure GUID
    let (resolved_version_guid, display_version) =
        resolve_version_to_guid(secret_manager, &vault_name, name, version).await?;

    // Confirm rollback unless force flag is used
    if !force {
        let prompt = InteractivePrompt::new();
        let confirm = prompt.confirm(
            &format!(
                "Are you sure you want to rollback secret '{name}' to version '{display_version}'?"
            ),
            false,
        )?;

        if !confirm {
            println!("Rollback cancelled.");
            return Ok(());
        }
    }

    // Perform rollback using the secret operations
    let result = secret_manager
        .secret_ops()
        .rollback_secret(&vault_name, name, &resolved_version_guid)
        .await?;

    output::success(&format!(
        "Successfully rolled back secret '{name}' to version '{display_version}'"
    ));
    println!("New version GUID: {}", result.version);

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_rotate(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    length: usize,
    charset: CharsetType,
    custom_generator: Option<String>,
    show_value: bool,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretRequest;
    use crate::utils::interactive::InteractivePrompt;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Check if the secret exists first
    let existing_secret = secret_manager
        .secret_ops()
        .get_secret(&vault_name, name, true)
        .await
        .map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to verify secret exists: {}. Use 'xv set' to create a new secret.",
                e
            ))
        })?;

    output::step(&format!("Rotating secret: {}", name));

    // Show generation parameters
    if let Some(ref script) = custom_generator {
        println!("  Generator: {} (length: {})", script, length);
    } else {
        println!("  Character set: {:?}", charset);
        println!("  Length: {}", length);
    }

    // Confirm rotation unless force flag is used
    if !force {
        let prompt = InteractivePrompt::new();
        let confirm = prompt.confirm(
            &format!(
                "Are you sure you want to rotate secret '{}'? This will generate a new value and increment the version.",
                name
            ),
            false,
        )?;

        if !confirm {
            println!("Rotation cancelled.");
            return Ok(());
        }
    }

    // Generate the new value
    let new_value = generate_random_value(length, charset, custom_generator)?;

    // Preserve existing secret metadata
    let set_request = SecretRequest {
        name: name.to_string(),
        value: new_value.clone(),
        content_type: if existing_secret.content_type.is_empty() {
            None
        } else {
            Some(existing_secret.content_type)
        },
        enabled: Some(true),
        expires_on: existing_secret.expires_on,
        not_before: existing_secret.not_before,
        tags: if existing_secret.tags.is_empty() {
            None
        } else {
            Some(existing_secret.tags)
        },
        groups: None, // Groups are managed via tags
        note: None,
        folder: None,
    };

    // Set the rotated secret
    let result = secret_manager
        .secret_ops()
        .set_secret(&vault_name, &set_request)
        .await?;

    output::success(&format!("Successfully rotated secret '{}'", name));
    println!("New version: {}", result.version);

    if show_value {
        println!("Generated value: {}", new_value.as_str());
    } else {
        println!("Generated value: [hidden] (use --show-value to display)");
    }

    output::hint(&format!("Use 'xv history {}' to see version history", name));

    Ok(())
}

/// Resolve a single `xv://` URI reference to its secret, dispatching to the
/// active backend or a cross-backend instance as needed.
///
/// `cross_backends` caches freshly-created backends by kind so the SDK is not
/// re-initialised per URI. Shared by `execute_secret_run` and
/// `execute_secret_inject` to keep cross-backend resolution logic in one place.
async fn resolve_uri_secret(
    backend_ref: &BackendRef,
    secret_name: &str,
    secret_manager: &crate::secret::manager::SecretManager,
    config: &Config,
    active_kind: BackendKind,
    cross_backends: &mut std::collections::HashMap<BackendKind, Arc<dyn crate::backend::Backend>>,
) -> Result<crate::secret::manager::SecretProperties> {
    if let Some(backend_kind) = backend_ref.backend {
        if backend_kind != active_kind {
            // Cross-backend: reuse or create a cached backend instance
            if !cross_backends.contains_key(&backend_kind) {
                let b = BackendRegistry::create_for_kind(backend_kind, config)
                    .await
                    .map_err(CrosstacheError::from)?;
                cross_backends.insert(backend_kind, b);
            }
            return cross_backends[&backend_kind]
                .secrets()
                .get_secret(&backend_ref.vault, secret_name, true)
                .await
                .map_err(CrosstacheError::from);
        }
    }
    secret_manager
        .secret_ops()
        .get_secret(&backend_ref.vault, secret_name, true)
        .await
}

async fn execute_secret_run(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    groups: Vec<String>,
    no_masking: bool,
    inherit_env: bool,
    command: Vec<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::helpers::to_env_var_name;
    use regex::Regex;
    use std::collections::HashMap;
    use std::process::{Command, Stdio};

    if command.is_empty() {
        return Err(CrosstacheError::config("No command specified"));
    }

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Parse current environment for xv:// URI references (supports optional backend prefix)
    let mut uri_refs: Vec<(String, BackendRef)> = Vec::new(); // (original_uri, parsed_ref)
    let uri_regex = Regex::new(r"xv://([^/\s]+)/([^/\s]+)").unwrap();

    for (_env_name, env_value) in std::env::vars() {
        for captures in uri_regex.captures_iter(&env_value) {
            let vault_part = captures.get(1).map_or("", |m| m.as_str());
            let secret_part = captures.get(2).map_or("", |m| m.as_str());
            let uri_key = format!("xv://{vault_part}/{secret_part}");
            if uri_refs.iter().any(|(uri, _)| uri == &uri_key) {
                continue;
            }
            match BackendRef::parse(&format!("{vault_part}/{secret_part}")) {
                Ok(r) => uri_refs.push((uri_key, r)),
                Err(e) => output::warn(&format!("Skipping invalid URI '{uri_key}': {e}")),
            }
        }
    }

    // Get all secrets from the vault
    let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await;
    progress.finish_clear();
    let secrets = secrets?;

    // Filter secrets by groups if specified
    let filtered_secrets = if !groups.is_empty() {
        secrets
            .into_iter()
            .filter(|secret| {
                if let Some(secret_groups) = &secret.groups {
                    // Secret can have multiple groups (comma-separated)
                    let secret_group_list: Vec<&str> =
                        secret_groups.split(',').map(|g| g.trim()).collect();
                    groups
                        .iter()
                        .any(|filter_group| secret_group_list.contains(&filter_group.as_str()))
                } else {
                    false
                }
            })
            .collect()
    } else {
        secrets
    };

    if filtered_secrets.is_empty() {
        output::info("No secrets found to inject");
        return Ok(());
    }

    output::step(&format!(
        "Injecting {} secret(s) as environment variables...",
        filtered_secrets.len()
    ));

    // Fetch secret values and build environment map
    let mut env_vars: HashMap<String, Zeroizing<String>> = HashMap::new();
    let mut secret_values: Vec<Zeroizing<String>> = Vec::new(); // For masking
    let mut uri_values: HashMap<String, Zeroizing<String>> = HashMap::new(); // URI -> value mapping

    // Fetch secrets from current vault (group-filtered)
    for secret in filtered_secrets {
        // Get the secret value
        match secret_manager
            .secret_ops()
            .get_secret(&vault_name, &secret.name, true)
            .await
        {
            Ok(secret_props) => {
                if let Some(value) = secret_props.value {
                    let env_name = to_env_var_name(&secret.name);
                    env_vars.insert(env_name, value.clone());

                    // Store for masking (if enabled)
                    if !no_masking && !value.is_empty() {
                        secret_values.push(value.clone());
                    }
                }
            }
            Err(e) => {
                output::warn(&format!(
                    "Failed to get value for secret '{}': {}",
                    secret.name, e
                ));
            }
        }
    }

    // Fetch URI-referenced secrets from environment variables
    if !uri_refs.is_empty() {
        output::info(&format!(
            "Found {} URI reference(s) in environment",
            uri_refs.len()
        ));

        let active_kind: BackendKind = config
            .effective_backend_name()
            .parse()
            .unwrap_or(BackendKind::Azure);

        // Cache backends by kind — avoids re-initialising the SDK per URI
        let mut cross_backends: std::collections::HashMap<
            BackendKind,
            Arc<dyn crate::backend::Backend>,
        > = std::collections::HashMap::new();

        for (uri, backend_ref) in &uri_refs {
            let secret_name = match &backend_ref.secret {
                Some(s) => s.clone(),
                None => {
                    output::warn(&format!("URI '{uri}' has no secret segment — skipping"));
                    continue;
                }
            };

            let fetch_result = resolve_uri_secret(
                backend_ref,
                &secret_name,
                secret_manager,
                config,
                active_kind,
                &mut cross_backends,
            )
            .await;

            match fetch_result {
                Ok(secret_props) => {
                    if let Some(value) = secret_props.value {
                        uri_values.insert(uri.clone(), value.clone());
                        if !no_masking && !value.is_empty() {
                            secret_values.push(value);
                        }
                    } else {
                        output::warn(&format!("URI '{uri}' resolved but has no value"));
                    }
                }
                Err(e) => {
                    output::warn(&format!("Failed to resolve URI '{uri}': {e}"));
                }
            }
        }
    }

    // Set up the command
    let mut cmd = Command::new(&command[0]);
    if command.len() > 1 {
        cmd.args(&command[1..]);
    }

    // Set environment variables from vault secrets
    if !inherit_env {
        cmd.env_clear();
    }
    cmd.envs(&env_vars);

    // Resolve URI references in existing environment variables
    if !uri_values.is_empty() {
        for (env_name, env_value) in std::env::vars() {
            let mut resolved_value = env_value.clone();

            // Replace any xv:// URIs with actual secret values
            for (uri, secret_value) in &uri_values {
                resolved_value = resolved_value.replace(uri, secret_value);
            }

            // Only set if the value changed (had URI references)
            if resolved_value != env_value {
                cmd.env(env_name, resolved_value);
            }
        }
    }

    output::step(&format!("Executing: {}", command.join(" ")));

    if no_masking {
        // Direct passthrough — use .status() so inherited stdio works correctly
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let status = cmd.status().map_err(|e| {
            CrosstacheError::config(format!("Failed to execute command '{}': {}", command[0], e))
        })?;

        // Explicitly drop secret-holding variables to zeroize them after child exits
        drop(env_vars);
        drop(uri_values);
        drop(secret_values);

        std::process::exit(status.code().unwrap_or(1));
    } else {
        // Stream output line-by-line with masking
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            CrosstacheError::config(format!("Failed to execute command '{}': {}", command[0], e))
        })?;

        // Drop env vars now — they're already set on the child process
        drop(env_vars);
        drop(uri_values);

        // secret_values is moved into stream_and_mask, which wraps it in Arc.
        // After threads join, Arc drop triggers Zeroizing::drop on each secret.
        let exit_code = stream_and_mask(child, secret_values)?;
        std::process::exit(exit_code);
    }
}

/// Stream child process stdout/stderr line-by-line, masking secret values in each line.
/// Returns the child's exit code.
///
/// `secret_values` is moved into an `Arc` and shared across two reader threads.
/// After both threads join, this function holds the last `Arc` reference —
/// dropping it triggers `Zeroizing::drop` on each secret value.
fn stream_and_mask(
    mut child: std::process::Child,
    secret_values: Vec<Zeroizing<String>>,
) -> Result<i32> {
    use std::io::{BufRead, BufReader, Write};

    let stdout = child.stdout.take().ok_or_else(|| {
        CrosstacheError::config("failed to capture child stdout: pipe was not set")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CrosstacheError::config("failed to capture child stderr: pipe was not set")
    })?;

    // Move secret_values into Arc for sharing across threads.
    // After threads join, the Arc in this function is the last reference.
    let secrets = Arc::new(secret_values);
    let secrets_for_stderr = Arc::clone(&secrets);

    // Thread 1: stream stdout
    let stdout_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
            let line = String::from_utf8_lossy(&buf);
            let masked = mask_secrets(&line, &secrets);
            print!("{}", masked);
            buf.clear();
        }
    });

    // Thread 2: stream stderr
    let stderr_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
            let line = String::from_utf8_lossy(&buf);
            let masked = mask_secrets(&line, &secrets_for_stderr);
            eprint!("{}", masked);
            buf.clear();
        }
    });

    // Wait for child to exit
    let status = child
        .wait()
        .map_err(|e| CrosstacheError::config(format!("failed to wait on child process: {e}")))?;

    // Join threads (they'll finish once child closes pipe write-ends)
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    // Flush before process::exit (which does not flush stdio buffers)
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();

    Ok(status.code().unwrap_or(1))
}

async fn execute_secret_inject(
    secret_manager: &crate::secret::manager::SecretManager,
    vault: Option<String>,
    template_file: Option<String>,
    output_file: Option<String>,
    groups: Vec<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use regex::Regex;
    use std::collections::HashMap;
    use std::fs;
    use std::io::{self, Read};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Read template content
    let template_content = match template_file {
        Some(path) => fs::read_to_string(&path).map_err(|e| {
            CrosstacheError::config(format!("Failed to read template file '{}': {}", path, e))
        })?,
        None => {
            // Read from stdin
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer).map_err(|e| {
                CrosstacheError::config(format!("Failed to read from stdin: {}", e))
            })?;
            buffer
        }
    };

    // Parse template for secret references
    // Supports: {{ secret:name }} and xv://[backend:]vault/secret
    let secret_regex = Regex::new(r"\{\{\s*secret:([^}\s]+)\s*\}\}").unwrap();
    let uri_regex = Regex::new(r"xv://([^/\s]+)/([^/\s]+)").unwrap();

    let mut required_secrets: Vec<String> = Vec::new();
    let mut cross_vault_refs: Vec<(String, BackendRef)> = Vec::new(); // (original_uri, parsed_ref)

    // Find {{ secret:name }} references (current vault)
    for captures in secret_regex.captures_iter(&template_content) {
        if let Some(secret_name) = captures.get(1) {
            let name = secret_name.as_str().to_string();
            if !required_secrets.contains(&name) {
                required_secrets.push(name);
            }
        }
    }

    // Find xv://[backend:]vault/secret URI references
    for captures in uri_regex.captures_iter(&template_content) {
        let vault_part = captures.get(1).map_or("", |m| m.as_str());
        let secret_part = captures.get(2).map_or("", |m| m.as_str());
        let uri_key = format!("xv://{vault_part}/{secret_part}");
        if cross_vault_refs.iter().any(|(uri, _)| uri == &uri_key) {
            continue;
        }
        match BackendRef::parse(&format!("{vault_part}/{secret_part}")) {
            Ok(r) => cross_vault_refs.push((uri_key, r)),
            Err(e) => output::warn(&format!("Skipping invalid URI '{uri_key}': {e}")),
        }
    }

    if required_secrets.is_empty() && cross_vault_refs.is_empty() {
        output::warn("No secret references found in template");
        println!("    Use {{ secret:name }} syntax or xv://[backend:]vault/secret URIs");

        // Still write the template content as-is to output
        match output_file {
            Some(path) => {
                crate::utils::helpers::write_sensitive_file(
                    std::path::Path::new(&path),
                    template_content.as_bytes(),
                )
                .map_err(|e| {
                    CrosstacheError::config(format!(
                        "Failed to write to output file '{}': {}",
                        path, e
                    ))
                })?;
                println!("Template written to '{}'", path);
            }
            None => {
                print!("{}", template_content);
            }
        }
        return Ok(());
    }

    let total_references = required_secrets.len() + cross_vault_refs.len();
    output::info(&format!(
        "Found {} secret reference(s) in template",
        total_references
    ));

    if !required_secrets.is_empty() {
        println!(
            "  Current vault ({}): {} secret(s)",
            vault_name,
            required_secrets.len()
        );
    }
    if !cross_vault_refs.is_empty() {
        println!("  Cross-vault/backend: {} secret(s)", cross_vault_refs.len());
    }

    // Get all secrets from the vault
    let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await;
    progress.finish_clear();
    let secrets = secrets?;

    // Filter secrets by groups if specified
    let available_secrets = if !groups.is_empty() {
        secrets
            .into_iter()
            .filter(|secret| {
                if let Some(secret_groups) = &secret.groups {
                    let secret_group_list: Vec<&str> =
                        secret_groups.split(',').map(|g| g.trim()).collect();
                    groups
                        .iter()
                        .any(|filter_group| secret_group_list.contains(&filter_group.as_str()))
                } else {
                    false
                }
            })
            .collect()
    } else {
        secrets
    };

    // Build a map of secret names/URIs to values
    let mut secret_values: HashMap<String, Zeroizing<String>> = HashMap::new();
    let mut cross_vault_values: HashMap<String, Zeroizing<String>> = HashMap::new(); // URI -> value
    let mut missing_secrets: Vec<String> = Vec::new();

    // Fetch secrets from current vault
    for secret_name in &required_secrets {
        // Check if the secret exists in the available secrets
        if let Some(secret_summary) = available_secrets.iter().find(|s| s.name == *secret_name) {
            // Get the secret value
            match secret_manager
                .secret_ops()
                .get_secret(&vault_name, &secret_summary.name, true)
                .await
            {
                Ok(secret_props) => {
                    if let Some(value) = secret_props.value {
                        secret_values.insert(secret_name.clone(), value);
                    } else {
                        missing_secrets.push(secret_name.clone());
                    }
                }
                Err(e) => {
                    output::warn(&format!(
                        "Failed to get value for secret '{}' from vault '{}': {}",
                        secret_name, vault_name, e
                    ));
                    missing_secrets.push(secret_name.clone());
                }
            }
        } else {
            missing_secrets.push(secret_name.clone());
        }
    }

    // Fetch URI-referenced secrets (supports optional backend prefix)
    {
        let active_kind: BackendKind = config
            .effective_backend_name()
            .parse()
            .unwrap_or(BackendKind::Azure);

        // Cache backends by kind — avoids re-initialising the SDK per URI
        let mut cross_backends: std::collections::HashMap<
            BackendKind,
            Arc<dyn crate::backend::Backend>,
        > = std::collections::HashMap::new();

        for (uri, backend_ref) in &cross_vault_refs {
            let secret_name = match &backend_ref.secret {
                Some(s) => s.clone(),
                None => {
                    output::warn(&format!("URI '{uri}' has no secret segment — skipping"));
                    missing_secrets.push(uri.clone());
                    continue;
                }
            };

            let fetch_result = resolve_uri_secret(
                backend_ref,
                &secret_name,
                secret_manager,
                config,
                active_kind,
                &mut cross_backends,
            )
            .await;

            match fetch_result {
                Ok(secret_props) => {
                    if let Some(value) = secret_props.value {
                        cross_vault_values.insert(uri.clone(), value);
                    } else {
                        output::warn(&format!("URI '{uri}' resolved but has no value"));
                        missing_secrets.push(uri.clone());
                    }
                }
                Err(e) => {
                    output::warn(&format!("Failed to resolve URI '{uri}': {e}"));
                    missing_secrets.push(uri.clone());
                }
            }
        }
    }

    if !missing_secrets.is_empty() {
        return Err(CrosstacheError::config(format!(
            "Missing secrets: {}",
            missing_secrets.join(", ")
        )));
    }

    let total_injected = secret_values.len() + cross_vault_values.len();
    output::step(&format!(
        "Injecting {} secret(s) into template...",
        total_injected
    ));

    // Replace secret references with actual values
    let mut result_content = Zeroizing::new(template_content);

    // Replace {{ secret:name }} references (current vault)
    for (secret_name, secret_value) in &secret_values {
        let pattern = format!(r"\{{\{{\s*secret:{}\s*\}}\}}", regex::escape(secret_name));
        let regex_pattern = Regex::new(&pattern).unwrap();
        *result_content = regex_pattern
            .replace_all(&result_content, secret_value.as_str())
            .to_string();
    }

    // Replace xv://vault/secret URI references
    for (uri, secret_value) in &cross_vault_values {
        *result_content = result_content.replace(uri, secret_value.as_str());
    }

    // Write result
    match output_file {
        Some(path) => {
            crate::utils::helpers::write_sensitive_file(
                std::path::Path::new(&path),
                result_content.as_bytes(),
            )
            .map_err(|e| {
                CrosstacheError::config(format!("Failed to write to output file '{}': {}", path, e))
            })?;
            output::success(&format!(
                "Template resolved and written to '{}' (permissions: owner-only)",
                path
            ));
            output::warn("Output file contains resolved secrets -- treat as sensitive");
        }
        None => {
            print!("{}", result_content.as_str());
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_secret_list(
    secret_manager: &crate::secret::manager::SecretManager,
    group: Option<String>,
    show_all: bool,
    expiring: Option<String>,
    expired: bool,
    pagination: Pagination,
    pager: bool,
    names_only: bool,
    config: &Config,
) -> Result<Vec<crate::secret::manager::SecretSummary>> {
    use crate::config::ContextManager;
    use crate::utils::format::TableFormatter;
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};
    use std::fmt::Write as _;

    let vault_name = config.resolve_vault_name(None).await?;

    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    // Always fetch the complete unfiltered list so the caller can cache
    // the full dataset. Filters are applied in-memory below.
    let all_secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await?;

    // Apply group and enabled filters for display
    let mut secrets: Vec<_> = all_secrets.clone();
    if !show_all {
        secrets.retain(|s| s.enabled);
    }
    if let Some(ref g) = group {
        secrets.retain(|s| secret_summary_matches_group(s, g));
    }

    // Apply expiry filtering if requested (requires per-secret API calls)
    if expired || expiring.is_some() {
        use crate::utils::datetime::{is_expired, is_expiring_within};

        let mut filtered_secrets = Vec::new();

        for secret_summary in secrets {
            match secret_manager
                .get_secret_safe(&vault_name, &secret_summary.name, false, true)
                .await
            {
                Ok(secret_props) => {
                    let should_include = if expired {
                        is_expired(secret_props.expires_on)
                    } else if let Some(ref duration) = expiring {
                        match is_expiring_within(secret_props.expires_on, duration) {
                            Ok(is_exp) => is_exp,
                            Err(e) => {
                                eprintln!("Warning: Invalid duration '{}': {}", duration, e);
                                false
                            }
                        }
                    } else {
                        true
                    };

                    if should_include {
                        filtered_secrets.push(secret_summary);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to get details for secret '{}': {}",
                        secret_summary.name, e
                    );
                }
            }
        }

        secrets = filtered_secrets;
    }

    // Early exit for names-only mode (no pagination for pipe-friendly output)
    if names_only {
        for s in &secrets {
            let display = if s.original_name.is_empty() {
                &s.name
            } else {
                &s.original_name
            };
            println!("{display}");
        }
        return Ok(all_secrets);
    }

    let paged = paginate_slice(&secrets, pagination);
    let display_secrets = paged.items.clone();

    // Display results
    if human_table_like {
        let mut output = String::new();
        output.push('\n');
        // Color only for styled table; plain/raw must not emit ANSI escapes
        if !config.no_color && fmt == OutputFormat::Table {
            let _ = writeln!(output, "\x1b[36mVault: {}\x1b[0m", vault_name);
        } else {
            let _ = writeln!(output, "Vault: {}", vault_name);
        }
        output.push('\n');

        if secrets.is_empty() {
            let msg = if expired || expiring.is_some() {
                let filter_desc = if expired { "expired" } else { "expiring" };
                format!(
                    "No {} secrets found in vault '{}'.",
                    filter_desc, vault_name
                )
            } else if show_all {
                "No secrets found in vault.".to_string()
            } else {
                "No enabled secrets found in vault. Use --all to show disabled secrets.".to_string()
            };
            output.push_str(&output::format_line(
                output::Level::Info,
                &msg,
                output::should_use_rich_stdout(),
            ));
            crate::utils::pager::print_output(&output, pager)?;
        } else {
            let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
            output.push_str(&formatter.format_table(&display_secrets)?);

            let qualifier = if expired {
                Some("expired".to_string())
            } else {
                expiring
                    .as_ref()
                    .map(|duration| format!("secret(s) expiring within {duration}"))
            };
            let count_label = if let Some(ref q) = qualifier {
                if expired {
                    secret_count_label(
                        display_secrets.len(),
                        paged.total_items,
                        Some(q),
                        paged.page_size.is_some(),
                    )
                } else if paged.page_size.is_some() {
                    format!(
                        "Showing {} of {} {}",
                        display_secrets.len(),
                        paged.total_items,
                        q
                    )
                } else {
                    format!("{} {}", display_secrets.len(), q)
                }
            } else {
                secret_count_label(
                    display_secrets.len(),
                    paged.total_items,
                    None,
                    paged.page_size.is_some(),
                )
            };
            output.push('\n');
            let _ = writeln!(output, "{} in vault '{}'", count_label, vault_name);
            if let Some(footer) = pagination_footer_text(&paged, "secret", fmt) {
                output.push('\n');
                output.push_str(&footer);
            }
            crate::utils::pager::print_output(&output, pager)?;
        }
    } else {
        let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
        let output = formatter.format_table(&display_secrets)?;
        crate::utils::pager::print_output(&output, pager)?;
    }

    Ok(all_secrets)
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
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm(
            &format!("Are you sure you want to delete secret '{name}' from vault '{vault_name}'?"),
            false,
        )? {
            println!("Delete operation cancelled.");
            return Ok(());
        }
    }

    secret_manager
        .delete_secret_safe(&vault_name, name, force)
        .await?;
    output::success(&format!("Successfully deleted secret '{name}'"));
    output::hint(&format!(
        "Undo with 'xv restore {name}' (before purge retention expires)"
    ));

    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
    expires: Option<String>,
    not_before: Option<String>,
    clear_expires: bool,
    clear_not_before: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretUpdateRequest;
    use std::collections::HashMap;
    use std::io::{self, Read};

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(vault).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get new value if explicitly provided (but don't prompt)
    let new_value = if let Some(v) = value {
        // Validate provided value
        if v.is_empty() {
            return Err(CrosstacheError::config("Secret value cannot be empty"));
        }
        Some(Zeroizing::new(v))
    } else if stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        let trimmed = buffer.trim().to_string();
        if trimmed.is_empty() {
            return Err(CrosstacheError::config("Secret value cannot be empty"));
        }
        Some(Zeroizing::new(trimmed))
    } else {
        None // Don't update value, just metadata
    };

    // Ensure at least one update is specified
    if new_value.is_none()
        && tags.is_empty()
        && groups.is_empty()
        && rename.is_none()
        && note.is_none()
        && folder.is_none()
        && expires.is_none()
        && not_before.is_none()
        && !clear_expires
        && !clear_not_before
    {
        return Err(CrosstacheError::invalid_argument(
            "No updates specified. Use 'secret update' to modify metadata (groups, tags, folder, note, expiry) or rename secrets. Use 'secret set' to update secret values.",
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
            return Err(CrosstacheError::invalid_argument(
                "New secret name cannot be empty",
            ));
        }
        if new_name == name {
            return Err(CrosstacheError::invalid_argument(
                "New secret name must be different from current name",
            ));
        }
    }

    // Parse expiry dates if provided
    let expires_on = if clear_expires {
        None // Explicitly clear the expiry date
    } else if let Some(expires_str) = expires.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(expires_str)?)
    } else {
        None // No change to expiry
    };

    let not_before_on = if clear_not_before {
        None // Explicitly clear the not-before date
    } else if let Some(not_before_str) = not_before.as_deref() {
        use crate::utils::datetime::parse_datetime_or_duration;
        Some(parse_datetime_or_duration(not_before_str)?)
    } else {
        None // No change to not-before
    };

    // Create update request with enhanced parameters
    let update_request = SecretUpdateRequest {
        name: name.to_string(),
        new_name: rename.clone(),
        value: new_value.clone(),
        content_type: None,
        enabled: None,
        expires_on,
        not_before: not_before_on,
        tags: tags_map,
        groups: groups_vec,
        note: note.clone(),
        folder: folder.clone(),
        replace_tags,
        replace_groups,
    };

    // Show update summary
    println!("Updating secret '{name}'...");
    if let Some(ref new_name) = rename {
        println!("  → Renaming to: {new_name}");
    }
    if new_value.is_some() {
        println!("  → Updating value");
    }
    if !update_request
        .tags
        .as_ref()
        .map(|t| t.is_empty())
        .unwrap_or(true)
    {
        let action = if replace_tags { "Replacing" } else { "Merging" };
        println!(
            "  → {} tags: {}",
            action,
            update_request.tags.as_ref().unwrap().len()
        );
    }
    if !update_request
        .groups
        .as_ref()
        .map(|g| g.is_empty())
        .unwrap_or(true)
    {
        let action = if replace_groups {
            "Replacing"
        } else {
            "Adding to"
        };
        println!(
            "  → {} groups: {:?}",
            action,
            update_request.groups.as_ref().unwrap()
        );
    }
    if let Some(ref note_text) = note {
        println!("  → Adding note: {note_text}");
    }
    if let Some(ref folder_path) = folder {
        println!("  → Setting folder: {folder_path}");
    }
    if clear_expires {
        println!("  → Clearing expiry date");
    } else if let Some(ref expires_str) = expires {
        println!("  → Setting expiry: {expires_str}");
    }
    if clear_not_before {
        println!("  → Clearing not-before date");
    } else if let Some(ref not_before_str) = not_before {
        println!("  → Setting not-before: {not_before_str}");
    }

    // Perform enhanced secret update
    let secret = secret_manager
        .update_secret_enhanced(&vault_name, &update_request)
        .await?;

    output::success(&format!(
        "Successfully updated secret '{}'",
        secret.original_name
    ));
    println!("   Vault: {vault_name}");
    println!("   Version: {}", secret.version);

    if let Some(ref new_name) = rename {
        println!("   New Name: {new_name}");
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
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm(&format!(
            "Are you sure you want to PERMANENTLY DELETE secret '{name}' from vault '{vault_name}'? This cannot be undone!"
        ), false)? {
            println!("Purge operation cancelled.");
            return Ok(());
        }
    }

    // Permanently purge the secret using the secret manager
    secret_manager
        .purge_secret_safe(&vault_name, name, force)
        .await?;
    output::success(&format!("Successfully purged secret '{name}'"));
    output::warn("This is permanent. The secret cannot be recovered.");

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

    println!("Restoring deleted secret '{name}'...");

    // Restore the secret using the secret manager
    let restored_secret = secret_manager
        .restore_secret_safe(&vault_name, name)
        .await?;

    output::success(&format!(
        "Successfully restored secret '{}'",
        restored_secret.original_name
    ));
    println!("   Vault: {vault_name}");
    println!("   Version: {}", restored_secret.version);
    println!("   Enabled: {}", restored_secret.enabled);
    println!("   Created: {}", restored_secret.created_on);
    println!("   Updated: {}", restored_secret.updated_on);

    if !restored_secret.tags.is_empty() {
        println!("   Tags: {}", restored_secret.tags.len());
    }

    Ok(())
}

async fn execute_secret_copy(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    _config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::secret::manager::SecretRequest;

    // Determine target name (use new_name if provided, otherwise use original)
    let target_name = new_name.as_deref().unwrap_or(name);

    println!(
        "Copying secret '{}' from vault '{}' to vault '{}' as '{}'...",
        name, from_vault, to_vault, target_name
    );

    // Get the source secret with all its metadata
    let source_secret = secret_manager
        .get_secret_safe(from_vault, name, true, true)
        .await?;

    // Check if target secret already exists
    if secret_manager
        .get_secret_safe(to_vault, target_name, false, true)
        .await
        .is_ok()
    {
        return Err(CrosstacheError::config(format!(
            "Secret '{}' already exists in vault '{}'. Use 'xv move' with --force or delete the target secret first.",
            target_name, to_vault
        )));
    }

    // Create the request for the target vault preserving all metadata
    let secret_request = SecretRequest {
        name: target_name.to_string(),
        value: source_secret.value.unwrap_or_default(),
        content_type: Some(source_secret.content_type),
        enabled: Some(source_secret.enabled),
        expires_on: source_secret.expires_on,
        not_before: source_secret.not_before,
        tags: Some(source_secret.tags),
        groups: None, // Will be preserved through tags
        note: None,   // Will be preserved through tags
        folder: None, // Will be preserved through tags
    };

    // Set the secret in the target vault
    let value = secret_request.value.clone();
    let copied_secret = secret_manager
        .set_secret_safe(to_vault, target_name, &value, Some(secret_request))
        .await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(to_vault).await;

    output::success(&format!(
        "Successfully copied secret '{}' to vault '{}'",
        copied_secret.original_name, to_vault
    ));
    println!("   Source: {}/{}", from_vault, name);
    println!("   Target: {}/{}", to_vault, target_name);
    println!("   Version: {}", copied_secret.version);
    println!("   Enabled: {}", copied_secret.enabled);

    if let Some(expires_on) = copied_secret.expires_on {
        use crate::utils::datetime::format_datetime;
        println!("   Expires: {}", format_datetime(Some(expires_on)));
    }

    Ok(())
}

async fn execute_secret_move(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    from_vault: &str,
    to_vault: &str,
    new_name: Option<String>,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::utils::interactive::InteractivePrompt;

    // Determine target name (use new_name if provided, otherwise use original)
    let target_name = new_name.as_deref().unwrap_or(name);

    println!(
        "Moving secret '{}' from vault '{}' to vault '{}' as '{}'...",
        name, from_vault, to_vault, target_name
    );

    // Confirmation prompt if not forced
    if !force {
        let prompt = InteractivePrompt::new();
        let message = format!(
            "This will delete secret '{}' from vault '{}' after copying it to vault '{}'. Continue?",
            name, from_vault, to_vault
        );
        if !prompt.confirm(&message, false)? {
            println!("Move operation cancelled.");
            return Ok(());
        }
    }

    // Check if target secret already exists and handle accordingly
    if secret_manager
        .get_secret_safe(to_vault, target_name, false, true)
        .await
        .is_ok()
    {
        if !force {
            return Err(CrosstacheError::config(format!(
                "Secret '{}' already exists in vault '{}'. Use --force to overwrite.",
                target_name, to_vault
            )));
        } else {
            output::warn(&format!(
                "Overwriting existing secret '{}' in vault '{}'",
                target_name, to_vault
            ));
        }
    }

    // First copy the secret
    execute_secret_copy(
        secret_manager,
        name,
        from_vault,
        to_vault,
        new_name.clone(),
        config,
    )
    .await?;

    // Then delete from source
    println!(
        "Deleting source secret '{}' from vault '{}'...",
        name, from_vault
    );
    secret_manager
        .delete_secret_safe(from_vault, name, true)
        .await?;

    output::success(&format!(
        "Successfully moved secret '{}' from '{}' to '{}'",
        name, from_vault, to_vault
    ));

    Ok(())
}

async fn execute_secret_parse(
    secret_manager: &crate::secret::manager::SecretManager,
    connection_string: &str,
    format: &str,
    config: &Config,
) -> Result<()> {
    let components = secret_manager
        .parse_connection_string(connection_string)
        .await?;

    match format.to_lowercase().as_str() {
        "json" => {
            let json_output = serde_json::to_string_pretty(&components).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize components: {e}"))
            })?;
            println!("{json_output}");
        }
        "table" => {
            if components.is_empty() {
                println!("No components found in connection string");
            } else {
                use crate::utils::format::format_table;
                use tabled::Table;

                let table = Table::new(&components);
                println!("{}", format_table(table, config.no_color));
            }
        }
        _ => {
            return Err(CrosstacheError::invalid_argument(format!(
                "Unsupported format '{format}' for this command. Use 'json' or 'table'."
            )));
        }
    }

    Ok(())
}

async fn execute_secret_share(
    vault_manager: &crate::vault::manager::VaultManager,
    auth_provider: &std::sync::Arc<dyn crate::auth::provider::AzureAuthProvider>,
    command: ShareCommands,
    config: &Config,
) -> Result<()> {
    use crate::vault::models::AccessLevel;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;
    let resource_group = config.default_resource_group.clone();

    match command {
        ShareCommands::Grant {
            secret_name,
            user,
            level,
        } => {
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
                .grant_secret_access(
                    &vault_name,
                    &resource_group,
                    &secret_name,
                    &object_id,
                    access_level,
                )
                .await?;

            println!(
                "Successfully granted {} access to secret '{}' for '{}' in vault '{}'",
                level, secret_name, user, vault_name
            );
        }
        ShareCommands::Revoke { secret_name, user } => {
            let object_id = auth_provider.resolve_user_to_object_id(&user).await?;
            if object_id != user {
                println!("Resolved '{}' to object ID '{}'", user, object_id);
            }

            vault_manager
                .revoke_secret_access(&vault_name, &resource_group, &secret_name, &object_id)
                .await?;

            println!(
                "Successfully revoked access to secret '{}' for '{}' in vault '{}'",
                secret_name, user, vault_name
            );
        }
        ShareCommands::List {
            secret_name,
            all,
            page,
            page_size,
            pager,
        } => {
            use crate::utils::pagination::{paginate_slice, pagination_footer_text, Pagination};
            use std::fmt::Write as _;

            let mut roles = vault_manager
                .list_secret_access(&vault_name, &resource_group, &secret_name)
                .await?;

            vault_manager
                .resolve_and_filter_roles(&mut roles, all)
                .await?;

            let pagination = Pagination::from_args(page, page_size)?;
            let paged = paginate_slice(&roles, pagination);

            if roles.is_empty() {
                println!(
                    "No access assignments found for secret '{}' in vault '{}'",
                    secret_name, vault_name
                );
            } else {
                let mut output = String::new();
                let _ = writeln!(
                    output,
                    "Access assignments for secret '{}' in vault '{}':",
                    secret_name, vault_name
                );
                let formatter = crate::utils::format::TableFormatter::new(
                    crate::utils::format::OutputFormat::Table,
                    config.no_color,
                    None,
                );
                let table_output = formatter.format_table(&paged.items)?;
                output.push_str(&table_output);
                if let Some(footer) = pagination_footer_text(
                    &paged,
                    "assignment",
                    crate::utils::format::OutputFormat::Table,
                ) {
                    output.push('\n');
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
        }
    }

    Ok(())
}

/// Execute bulk secret set operation
async fn execute_secret_set_bulk(
    secret_manager: &crate::secret::manager::SecretManager,
    args: Vec<String>,
    note: Option<String>,
    folder: Option<String>,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use std::fs;
    use std::path::Path;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Parse KEY=value pairs
    let mut secrets_to_set = Vec::new();

    for arg in args {
        if let Some(pos) = arg.find('=') {
            let key = arg[..pos].trim();
            let value_part = arg[pos + 1..].trim();

            if key.is_empty() {
                return Err(CrosstacheError::invalid_argument(format!(
                    "Invalid KEY=value pair: empty key in '{}'",
                    arg
                )));
            }

            // Handle @file syntax for value
            let value = if value_part.starts_with('@') {
                let file_path = value_part.strip_prefix('@').unwrap();

                if !Path::new(file_path).exists() {
                    return Err(CrosstacheError::config(format!(
                        "File not found: {}",
                        file_path
                    )));
                }

                fs::read_to_string(file_path).map_err(|e| {
                    CrosstacheError::config(format!("Failed to read file '{}': {}", file_path, e))
                })?
            } else {
                value_part.to_string()
            };

            if value.is_empty() {
                return Err(CrosstacheError::config(format!(
                    "Secret value cannot be empty for key '{}'",
                    key
                )));
            }

            secrets_to_set.push((key.to_string(), value));
        } else {
            return Err(CrosstacheError::invalid_argument(format!(
                "Invalid format: '{}'. Expected KEY=value or KEY=@/path/to/file",
                arg
            )));
        }
    }

    if secrets_to_set.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "No valid KEY=value pairs provided",
        ));
    }

    output::step(&format!(
        "Setting {} secret(s) in vault '{}'...",
        secrets_to_set.len(),
        vault_name
    ));

    let mut success_count = 0;
    let mut error_count = 0;

    for (key, value) in secrets_to_set {
        // Create secret request with note and/or folder if provided
        let secret_request = if note.is_some() || folder.is_some() {
            Some(crate::secret::manager::SecretRequest {
                name: key.clone(),
                value: Zeroizing::new(value.clone()),
                content_type: None,
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: note.clone(),
                folder: folder.clone(),
            })
        } else {
            None
        };

        match secret_manager
            .set_secret_safe(&vault_name, &key, &value, secret_request)
            .await
        {
            Ok(secret) => {
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &format!(
                            "{}: {} (version {})",
                            key, secret.original_name, secret.version
                        ),
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  {}",
                    output::format_line(
                        output::Level::Error,
                        &format!("{}: {}", key, e),
                        output::should_use_rich_stderr(),
                    )
                );
                error_count += 1;
            }
        }
    }

    println!();
    output::info("Bulk Set Summary:");
    println!(
        "  {}",
        output::format_line(
            output::Level::Success,
            &format!("Successful: {}", success_count),
            output::should_use_rich_stdout()
        )
    );
    if error_count > 0 {
        println!(
            "  {}",
            output::format_line(
                output::Level::Error,
                &format!("Failed: {}", error_count),
                output::should_use_rich_stdout()
            )
        );
    }

    if error_count > 0 {
        Err(CrosstacheError::config(format!(
            "{} secret(s) failed to set",
            error_count
        )))
    } else {
        Ok(())
    }
}

/// Execute group delete operation
async fn execute_secret_delete_group(
    secret_manager: &crate::secret::manager::SecretManager,
    group_name: &str,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;

    // Determine vault name using context resolution
    let vault_name = config.resolve_vault_name(None).await?;

    // Update context usage tracking
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Get all secrets from the vault
    let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, Some(group_name))
        .await;
    progress.finish_clear();
    let secrets = secrets?;

    if secrets.is_empty() {
        output::info(&format!("No secrets found in group '{}'", group_name));
        return Ok(());
    }

    output::info(&format!(
        "Found {} secret(s) in group '{}' to delete:",
        secrets.len(),
        group_name
    ));

    for secret in &secrets {
        println!("  - {}", secret.name);
    }

    // Confirmation unless forced
    if !force {
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm(
            &format!(
                "Are you sure you want to delete ALL {} secret(s) in group '{}'?",
                secrets.len(),
                group_name
            ),
            false,
        )? {
            output::info("Group delete operation cancelled.");
            return Ok(());
        }
    }

    output::step(&format!(
        "Deleting {} secret(s) from group '{}'...",
        secrets.len(),
        group_name
    ));

    let mut success_count = 0;
    let mut error_count = 0;

    for secret in secrets {
        match secret_manager
            .delete_secret_safe(&vault_name, &secret.name, true) // force=true to avoid individual prompts
            .await
        {
            Ok(_) => {
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &format!("Deleted: {}", secret.name),
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  {}",
                    output::format_line(
                        output::Level::Error,
                        &format!("Failed to delete '{}': {}", secret.name, e),
                        output::should_use_rich_stderr(),
                    )
                );
                error_count += 1;
            }
        }
    }

    println!();
    output::info("Group Delete Summary:");
    println!(
        "  {}",
        output::format_line(
            output::Level::Success,
            &format!("Successful: {}", success_count),
            output::should_use_rich_stdout()
        )
    );
    if error_count > 0 {
        println!(
            "  {}",
            output::format_line(
                output::Level::Error,
                &format!("Failed: {}", error_count),
                output::should_use_rich_stdout()
            )
        );
    }

    if error_count > 0 {
        Err(CrosstacheError::config(format!(
            "{} secret(s) failed to delete from group '{}'",
            error_count, group_name
        )))
    } else {
        output::success(&format!(
            "Successfully deleted all secrets from group '{}'",
            group_name
        ));
        Ok(())
    }
}

/// Parse bulk set arguments into (key, value) pairs.
/// Supports `KEY=value` and `KEY=@/path/to/file` syntax.
fn parse_bulk_set_args(args: Vec<String>) -> Result<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    for arg in args {
        if let Some(pos) = arg.find('=') {
            let key = arg[..pos].trim();
            let value_part = arg[pos + 1..].trim();
            if key.is_empty() {
                return Err(CrosstacheError::invalid_argument(format!(
                    "Invalid KEY=value pair: empty key in '{arg}'"
                )));
            }
            let value = if value_part.starts_with('@') {
                let file_path = value_part.strip_prefix('@').unwrap();
                if !std::path::Path::new(file_path).exists() {
                    return Err(CrosstacheError::config(format!(
                        "File not found: {file_path}"
                    )));
                }
                std::fs::read_to_string(file_path).map_err(|e| {
                    CrosstacheError::config(format!("Failed to read file '{file_path}': {e}"))
                })?
            } else {
                value_part.to_string()
            };
            if value.is_empty() {
                return Err(CrosstacheError::config(format!(
                    "Secret value cannot be empty for key '{key}'"
                )));
            }
            pairs.push((key.to_string(), value));
        } else {
            return Err(CrosstacheError::invalid_argument(format!(
                "Invalid format: '{arg}'. Expected KEY=value or KEY=@/path/to/file"
            )));
        }
    }
    if pairs.is_empty() {
        return Err(CrosstacheError::invalid_argument(
            "No valid KEY=value pairs provided",
        ));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::error::BackendError;
    use crate::backend::secret::SecretBackend;
    use crate::backend::{Backend, BackendCapabilities, BackendKind, NameCharset};
    use std::process::{Command, Stdio};

    struct TestBackend {
        kind: BackendKind,
    }

    impl TestBackend {
        fn azure() -> Self {
            Self {
                kind: BackendKind::Azure,
            }
        }

        fn local() -> Self {
            Self {
                kind: BackendKind::Local,
            }
        }
    }

    #[async_trait::async_trait]
    impl SecretBackend for TestBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: crate::secret::manager::SecretRequest,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<crate::secret::manager::SecretSummary>, BackendError> {
            Ok(Vec::new())
        }

        async fn delete_secret(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }

        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: crate::secret::manager::SecretUpdateRequest,
        ) -> std::result::Result<crate::secret::manager::SecretProperties, BackendError> {
            Err(BackendError::Unsupported("test backend".into()))
        }
    }

    #[async_trait::async_trait]
    impl Backend for TestBackend {
        fn name(&self) -> &'static str {
            match self.kind {
                BackendKind::Azure => "azure",
                BackendKind::Local => "local",
                BackendKind::Aws => "aws",
            }
        }

        fn kind(&self) -> BackendKind {
            self.kind
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                has_vaults: self.kind == BackendKind::Local,
                has_file_storage: false,
                has_rbac: false,
                has_audit: false,
                has_versioning: true,
                has_soft_delete: true,
                has_secret_rotation: false,
                has_groups: true,
                has_folders: true,
                has_notes: true,
                has_expiry: true,
                max_secret_size: None,
                max_name_length: None,
                name_charset: NameCharset::Unrestricted,
            }
        }

        fn secrets(&self) -> &dyn SecretBackend {
            self
        }

        async fn health_check(&self) -> std::result::Result<(), BackendError> {
            Ok(())
        }
    }

    /// Helper: run stream_and_mask but redirect its print!/eprint! output to files
    /// so we can verify masking actually happened.
    fn stream_and_mask_to_files(
        mut child: std::process::Child,
        secret_values: Vec<Zeroizing<String>>,
        stdout_file: &std::path::Path,
        stderr_file: &std::path::Path,
    ) -> i32 {
        use std::fs::OpenOptions;
        use std::io::{BufRead, BufReader, Write};

        let stdout_handle = child.stdout.take().expect("stdout was piped");
        let stderr_handle = child.stderr.take().expect("stderr was piped");

        let secrets = Arc::new(secret_values);
        let secrets_for_stderr = Arc::clone(&secrets);

        let stdout_path = stdout_file.to_path_buf();
        let stderr_path = stderr_file.to_path_buf();

        let stdout_thread = std::thread::spawn(move || {
            let mut out = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&stdout_path)
                .unwrap();
            let mut reader = BufReader::new(stdout_handle);
            let mut buf = Vec::new();
            while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
                let line = String::from_utf8_lossy(&buf);
                let masked = mask_secrets(&line, &secrets);
                write!(out, "{}", masked).unwrap();
                buf.clear();
            }
        });

        let stderr_thread = std::thread::spawn(move || {
            let mut out = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&stderr_path)
                .unwrap();
            let mut reader = BufReader::new(stderr_handle);
            let mut buf = Vec::new();
            while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
                let line = String::from_utf8_lossy(&buf);
                let masked = mask_secrets(&line, &secrets_for_stderr);
                write!(out, "{}", masked).unwrap();
                buf.clear();
            }
        });

        let status = child.wait().expect("failed to wait on child");
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        status.code().unwrap_or(1)
    }

    fn summary_with_groups(groups: Option<&str>) -> crate::secret::manager::SecretSummary {
        summary_named("secret", groups, true)
    }

    fn summary_named(
        name: &str,
        groups: Option<&str>,
        enabled: bool,
    ) -> crate::secret::manager::SecretSummary {
        crate::secret::manager::SecretSummary {
            name: name.to_string(),
            original_name: name.to_string(),
            note: None,
            folder: None,
            groups: groups.map(str::to_string),
            updated_on: "2026-04-28".to_string(),
            enabled,
            content_type: String::new(),
        }
    }

    #[tokio::test]
    async fn azure_trait_vault_resolution_does_not_fallback_to_default() {
        let registry = BackendRegistry::new(Arc::new(TestBackend::azure()));
        let config = Config {
            backend: Some("azure".to_string()),
            default_vault: String::new(),
            ..Default::default()
        };

        let err = resolve_vault_for_trait(&config, Some(&registry))
            .await
            .expect_err("azure should preserve missing-vault config error");
        assert!(err.to_string().contains("No vault specified"));
    }

    #[tokio::test]
    async fn local_trait_vault_resolution_can_fallback_to_local_default() {
        let registry = BackendRegistry::new(Arc::new(TestBackend::local()));
        let config = Config {
            backend: Some("local".to_string()),
            default_vault: String::new(),
            local: Some(crate::config::settings::LocalConfig {
                store_path: None,
                key_file: None,
                default_vault: Some("local-vault".to_string()),
            }),
            ..Default::default()
        };

        let resolved = resolve_vault_for_trait(&config, Some(&registry))
            .await
            .unwrap();
        assert_eq!(resolved, "local-vault");
    }

    #[test]
    fn expiry_filter_candidates_apply_group_and_enabled_filters_before_detail_fetches() {
        let candidates = filter_secret_summaries_for_display(
            vec![
                summary_named("prod-enabled", Some("prod"), true),
                summary_named("prod-disabled", Some("prod"), false),
                summary_named("dev-enabled", Some("dev"), true),
                summary_named("ungrouped", None, true),
            ],
            Some("prod"),
            false,
        );

        let names: Vec<_> = candidates.into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["prod-enabled"]);
    }

    #[test]
    fn trait_secret_cache_key_and_invalidation_use_same_resolved_vault_name() {
        let key = trait_secret_cache_key("local-vault");
        assert_eq!(key.to_string(), "secrets:local-vault");
    }

    #[test]
    fn test_secret_summary_group_filter_is_exact_comma_separated_match() {
        assert!(secret_summary_matches_group(
            &summary_with_groups(Some("prod, infra")),
            "prod"
        ));
        assert!(secret_summary_matches_group(
            &summary_with_groups(Some("prod, infra")),
            "infra"
        ));
        assert!(!secret_summary_matches_group(
            &summary_with_groups(Some("production")),
            "prod"
        ));
        assert!(!secret_summary_matches_group(
            &summary_with_groups(None),
            "prod"
        ));
    }

    #[test]
    fn test_secret_count_label_distinguishes_paginated_total() {
        assert_eq!(
            secret_count_label(10, 137, None, true),
            "Showing 10 of 137 secret(s)"
        );
        assert_eq!(secret_count_label(137, 137, None, false), "137 secret(s)");
    }

    #[test]
    fn test_stream_and_mask_stdout_masks_secrets() {
        let secret = Zeroizing::new("SUPERSECRET".to_string());
        let secrets = vec![secret];
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        let stderr_path = dir.path().join("stderr.txt");

        let child = Command::new("echo")
            .arg("hello SUPERSECRET world")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn echo");

        let exit_code = stream_and_mask_to_files(child, secrets, &stdout_path, &stderr_path);
        assert_eq!(exit_code, 0);

        let output = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(
            output.contains("[MASKED]"),
            "Expected [MASKED] in stdout, got: {}",
            output
        );
        assert!(
            !output.contains("SUPERSECRET"),
            "Secret should not appear in output"
        );
    }

    #[test]
    fn test_stream_and_mask_both_streams() {
        let secret = Zeroizing::new("TOPSECRET".to_string());
        let secrets = vec![secret];
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        let stderr_path = dir.path().join("stderr.txt");

        let child = Command::new("sh")
            .arg("-c")
            .arg("echo 'stdout TOPSECRET line'; echo 'stderr TOPSECRET line' >&2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask_to_files(child, secrets, &stdout_path, &stderr_path);
        assert_eq!(exit_code, 0);

        let stdout_output = std::fs::read_to_string(&stdout_path).unwrap();
        let stderr_output = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(
            stdout_output.contains("[MASKED]"),
            "Expected [MASKED] in stdout"
        );
        assert!(
            stderr_output.contains("[MASKED]"),
            "Expected [MASKED] in stderr"
        );
        assert!(
            !stdout_output.contains("TOPSECRET"),
            "Secret should not appear in stdout"
        );
        assert!(
            !stderr_output.contains("TOPSECRET"),
            "Secret should not appear in stderr"
        );
    }

    #[test]
    fn test_stream_and_mask_exit_code() {
        let secrets = vec![];

        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 42")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask(child, secrets).unwrap();
        assert_eq!(exit_code, 42);
    }

    #[test]
    fn test_stream_and_mask_large_output_no_oom() {
        // Verify streaming works for output larger than typical pipe buffer (64KB)
        let secret = Zeroizing::new("HIDDEN".to_string());
        let secrets = vec![secret];

        let child = Command::new("sh")
            .arg("-c")
            // Use awk for portability (seq not available in all environments)
            .arg("awk 'BEGIN{for(i=1;i<=3000;i++) print \"line \" i \" contains HIDDEN data\"}'")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask(child, secrets).unwrap();
        assert_eq!(exit_code, 0);
    }
}
