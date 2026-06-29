//! Config, context, cache, and environment command execution handlers.

use crate::cli::commands::{
    CacheCommands, CharsetType, ConfigCommands, ContextCommands, EnvCommands,
};
use crate::cli::helpers::format_cache_size;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::output;
use crate::vault::VaultManager;
use zeroize::Zeroizing;

// ── Config ───────────────────────────────────────────────────────────────────

pub(crate) async fn execute_config_command(command: ConfigCommands, config: Config) -> Result<()> {
    match command {
        ConfigCommands::Show { resolved } => {
            if resolved {
                execute_config_show_resolved(&config).await?;
            } else {
                execute_config_show(&config).await?;
            }
        }
        ConfigCommands::Set { key, value } => {
            execute_config_set(&key, &value, config).await?;
        }
        ConfigCommands::Path => {
            execute_config_path().await?;
        }
        ConfigCommands::Edit => {
            execute_config_edit(&config).await?;
        }
    }
    Ok(())
}

async fn execute_config_show(config: &Config) -> Result<()> {
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    #[derive(Tabled)]
    struct ConfigItem {
        #[tabled(rename = "Setting")]
        key: String,
        #[tabled(rename = "Value")]
        value: String,
        #[tabled(rename = "Source")]
        source: String,
    }

    let items = vec![
        ConfigItem {
            key: "debug".to_string(),
            value: config.debug.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "subscription_id".to_string(),
            value: if config.subscription_id.is_empty() {
                "<not set>".to_string()
            } else {
                config.subscription_id.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_vault".to_string(),
            value: if config.default_vault.is_empty() {
                "<not set>".to_string()
            } else {
                config.default_vault.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_resource_group".to_string(),
            value: config.default_resource_group.clone(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "default_location".to_string(),
            value: config.default_location.clone(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "tenant_id".to_string(),
            value: if config.tenant_id.is_empty() {
                "<not set>".to_string()
            } else {
                config.tenant_id.clone()
            },
            source: "config".to_string(),
        },
        ConfigItem {
            key: "cache_enabled".to_string(),
            value: config.cache_enabled.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "cache_ttl_secs".to_string(),
            value: format!("{}s", config.cache_ttl_secs),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "output_json".to_string(),
            value: config.output_json.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "no_color".to_string(),
            value: config.no_color.to_string(),
            source: "config".to_string(),
        },
    ];

    // Add blob storage configuration items
    let mut items = items;
    let blob_config = config.get_blob_config();

    // Add credential priority
    items.push(ConfigItem {
        key: "azure_credential_priority".to_string(),
        value: config.azure_credential_priority.to_string(),
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "storage_account".to_string(),
        value: if blob_config.storage_account.is_empty() {
            "<not set>".to_string()
        } else {
            blob_config.storage_account
        },
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "storage_container".to_string(),
        value: blob_config.container_name,
        source: "config".to_string(),
    });

    if let Some(endpoint) = blob_config.endpoint {
        items.push(ConfigItem {
            key: "storage_endpoint".to_string(),
            value: endpoint,
            source: "config".to_string(),
        });
    }

    items.push(ConfigItem {
        key: "blob_chunk_size_mb".to_string(),
        value: blob_config.chunk_size_mb.to_string(),
        source: "config".to_string(),
    });

    items.push(ConfigItem {
        key: "blob_max_concurrent_uploads".to_string(),
        value: blob_config.max_concurrent_uploads.to_string(),
        source: "config".to_string(),
    });

    let items = items;

    if config.output_json {
        let json_output = serde_json::to_string_pretty(config).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize config: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let table = Table::new(&items);
        println!("{}", format_table(table, config.no_color));
    }

    Ok(())
}

async fn execute_config_path() -> Result<()> {
    let config_path = Config::get_config_path()?;
    println!("{}", config_path.display());
    Ok(())
}

/// `xv config edit` — open the config file in the user's editor.
///
/// Editor resolution, highest priority first:
///   1. `$VISUAL`
///   2. `$EDITOR`
///   3. platform default (`nano` on Unix, `notepad` on Windows)
///
/// When the config file does not yet exist it is seeded with a valid,
/// serialized default configuration (NOT an empty file — an empty file
/// fails to parse on the next load with "missing field `debug`"). The
/// parent directory is created as needed so the editor always opens on a
/// real, writable path. `$VISUAL`/`$EDITOR` may contain arguments (e.g.
/// `code --wait`), so the value is split on whitespace: the first token is
/// the program and the rest are passed as leading arguments.
async fn execute_config_edit(config: &Config) -> Result<()> {
    use std::process::Command;

    let config_path = Config::get_config_path()?;

    // Seed a missing config file with a VALID default so the editor opens on
    // a real path and the next `xv` invocation can still parse it. We never
    // clobber an existing file. `save_config` handles parent-dir creation
    // and the 0600 sensitive-file write.
    if !config_path.exists() {
        crate::config::settings::save_config(config)
            .await
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "Failed to create config file {}: {e}",
                    config_path.display()
                ))
            })?;
    }

    let editor = resolve_editor();
    // Split so `$EDITOR` values like `code --wait` or `emacs -nw` work.
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| CrosstacheError::config("Resolved editor command was empty".to_string()))?;

    let status = Command::new(program)
        .args(parts)
        .arg(&config_path)
        .status()
        .map_err(|e| {
            CrosstacheError::config(format!(
                "Failed to launch editor '{program}': {e}. \
                 Set $EDITOR or $VISUAL to a valid editor command."
            ))
        })?;

    if !status.success() {
        return Err(CrosstacheError::config(format!(
            "Editor '{program}' exited with a non-zero status: {status}"
        )));
    }

    Ok(())
}

/// Resolve the editor command from `$VISUAL`, then `$EDITOR`, then a
/// platform default. Empty/whitespace-only env values are ignored.
fn resolve_editor() -> String {
    resolve_editor_from(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok())
}

/// Pure resolution logic for [`resolve_editor`], split out so it can be
/// unit-tested without mutating process-global environment variables.
///
/// Precedence: `visual` (`$VISUAL`) > `editor` (`$EDITOR`) > platform
/// default. Empty or whitespace-only values are treated as unset.
fn resolve_editor_from(visual: Option<String>, editor: Option<String>) -> String {
    for candidate in [visual, editor].into_iter().flatten() {
        if !candidate.trim().is_empty() {
            return candidate;
        }
    }

    #[cfg(windows)]
    {
        "notepad".to_string()
    }
    #[cfg(not(windows))]
    {
        "nano".to_string()
    }
}

/// `xv config show --resolved` — prints the effective active backend,
/// env, vault, and resource group AND the source layer of each value.
///
/// Resolution layers (per `config::project::resolve_effective_backend`):
///   1. `--backend` CLI flag
///   2. active `.xv.toml` env profile's `backend` field
///   3. global config `backend` key (or `XV_BACKEND` env var, which loads
///      into the same field)
///   4. built-in default (`azure`)
///
/// We can't introspect the CLI flag from here (the value already collapsed
/// into `config.backend` in `main::run`), so we replay the resolution by
/// looking at the same inputs `main` did:
///   - `XV_BACKEND` env var (if set, the global config's backend came from
///     there)
///   - active `.xv.toml` env profile
///   - global config file
async fn execute_config_show_resolved(config: &Config) -> Result<()> {
    use crate::config::project;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    #[derive(Tabled, serde::Serialize)]
    struct Row {
        #[tabled(rename = "Setting")]
        setting: String,
        #[tabled(rename = "Value")]
        value: String,
        #[tabled(rename = "Source")]
        source: String,
    }

    // --- Discover the active .xv.toml (if any) ---
    let cwd = std::env::current_dir().ok();
    let project_hit = if let Some(ref c) = cwd {
        project::find_project_config(c).await.ok().flatten()
    } else {
        None
    };

    // Resolve active env profile (if a .xv.toml was found). Track WHY
    // resolution failed when it did — Bugbot finding on PR #216 (Medium):
    // displaying vault/RG values that real commands won't use is worse
    // than showing nothing.
    let (project_path, active_env_name, active_profile, env_resolve_err) = match &project_hit {
        Some((path, cfg)) => match project::resolve_env(cfg, config.env_flag.as_deref()) {
            Ok((name, profile)) => (
                Some(path.clone()),
                Some(name.to_string()),
                Some(profile.clone()),
                None,
            ),
            Err(e) => (Some(path.clone()), None, None, Some(format!("{e}"))),
        },
        None => (None, None, None, None),
    };

    // --- Backend resolution ---
    // Precedence per resolve_effective_backend:
    //   1. --backend CLI flag (if actually on the cmdline)
    //   2. clap's `env = XV_BACKEND` populates cli.backend (== Some(env_val))
    //   3. .xv.toml [env.<active>] backend
    //   4. global config backend (xv.conf on disk)
    //   5. built-in default "azure"
    //
    // Bugbot findings #1/#2 on PR #216: the v1 logic infer-by-string-match
    // got CLI/env/profile confused when values coincided, and labeled the
    // built-in default as "global config" because main.rs always overwrites
    // `config.backend` with the resolved value. Fix by reading the original
    // sources (cli_backend, cli_backend_was_arg, disk_backend) stashed in
    // main.rs BEFORE the overwrite, plus XV_BACKEND directly.
    let xv_backend_env = std::env::var("XV_BACKEND").ok();
    let profile_backend = active_profile
        .as_ref()
        .and_then(|p| p.backend.as_deref().map(String::from));
    let effective_backend = config.effective_backend_name().to_string();

    let mut resolution_notes: Vec<String> = Vec::new();

    let backend_source = if config.cli_backend_was_arg && config.cli_backend.is_some() {
        // --backend was explicitly passed; it wins over everything below.
        "--backend CLI flag".to_string()
    } else if let Some(ref pb) = profile_backend {
        // Profile is next in precedence (it outranks env + global config).
        if pb == &effective_backend {
            format!(
                ".xv.toml [env.{}] backend",
                active_env_name.as_deref().unwrap_or("?")
            )
        } else {
            // Shouldn't happen — profile is set but effective differs and
            // no CLI override fired. Be honest.
            format!("<unexpected: profile={pb} effective={effective_backend}>")
        }
    } else if let Some(ref env_val) = xv_backend_env {
        if env_val == &effective_backend {
            "XV_BACKEND env var".to_string()
        } else {
            format!("<unexpected: XV_BACKEND={env_val} effective={effective_backend}>")
        }
    } else if let Some(ref dv) = config.disk_backend {
        // No CLI, no profile, no env — value came from the on-disk config.
        if dv == &effective_backend {
            "global config `backend`".to_string()
        } else {
            format!("<unexpected: disk={dv} effective={effective_backend}>")
        }
    } else {
        // No source set anything — falling back to built-in default.
        "built-in default".to_string()
    };

    if active_profile
        .as_ref()
        .is_some_and(|profile| profile.backend.is_none())
    {
        resolution_notes.push(
            "active env has no backend, so backend falls through to --backend/XV_BACKEND/global config/built-in default".to_string(),
        );
    }

    // --- Vault & resource_group resolution ---
    // Bugbot finding #4 on PR #216: if .xv.toml exists but resolve_env
    // failed (unknown XV_ENV / --env), real commands error out via
    // `resolve_vault_name`. Showing the global-config or context fallback
    // here misrepresents what `xv list` would actually use. Surface the
    // env error instead.
    let context_manager = crate::config::ContextManager::load()
        .await
        .unwrap_or_default();

    let (vault_value, vault_source) = if let Some(ref err) = env_resolve_err {
        (
            "<error>".to_string(),
            format!("env resolution failed: {err}"),
        )
    } else if let Some(v) = active_profile.as_ref().and_then(|p| p.vault.as_deref()) {
        if context_manager.current_vault().is_some() || !config.default_vault.is_empty() {
            resolution_notes.push(format!(
                "active env `{}` supplies vault `{v}`, so vault context/global default are ignored",
                active_env_name.as_deref().unwrap_or("?")
            ));
        }
        (
            v.to_string(),
            format!(
                ".xv.toml [env.{}] vault",
                active_env_name.as_deref().unwrap_or("?")
            ),
        )
    } else if let Some(v) = context_manager.current_vault() {
        if active_env_name.is_some() {
            resolution_notes.push(
                "active env has no vault, so vault falls through to the current context"
                    .to_string(),
            );
        }
        (
            v.to_string(),
            context_manager.scope_description().to_string(),
        )
    } else if !config.default_vault.is_empty() {
        if active_env_name.is_some() {
            resolution_notes.push(
                "active env has no vault and no context is set, so vault falls through to global config".to_string(),
            );
        }
        (
            config.default_vault.clone(),
            "global config `default_vault`".to_string(),
        )
    } else {
        ("<unset>".to_string(), "(none)".to_string())
    };

    // Bugbot finding #3 on PR #216: real `Config::resolve_resource_group`
    // precedence is .xv.toml → context → global config. The display
    // previously skipped the context layer. Mirror the real resolver.
    let (rg_value, rg_source) = if env_resolve_err.is_some() {
        (
            "<error>".to_string(),
            "env resolution failed (see vault row)".to_string(),
        )
    } else if let Some(rg) = active_profile
        .as_ref()
        .and_then(|p| p.resource_group.as_deref())
    {
        if context_manager.current_resource_group().is_some()
            || !config.default_resource_group.is_empty()
        {
            resolution_notes.push(format!(
                "active env `{}` supplies resource_group `{rg}`, so context/global resource_group are ignored",
                active_env_name.as_deref().unwrap_or("?")
            ));
        }
        (
            rg.to_string(),
            format!(
                ".xv.toml [env.{}] resource_group",
                active_env_name.as_deref().unwrap_or("?")
            ),
        )
    } else if let Some(rg) = context_manager.current_resource_group() {
        if active_env_name.is_some() {
            resolution_notes.push(
                "active env has no resource_group, so resource_group falls through to the current context".to_string(),
            );
        }
        (
            rg.to_string(),
            context_manager.scope_description().to_string(),
        )
    } else if !config.default_resource_group.is_empty() {
        if active_env_name.is_some() {
            resolution_notes.push(
                "active env has no resource_group and no context is set, so resource_group falls through to global config".to_string(),
            );
        }
        (
            config.default_resource_group.clone(),
            "global config `default_resource_group`".to_string(),
        )
    } else {
        ("<unset>".to_string(), "(none)".to_string())
    };

    // --- Build the resolution table ---
    let mut rows: Vec<Row> = Vec::new();

    rows.push(Row {
        setting: "backend".to_string(),
        value: effective_backend.clone(),
        source: backend_source,
    });

    rows.push(Row {
        setting: "env".to_string(),
        value: active_env_name
            .clone()
            .unwrap_or_else(|| "(none)".to_string()),
        source: match (&project_path, &active_env_name) {
            (Some(p), Some(_)) => match std::env::var("XV_ENV") {
                Ok(_) => "XV_ENV env var".to_string(),
                Err(_) => match (config.env_flag.as_deref(), &project_hit) {
                    (Some(_), _) => "--env CLI flag".to_string(),
                    (None, Some((_, cfg))) if cfg.default_env.is_some() => {
                        format!(".xv.toml default_env ({})", p.display())
                    }
                    _ => format!(".xv.toml ({})", p.display()),
                },
            },
            (Some(_), None) => "(.xv.toml present but no env resolved)".to_string(),
            (None, _) => "(no .xv.toml found)".to_string(),
        },
    });

    rows.push(Row {
        setting: "vault".to_string(),
        value: vault_value,
        source: vault_source,
    });

    rows.push(Row {
        setting: "resource_group".to_string(),
        value: rg_value,
        source: rg_source,
    });

    // Backend-specific extras: region/profile for AWS; storage_account for Azure.
    //
    // Resolution order must match `backend::aws::auth::load_sdk_config`:
    //   region:  CLI --region > config aws.region > AWS_REGION > AWS_DEFAULT_REGION
    //   profile: CLI --profile > config aws.profile > AWS_PROFILE (via SDK chain)
    // `--resolved` has no CLI flags in scope, so it reports the remaining order.
    if effective_backend == "aws" {
        let (region_val, region_src) = match config.aws.as_ref().and_then(|a| a.region.clone()) {
            Some(r) => (r, "global config `aws.region`".to_string()),
            None => match std::env::var("AWS_REGION") {
                Ok(v) if !v.is_empty() => (v, "AWS_REGION env var".to_string()),
                _ => match std::env::var("AWS_DEFAULT_REGION") {
                    Ok(v) if !v.is_empty() => (v, "AWS_DEFAULT_REGION env var".to_string()),
                    _ => ("<unset>".to_string(), "(none)".to_string()),
                },
            },
        };
        rows.push(Row {
            setting: "aws_region".to_string(),
            value: region_val,
            source: region_src,
        });
        let (prof_val, prof_src) = match config.aws.as_ref().and_then(|a| a.profile.clone()) {
            Some(p) => (p, "global config `aws.profile`".to_string()),
            None => match std::env::var("AWS_PROFILE") {
                Ok(v) if !v.is_empty() => (v, "AWS_PROFILE env var".to_string()),
                _ => ("<unset>".to_string(), "(none)".to_string()),
            },
        };
        rows.push(Row {
            setting: "aws_profile".to_string(),
            value: prof_val,
            source: prof_src,
        });
    } else if effective_backend == "azure" {
        let blob = config.get_blob_config();
        if !blob.storage_account.is_empty() {
            rows.push(Row {
                setting: "storage_account".to_string(),
                value: blob.storage_account,
                source: "global config `blob.storage_account`".to_string(),
            });
        }
        if !config.subscription_id.is_empty() {
            rows.push(Row {
                setting: "subscription_id".to_string(),
                value: config.subscription_id.clone(),
                source: "global config `subscription_id`".to_string(),
            });
        }
    }

    if config.output_json {
        let json = serde_json::to_string_pretty(&rows).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize resolved config: {e}"))
        })?;
        println!("{json}");
    } else {
        if let Some(p) = &project_path {
            println!("Project config: {}", p.display());
        } else {
            println!("Project config: (none — no .xv.toml found)");
        }
        let table = Table::new(&rows);
        println!("{}", format_table(table, config.no_color));
        println!();
        println!("Precedence (highest → lowest):");
        println!("  backend         : --backend flag > .xv.toml profile > XV_BACKEND / global config > built-in (azure)");
        println!("  env             : XV_ENV > --env flag > .xv.toml default_env");
        println!("  vault           : --vault arg > .xv.toml profile.vault > context > global default_vault");
        println!("  resource_group  : --resource-group > .xv.toml profile.resource_group > context > global default_resource_group");
        println!();
        println!("Naming convention: the global config uses a `default_` prefix (default_vault,");
        println!("default_resource_group) because those values are fallbacks; a .xv.toml env");
        println!("profile uses the bare name (vault, resource_group) because it sets a specific");
        println!(
            "value that overrides the global default. Same concept, the prefix signals the layer."
        );
        if !resolution_notes.is_empty() {
            println!();
            println!("Layer notes:");
            resolution_notes.sort();
            resolution_notes.dedup();
            for note in resolution_notes {
                println!("  - {note}");
            }
        }
    }

    Ok(())
}

async fn execute_config_set(key: &str, value: &str, mut config: Config) -> Result<()> {
    match key {
        "debug" => {
            config.debug = value.to_lowercase() == "true" || value == "1";
        }
        "subscription_id" => {
            config.subscription_id = value.to_string();
        }
        "default_vault" => {
            config.default_vault = value.to_string();
        }
        "default_resource_group" => {
            config.default_resource_group = value.to_string();
        }
        "default_location" => {
            config.default_location = value.to_string();
        }
        "tenant_id" => {
            config.tenant_id = value.to_string();
        }
        "cache_enabled" => {
            config.cache_enabled = value.to_lowercase() == "true" || value == "1";
        }
        "cache_ttl" | "cache_ttl_secs" => {
            let seconds = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for cache_ttl_secs: {value}"))
            })?;
            config.cache_ttl_secs = seconds;
        }
        "output_json" => {
            config.output_json = value.to_lowercase() == "true" || value == "1";
        }
        "no_color" => {
            config.no_color = value.to_lowercase() == "true" || value == "1";
        }
        "azure_credential_priority" => {
            use crate::config::settings::AzureCredentialType;
            use std::str::FromStr;
            config.azure_credential_priority =
                AzureCredentialType::from_str(value).map_err(CrosstacheError::config)?;
        }
        // Blob storage configuration
        "storage_account" => {
            let mut blob_config = config.get_blob_config();
            blob_config.storage_account = value.to_string();
            config.set_blob_config(blob_config);
        }
        "storage_container" => {
            let mut blob_config = config.get_blob_config();
            blob_config.container_name = value.to_string();
            config.set_blob_config(blob_config);
        }
        "storage_endpoint" => {
            let mut blob_config = config.get_blob_config();
            blob_config.endpoint = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            config.set_blob_config(blob_config);
        }
        "blob_chunk_size_mb" => {
            let chunk_size = value.parse::<usize>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for blob_chunk_size_mb: {value}"))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.chunk_size_mb = chunk_size;
            config.set_blob_config(blob_config);
        }
        "blob_max_concurrent_uploads" => {
            let max_uploads = value.parse::<usize>().map_err(|_| {
                CrosstacheError::config(format!(
                    "Invalid value for blob_max_concurrent_uploads: {value}"
                ))
            })?;
            let mut blob_config = config.get_blob_config();
            blob_config.max_concurrent_uploads = max_uploads;
            config.set_blob_config(blob_config);
        }
        "clipboard_timeout" => {
            config.clipboard_timeout = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!(
                    "Invalid value for clipboard_timeout: {value} (expected seconds as integer, 0 to disable)"
                ))
            })?;
        }
        "gen_default_charset" => {
            let charset = value
                .parse::<CharsetType>()
                .map_err(CrosstacheError::config)?;
            config.gen_default_charset = Some(charset.to_string());
        }
        _ => {
            return Err(CrosstacheError::config(format!(
                "Unknown configuration key: {key}. Available keys: debug, subscription_id, default_vault, default_resource_group, default_location, tenant_id, cache_enabled, cache_ttl_secs, output_json, no_color, azure_credential_priority, storage_account, storage_container, storage_endpoint, blob_chunk_size_mb, blob_max_concurrent_uploads, clipboard_timeout, gen_default_charset"
            )));
        }
    }

    config.save().await?;
    output::success(&format!("Configuration updated: {key} = {value}"));

    Ok(())
}

// ── Cache ────────────────────────────────────────────────────────────────────

pub(crate) async fn execute_cache_command(command: CacheCommands, config: Config) -> Result<()> {
    use crate::cache::CacheManager;

    let cache_manager = CacheManager::from_config(&config);

    match command {
        CacheCommands::Clear { vault } => {
            let vault_ref = vault.as_deref();
            cache_manager.clear(vault_ref);
            match vault_ref {
                Some(name) => output::success(&format!("Cache cleared for vault '{name}'.")),
                None => output::success("Cache cleared."),
            }
        }
        CacheCommands::Status => {
            let status = cache_manager.status();
            println!("Cache directory : {}", status.cache_dir.display());
            println!("Enabled         : {}", status.enabled);
            println!("TTL             : {}s", status.ttl_secs);
            println!("Entries         : {}", status.entry_count);
            println!(
                "Total size      : {}",
                format_cache_size(status.total_size_bytes)
            );
            if !status.entries.is_empty() {
                println!("\nEntries:");
                for entry in &status.entries {
                    let freshness = if entry.is_stale { "stale" } else { "fresh" };
                    println!(
                        "  {} — created {} — expires {} [{}]",
                        entry.key,
                        entry.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
                        entry.expires_at.format("%Y-%m-%d %H:%M:%S UTC"),
                        freshness,
                    );
                }
            }
        }
        CacheCommands::Refresh { key } => {
            execute_cache_refresh(&key, config).await?;
        }
    }
    Ok(())
}

async fn execute_cache_refresh(key: &str, config: Config) -> Result<()> {
    use crate::cache::refresh::release_lock;
    use crate::cache::{CacheKey, CacheManager};

    let cache_key: CacheKey = key.parse().map_err(CrosstacheError::invalid_argument)?;

    let cache_manager = CacheManager::from_config(&config);
    let lock_path = cache_key
        .to_path(cache_manager.cache_dir())
        .with_extension("lock");

    let result = match cache_key {
        CacheKey::SecretsList { ref vault_name } => {
            refresh_secrets_list(vault_name.clone(), config).await
        }
        CacheKey::VaultList => refresh_vault_list(config).await,
        CacheKey::FileList {
            ref vault_name,
            recursive,
        } => {
            #[cfg(feature = "file-ops")]
            {
                crate::cli::file_ops::refresh_file_list(vault_name.clone(), recursive, config).await
            }
            #[cfg(not(feature = "file-ops"))]
            {
                let _ = (vault_name, recursive);
                Ok(())
            }
        }
    };

    release_lock(&lock_path);
    result
}

async fn refresh_secrets_list(vault_name: String, config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
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
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await?;

    let cache_manager = CacheManager::from_config(&config);
    let cache_key = CacheKey::SecretsList { vault_name };
    cache_manager.set(&cache_key, &secrets);

    Ok(())
}

async fn refresh_vault_list(config: Config) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
    use std::sync::Arc;

    let auth_provider = Arc::new(
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
        .list_vaults_formatted(
            Some(&config.subscription_id),
            None,
            crate::utils::format::OutputFormat::Json,
            None,
        )
        .await?;

    let cache_manager = CacheManager::from_config(&config);
    cache_manager.set(&CacheKey::VaultList, &vaults);

    Ok(())
}

// ── Context ──────────────────────────────────────────────────────────────────

pub(crate) async fn execute_context_command(
    command: ContextCommands,
    config: Config,
) -> Result<()> {
    match command {
        ContextCommands::Show => {
            execute_context_show(&config).await?;
        }
        ContextCommands::Use {
            vault_name,
            resource_group,
            global,
            local,
        } => {
            execute_context_use(&vault_name, resource_group, global, local, &config).await?;
        }
        ContextCommands::List => {
            execute_context_list(&config).await?;
        }
        ContextCommands::Clear { global } => {
            execute_context_clear(global, &config).await?;
        }
        ContextCommands::Envs => {
            execute_context_envs(&config).await?;
        }
        ContextCommands::Init {
            env,
            vault,
            resource_group,
            backend,
            non_interactive,
            force,
        } => {
            execute_context_init(
                env,
                vault,
                resource_group,
                backend,
                non_interactive,
                force,
                &config,
            )
            .await?;
        }
    }
    Ok(())
}

async fn execute_context_show(config: &Config) -> Result<()> {
    use crate::config::ContextManager;

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if let Some(ref context) = context_manager.current {
        println!("Current Vault Context:");
        println!("  Vault: {}", context.vault_name);
        if let Some(ref rg) = context.resource_group {
            println!("  Resource Group: {rg}");
        }
        if let Some(ref sub) = context.subscription_id {
            println!("  Subscription: {sub}");
        }
        println!(
            "  Last Used: {}",
            context.last_used.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!("  Usage Count: {}", context.usage_count);

        // Show context source
        println!("  Scope: {}", context_manager.scope_description());
    } else {
        output::info("No vault context set");
        if !config.default_vault.is_empty() {
            println!("Using config default: {}", config.default_vault);
        } else {
            println!("Hint: Use 'xv context use <vault-name>' to set a context");
        }
    }

    // New: project-config (.xv.toml) section.
    let cwd = std::env::current_dir()?;
    if let Ok(Some((path, cfg))) = crate::config::project::find_project_config(&cwd).await {
        match crate::config::project::resolve_env(&cfg, config.env_flag.as_deref()) {
            Ok((name, profile)) => {
                println!();
                println!("active env: {name} (from {})", path.display());
                if let Some(v) = &profile.vault {
                    println!("  vault: {v}");
                }
                if let Some(rg) = &profile.resource_group {
                    println!("  resource_group: {rg}");
                }
                if let Some(g) = &profile.group {
                    println!("  group: {g}");
                }
                if let Some(f) = &profile.folder {
                    println!("  folder: {f}");
                }
                println!(
                    "  hint: env profiles override context/global defaults when a field is set; missing env fields fall back to the vault context, then global config."
                );
            }
            Err(e) => {
                println!();
                println!("project config: {} (error: {e})", path.display());
            }
        }
    }

    Ok(())
}

async fn execute_context_use(
    vault_name: &str,
    resource_group: Option<String>,
    global: bool,
    local: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::{ContextManager, VaultContext};

    // P0.1: If the name matches a .xv.toml env profile, reject with a targeted hint.
    let cwd = std::env::current_dir()?;
    if let Ok(Some((_path, proj_cfg))) = crate::config::project::find_project_config(&cwd).await {
        if proj_cfg.envs.contains_key(vault_name) {
            return Err(CrosstacheError::config(format!(
                "'{vault_name}' is an env profile in .xv.toml, not a vault name. \
Use `xv --env {vault_name} <command>` to activate it, or set XV_ENV={vault_name} in your shell."
            )));
        }
    }

    let mut context_manager = if local {
        // Create local context
        ContextManager::new_local()?
    } else if global {
        // Use global context
        ContextManager::new_global()?
    } else {
        // Load existing or create new (defaults to global)
        ContextManager::load()
            .await
            .unwrap_or_else(|_| ContextManager::new_global().unwrap_or_default())
    };

    // Create new context
    let new_context = VaultContext::new(
        vault_name.to_string(),
        resource_group.or_else(|| {
            if !config.default_resource_group.is_empty() {
                Some(config.default_resource_group.clone())
            } else {
                None
            }
        }),
        if !config.subscription_id.is_empty() {
            Some(config.subscription_id.clone())
        } else {
            None
        },
    );

    // Update context manager
    context_manager.set_context(new_context).await?;

    let scope = if local { "local" } else { "global" };
    output::success(&format!(
        "Switched to vault '{vault_name}' ({scope} context)"
    ));

    if let Some(ref rg) = context_manager.current_resource_group() {
        println!("   Resource Group: {rg}");
    }

    Ok(())
}

async fn execute_context_list(_config: &Config) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::format::format_table;
    use tabled::{Table, Tabled};

    let context_manager = ContextManager::load().await.unwrap_or_default();

    if context_manager.recent.is_empty() && context_manager.current.is_none() {
        output::info("No vault contexts found");
        println!("Hint: Use 'xv context use <vault-name>' to create a context");
        return Ok(());
    }

    #[derive(Tabled)]
    struct ContextItem {
        #[tabled(rename = "Status")]
        status: String,
        #[tabled(rename = "Vault")]
        vault: String,
        #[tabled(rename = "Resource Group")]
        resource_group: String,
        #[tabled(rename = "Last Used")]
        last_used: String,
        #[tabled(rename = "Usage Count")]
        usage_count: String,
    }

    let mut items = Vec::new();

    // Add current context
    if let Some(ref context) = context_manager.current {
        items.push(ContextItem {
            status: "● Current".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    // Add recent contexts
    for context in context_manager.list_recent() {
        // Skip if it's the current context
        if let Some(ref current) = context_manager.current {
            if current.vault_name == context.vault_name {
                continue;
            }
        }

        items.push(ContextItem {
            status: "  Recent".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    if !items.is_empty() {
        let table = Table::new(&items);
        println!("{}", format_table(table, false));

        println!("\nScope: {}", context_manager.scope_description());
        if ContextManager::local_context_exists() {
            println!("Note: Local context file found in current directory (.xv/context)");
        }
    }

    Ok(())
}

async fn execute_context_clear(global: bool, _config: &Config) -> Result<()> {
    use crate::config::ContextManager;

    let mut context_manager = if global {
        ContextManager::new_global()?
    } else {
        ContextManager::load().await.unwrap_or_default()
    };

    if context_manager.current.is_none() {
        output::info("No active context to clear");
        return Ok(());
    }

    let vault_name = context_manager
        .current_vault()
        .unwrap_or("unknown")
        .to_string();
    context_manager.clear_context().await?;

    let scope = if global {
        "global"
    } else {
        context_manager.scope_description()
    };
    output::success(&format!(
        "Cleared vault context for '{vault_name}' ({scope} scope)"
    ));

    Ok(())
}

async fn execute_context_envs(config: &Config) -> Result<()> {
    execute_env_list(config).await
}

async fn execute_context_init(
    env_name: String,
    vault_arg: Option<String>,
    rg_arg: Option<String>,
    backend_arg: Option<String>,
    non_interactive: bool,
    force: bool,
    config: &Config,
) -> Result<()> {
    use crate::config::project::{EnvProfile, ProjectConfig};
    use crate::error::CrosstacheError;
    use std::collections::BTreeMap;

    let cwd = std::env::current_dir()?;
    let path = cwd.join(".xv.toml");
    if path.exists() && !force {
        return Err(CrosstacheError::config(format!(
            ".xv.toml already exists at {} (use --force to overwrite)",
            path.display()
        )));
    }

    use crate::config::project::validate_env_profile_backend;

    let profile_backend = if let Some(ref b) = backend_arg {
        validate_env_profile_backend(b)?;
        Some(b.as_str())
    } else {
        None
    };

    // Effective backend for prompts/validation: explicit --backend, else the
    // already-resolved global backend (xv.conf / XV_BACKEND / top-level --backend).
    // Only write a profile backend when --backend was passed — unset inherits
    // via resolve_effective_backend at runtime.
    let effective_backend = profile_backend.unwrap_or_else(|| config.effective_backend_name());

    // Resolve vault/RG: explicit flag → interactive prompt → config default.
    // Azure requires resource_group; aws/local do not.
    let (vault, resource_group) = if non_interactive {
        let vault = vault_arg.ok_or_else(|| {
            CrosstacheError::invalid_argument("--non-interactive requires --vault")
        })?;
        let rg = if effective_backend == "azure" {
            Some(rg_arg.ok_or_else(|| {
                CrosstacheError::invalid_argument(
                    "--non-interactive requires --resource-group when --backend azure",
                )
            })?)
        } else {
            rg_arg
        };
        (vault, rg)
    } else {
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        let vault = match vault_arg {
            Some(v) => v,
            None => prompt.input_text(
                &format!("Vault for env '{env_name}'"),
                if !config.default_vault.is_empty() {
                    Some(config.default_vault.as_str())
                } else {
                    None
                },
            )?,
        };
        let rg = match rg_arg {
            Some(r) => Some(r),
            None if effective_backend == "azure" => Some(prompt.input_text(
                &format!("Resource group for env '{env_name}'"),
                if !config.default_resource_group.is_empty() {
                    Some(config.default_resource_group.as_str())
                } else {
                    None
                },
            )?),
            None => None,
        };
        (vault, rg)
    };

    let mut envs = BTreeMap::new();
    envs.insert(
        env_name.clone(),
        EnvProfile {
            vault: Some(vault),
            resource_group,
            group: None,
            folder: None,
            backend: profile_backend.map(String::from),
        },
    );

    let cfg = ProjectConfig {
        default_env: Some(env_name.clone()),
        envs,
        scan: None,
    };

    let body = toml::to_string_pretty(&cfg)
        .map_err(|e| CrosstacheError::config(format!("failed to serialize .xv.toml: {e}")))?;

    // Use the same header `ProjectConfig::save()` writes, so the comment
    // survives later `xv env use/create/delete` rewrites.
    let full = format!("{}{body}", crate::config::project::ProjectConfig::HEADER);

    tokio::fs::write(&path, full).await?;
    crate::utils::output::success(&format!(
        ".xv.toml written to {} (env: {env_name})",
        path.display()
    ));
    Ok(())
}

// ── Env Commands ─────────────────────────────────────────────────────────────

pub(crate) async fn execute_env_command(
    command: EnvCommands,
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    match command {
        EnvCommands::List => execute_env_list(&config).await,
        EnvCommands::Use { name } => execute_env_use(&name, &config).await,
        EnvCommands::Create {
            name,
            vault,
            resource_group,
            backend,
            group,
            folder,
            default,
            force,
        } => {
            execute_env_create(
                &name,
                &vault,
                &resource_group,
                backend.as_deref(),
                group.as_deref(),
                folder.as_deref(),
                default,
                force,
                &config,
            )
            .await
        }
        EnvCommands::Delete { name, force } => execute_env_delete(&name, force, &config).await,
        EnvCommands::Show => execute_env_show(&config).await,
        EnvCommands::Pull {
            format,
            group,
            output,
        } => execute_env_pull(&format, group, output, &config, registry).await,
        EnvCommands::Push { file, overwrite } => {
            execute_env_push(file, overwrite, &config, registry).await
        }
    }
}

async fn execute_env_list(config: &Config) -> Result<()> {
    use crate::config::project;

    let cwd = std::env::current_dir()?;
    let Some((path, cfg)) = project::find_project_config(&cwd).await? else {
        output::info(&format!(
            "No .xv.toml found from {}. Create one with: xv context init",
            cwd.display()
        ));
        return Ok(());
    };

    let active = project::resolve_env(&cfg, config.env_flag.as_deref())
        .ok()
        .map(|(name, _)| name.to_string());

    let default_label = cfg
        .default_env
        .as_deref()
        .map(|d| format!(", default: {d}"))
        .unwrap_or_default();
    println!("Project envs (from {}{}):", path.display(), default_label);
    use crate::config::project::resolve_effective_backend;
    // Precedence for every row mirrors `resolve_effective_backend`:
    //   cli_backend (--backend / XV_BACKEND via clap) > profile.backend > global.
    // `cli_backend` is the raw flag/env snapshot; `disk_backend` is the global
    // config value taken BEFORE main.rs folded the active env's profile in
    // (using `effective_backend_name()` here would make inactive envs inherit
    // the active env's backend). A `None` profile backend falls through to the
    // global layer rather than silently defaulting to "azure".
    let cli_backend = config.cli_backend.as_deref();
    let global_backend = config.disk_backend.as_deref();
    for (name, profile) in &cfg.envs {
        let marker = if active.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        // Resolve through the canonical precedence helper so a --backend
        // override is reflected even on rows that pin their own backend, and
        // an unset profile backend shows the inherited global value.
        let resolved =
            resolve_effective_backend(cli_backend, profile.backend.as_deref(), global_backend);
        // "(inherited)" marks rows whose env profile set no `backend` of its
        // own — the displayed value came from outside the profile (CLI flag,
        // XV_BACKEND, global config, or the built-in default). This must key
        // strictly on the profile field below: the CLI override is populated
        // from XV_BACKEND even when --backend is absent, so it is not a
        // reliable signal for "this row has no profile-level backend".
        let backend_note = if profile.backend.is_none() {
            " (inherited)"
        } else {
            ""
        };
        let vault = profile.vault.as_deref().unwrap_or("(unset)");
        let mut extras = String::new();
        if let Some(rg) = &profile.resource_group {
            extras.push_str(&format!("  resource_group={rg}"));
        }
        println!("  {marker} {name}  backend={resolved}{backend_note}  vault={vault}{extras}");
    }

    // Summary: what the active env actually resolves to right now, after full
    // precedence (this is the "effective profile" §P2-4 asks for). Only shown
    // when an env is active so single-env or no-active-env cases stay terse.
    if let Some(active_name) = &active {
        if let Some(profile) = cfg.envs.get(active_name) {
            let eff_backend =
                resolve_effective_backend(cli_backend, profile.backend.as_deref(), global_backend);
            // Vault resolution must mirror Config::resolve_vault_name and
            // `config show --resolved`: profile.vault > context vault > global
            // default_vault. Skipping the context layer made the summary
            // disagree with what commands actually use.
            let context_manager = crate::config::ContextManager::load()
                .await
                .unwrap_or_default();
            let eff_vault = profile
                .vault
                .as_deref()
                .or_else(|| context_manager.current_vault())
                .or(if config.default_vault.is_empty() {
                    None
                } else {
                    Some(config.default_vault.as_str())
                })
                .unwrap_or("(unset)");
            println!();
            println!("Effective ({active_name}): backend={eff_backend}  vault={eff_vault}");
        }
    }
    output::hint(
        "`context envs` lists .xv.toml env profiles, not the vault context; run `xv config show --resolved` to see the effective backend/vault after env → context → global fallbacks.",
    );
    Ok(())
}

async fn execute_env_use(name: &str, _config: &Config) -> Result<()> {
    use crate::config::project;

    let cwd = std::env::current_dir()?;
    let (path, mut cfg) = match project::find_project_config(&cwd).await? {
        Some(result) => result,
        None => {
            return Err(CrosstacheError::config(format!(
                "No .xv.toml found from {}. Create one with: xv context init",
                cwd.display()
            )));
        }
    };

    if !cfg.envs.contains_key(name) {
        let available: Vec<String> = cfg.envs.keys().cloned().collect();
        return Err(CrosstacheError::config(format!(
            "No env profile '{}' in {}. Available: {}",
            name,
            path.display(),
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        )));
    }

    cfg.default_env = Some(name.to_string());
    cfg.save(&path).await?;
    output::success(&format!(
        "Set default_env = {:?} in {}",
        name,
        path.display()
    ));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_env_create(
    name: &str,
    vault: &str,
    resource_group: &str,
    backend: Option<&str>,
    group: Option<&str>,
    folder: Option<&str>,
    set_default: bool,
    force: bool,
    _config: &Config,
) -> Result<()> {
    use crate::config::project::{validate_env_profile_backend, EnvProfile};

    if let Some(b) = backend {
        validate_env_profile_backend(b)?;
    }

    let cwd = std::env::current_dir()?;
    let (path, mut cfg) = crate::config::project::find_or_create_project_config(&cwd).await?;

    if cfg.envs.contains_key(name) && !force {
        return Err(CrosstacheError::config(format!(
            "env profile '{name}' already exists in {}. Use --force to overwrite.",
            path.display()
        )));
    }

    let profile = EnvProfile {
        vault: Some(vault.to_string()),
        resource_group: Some(resource_group.to_string()),
        backend: backend.map(String::from),
        group: group.map(String::from),
        folder: folder.map(String::from),
    };

    cfg.envs.insert(name.to_string(), profile);

    if set_default {
        cfg.default_env = Some(name.to_string());
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            CrosstacheError::config(format!(
                "failed to create directory {}: {e}",
                parent.display()
            ))
        })?;
    }

    cfg.save(&path).await?;
    output::success(&format!("Added [env.{}] to {}", name, path.display()));
    if set_default {
        println!("  default_env = {:?}", name);
    }
    Ok(())
}

async fn execute_env_delete(name: &str, force: bool, _config: &Config) -> Result<()> {
    use crate::config::project;

    let cwd = std::env::current_dir()?;
    let (path, mut cfg) = match project::find_project_config(&cwd).await? {
        Some(result) => result,
        None => {
            return Err(CrosstacheError::config(format!(
                "No .xv.toml found from {}. Create one with: xv context init",
                cwd.display()
            )));
        }
    };

    if !cfg.envs.contains_key(name) {
        let available: Vec<String> = cfg.envs.keys().cloned().collect();
        return Err(CrosstacheError::config(format!(
            "No env profile '{}' in {}. Available: {}",
            name,
            path.display(),
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        )));
    }

    if !force {
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        if !prompt.confirm(
            &format!("Remove [env.{name}] from {}?", path.display()),
            false,
        )? {
            println!("Delete cancelled");
            return Ok(());
        }
    }

    cfg.envs.remove(name);

    if cfg.default_env.as_deref() == Some(name) {
        cfg.default_env = None;
    }

    cfg.save(&path).await?;
    output::success(&format!("Removed [env.{}] from {}", name, path.display()));
    Ok(())
}

async fn execute_env_show(config: &Config) -> Result<()> {
    use crate::config::project;

    let cwd = std::env::current_dir()?;
    let Some((path, cfg)) = project::find_project_config(&cwd).await? else {
        output::info(&format!(
            "No .xv.toml found from {}. Create one with: xv context init",
            cwd.display()
        ));
        return Ok(());
    };

    let (name, profile) = project::resolve_env(&cfg, config.env_flag.as_deref())?;

    println!("Active env: {name} (from {})", path.display());
    if let Some(b) = &profile.backend {
        println!("  backend: {b}");
    }
    if let Some(v) = &profile.vault {
        println!("  vault: {v}");
    }
    if let Some(rg) = &profile.resource_group {
        println!("  resource_group: {rg}");
    }
    if let Some(g) = &profile.group {
        println!("  group: {g}");
    }
    if let Some(f) = &profile.folder {
        println!("  folder: {f}");
    }
    Ok(())
}

async fn execute_env_pull(
    format: &crate::utils::format::OutputFormat,
    groups: Vec<String>,
    output: Option<String>,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::utils::format::OutputFormat;

    // Route through the active backend trait so `xv env pull` works on every
    // backend (azure/local/aws), not just Azure.
    let reg = registry.ok_or_else(|| {
        CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        )
    })?;
    let secrets_backend = reg.active().secrets();

    // Determine vault name
    let vault_name = config.resolve_vault_name(None).await?;

    eprintln!("Pulling secrets from vault '{}'...", vault_name);

    // Get all secrets or filtered by group
    let mut all_secrets = Vec::new();
    if groups.is_empty() {
        // Get all secrets
        let secrets = secrets_backend
            .list_secrets(&vault_name, None)
            .await
            .map_err(CrosstacheError::from)?;
        for secret_summary in secrets {
            match secrets_backend
                .get_secret(&vault_name, &secret_summary.name, true)
                .await
            {
                Ok(secret) => all_secrets.push(secret),
                Err(e) => eprintln!(
                    "Warning: Failed to get secret '{}': {}",
                    secret_summary.name, e
                ),
            }
        }
    } else {
        // Get secrets filtered by groups
        for group in &groups {
            let secrets = secrets_backend
                .list_secrets(&vault_name, Some(group))
                .await
                .map_err(CrosstacheError::from)?;
            for secret_summary in secrets {
                match secrets_backend
                    .get_secret(&vault_name, &secret_summary.name, true)
                    .await
                {
                    Ok(secret) => all_secrets.push(secret),
                    Err(e) => eprintln!(
                        "Warning: Failed to get secret '{}': {}",
                        secret_summary.name, e
                    ),
                }
            }
        }
    }

    // Format the secrets based on the requested output format
    let content = match format.resolve_for_stdout() {
        OutputFormat::Json => {
            // Build a simple JSON array of {name, value} objects
            let entries: Vec<serde_json::Value> = all_secrets
                .iter()
                .filter_map(|s| {
                    s.value.as_ref().map(
                        |v| serde_json::json!({ "name": s.original_name, "value": v.as_str() }),
                    )
                })
                .collect();
            serde_json::to_string_pretty(&entries).map_err(|e| {
                CrosstacheError::serialization(format!("JSON serialization failed: {e}"))
            })?
        }
        OutputFormat::Yaml => {
            let entries: Vec<serde_json::Value> = all_secrets
                .iter()
                .filter_map(|s| {
                    s.value.as_ref().map(
                        |v| serde_json::json!({ "name": s.original_name, "value": v.as_str() }),
                    )
                })
                .collect();
            serde_yaml::to_string(&entries).map_err(|e| {
                CrosstacheError::serialization(format!("YAML serialization failed: {e}"))
            })?
        }
        OutputFormat::Csv => {
            let mut csv = String::from("name,value\n");
            for s in &all_secrets {
                if let Some(ref v) = s.value {
                    let escaped = v.replace('"', "\"\"");
                    csv.push_str(&format!("{},\"{}\"\n", s.original_name, escaped));
                }
            }
            csv
        }
        // Plain / Auto / Table / Template / Raw: use dotenv format
        _ => {
            let mut dotenv_content = String::new();
            for secret in &all_secrets {
                if let Some(ref value) = secret.value {
                    let key = &secret.original_name;
                    let escaped_value =
                        if value.contains('\n') || value.contains('"') || value.contains('\\') {
                            format!(
                                "\"{}\"",
                                value
                                    .replace('\\', "\\\\")
                                    .replace('"', "\\\"")
                                    .replace('\n', "\\n")
                            )
                        } else if value.contains(' ') || value.starts_with('#') {
                            format!("\"{}\"", value.as_str())
                        } else {
                            value.to_string()
                        };
                    dotenv_content.push_str(&format!("{}={}\n", key, escaped_value));
                }
            }
            dotenv_content
        }
    };

    // Output to file or stdout
    if let Some(output_path) = output {
        crate::utils::helpers::write_sensitive_file(
            std::path::Path::new(&output_path),
            content.as_bytes(),
        )?;
        output::success(&format!(
            "Successfully exported {} secret(s) to '{}' (permissions: owner-only)",
            all_secrets.len(),
            output_path
        ));
    } else {
        print!("{}", content);
    }

    Ok(())
}

async fn execute_env_push(
    file: Option<String>,
    overwrite: bool,
    config: &Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    use crate::secret::manager::SecretRequest;
    use std::collections::HashMap;
    use std::io::Read;

    // Route through the active backend trait so `xv env push` works on every
    // backend (azure/local/aws), not just Azure.
    let reg = registry.ok_or_else(|| {
        CrosstacheError::config(
            "No backend registry available. Run 'xv config show' to check your configuration.",
        )
    })?;
    let secrets_backend = reg.active().secrets();

    // Determine vault name
    let vault_name = config.resolve_vault_name(None).await?;

    // Read .env content from file or stdin
    let env_content = if let Some(file_path) = file {
        println!("Reading .env file from '{}'...", file_path);
        std::fs::read_to_string(&file_path)?
    } else {
        println!("Reading .env content from stdin...");
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    };

    // Parse .env content
    let mut secrets = HashMap::new();
    for (line_num, line) in env_content.lines().enumerate() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse KEY=VALUE format
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let value = line[eq_pos + 1..].trim();

            // Handle quoted values
            let processed_value =
                if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                    let unquoted = &value[1..value.len() - 1];
                    // Unescape quoted content
                    unquoted
                        .replace("\\\"", "\"")
                        .replace("\\n", "\n")
                        .replace("\\\\", "\\")
                } else {
                    value.to_string()
                };

            if key.is_empty() {
                eprintln!("Warning: Empty key on line {} - skipping", line_num + 1);
                continue;
            }

            secrets.insert(key.to_string(), processed_value);
        } else {
            eprintln!(
                "Warning: Invalid format on line {} - skipping: {}",
                line_num + 1,
                line
            );
        }
    }

    if secrets.is_empty() {
        println!("No valid key=value pairs found in input");
        return Ok(());
    }

    println!(
        "Pushing {} secret(s) to vault '{}'...",
        secrets.len(),
        vault_name
    );

    // Check for existing secrets if not overwriting
    if !overwrite {
        let mut existing_secrets = Vec::new();
        for key in secrets.keys() {
            if secrets_backend
                .get_secret(&vault_name, key, false)
                .await
                .is_ok()
            {
                existing_secrets.push(key);
            }
        }

        if !existing_secrets.is_empty() {
            return Err(CrosstacheError::config(format!(
                "The following secret(s) already exist: {}. Use --overwrite to replace them.",
                existing_secrets
                    .into_iter()
                    .map(|s| format!("'{}'", s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // Set each secret
    let mut success_count = 0;
    let mut error_count = 0;

    for (key, value) in secrets {
        let secret_request = SecretRequest {
            name: key.clone(),
            value: Zeroizing::new(value.clone()),
            content_type: Some("text/plain".to_string()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(HashMap::new()),
            groups: None,
            note: None,
            folder: None,
        };

        match secrets_backend
            .set_secret(&vault_name, secret_request)
            .await
        {
            Ok(_) => {
                println!(
                    "  {}",
                    output::format_line(
                        output::Level::Success,
                        &format!("Set '{}'", key),
                        output::should_use_rich_stdout()
                    )
                );
                success_count += 1;
            }
            Err(e) => {
                output::error(&format!("  Failed to set '{}': {}", key, e));
                error_count += 1;
            }
        }
    }

    if error_count > 0 {
        // Surface partial failures as a non-zero exit so CI/scripts don't treat
        // a half-finished push as success.
        return Err(CrosstacheError::unknown(format!(
            "env push: {error_count} of {} secret(s) failed to set in vault '{vault_name}'",
            success_count + error_count
        )));
    }
    output::success(&format!(
        "Successfully pushed {} secret(s) to vault '{}'",
        success_count, vault_name
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_editor_from;

    #[cfg(not(windows))]
    const DEFAULT_EDITOR: &str = "nano";
    #[cfg(windows)]
    const DEFAULT_EDITOR: &str = "notepad";

    #[test]
    fn visual_takes_precedence_over_editor() {
        let got = resolve_editor_from(Some("vim".into()), Some("emacs".into()));
        assert_eq!(got, "vim");
    }

    #[test]
    fn falls_back_to_editor_when_visual_unset() {
        let got = resolve_editor_from(None, Some("emacs".into()));
        assert_eq!(got, "emacs");
    }

    #[test]
    fn empty_or_whitespace_values_are_ignored() {
        // Empty VISUAL and whitespace-only EDITOR -> platform default.
        let got = resolve_editor_from(Some(String::new()), Some("   ".into()));
        assert_eq!(got, DEFAULT_EDITOR);
    }

    #[test]
    fn empty_visual_falls_through_to_editor() {
        let got = resolve_editor_from(Some("  ".into()), Some("micro".into()));
        assert_eq!(got, "micro");
    }

    #[test]
    fn platform_default_when_both_unset() {
        let got = resolve_editor_from(None, None);
        assert_eq!(got, DEFAULT_EDITOR);
    }

    #[test]
    fn editor_with_arguments_is_preserved_verbatim() {
        // The command-with-args string is returned intact; argument splitting
        // happens at the call site in execute_config_edit.
        let got = resolve_editor_from(Some("code --wait".into()), None);
        assert_eq!(got, "code --wait");
    }
}
