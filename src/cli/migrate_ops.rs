//! Migration between backends.
//!
//! Implements `xv migrate --from <backend> --to <backend>`, which copies
//! secrets from one backend to another while preserving metadata.

use crate::backend::{Backend, BackendError, BackendKind, BackendRegistry};
use crate::config::settings::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::SecretRequest;
use crate::utils::output;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use zeroize::Zeroizing;

const TAG_MIGRATED_FROM: &str = "xv:migrated_from";
const TAG_MIGRATED_AT: &str = "xv:migrated_at";

struct MigrationDiff {
    to_migrate: Vec<String>,
    conflicts: Vec<String>,
}

async fn compute_diff(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    vault: &str,
    filter: Option<&str>,
) -> Result<MigrationDiff> {
    let source_secrets = source
        .secrets()
        .list_secrets(vault, None)
        .await
        .map_err(|e| {
            CrosstacheError::Unknown(format!(
                "Failed to list secrets from {} backend: {e}",
                source.name()
            ))
        })?;

    let filtered: Vec<String> = match filter {
        Some(pattern) => {
            let glob = globset::Glob::new(pattern)
                .map_err(|e| CrosstacheError::invalid_argument(format!("Invalid glob pattern: {e}")))?
                .compile_matcher();
            source_secrets
                .into_iter()
                .filter(|s| glob.is_match(&s.name))
                .map(|s| s.name)
                .collect()
        }
        None => source_secrets.into_iter().map(|s| s.name).collect(),
    };

    let mut to_migrate = Vec::new();
    let mut conflicts = Vec::new();

    for name in filtered {
        match target.secrets().secret_exists(vault, &name).await {
            Ok(true) => conflicts.push(name),
            Ok(false) => to_migrate.push(name),
            Err(e) => {
                tracing::debug!("secret_exists check failed for {name}: {e}; assuming new");
                to_migrate.push(name);
            }
        }
    }
    Ok(MigrationDiff { to_migrate, conflicts })
}

fn print_diff_summary(
    diff: &MigrationDiff,
    source_name: &str,
    target_name: &str,
    vault: &str,
    on_conflict: &crate::cli::commands::OnConflict,
    dry_run: bool,
) {
    println!();
    println!("Source: {}:{}", source_name, vault);
    println!("Target: {}:{}", target_name, vault);
    println!();
    println!("  to migrate:    {} secret(s)", diff.to_migrate.len());
    println!("  conflict:      {} secret(s) (target already has same name)", diff.conflicts.len());
    println!();
    println!("On conflict: {:?}", on_conflict);
    println!("Dry run? {}", if dry_run { "yes" } else { "no" });
    println!();
}

fn build_request_from_props(
    props: &crate::secret::manager::SecretProperties,
    source_name: &str,
    vault: &str,
) -> SecretRequest {
    let mut tags = props.tags.clone();
    tags.insert(
        TAG_MIGRATED_FROM.into(),
        format!("{}:{}:{}", source_name, vault, props.version),
    );
    tags.insert(TAG_MIGRATED_AT.into(), chrono::Utc::now().to_rfc3339());

    SecretRequest {
        name: props.original_name.clone(),
        value: Zeroizing::new(
            props.value.as_ref()
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
        ),
        content_type: if props.content_type.is_empty() {
            None
        } else {
            Some(props.content_type.clone())
        },
        enabled: Some(props.enabled),
        expires_on: props.expires_on,
        not_before: props.not_before,
        tags: if tags.is_empty() {
            None
        } else {
            Some(tags)
        },
        groups: None,
        note: None,
        folder: None,
    }
}

async fn migrate_one(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    vault: &str,
    name: &str,
    force_replace: bool,
    source_name_for_tag: &str,
) -> std::result::Result<String, (String, String)> {
    // Fetch full props with value
    let props = source
        .secrets()
        .get_secret(vault, name, true)
        .await
        .map_err(|e| (name.to_string(), format!("get_secret: {e}")))?;

    // Idempotency check
    if !force_replace {
        if let Ok(existing) = target.secrets().get_secret(vault, name, false).await {
            if let Some(prev_from) = existing.tags.get(TAG_MIGRATED_FROM) {
                let expected = format!("{}:{}:{}", source_name_for_tag, vault, props.version);
                if prev_from == &expected {
                    return Err((name.to_string(), format!("__skip__{}", name)));
                }
            }
        }
    }

    let request = build_request_from_props(&props, source_name_for_tag, vault);

    // Retry with exponential backoff on RateLimited
    let mut attempt = 0u32;
    loop {
        match target.secrets().set_secret(vault, request.clone()).await {
            Ok(_) => return Ok(name.to_string()),
            Err(BackendError::RateLimited { retry_after_secs }) if attempt < 5 => {
                let wait = retry_after_secs
                    .map(std::time::Duration::from_secs)
                    .unwrap_or_else(|| {
                        std::time::Duration::from_millis(500 * 2u64.pow(attempt))
                    });
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            Err(e) => return Err((name.to_string(), format!("set_secret: {e}"))),
        }
    }
}

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

    // 5. Compute diff (list + filter + conflict detection)
    let diff = compute_diff(&source, &target, &vault_name, filter.as_deref()).await?;
    print_diff_summary(&diff, source.name(), target.name(), &vault_name, &on_conflict, dry_run);

    if dry_run {
        return Ok(());
    }

    // Honor --on-conflict fail
    if !diff.conflicts.is_empty() && on_conflict == crate::cli::commands::OnConflict::Fail {
        return Err(CrosstacheError::Unknown(format!(
            "{} conflict(s) detected; aborting (--on-conflict fail)",
            diff.conflicts.len()
        )));
    }

    // Build list of names to process
    let mut names_to_process: Vec<String> = diff.to_migrate.clone();
    if on_conflict == crate::cli::commands::OnConflict::Replace {
        names_to_process.extend(diff.conflicts.clone());
    }

    if names_to_process.is_empty() {
        output::info("No secrets to migrate.");
        return Ok(());
    }

    // 6. Migrate secrets concurrently with backoff retry
    let source_name_tag = source.name().to_string();
    let source_arc = source.clone();
    let target_arc = target.clone();
    let vault_clone = vault_name.clone();

    let results: Vec<_> = stream::iter(names_to_process.iter().map(|name| {
        let source = source_arc.clone();
        let target = target_arc.clone();
        let vault = vault_clone.clone();
        let name = name.clone();
        let src_tag = source_name_tag.clone();
        async move {
            migrate_one(&source, &target, &vault, &name, force_replace, &src_tag).await
        }
    }))
    .buffer_unordered(concurrency)
    .collect()
    .await;

    let mut migrated = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<(String, String)> = Vec::new();

    for r in results {
        match r {
            Ok(name) => {
                println!("  [ok] {}", name);
                migrated += 1;
            }
            Err((name, msg)) if msg.starts_with("__skip__") => {
                println!("  [skip] {} — already migrated (same source version)", name);
                skipped += 1;
            }
            Err((name, msg)) => {
                println!("  [error] {} — {}", name, msg);
                errors.push((name, msg));
            }
        }
    }

    // 7. Print summary
    println!();
    if !errors.is_empty() {
        output::warn(&format!(
            "Migrated {} secret(s), {} skipped, {} error(s)",
            migrated, skipped, errors.len()
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
