//! Migration between backends.
//!
//! Implements `xv migrate --from <backend> --to <backend>`, which copies
//! secrets from one backend to another while preserving metadata.

use crate::backend::{Backend, BackendError, BackendRef, BackendRegistry};
use crate::config::settings::Config;
use crate::error::{CrosstacheError, Result};
#[cfg(test)]
use crate::secret::domain::SecretMetadata;
use crate::secret::domain::SecretRequest;
use crate::secret::domain::SecretValue;
use crate::utils::output;
use futures::stream::{self, StreamExt};
use std::sync::Arc;

const TAG_MIGRATED_FROM: &str = "xv:migrated_from";
const TAG_MIGRATED_AT: &str = "xv:migrated_at";

/// Outcome of a single secret migration attempt.
#[derive(Debug)]
enum MigrateOutcome {
    /// Secret was successfully copied to the target backend.
    Migrated(String),
    /// Secret was already migrated (same source version exists in target).
    Skipped(String),
}

struct MigrationDiff {
    to_migrate: Vec<MigrationName>,
    conflicts: Vec<MigrationName>,
}

#[derive(Clone)]
struct MigrationName {
    source: String,
    destination: String,
}

async fn compute_diff(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    source_vault: &str,
    target_vault: &str,
    filter: Option<&str>,
    target_missing: bool,
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
        let props = source
            .secrets()
            .get_secret_metadata(source_vault, &name)
            .await?;
        let name = MigrationName {
            source: name,
            // `build_request_from_props` derives the destination name from
            // `original_name` alone; planning needs the name, not the value.
            destination: props.original_name.clone(),
        };
        if target_missing {
            to_migrate.push(name);
            continue;
        }
        match target
            .secrets()
            .secret_exists(target_vault, &name.destination)
            .await
        {
            Ok(true) => conflicts.push(name),
            Ok(false) | Err(BackendError::VaultNotFound { .. }) => to_migrate.push(name),
            Err(e) => {
                return Err(CrosstacheError::Unknown(format!(
                    "Failed to determine whether target secret '{}' exists: {e}",
                    name.destination
                )));
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
    props: &crate::secret::domain::Secret,
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
        value: SecretValue::new(props.value.expose_secret().to_string()),
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

#[allow(clippy::too_many_arguments)]
async fn migrate_one(
    source: &Arc<dyn Backend>,
    target: &Arc<dyn Backend>,
    source_vault: &str,
    target_vault: &str,
    name: &str,
    force_replace: bool,
    source_name_for_tag: &str,
    planned_destination_name: &str,
) -> std::result::Result<MigrateOutcome, (String, String)> {
    // Fetch full props with value
    let props = source
        .secrets()
        .get_secret(source_vault, name)
        .await
        .map_err(|e| (name.to_string(), format!("get_secret: {e}")))?;

    // Marked attachment-key custody records are managed by the key ring and
    // are not migrated through generic migration (design §E): a portable copy
    // needs its provider-version hints rewritten, which is a dedicated custody
    // operation, not a blind secret copy. An unmarked strict-format user
    // collision is an ordinary secret and migrates normally.
    if crate::secret::attachment_key::is_strict_retained_record_name(name)
        && crate::secret::attachment_key::is_marked_key_record(&props.content_type)
    {
        output::warn(&format!(
            "skipping '{name}': attachment key custody record, managed by the key ring \
             and not migrated through generic migration"
        ));
        return Ok(MigrateOutcome::Skipped(name.to_string()));
    }

    let request = build_request_from_props(&props, source_name_for_tag, source_vault);
    if request.name != planned_destination_name {
        return Err((
            name.to_string(),
            "destination name changed after migration preflight".into(),
        ));
    }

    // Idempotency checks and writes must address the same planned name.
    if !force_replace {
        match target
            .secrets()
            .get_secret_metadata(target_vault, &request.name)
            .await
        {
            Ok(existing) => {
                if let Some(prev_from) = existing.tags.get(TAG_MIGRATED_FROM) {
                    let expected =
                        format!("{}:{}:{}", source_name_for_tag, source_vault, props.version);
                    if prev_from == &expected {
                        return Ok(MigrateOutcome::Skipped(name.to_string()));
                    }
                }
            }
            Err(BackendError::NotFound { .. }) => {}
            Err(e) => {
                return Err((
                    name.to_string(),
                    format!("target existence check failed: {e}"),
                ));
            }
        }
    }

    // The idempotency check above is skipped under `force_replace`, so a
    // blind overwrite of the target vault's OWN reserved attachment key
    // (distinct from the source's copy of the same well-known name) would
    // otherwise make every attachment already in the target unreadable.
    // Migrating the key into a vault that doesn't have one yet is fine —
    // that vault has no attachments depending on it.
    if request.name == crate::secret::attachments::ATTACHMENT_KEY_SECRET {
        match target
            .secrets()
            .get_secret_metadata(target_vault, &request.name)
            .await
        {
            Ok(_) => {
                output::warn(&format!(
                    "skipping '{name}': target vault '{target_vault}' already has its own \
                     attachment encryption key; overwriting it would make existing attachments \
                     there unreadable, so it was preserved"
                ));
                return Ok(MigrateOutcome::Skipped(name.to_string()));
            }
            Err(BackendError::NotFound { .. }) => {}
            Err(e) => {
                return Err((
                    name.to_string(),
                    format!("target existence check failed: {e}"),
                ));
            }
        }
    }

    crate::backend::secret::validate_transfer_request(target.as_ref(), &request)
        .map_err(|error| (name.to_string(), format!("destination request: {error}")))?;
    target
        .secrets()
        .validate_transfer_metadata(target_vault, &request)
        .await
        .map_err(|error| (name.to_string(), format!("destination metadata: {error}")))?;

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
    attachments: crate::cli::transfer_support::AttachmentTransferOptions,
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

    let target_missing = if let Some(vaults) = target.vaults() {
        match vaults.get_vault(&target_vault, None).await {
            Ok(_) => false,
            Err(BackendError::VaultNotFound { .. }) => true,
            Err(error) => return Err(error.into()),
        }
    } else {
        false
    };

    // 5. Compute diff (list + filter + conflict detection)
    let diff = compute_diff(
        &source,
        &target,
        &source_vault,
        &target_vault,
        filter.as_deref(),
        target_missing,
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

    if !diff.conflicts.is_empty() && on_conflict == crate::cli::commands::OnConflict::Fail {
        return Err(CrosstacheError::conflict(format!(
            "{} conflict(s) detected; aborting (--on-conflict fail)",
            diff.conflicts.len()
        )));
    }
    let mut selected = diff.to_migrate.clone();
    if on_conflict == crate::cli::commands::OnConflict::Replace {
        selected.extend(diff.conflicts.clone());
    }
    let mut names_to_process = Vec::new();
    let mut preflight_skipped = 0usize;
    let mut destination_names: Vec<String> = Vec::new();
    #[cfg(feature = "file-ops")]
    let mut attached_intents = Vec::new();
    // Complete read-only preflight. Even an error in the last selected item
    // precedes vault creation, recovery directories, and the first secret write.
    for selected_name in selected {
        let name = selected_name.source;
        let props = source.secrets().get_secret(&source_vault, &name).await?;
        if crate::secret::attachment_key::is_strict_retained_record_name(&name)
            && crate::secret::attachment_key::is_marked_key_record(&props.content_type)
        {
            output::warn(&format!("skipping '{name}': attachment key custody record"));
            preflight_skipped += 1;
            continue;
        }
        let request = build_request_from_props(&props, source.name(), &source_vault);
        if request.name != selected_name.destination {
            return Err(CrosstacheError::conflict(
                "destination name changed during migration preflight",
            ));
        }
        let destination_name = &request.name;
        let existing = if target_missing {
            None
        } else {
            match target
                .secrets()
                .get_secret_metadata(&target_vault, destination_name)
                .await
            {
                Ok(props) => Some(props),
                Err(BackendError::NotFound { .. } | BackendError::VaultNotFound { .. }) => None,
                Err(error) => return Err(error.into()),
            }
        };
        if destination_name == crate::secret::attachments::ATTACHMENT_KEY_SECRET
            && existing.is_some()
        {
            output::warn(&format!(
                "skipping '{name}': preserving target attachment key"
            ));
            preflight_skipped += 1;
            continue;
        }
        if !force_replace
            && existing.as_ref().is_some_and(|p| {
                p.tags.get(TAG_MIGRATED_FROM)
                    == Some(&format!(
                        "{}:{}:{}",
                        source.name(),
                        source_vault,
                        props.version
                    ))
            })
        {
            preflight_skipped += 1;
            continue;
        }
        for previous in &destination_names {
            if target
                .transfer_secret_names_collide(&target_vault, previous, destination_name)
                .await?
            {
                return Err(CrosstacheError::conflict(
                    "selected migration entries collide in the destination namespace",
                ));
            }
        }
        destination_names.push(destination_name.clone());
        if !target_missing {
            crate::cli::transfer_support::reject_self_target(
                source.as_ref(),
                &source_vault,
                &name,
                target.as_ref(),
                &target_vault,
                destination_name,
            )
            .await?;
            crate::backend::ensure_no_attachments(target.as_ref(), &target_vault, destination_name)
                .await?;
        }
        let attached = !source
            .attachment_names(&source_vault, &name)
            .await?
            .is_empty();
        if attached {
            if !attachments.with_attachments {
                return Err(BackendError::AttachmentsPresent { name }.into());
            }
            if target_missing {
                return Err(CrosstacheError::config(format!("attachment destination vault '{target_vault}' must already exist with a healthy key; create it and run xv attachment-key initialize --vault {target_vault} --apply --offline, then supply --to-key-id")));
            }
            #[cfg(feature = "file-ops")]
            {
                use crate::secret::attachment_transfer::{
                    TransferEndpoint, TransferIntent, TransferOperation,
                };
                let intent = TransferIntent {
                    source: TransferEndpoint {
                        identity: source.name().into(),
                        vault: source_vault.clone(),
                    },
                    destination: TransferEndpoint {
                        identity: target.name().into(),
                        vault: target_vault.clone(),
                    },
                    source_name: name.clone(),
                    destination_name: destination_name.clone(),
                    operation: TransferOperation::Copy,
                    destination_key_id: attachments.to_key_id.clone(),
                    destination_folder: None,
                };
                if !dry_run && !attachments.offline {
                    return Err(CrosstacheError::invalid_argument(
                        "attached migration requires --offline after stopping other writers",
                    ));
                }
                attached_intents.push(intent);
            }
            #[cfg(not(feature = "file-ops"))]
            return Err(CrosstacheError::config(
                "attachment transfers require a build with file-ops",
            ));
        } else {
            if crate::secret::attachment_key::generic_mutation_blocked_canonical(destination_name) {
                return Err(CrosstacheError::conflict(format!("migration destination name '{destination_name}' is reserved for attachment custody; no changes were written")));
            }
            crate::backend::secret::validate_transfer_request(target.as_ref(), &request)?;
            target
                .secrets()
                .validate_transfer_metadata(&target_vault, &request)
                .await?;
            names_to_process.push((name, destination_name.clone()));
        }
    }
    #[cfg(feature = "file-ops")]
    {
        let previews = crate::secret::attachment_transfer_execution::preflight_batch(
            source.as_ref(),
            target.as_ref(),
            &attached_intents,
        )
        .await?;
        if dry_run {
            for preview in previews {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            }
        }
    }
    if dry_run {
        return Ok(());
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
                    return Err(CrosstacheError::Unknown(format!(
                        "Failed to determine whether target vault '{target_vault}' exists: {e}"
                    )));
                }
            }
        }
    }

    let invalidate_destination = || {
        let target_backend = to_kind.to_string();
        crate::cache::invalidation::on_secret_mutation(&config, &target_backend, &target_vault);
        crate::cache::invalidation::on_file_mutation(&config, &target_backend, &target_vault);
    };

    #[cfg(feature = "file-ops")]
    let attached_count = attached_intents.len();
    #[cfg(not(feature = "file-ops"))]
    let attached_count = 0;
    #[cfg(feature = "file-ops")]
    for intent in attached_intents {
        if let Err(e) = crate::cli::transfer_support::run_attached(
            source.as_ref(),
            target.as_ref(),
            intent,
            &attachments,
            false,
        )
        .await
        {
            invalidate_destination();
            return Err(e);
        }
    }

    // 6. Migrate secrets concurrently with backoff retry
    let source_name_tag = source.name().to_string();
    let source_arc = source.clone();
    let target_arc = target.clone();
    let src_vault_clone = source_vault.clone();
    let tgt_vault_clone = target_vault.clone();

    let results: Vec<_> = stream::iter(names_to_process.iter().map(|(name, destination_name)| {
        let source = source_arc.clone();
        let target = target_arc.clone();
        let sv = src_vault_clone.clone();
        let tv = tgt_vault_clone.clone();
        let name = name.clone();
        let destination_name = destination_name.clone();
        let src_tag = source_name_tag.clone();
        async move {
            migrate_one(
                &source,
                &target,
                &sv,
                &tv,
                &name,
                force_replace,
                &src_tag,
                &destination_name,
            )
            .await
        }
    }))
    .buffer_unordered(concurrency)
    .collect()
    .await;

    let mut migrated = attached_count;
    let mut skipped = preflight_skipped;
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
    if migrated > 0 {
        invalidate_destination();
    }
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

        let props = crate::secret::domain::Secret {
            metadata: SecretMetadata {
                name: "db-password".to_string(),
                original_name: "db-password".to_string(),
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
            },
            value: SecretValue::new("secret-value".to_string()),
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
            Default::default(),
            Config::default(),
        )
        .await;

        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("concurrency"));
    }

    #[tokio::test]
    async fn migration_cannot_bypass_a_policy_wrapped_source() {
        let source_tmp = TempDir::new().unwrap();
        let target_tmp = TempDir::new().unwrap();
        let local_config = |tmp: &TempDir| LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
        };
        let raw_source: Arc<dyn Backend> = Arc::new(
            crate::backend::local::LocalBackend::new(Some(&local_config(&source_tmp))).unwrap(),
        );
        raw_source
            .secrets()
            .set_secret(
                "default",
                SecretRequest {
                    name: "existing".into(),
                    value: SecretValue::new("would-leak-if-delegated"),
                    content_type: None,
                    enabled: Some(true),
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: None,
                    folder: None,
                },
            )
            .await
            .unwrap();
        let policy =
            crate::agent::policy::CompiledPolicy::compile(&crate::config::settings::AgentConfig {
                enforce: true,
                ..Default::default()
            })
            .unwrap();
        let source: Arc<dyn Backend> =
            Arc::new(crate::agent::enforce::PolicyEnforcedBackend::for_test(
                raw_source,
                crate::agent::AgentIdentity::new(
                    crate::agent::IdentitySource::EnvAssertion,
                    "migration-agent",
                ),
                policy,
                source_tmp.path().join("decisions.jsonl"),
            ));
        let target: Arc<dyn Backend> = Arc::new(
            crate::backend::local::LocalBackend::new(Some(&local_config(&target_tmp))).unwrap(),
        );

        let error = migrate_one(
            &source, &target, "default", "default", "existing", false, "local", "existing",
        )
        .await
        .unwrap_err();
        assert!(error.1.contains("agent policy denied"), "{error:?}");
        assert!(!target
            .secrets()
            .secret_exists("default", "existing")
            .await
            .unwrap());
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
            audit: None,
            git: None,
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
            audit: None,
            git: None,
        };

        // Create source backend and seed it with secrets
        let source = crate::backend::local::LocalBackend::new(Some(&source_config)).unwrap();
        let target = crate::backend::local::LocalBackend::new(Some(&target_config)).unwrap();

        // Seed source with test secrets
        for name in ["db-password", "api-key", "cache-token"] {
            let req = SecretRequest {
                name: name.to_string(),
                value: SecretValue::new(format!("value-for-{name}")),
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
                .get_secret("default", &summary.name)
                .await
                .unwrap();
            let (props, value) = props.into_parts();
            let req = SecretRequest {
                name: props.original_name.clone(),
                value,
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
                .get_secret("default", name)
                .await
                .unwrap();
            let tgt = target_arc
                .secrets()
                .get_secret("default", name)
                .await
                .unwrap();
            assert_eq!(src.value, tgt.value);
        }
    }

    #[tokio::test]
    async fn migration_batch_rejects_duplicate_final_names_before_vault_creation() {
        migration_batch_destination_names(["same", "same"], false, true).await;
    }

    #[tokio::test]
    async fn migration_batch_rejects_duplicate_final_names_with_opaque_storage() {
        migration_batch_destination_names(["same", "same"], true, true).await;
    }

    #[tokio::test]
    async fn migration_batch_legacy_case_aliases_follow_filesystem_semantics() {
        let probe = TempDir::new().unwrap();
        std::fs::write(probe.path().join("case-probe"), b"probe").unwrap();
        let case_insensitive = probe.path().join("CASE-PROBE").exists();
        migration_batch_destination_names(["a", "A"], false, case_insensitive).await;
    }

    #[tokio::test]
    async fn migration_batch_opaque_case_names_remain_distinct() {
        migration_batch_destination_names(["a", "A"], true, false).await;
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn destination_file_collisions_across_migration_batch_precede_earlier_writes() {
        let tmp = TempDir::new().unwrap();
        let mut local = LocalConfig {
            store_path: Some(tmp.path().join("store").display().to_string()),
            key_file: Some(tmp.path().join("identity").display().to_string()),
            default_vault: Some("source".into()),
            encrypt_metadata: Some(false),
            opaque_filenames: Some(false),
            ..Default::default()
        };
        let source = crate::backend::local::LocalBackend::new(Some(&local)).unwrap();
        for (lookup, final_name) in [("provider-one", "a"), ("provider-two", "A")] {
            source
                .secrets()
                .set_secret(
                    "source",
                    SecretRequest {
                        name: lookup.into(),
                        value: SecretValue::new(format!("value-{lookup}")),
                        content_type: None,
                        enabled: None,
                        expires_on: None,
                        not_before: None,
                        tags: None,
                        groups: None,
                        note: None,
                        folder: None,
                    },
                )
                .await
                .unwrap();
            let path = tmp
                .path()
                .join(format!("store/vaults/source/secrets/{lookup}.meta.json"));
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            metadata["original_name"] = final_name.into();
            std::fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
            crate::secret::attachments::upload_encrypted(
                source.attachment_keys().as_ref(),
                source.files().unwrap(),
                "source",
                crate::blob::models::FileUploadRequest {
                    name: format!("attachments/{lookup}/proof.txt"),
                    content: lookup.as_bytes().to_vec(),
                    content_type: Some("text/plain".into()),
                    groups: vec![],
                    tags: HashMap::new(),
                    metadata: HashMap::new(),
                },
                None,
            )
            .await
            .unwrap();
        }
        local.default_vault = Some("target".into());
        local.opaque_filenames = Some(true);
        let target = crate::backend::local::LocalBackend::new(Some(&local)).unwrap();
        let initialized = crate::secret::attachment_lifecycle::initialize(
            target.attachment_keys().as_ref(),
            target.files().unwrap(),
            "target",
            true,
        )
        .await
        .unwrap();
        let parent = tmp.path().join("store/vaults/target");
        std::fs::write(parent.join("case-probe"), b"").unwrap();
        let insensitive = parent.join("CASE-PROBE").exists();
        std::fs::remove_file(parent.join("case-probe")).unwrap();
        let recovery = tmp.path().join("recovery");
        let result = execute_migrate(
            "local:source".into(),
            "local:target".into(),
            None,
            None,
            false,
            crate::cli::commands::OnConflict::Replace,
            true,
            1,
            crate::cli::transfer_support::AttachmentTransferOptions {
                with_attachments: true,
                offline: true,
                to_key_id: initialized.active_key_id,
                recovery_dir: Some(recovery.clone()),
            },
            Config {
                backend: Some("local".into()),
                local: Some(local),
                ..Default::default()
            },
        )
        .await;
        let unknown = result.as_ref().err().is_some_and(|e| {
            e.to_string()
                .contains("cannot establish destination filesystem case semantics")
        });
        if insensitive || unknown {
            let error =
                result.expect_err("opaque secrets still share encoded attachment object names");
            for name in ["a", "A"] {
                assert!(target
                    .secrets()
                    .get_secret_metadata("target", name)
                    .await
                    .is_err());
            }
            assert!(
                !parent.join("files").exists(),
                "later collision must prevent the first namespace preparation"
            );
            assert!(
                !recovery.exists(),
                "later collision must prevent the first recovery journal"
            );
            assert!(error.to_string().contains("collid") || unknown, "{error}");
        } else {
            result.unwrap();
            for name in ["a", "A"] {
                assert_eq!(
                    target.attachment_names("target", name).await.unwrap().len(),
                    1
                );
            }
        }
        for lookup in ["provider-one", "provider-two"] {
            // Reopening in opaque mode migrates legacy source stems as well.
            assert_eq!(
                target
                    .secrets()
                    .get_secret("source", lookup)
                    .await
                    .unwrap()
                    .value
                    .expose_secret(),
                format!("value-{lookup}")
            );
            assert_eq!(
                target
                    .attachment_names("source", lookup)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
    }

    async fn migration_batch_destination_names(
        final_names: [&str; 2],
        opaque: bool,
        expect_collision: bool,
    ) {
        let tmp = TempDir::new().unwrap();
        let mut local = LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().into_owned()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().into_owned()),
            default_vault: Some("source".into()),
            encrypt_metadata: Some(false),
            opaque_filenames: Some(false),
            ..Default::default()
        };
        let source = crate::backend::local::LocalBackend::new(Some(&local)).unwrap();
        // Provider lookup names and original names are separate persisted fields.
        // AWS permits this through xv:original_name; Local plaintext metadata
        // gives this regression the same real request-builder input without AWS.
        for (lookup, original) in ["provider-one", "provider-two"]
            .into_iter()
            .zip(final_names)
        {
            source
                .secrets()
                .set_secret(
                    "source",
                    SecretRequest {
                        name: lookup.into(),
                        value: SecretValue::new(format!("value-{lookup}")),
                        content_type: None,
                        enabled: None,
                        expires_on: None,
                        not_before: None,
                        tags: None,
                        groups: None,
                        note: None,
                        folder: None,
                    },
                )
                .await
                .unwrap();
            let path = tmp
                .path()
                .join(format!("store/vaults/source/secrets/{lookup}.meta.json"));
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            metadata["original_name"] = original.into();
            std::fs::write(path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        }
        // Opaque mode supports reading existing legacy source entries; newly
        // created destination entries must use distinct keyed filename stems.
        local.opaque_filenames = Some(opaque);
        let result = execute_migrate(
            "local:source".into(),
            "local:target".into(),
            None,
            None,
            false,
            crate::cli::commands::OnConflict::Replace,
            true,
            1,
            Default::default(),
            Config {
                backend: Some("local".into()),
                local: Some(local.clone()),
                ..Default::default()
            },
        )
        .await;
        // Unknown mount semantics (for example Linux overlay) may safely refuse
        // only the ambiguous legacy pair. This must still precede provisioning.
        let unknown_case_semantics = !opaque
            && final_names == ["a", "A"]
            && matches!(&result, Err(CrosstacheError::InvalidArgument(message))
                if message == "operation not supported: cannot establish destination filesystem case semantics for potentially aliasing names");
        if unknown_case_semantics {
            assert!(!tmp.path().join("store/vaults/target").exists());
        } else if expect_collision {
            let error =
                result.expect_err("duplicate actual destination identities must fail preflight");
            assert!(error.to_string().contains("collide"), "{error}");
            assert!(
                !tmp.path().join("store/vaults/target").exists(),
                "collision must precede target vault creation"
            );
        } else {
            result.unwrap();
            let target = crate::backend::local::LocalBackend::new(Some(&local)).unwrap();
            for (lookup, destination) in ["provider-one", "provider-two"]
                .into_iter()
                .zip(final_names)
            {
                let actual = target
                    .secrets()
                    .get_secret("target", destination)
                    .await
                    .unwrap();
                assert_eq!(actual.value.expose_secret(), format!("value-{lookup}"));
            }
        }
        for lookup in ["provider-one", "provider-two"] {
            let actual = source.secrets().get_secret("source", lookup).await.unwrap();
            assert_eq!(actual.value.expose_secret(), format!("value-{lookup}"));
        }
    }

    #[tokio::test]
    async fn forced_bulk_migration_preflights_all_attachments_before_first_secret_write() {
        let tmp = TempDir::new().unwrap();
        let local = LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
            default_vault: Some("source".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
        };
        let config = Config {
            backend: Some("local".into()),
            local: Some(local.clone()),
            ..Default::default()
        };
        let source = crate::backend::local::LocalBackend::new(Some(&local)).unwrap();
        for name in ["a-clean", "z-attached"] {
            source
                .secrets()
                .set_secret(
                    "source",
                    SecretRequest {
                        name: name.into(),
                        value: SecretValue::new("value"),
                        content_type: None,
                        enabled: None,
                        expires_on: None,
                        not_before: None,
                        tags: None,
                        groups: None,
                        note: None,
                        folder: None,
                    },
                )
                .await
                .unwrap();
        }
        let files = tmp.path().join("store/vaults/source/files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(
            files.join("attached.meta.json"),
            br#"{"name":"attachments/z-attached/proof.txt"}"#,
        )
        .unwrap();

        let error = execute_migrate(
            "local:source".into(),
            "local:target".into(),
            None,
            None,
            false,
            crate::cli::commands::OnConflict::Replace,
            true,
            2,
            Default::default(),
            config,
        )
        .await
        .expect_err("--force-replace cannot bypass attachment preflight");
        assert!(error.to_string().contains("attachments"), "{error}");

        assert!(
            !tmp.path().join("store/vaults/target").exists(),
            "attachment preflight must precede even destination vault creation"
        );
    }

    #[tokio::test]
    async fn migrate_one_preserves_targets_own_reserved_attachment_key_under_force() {
        let source_tmp = TempDir::new().unwrap();
        let target_tmp = TempDir::new().unwrap();
        let source_config = LocalConfig {
            store_path: Some(
                source_tmp
                    .path()
                    .join("store")
                    .to_string_lossy()
                    .into_owned(),
            ),
            key_file: Some(
                source_tmp
                    .path()
                    .join("key.txt")
                    .to_string_lossy()
                    .into_owned(),
            ),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
        };
        let target_config = LocalConfig {
            store_path: Some(
                target_tmp
                    .path()
                    .join("store")
                    .to_string_lossy()
                    .into_owned(),
            ),
            key_file: Some(
                target_tmp
                    .path()
                    .join("key.txt")
                    .to_string_lossy()
                    .into_owned(),
            ),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
        };
        let source = crate::backend::local::LocalBackend::new(Some(&source_config)).unwrap();
        let target = crate::backend::local::LocalBackend::new(Some(&target_config)).unwrap();

        let reserved = crate::secret::attachments::ATTACHMENT_KEY_SECRET;
        let seed = |value: &str| SecretRequest {
            name: reserved.to_string(),
            value: SecretValue::new(value.to_string()),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        };
        source
            .secrets()
            .set_secret("default", seed("source-key"))
            .await
            .unwrap();
        target
            .secrets()
            .set_secret("default", seed("target-key"))
            .await
            .unwrap();

        let source_arc: Arc<dyn Backend> = Arc::new(source);
        let target_arc: Arc<dyn Backend> = Arc::new(target);

        // force_replace=true would normally skip the idempotency check and
        // overwrite unconditionally — the reserved-key guard must still
        // preserve the target's own key rather than clobbering it with the
        // source's.
        let outcome = migrate_one(
            &source_arc,
            &target_arc,
            "default",
            "default",
            reserved,
            true,
            "local",
            reserved,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, MigrateOutcome::Skipped(_)), "{outcome:?}");

        let tgt = target_arc
            .secrets()
            .get_secret("default", reserved)
            .await
            .unwrap();
        assert_eq!(tgt.value.expose_secret(), "target-key");

        // force_replace=false should also skip and preserve the target's key.
        let outcome = migrate_one(
            &source_arc,
            &target_arc,
            "default",
            "default",
            reserved,
            false,
            "local",
            reserved,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, MigrateOutcome::Skipped(_)), "{outcome:?}");

        let tgt = target_arc
            .secrets()
            .get_secret("default", reserved)
            .await
            .unwrap();
        assert_eq!(tgt.value.expose_secret(), "target-key");
    }

    #[test]
    fn glob_filter_works() {
        let glob = globset::Glob::new("db-*").unwrap().compile_matcher();
        assert!(glob.is_match("db-password"));
        assert!(glob.is_match("db-host"));
        assert!(!glob.is_match("api-key"));
    }
}
