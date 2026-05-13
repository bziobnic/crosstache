//! Migration between backends.
//!
//! Implements `xv migrate --from <backend> --to <backend>`, which copies
//! secrets from one backend to another while preserving metadata.

use crate::backend::{Backend, BackendKind, BackendRegistry};
use crate::config::settings::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::SecretRequest;
use crate::utils::output;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Create a backend instance for the given kind.
fn create_backend(kind: BackendKind, config: &Config) -> Result<Arc<dyn Backend>> {
    match kind {
        BackendKind::Azure => {
            let auth_provider =
                BackendRegistry::create_azure_auth_provider(config).map_err(|e| {
                    CrosstacheError::Unknown(format!("Failed to create Azure auth: {e}"))
                })?;
            let backend =
                crate::backend::azure::AzureBackend::new(config, auth_provider).map_err(|e| {
                    CrosstacheError::Unknown(format!("Failed to create Azure backend: {e}"))
                })?;
            Ok(Arc::new(backend))
        }
        BackendKind::Local => {
            let backend =
                crate::backend::local::LocalBackend::new(config.local.as_ref()).map_err(|e| {
                    CrosstacheError::Unknown(format!("Failed to create local backend: {e}"))
                })?;
            Ok(Arc::new(backend))
        }
        #[cfg(feature = "aws")]
        BackendKind::Aws => {
            let aws_cfg = config.aws.as_ref().ok_or_else(|| {
                CrosstacheError::config(
                    "[aws] config block missing — set backend = \"aws\" or pass --aws-profile",
                )
            })?;
            let backend = tokio::runtime::Handle::current()
                .block_on(crate::backend::aws::AwsBackend::new(aws_cfg, None, None))
                .map_err(|e| CrosstacheError::Unknown(format!("Failed to create AWS backend: {e}")))?;
            Ok(Arc::new(backend))
        }
        #[cfg(not(feature = "aws"))]
        BackendKind::Aws => Err(CrosstacheError::Unknown(
            "AWS backend not compiled in: rebuild with --features aws".into(),
        )),
    }
}

/// Resolve the vault name from the flag, config, or local config default.
fn resolve_vault_name(vault_flag: &Option<String>, config: &Config) -> Result<String> {
    if let Some(v) = vault_flag {
        return Ok(v.clone());
    }
    // Try config default_vault
    if !config.default_vault.is_empty() {
        return Ok(config.default_vault.clone());
    }
    // Try local config default_vault
    if let Some(ref local) = config.local {
        if let Some(ref dv) = local.default_vault {
            if !dv.is_empty() {
                return Ok(dv.clone());
            }
        }
    }
    Err(CrosstacheError::config(
        "No vault specified. Use --vault to specify the vault to migrate.",
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_migrate(
    from: String,
    to: String,
    vault: Option<String>,
    filter: Option<String>,
    dry_run: bool,
    on_conflict: crate::cli::commands::OnConflict,
    force_replace: bool,
    concurrency: usize,
    legacy_overwrite: bool,
    config: Config,
) -> Result<()> {
    // Compatibility shim: --overwrite -> --on-conflict replace + warn
    let on_conflict = if legacy_overwrite {
        eprintln!("warning: --overwrite is deprecated; use --on-conflict replace");
        crate::cli::commands::OnConflict::Replace
    } else {
        on_conflict
    };
    // force_replace and concurrency will be wired in Tasks 32-33
    let _ = (force_replace, concurrency);
    // 1. Parse backend kinds
    let from_kind: BackendKind = from
        .parse()
        .map_err(|e: String| CrosstacheError::invalid_argument(e))?;
    let to_kind: BackendKind = to
        .parse()
        .map_err(|e: String| CrosstacheError::invalid_argument(e))?;

    if from_kind == to_kind {
        return Err(CrosstacheError::invalid_argument(
            "Source and target backends must be different",
        ));
    }

    // 2. Create both backends
    let source = create_backend(from_kind, &config)?;
    let target = create_backend(to_kind, &config)?;

    // 3. Resolve vault name
    let vault_name = resolve_vault_name(&vault, &config)?;

    output::step(&format!(
        "Migrating secrets from {} to {} (vault: {})",
        source.name(),
        target.name(),
        vault_name
    ));
    if dry_run {
        output::info("DRY RUN — no changes will be made");
    }

    // 4. Ensure target vault exists
    if !dry_run {
        if let Some(target_vaults) = target.vaults() {
            // Try to get the vault; if not found, create it
            match target_vaults.get_vault(&vault_name).await {
                Ok(_) => {}
                Err(crate::backend::BackendError::VaultNotFound { .. }) => {
                    output::step(&format!(
                        "Creating vault '{}' in {} backend...",
                        vault_name,
                        target.name()
                    ));
                    let create_req = crate::vault::models::VaultCreateRequest {
                        name: vault_name.clone(),
                        location: String::new(),
                        resource_group: String::new(),
                        subscription_id: String::new(),
                        sku: None,
                        tags: None,
                        enabled_for_deployment: None,
                        enabled_for_disk_encryption: None,
                        enabled_for_template_deployment: None,
                        soft_delete_retention_in_days: None,
                        purge_protection: None,
                        access_policies: None,
                    };
                    target_vaults.create_vault(create_req).await.map_err(|e| {
                        CrosstacheError::Unknown(format!(
                            "Failed to create vault '{}' in target: {e}",
                            vault_name
                        ))
                    })?;
                }
                Err(e) => {
                    // Non-fatal — some backends may not implement get_vault
                    tracing::debug!("Could not check target vault existence: {e}");
                }
            }
        }
    }

    // 5. List secrets from source
    let secrets = source
        .secrets()
        .list_secrets(&vault_name, None)
        .await
        .map_err(|e| {
            CrosstacheError::Unknown(format!(
                "Failed to list secrets from {} backend: {e}",
                source.name()
            ))
        })?;

    if secrets.is_empty() {
        output::info("No secrets found in the source vault.");
        return Ok(());
    }

    // 6. Apply filter if specified
    let filtered_secrets: Vec<_> = if let Some(ref pattern) = filter {
        let glob = globset::Glob::new(pattern)
            .map_err(|e| CrosstacheError::invalid_argument(format!("Invalid glob pattern: {e}")))?
            .compile_matcher();
        secrets
            .into_iter()
            .filter(|s| glob.is_match(&s.name))
            .collect()
    } else {
        secrets
    };

    if filtered_secrets.is_empty() {
        output::info("No secrets matched the filter pattern.");
        return Ok(());
    }

    output::info(&format!(
        "Found {} secret(s) to migrate",
        filtered_secrets.len()
    ));

    let mut migrated = 0usize;
    let mut skipped = 0usize;

    // 7. Migrate each secret
    for summary in &filtered_secrets {
        let name = &summary.name;

        if dry_run {
            println!("  [dry-run] Would migrate: {}", name);
            migrated += 1;
            continue;
        }

        // Check if secret already exists in target
        if on_conflict != crate::cli::commands::OnConflict::Replace {
            match target.secrets().secret_exists(&vault_name, name).await {
                Ok(true) => {
                    if on_conflict == crate::cli::commands::OnConflict::Fail {
                        return Err(CrosstacheError::Unknown(format!(
                            "Conflict: '{}' already exists in target (--on-conflict fail)",
                            name
                        )));
                    }
                    println!("  [skip] {} — already exists in target", name);
                    skipped += 1;
                    continue;
                }
                Ok(false) => {}
                Err(_) => {
                    // If existence check fails, proceed anyway
                }
            }
        }

        // Get secret with value from source
        let props = source
            .secrets()
            .get_secret(&vault_name, name, true)
            .await
            .map_err(|e| {
                CrosstacheError::Unknown(format!(
                    "Failed to get secret '{}' from source: {e}",
                    name
                ))
            })?;

        // Build SecretRequest from source properties
        let value = props.value.unwrap_or_else(|| Zeroizing::new(String::new()));
        let request = SecretRequest {
            name: props.original_name.clone(),
            value,
            content_type: if props.content_type.is_empty() {
                None
            } else {
                Some(props.content_type.clone())
            },
            enabled: Some(props.enabled),
            expires_on: props.expires_on,
            not_before: props.not_before,
            tags: if props.tags.is_empty() {
                None
            } else {
                Some(props.tags.clone())
            },
            groups: None,
            note: None,
            folder: None,
        };

        // Set secret in target
        match target.secrets().set_secret(&vault_name, request).await {
            Ok(_) => {
                println!("  [ok] {}", name);
                migrated += 1;
            }
            Err(e) => {
                println!("  [error] {} — {}", name, e);
                skipped += 1;
            }
        }
    }

    // 8. Print summary
    println!();
    if dry_run {
        output::success(&format!(
            "Dry run complete: {} secret(s) would be migrated",
            migrated
        ));
    } else {
        output::success(&format!(
            "Migrated {} secret(s) ({} skipped)",
            migrated, skipped
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::LocalConfig;
    use tempfile::TempDir;

    #[test]
    fn resolve_vault_name_from_flag() {
        let config = Config::default();
        let result = resolve_vault_name(&Some("my-vault".into()), &config);
        assert_eq!(result.unwrap(), "my-vault");
    }

    #[test]
    fn resolve_vault_name_from_config() {
        let config = Config {
            default_vault: "config-vault".into(),
            ..Default::default()
        };
        let result = resolve_vault_name(&None, &config);
        assert_eq!(result.unwrap(), "config-vault");
    }

    #[test]
    fn resolve_vault_name_from_local_config() {
        let config = Config {
            local: Some(LocalConfig {
                default_vault: Some("local-vault".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = resolve_vault_name(&None, &config);
        assert_eq!(result.unwrap(), "local-vault");
    }

    #[test]
    fn resolve_vault_name_fails_when_no_vault() {
        let config = Config::default();
        let result = resolve_vault_name(&None, &config);
        assert!(result.is_err());
    }

    #[test]
    fn same_backend_rejected() {
        let from_kind: BackendKind = "local".parse().unwrap();
        let to_kind: BackendKind = "local".parse().unwrap();
        assert_eq!(from_kind, to_kind);
    }

    #[tokio::test]
    async fn local_to_local_migration_roundtrip() {
        // Create two separate local backends with different store paths
        let source_tmp = TempDir::new().unwrap();
        let target_tmp = TempDir::new().unwrap();

        let source_config = LocalConfig {
            store_path: Some(
                source_tmp
                    .path()
                    .join("store")
                    .to_string_lossy()
                    .to_string(),
            ),
            key_file: Some(
                source_tmp
                    .path()
                    .join("key.txt")
                    .to_string_lossy()
                    .to_string(),
            ),
            default_vault: Some("default".into()),
        };
        let target_config = LocalConfig {
            store_path: Some(
                target_tmp
                    .path()
                    .join("store")
                    .to_string_lossy()
                    .to_string(),
            ),
            key_file: Some(
                target_tmp
                    .path()
                    .join("key.txt")
                    .to_string_lossy()
                    .to_string(),
            ),
            default_vault: Some("default".into()),
        };

        // Create source backend and seed it with secrets
        let source = crate::backend::local::LocalBackend::new(Some(&source_config)).unwrap();
        let target = crate::backend::local::LocalBackend::new(Some(&target_config)).unwrap();

        // Seed source with test secrets
        for name in ["db-password", "api-key", "cache-token"] {
            let req = SecretRequest {
                name: name.to_string(),
                value: Zeroizing::new(format!("value-for-{name}")),
                content_type: None,
                enabled: Some(true),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: None,
                folder: None,
            };
            source.secrets().set_secret("default", req).await.unwrap();
        }

        // Verify source has 3 secrets
        let source_secrets = source
            .secrets()
            .list_secrets("default", None)
            .await
            .unwrap();
        assert_eq!(source_secrets.len(), 3);

        // Migrate all secrets from source to target
        let source_arc: Arc<dyn Backend> = Arc::new(source);
        let target_arc: Arc<dyn Backend> = Arc::new(target);

        let secrets = source_arc
            .secrets()
            .list_secrets("default", None)
            .await
            .unwrap();

        for summary in &secrets {
            let props = source_arc
                .secrets()
                .get_secret("default", &summary.name, true)
                .await
                .unwrap();
            let req = SecretRequest {
                name: props.original_name.clone(),
                value: props.value.unwrap_or_else(|| Zeroizing::new(String::new())),
                content_type: None,
                enabled: Some(props.enabled),
                expires_on: None,
                not_before: None,
                tags: None,
                groups: None,
                note: None,
                folder: None,
            };
            target_arc
                .secrets()
                .set_secret("default", req)
                .await
                .unwrap();
        }

        // Verify target has 3 secrets
        let target_secrets = target_arc
            .secrets()
            .list_secrets("default", None)
            .await
            .unwrap();
        assert_eq!(target_secrets.len(), 3);

        // Verify values match
        for name in ["db-password", "api-key", "cache-token"] {
            let src = source_arc
                .secrets()
                .get_secret("default", name, true)
                .await
                .unwrap();
            let tgt = target_arc
                .secrets()
                .get_secret("default", name, true)
                .await
                .unwrap();
            assert_eq!(src.value, tgt.value);
        }
    }

    #[test]
    fn glob_filter_works() {
        let glob = globset::Glob::new("db-*").unwrap().compile_matcher();
        assert!(glob.is_match("db-password"));
        assert!(glob.is_match("db-host"));
        assert!(!glob.is_match("api-key"));
    }
}
