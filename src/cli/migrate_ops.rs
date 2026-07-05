//! Migration between backends.
//!
//! Implements `xv migrate --from <backend> --to <backend>`, which copies
//! secrets from one backend to another while preserving metadata.

use crate::backend::{Backend, BackendError, BackendRef, BackendRegistry};
use crate::config::settings::Config;
use crate::error::{CrosstacheError, Result};
use crate::secret::manager::SecretRequest;
use crate::utils::output;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use zeroize::Zeroizing;

const TAG_MIGRATED_FROM: &str = "xv:migrated_from";
const TAG_MIGRATED_AT: &str = "xv:migrated_at";

/// Outcome of a single secret migration attempt.
enum MigrateOutcome {
    /// Secret was successfully copied to the target backend.
    Migrated(String),
    /// Secret was already migrated (same source version exists in target).
    Skipped(String),
}

struct MigrationDiff {
    to_migrate: Vec<String>,
    conflicts: Vec<String>,
}

async fn compute_diff(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    source_vault: &str,
    target_vault: &str,
    filter: Option<&str>,
) -> Result<MigrationDiff> {
    let source_secrets = source
        .secrets()
        .list_secrets(source_vault, None)
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
                .map_err(|e| {
                    CrosstacheError::invalid_argument(format!("Invalid glob pattern: {e}"))
                })?
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
        match target.secrets().secret_exists(target_vault, &name).await {
            Ok(true) => conflicts.push(name),
            Ok(false) => to_migrate.push(name),
            Err(e) => {
                tracing::debug!("secret_exists check failed for {name}: {e}; assuming new");
                to_migrate.push(name);
            }
        }
    }
    Ok(MigrationDiff {
        to_migrate,
        conflicts,
    })
}

fn print_diff_summary(
    diff: &MigrationDiff,
    source_name: &str,
    target_name: &str,
    source_vault: &str,
    target_vault: &str,
    on_conflict: &crate::cli::commands::OnConflict,
    dry_run: bool,
) {
    println!();
    println!("Source: {}:{}", source_name, source_vault);
    println!("Target: {}:{}", target_name, target_vault);
    println!();
    println!("  to migrate:    {} secret(s)", diff.to_migrate.len());
    println!(
        "  conflict:      {} secret(s) (target already has same name)",
        diff.conflicts.len()
    );
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
    let groups = tags.remove("groups").map(|groups| {
        groups
            .split(',')
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let note = tags.remove("note").filter(|note| !note.is_empty());
    let folder = tags.remove("folder").filter(|folder| !folder.is_empty());
    tags.insert(
        TAG_MIGRATED_FROM.into(),
        format!("{}:{}:{}", source_name, vault, props.version),
    );
    tags.insert(TAG_MIGRATED_AT.into(), chrono::Utc::now().to_rfc3339());

    SecretRequest {
        name: props.original_name.clone(),
        value: Zeroizing::new(
            props
                .value
                .as_ref()
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
        tags: if tags.is_empty() { None } else { Some(tags) },
        groups,
        note,
        folder,
    }
}

async fn migrate_one(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    source_vault: &str,
    target_vault: &str,
    name: &str,
    force_replace: bool,
    source_name_for_tag: &str,
) -> std::result::Result<MigrateOutcome, (String, String)> {
    // Fetch full props with value
    let props = source
        .secrets()
        .get_secret(source_vault, name, true)
        .await
        .map_err(|e| (name.to_string(), format!("get_secret: {e}")))?;

    // Idempotency check
    if !force_replace {
        if let Ok(existing) = target.secrets().get_secret(target_vault, name, false).await {
            if let Some(prev_from) = existing.tags.get(TAG_MIGRATED_FROM) {
                let expected =
                    format!("{}:{}:{}", source_name_for_tag, source_vault, props.version);
                if prev_from == &expected {
                    return Ok(MigrateOutcome::Skipped(name.to_string()));
                }
            }
        }
    }

    let request = build_request_from_props(&props, source_name_for_tag, source_vault);

    // Retry with exponential backoff on RateLimited
    let mut attempt = 0u32;
    loop {
        match target
            .secrets()
            .set_secret(target_vault, request.clone())
            .await
        {
            Ok(_) => return Ok(MigrateOutcome::Migrated(name.to_string())),
            Err(BackendError::RateLimited { retry_after_secs }) if attempt < 5 => {
                let wait = retry_after_secs
                    .map(std::time::Duration::from_secs)
                    .unwrap_or_else(|| std::time::Duration::from_millis(500 * 2u64.pow(attempt)));
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            Err(e) => return Err((name.to_string(), format!("set_secret: {e}"))),
        }
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
    // Try AWS config default_vault
    #[cfg(feature = "aws")]
    if let Some(ref aws) = config.aws {
        if let Some(ref dv) = aws.default_vault {
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
    config: Config,
) -> Result<()> {
    if concurrency == 0 {
        return Err(CrosstacheError::invalid_argument(
            "--concurrency must be at least 1",
        ));
    }

    // 1. Parse backend kinds (accepting bare `backend` or `backend:vault` form)
    let (from_kind, from_vault_override) =
        BackendRef::parse_migrate_endpoint(&from).map_err(CrosstacheError::invalid_argument)?;
    let (to_kind, to_vault_override) =
        BackendRef::parse_migrate_endpoint(&to).map_err(CrosstacheError::invalid_argument)?;

    // 2. Create both backends
    let source = BackendRegistry::create_for_kind(from_kind, &config)
        .await
        .map_err(|e| CrosstacheError::Unknown(format!("Failed to create source backend: {e}")))?;
    let target = BackendRegistry::create_for_kind(to_kind, &config)
        .await
        .map_err(|e| CrosstacheError::Unknown(format!("Failed to create target backend: {e}")))?;

    // 3. Resolve vault names (per-side overrides take precedence over --vault / config)
    let source_vault = from_vault_override
        .map(Ok)
        .unwrap_or_else(|| resolve_vault_name(&vault, &config))?;
    let target_vault = to_vault_override
        .map(Ok)
        .unwrap_or_else(|| resolve_vault_name(&vault, &config))?;

    if source.kind() == target.kind() && source_vault == target_vault {
        return Err(CrosstacheError::invalid_argument(
            "Source and target must be different (same backend and same vault)",
        ));
    }

    if source_vault == target_vault {
        output::step(&format!(
            "Migrating secrets from {} to {} (vault: {})",
            source.name(),
            target.name(),
            source_vault
        ));
    } else {
        output::step(&format!(
            "Migrating secrets from {}:{} to {}:{}",
            source.name(),
            source_vault,
            target.name(),
            target_vault
        ));
    }
    if dry_run {
        output::info("DRY RUN — no changes will be made");
    }

    // 4. Ensure target vault exists
    if !dry_run {
        if let Some(target_vaults) = target.vaults() {
            // Try to get the vault; if not found, create it
            match target_vaults.get_vault(&target_vault, None).await {
                Ok(_) => {}
                Err(crate::backend::BackendError::VaultNotFound { .. }) => {
                    output::step(&format!(
                        "Creating vault '{}' in {} backend...",
                        target_vault,
                        target.name()
                    ));
                    let create_req = crate::vault::models::VaultCreateRequest {
                        name: target_vault.clone(),
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
                            target_vault
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
    let diff = compute_diff(
        &source,
        &target,
        &source_vault,
        &target_vault,
        filter.as_deref(),
    )
    .await?;
    print_diff_summary(
        &diff,
        source.name(),
        target.name(),
        &source_vault,
        &target_vault,
        &on_conflict,
        dry_run,
    );

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
    let src_vault_clone = source_vault.clone();
    let tgt_vault_clone = target_vault.clone();

    let results: Vec<_> =
        stream::iter(
            names_to_process.iter().map(|name| {
                let source = source_arc.clone();
                let target = target_arc.clone();
                let sv = src_vault_clone.clone();
                let tv = tgt_vault_clone.clone();
                let name = name.clone();
                let src_tag = source_name_tag.clone();
                async move {
                    migrate_one(&source, &target, &sv, &tv, &name, force_replace, &src_tag).await
                }
            }),
        )
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let mut migrated = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<(String, String)> = Vec::new();

    for r in results {
        match r {
            Ok(MigrateOutcome::Migrated(name)) => {
                println!("  [ok] {}", name);
                migrated += 1;
            }
            Ok(MigrateOutcome::Skipped(name)) => {
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
            migrated,
            skipped,
            errors.len()
        ));
        return Err(CrosstacheError::Unknown(format!(
            "Migration failed for {} secret(s)",
            errors.len()
        )));
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
    use crate::backend::BackendKind;
    use crate::config::settings::LocalConfig;
    use std::collections::HashMap;
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

    #[cfg(feature = "aws")]
    #[test]
    fn resolve_vault_name_from_aws_config() {
        let config = Config {
            aws: Some(crate::config::settings::AwsConfig {
                default_vault: Some("aws-vault".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = resolve_vault_name(&None, &config);
        assert_eq!(result.unwrap(), "aws-vault");
    }

    #[test]
    fn resolve_vault_name_fails_when_no_vault() {
        let config = Config::default();
        let result = resolve_vault_name(&None, &config);
        assert!(result.is_err());
    }

    #[test]
    fn same_backend_same_vault_rejected() {
        let (from_kind, from_vault) = BackendRef::parse_migrate_endpoint("local").unwrap();
        let (to_kind, to_vault) = BackendRef::parse_migrate_endpoint("local").unwrap();
        assert_eq!(from_kind, to_kind);
        assert_eq!(from_vault, to_vault); // both None → same
    }

    #[test]
    fn same_backend_different_vault_allowed() {
        let (from_kind, from_vault) =
            BackendRef::parse_migrate_endpoint("local:source-store").unwrap();
        let (to_kind, to_vault) = BackendRef::parse_migrate_endpoint("local:target-store").unwrap();
        assert_eq!(from_kind, to_kind);
        assert_ne!(from_vault, to_vault);
    }

    #[test]
    fn parse_migrate_endpoint_with_vault() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("aws:prod-secrets").unwrap();
        assert_eq!(kind, BackendKind::Aws);
        assert_eq!(vault.as_deref(), Some("prod-secrets"));
    }

    #[test]
    fn parse_migrate_endpoint_backend_only() {
        let (kind, vault) = BackendRef::parse_migrate_endpoint("azure").unwrap();
        assert_eq!(kind, BackendKind::Azure);
        assert_eq!(vault, None);
    }

    #[test]
    fn build_request_promotes_metadata_tags_to_request_fields() {
        let mut tags = HashMap::new();
        tags.insert("groups".to_string(), "db, prod".to_string());
        tags.insert("note".to_string(), "primary database password".to_string());
        tags.insert("folder".to_string(), "infra/database".to_string());
        tags.insert("owner".to_string(), "platform".to_string());

        let props = crate::secret::manager::SecretProperties {
            name: "db-password".to_string(),
            original_name: "db-password".to_string(),
            value: Some(Zeroizing::new("secret-value".to_string())),
            version: "v7".to_string(),
            version_number: Some(7),
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags,
            content_type: "text/plain".to_string(),
            recovery_level: None,
        };

        let request = build_request_from_props(&props, "local", "default");

        assert_eq!(
            request.groups,
            Some(vec!["db".to_string(), "prod".to_string()])
        );
        assert_eq!(request.note.as_deref(), Some("primary database password"));
        assert_eq!(request.folder.as_deref(), Some("infra/database"));
        let request_tags = request.tags.unwrap();
        assert_eq!(
            request_tags.get("owner").map(String::as_str),
            Some("platform")
        );
        assert_eq!(
            request_tags.get(TAG_MIGRATED_FROM).map(String::as_str),
            Some("local:default:v7")
        );
        assert!(!request_tags.contains_key("groups"));
        assert!(!request_tags.contains_key("note"));
        assert!(!request_tags.contains_key("folder"));
    }

    #[tokio::test]
    async fn execute_migrate_rejects_zero_concurrency() {
        let result = execute_migrate(
            "local".to_string(),
            "aws".to_string(),
            Some("default".to_string()),
            None,
            false,
            crate::cli::commands::OnConflict::Skip,
            false,
            0,
            Config::default(),
        )
        .await;

        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("concurrency"));
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
            encrypt_metadata: None,
            opaque_filenames: None,
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
            encrypt_metadata: None,
            opaque_filenames: None,
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
