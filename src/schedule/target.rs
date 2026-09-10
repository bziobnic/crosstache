//! Selected-backend identity digest for the schedule target manifest.
//!
//! `manifest.json`'s `target.backend_identity` pins *which account* a
//! scheduled run is allowed to touch, independent of the vault name (the
//! vault is recorded separately in `target.vault`). Two configs that select
//! the same backend *kind* but a different tenant, subscription, profile,
//! endpoint, or local store path must never collide on this digest — that
//! collision is exactly the "scheduled rotation silently ran against the
//! wrong account" failure this manifest exists to prevent.
//!
//! [`selected_backend_identity`] mirrors the registry-name resolution rules
//! in [`crate::backend::registry::BackendRegistry::construct_named`]: a
//! `config.named_backends` entry wins; otherwise the name must parse as a
//! built-in [`crate::backend::BackendKind`]. It intentionally does not reuse
//! [`crate::cache::fingerprint::config_fingerprint`] — that digest is
//! deliberately config-wide (every built-in backend the config configures)
//! and does not distinguish a specific *named* backend entry, which is
//! exactly the case this digest must separate.
//!
//! [`resolve_install_target`] is the other half: it resolves the config,
//! project, environment, context and workspace layers exactly once and
//! returns the complete [`crate::schedule::manifest::ManifestTarget`] that
//! both the install preview and the manifest describe.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::backend::local::config::ResolvedLocalConfig;
use crate::backend::BackendKind;
use crate::config::settings::{AwsConfig, Config, NamedBackendEntry};
use crate::error::{CrosstacheError, Result};
use crate::schedule::manifest::ManifestTarget;
use crate::workspace::{Workspace, WorkspaceEntry, WorkspaceSource};

/// Domain separator prepended to the canonical JSON before hashing, so this
/// digest can never collide with a digest computed by an unrelated feature
/// (e.g. `cache::config_fingerprint`) even if the JSON payload happened to
/// match byte-for-byte.
const IDENTITY_DOMAIN_SEPARATOR: &[u8] = b"xv-schedule-backend-v1\n";

/// The selected registry entry's stable identity, computed at install time
/// and re-derived at run time to detect drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedBackendIdentity {
    /// The registry name as given (a `named_backends` key, or a built-in
    /// kind name/alias such as `"azure"`, `"az"`, `"local"`, `"aws"`).
    pub name: String,
    /// The resolved backend kind: `"azure"`, `"local"`, or `"aws"`.
    pub kind: String,
    /// `sha256:<64 lowercase hex chars>` over the domain-separated,
    /// canonical-JSON identity payload.
    pub digest: String,
}

// ---------------------------------------------------------------------------
// Canonical identity payloads (private, fixed field order per spec table)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AzureIdentityFields<'a> {
    registry_name: &'a str,
    tenant_id: Option<&'a str>,
    subscription_id: Option<&'a str>,
    credential_priority: &'a str,
}

/// Only reachable in an AWS-enabled build; without the feature every AWS
/// selection is refused before an identity is computed.
#[cfg_attr(not(feature = "aws"), allow(dead_code))]
#[derive(Serialize)]
struct AwsIdentityFields<'a> {
    registry_name: &'a str,
    region: Option<&'a str>,
    profile: Option<&'a str>,
    endpoint_url: Option<&'a str>,
}

#[derive(Serialize)]
struct LocalIdentityFields<'a> {
    registry_name: &'a str,
    store_path: &'a str,
}

fn digest_payload<T: Serialize>(payload: &T) -> Result<String> {
    let json = serde_json::to_vec(payload).map_err(|e| {
        CrosstacheError::config(format!(
            "failed to serialize schedule backend identity: {e}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(IDENTITY_DOMAIN_SEPARATOR);
    hasher.update(&json);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Only reachable in a build without the `aws` feature, where every AWS
/// selection is refused instead of resolving an identity.
#[cfg_attr(feature = "aws", allow(dead_code))]
fn aws_not_compiled_error(registry_name: &str) -> CrosstacheError {
    CrosstacheError::config(format!(
        "schedule backend '{registry_name}' is aws, but this binary lacks AWS support (rebuild with --features aws)"
    ))
}

#[cfg_attr(not(feature = "aws"), allow(dead_code))]
fn aws_identity(registry_name: &str, aws_cfg: &AwsConfig) -> Result<SelectedBackendIdentity> {
    let fields = AwsIdentityFields {
        registry_name,
        region: aws_cfg.region.as_deref(),
        profile: aws_cfg.profile.as_deref(),
        endpoint_url: aws_cfg.endpoint_url.as_deref(),
    };
    Ok(SelectedBackendIdentity {
        name: registry_name.to_string(),
        kind: "aws".to_string(),
        digest: digest_payload(&fields)?,
    })
}

fn azure_identity(registry_name: &str, config: &Config) -> Result<SelectedBackendIdentity> {
    let azure = config.azure_settings();
    let credential_priority = config.azure_credential_priority.to_string();
    let fields = AzureIdentityFields {
        registry_name,
        tenant_id: azure.tenant_id.as_deref(),
        subscription_id: azure.subscription_id.as_deref(),
        credential_priority: &credential_priority,
    };
    Ok(SelectedBackendIdentity {
        name: registry_name.to_string(),
        kind: "azure".to_string(),
        digest: digest_payload(&fields)?,
    })
}

/// Lexically normalize `path` against the current working directory (no
/// filesystem access): resolve `.`/`..` components in the string, without
/// requiring the path to exist. Used as the fallback when a local store path
/// does not exist yet — matches the normalization
/// [`crate::schedule::manifest::validate_absolute_normalized_path`] requires
/// (absolute, no `.`/`..` components).
fn lexically_normalize(path: &Path) -> Result<PathBuf> {
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().map_err(|e| {
            CrosstacheError::config(format!(
                "cannot resolve local store path '{}' from the current directory: {e}",
                path.display()
            ))
        })?
    };
    Ok(crate::utils::helpers::lexically_normalize_from(&base, path))
}

/// Resolve the local store path exactly as the local backend does
/// ([`ResolvedLocalConfig::from_raw`]), then canonicalize it if it exists on
/// disk, or lexically normalize it otherwise (a store not yet created still
/// needs a stable identity to pin).
fn identity_store_path(
    local_cfg: Option<&crate::config::settings::LocalConfig>,
) -> Result<PathBuf> {
    let resolved = ResolvedLocalConfig::from_raw(local_cfg);
    if resolved.store_path.exists() {
        std::fs::canonicalize(&resolved.store_path).map_err(|e| {
            CrosstacheError::config(format!(
                "failed to canonicalize local store path '{}': {e}",
                resolved.store_path.display()
            ))
        })
    } else {
        lexically_normalize(&resolved.store_path)
    }
}

fn local_identity(
    registry_name: &str,
    local_cfg: Option<&crate::config::settings::LocalConfig>,
) -> Result<SelectedBackendIdentity> {
    let store_path = identity_store_path(local_cfg)?;
    let store_path = store_path.to_string_lossy().into_owned();
    let fields = LocalIdentityFields {
        registry_name,
        store_path: &store_path,
    };
    Ok(SelectedBackendIdentity {
        name: registry_name.to_string(),
        kind: "local".to_string(),
        digest: digest_payload(&fields)?,
    })
}

fn identity_from_named_entry(
    registry_name: &str,
    entry: &NamedBackendEntry,
) -> Result<SelectedBackendIdentity> {
    match entry {
        NamedBackendEntry::Aws(aws_cfg) => {
            #[cfg(feature = "aws")]
            {
                aws_identity(registry_name, aws_cfg)
            }
            #[cfg(not(feature = "aws"))]
            {
                let _ = aws_cfg;
                Err(aws_not_compiled_error(registry_name))
            }
        }
        NamedBackendEntry::Local(local_cfg) => local_identity(registry_name, Some(local_cfg)),
    }
}

fn identity_from_builtin_kind(
    registry_name: &str,
    kind: BackendKind,
    config: &Config,
) -> Result<SelectedBackendIdentity> {
    match kind {
        BackendKind::Azure => azure_identity(registry_name, config),
        BackendKind::Local => local_identity(registry_name, config.local.as_ref()),
        BackendKind::Aws => {
            #[cfg(feature = "aws")]
            {
                let aws_cfg = config.aws.clone().unwrap_or_default();
                aws_identity(registry_name, &aws_cfg)
            }
            #[cfg(not(feature = "aws"))]
            {
                let _ = config;
                Err(aws_not_compiled_error(registry_name))
            }
        }
    }
}

/// Compute the selected registry entry's stable identity digest.
///
/// Resolution mirrors
/// [`crate::backend::registry::BackendRegistry::construct_named`]: a
/// `config.named_backends` entry named `registry_name` wins; otherwise
/// `registry_name` must parse as a built-in [`BackendKind`]. An unknown
/// registry name, or an AWS selection in a binary built without the `aws`
/// feature, is an error rather than a partial/degraded identity — an
/// unattended scheduled run must never pin a target it cannot actually
/// resolve.
pub fn selected_backend_identity(
    config: &Config,
    registry_name: &str,
) -> Result<SelectedBackendIdentity> {
    if let Some(entry) = config.named_backends.get(registry_name) {
        return identity_from_named_entry(registry_name, entry);
    }

    let kind: BackendKind = registry_name.parse().map_err(|_: String| {
        CrosstacheError::config(format!(
            "unknown schedule backend registry name '{registry_name}': not a configured named backend and not a built-in backend kind (azure, local, aws)"
        ))
    })?;

    identity_from_builtin_kind(registry_name, kind, config)
}

// ---------------------------------------------------------------------------
// Canonical target resolution
// ---------------------------------------------------------------------------

/// A resolved schedule target: the complete manifest target plus the workspace
/// entry it came from, resolved exactly once at install time.
///
/// Everything a scheduled run is allowed to touch is decided here, so the
/// install preview, the unit that gets written, and (in a later task) the
/// manifest all describe the same target. Nothing downstream re-resolves.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedScheduleTarget {
    /// The manifest's `target` block, ready to serialize.
    pub(crate) target: ManifestTarget,
    /// The workspace entry that produced [`Self::target`] — its `vault` is
    /// the real vault to sweep, on registry backend `backend`, and its
    /// `alias` is the name a person recognizes it by.
    // Everything the *manifest* records already lives in `target`; these two
    // survive for the status/drift reporting that reads the resolved entry
    // back, which lands in a later task of this series.
    #[allow(dead_code)]
    pub(crate) entry: WorkspaceEntry,
    /// Which resolution layer produced the workspace.
    #[allow(dead_code)]
    pub(crate) workspace_source: WorkspaceSource,
    /// The canonical working directory resolution ran in, recorded so the
    /// scheduled run replays the same `.xv.toml`/context discovery.
    pub(crate) working_directory: PathBuf,
}

/// Canonicalize a path for the manifest: resolve symlinks and `.`/`..`, then
/// drop any Windows verbatim prefix so the result is a path
/// `manifest::validate_absolute_normalized_path` accepts on every platform.
///
/// The path must exist — every path recorded by target resolution is a file
/// or directory that was actually read.
///
/// The prefix stripping is [`crate::utils::helpers::strip_verbatim_prefix`],
/// the *same* helper `crate::config::project::canonicalize_project_path` uses,
/// so the `.xv.toml` path recorded here and the one a later run resolves are
/// the same string on Windows instead of `C:\…` versus `\\?\C:\…`.
pub(crate) fn canonical_path_for_manifest(path: &Path) -> Result<PathBuf> {
    crate::utils::helpers::canonicalize_without_verbatim_prefix(path).map_err(|e| {
        CrosstacheError::config(format!(
            "cannot resolve the schedule target path '{}': {e}",
            path.display()
        ))
    })
}

/// Render a canonical path as the manifest's string form.
pub(crate) fn manifest_path_string(field: &str, path: &Path) -> Result<String> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        CrosstacheError::config(format!(
            "schedule manifest field '{field}' cannot be recorded: '{}' is not valid UTF-8",
            path.display()
        ))
    })
}

fn workspace_source_label(source: WorkspaceSource) -> &'static str {
    match source {
        WorkspaceSource::ProjectToml => "project",
        WorkspaceSource::Context => "context",
        WorkspaceSource::Degenerate => "degenerate",
    }
}

/// Resolve the complete schedule target from the install-time inputs.
///
/// `config` must be the configuration parsed from `config_path`'s exact
/// `config_bytes` — not the process configuration with environment overrides
/// folded in. An unattended run replays a *saved* configuration, so the
/// recorded digest and the recorded target have to describe the same file.
///
/// `ambient_backend`, when given, is the backend name the *invoking shell*
/// resolves to (`XV_BACKEND`, `--backend`). It is compared against the backend
/// the saved configuration resolves to and a mismatch is refused: a scheduled
/// run inherits none of that environment, so installing anyway would pin a
/// target the user never tested.
///
/// Reads the context file for `cwd` through the same loader normal commands
/// use, then defers to [`resolve_install_target_from`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_install_target(
    config: &Config,
    config_path: &Path,
    config_bytes: &[u8],
    cwd: &Path,
    vault: Option<&str>,
    cli_env: Option<&str>,
    ambient_backend: Option<&str>,
) -> Result<ResolvedScheduleTarget> {
    let context = crate::config::ContextManager::load_for_cwd(cwd).await?;
    resolve_install_target_from(
        config,
        config_path,
        config_bytes,
        cwd,
        &context,
        vault,
        cli_env,
        ambient_backend,
    )
    .await
}

/// Core of [`resolve_install_target`], parameterized over the loaded context
/// so it is testable without the ambient context lookup (which reads
/// `XV_CONTEXT_DIR` and the user's global context file). Production code goes
/// through [`resolve_install_target`].
#[allow(clippy::too_many_arguments)]
async fn resolve_install_target_from(
    config: &Config,
    config_path: &Path,
    config_bytes: &[u8],
    cwd: &Path,
    context: &crate::config::ContextManager,
    vault: Option<&str>,
    cli_env: Option<&str>,
    ambient_backend: Option<&str>,
) -> Result<ResolvedScheduleTarget> {
    // 1. The exact global config file.
    let config_path = canonical_path_for_manifest(config_path)?;
    let config_digest = crate::config::content_digest(config_bytes);

    // 2. The exact working directory. A cwd that cannot be resolved is an
    //    error: every later layer (`.xv.toml` discovery, local context
    //    discovery) is defined relative to it.
    let cwd = canonical_path_for_manifest(cwd)?;

    // 3. The `.xv.toml` governing that directory, and the environment it
    //    selects right now — installation resolves `XV_ENV`/`--env` once and
    //    records the result; the runner replays it.
    let project = crate::config::project::resolve_project_at(&cwd, cli_env).await?;

    // The effective configuration for resolution: the saved file, plus the
    // active env profile's backend folded in the same way `src/main.rs` folds
    // it for an interactive command.
    let mut effective = config.clone();
    effective.env_flag = cli_env.map(str::to_string);
    if let Some(backend) = project
        .as_ref()
        .and_then(|p| p.profile())
        .and_then(|profile| profile.backend.clone())
    {
        crate::config::project::validate_env_profile_backend(&backend)?;
        effective.backend = Some(backend);
    }

    // The shell's backend and the saved one must agree. `XV_BACKEND` and
    // `--backend` are ambient: the scheduled run inherits neither, so a
    // schedule installed under one of them would sweep a different backend
    // than the one the user just tested. Refuse rather than silently pinning
    // the saved backend. A `.xv.toml` profile backend is NOT ambient — it is
    // folded above and replayed from the recorded project file.
    if let Some(ambient) = ambient_backend {
        let saved = effective.effective_backend_name();
        if ambient != saved {
            return Err(CrosstacheError::invalid_argument(format!(
                "this shell resolves the backend '{ambient}', but the saved configuration this \
                 schedule would replay resolves '{saved}'. A scheduled run inherits no \
                 environment, so it would sweep '{saved}'. Save the backend you are testing \
                 ('xv backend add' or 'xv init'), or unset XV_BACKEND, before installing a \
                 schedule."
            )));
        }
    }

    // 4. The active workspace, resolved once from those exact inputs.
    let snapshot = crate::workspace::resolve_workspace_snapshot(&effective, &cwd, context).await?;
    let workspace = snapshot.workspace;
    let (entry, workspace_alias) = select_entry(&workspace, &effective, vault)?;

    // A degenerate target built from an explicit `--vault` discards whatever
    // vault the context supplied, so the context did not contribute to the
    // recorded target and must not be pinned as if it had.
    let context_contributed = snapshot.context_contributed
        && !(workspace.source == WorkspaceSource::Degenerate && vault.is_some());

    // 5. The selected registry entry's kind and identity.
    let identity = selected_backend_identity(&effective, &entry.backend)?;

    // 6. Materialize only that backend and verify it read-only.
    verify_selected_target(&effective, &entry).await?;

    let (context_path, context_digest) = if context_contributed {
        let path = context
            .source_path()
            .map(canonical_path_for_manifest)
            .transpose()?;
        let path = path
            .as_deref()
            .map(|p| manifest_path_string("target.context_path", p))
            .transpose()?;
        (path, context.source_digest().map(str::to_string))
    } else {
        (None, None)
    };

    let project_path = project
        .as_ref()
        .map(|p| canonical_path_for_manifest(&p.path))
        .transpose()?;
    let project_path = project_path
        .as_deref()
        .map(|p| manifest_path_string("target.project_path", p))
        .transpose()?;

    let target = ManifestTarget {
        config_path: manifest_path_string("target.config_path", &config_path)?,
        config_digest,
        project_path,
        project_digest: project.as_ref().map(|p| p.bytes_digest.clone()),
        environment: project.as_ref().and_then(|p| p.environment.clone()),
        context_path,
        context_digest,
        workspace_source: workspace_source_label(workspace.source).to_string(),
        workspace_alias,
        backend_name: identity.name,
        backend_kind: identity.kind,
        backend_identity: identity.digest,
        vault: entry.vault.clone(),
    };

    Ok(ResolvedScheduleTarget {
        target,
        entry,
        workspace_source: workspace.source,
        working_directory: cwd,
    })
}

/// Apply `--vault` to the resolved workspace.
///
/// In a configured workspace `--vault` names an attached alias and nothing
/// else: the same text can mean an alias in one directory and a raw vault in
/// another, and an unattended job must not depend on which. In the degenerate
/// workspace-of-one there are no aliases, so `--vault` is a raw vault on the
/// effective backend and the manifest records a null alias.
fn select_entry(
    workspace: &Workspace,
    config: &Config,
    vault: Option<&str>,
) -> Result<(WorkspaceEntry, Option<String>)> {
    let configured = workspace.is_configured();
    let entry = match vault {
        Some(requested) if configured => workspace.entry(requested).cloned().ok_or_else(|| {
            let attached: Vec<&str> = workspace.entries.iter().map(|e| e.alias.as_str()).collect();
            CrosstacheError::invalid_argument(format!(
                "--vault '{requested}' is not attached to the active workspace; attached aliases: {}. \
                 A scheduled run must resolve its target exactly, so an unattached name is refused \
                 rather than retried as a raw vault name.",
                attached.join(", ")
            ))
        })?,
        Some(requested) => WorkspaceEntry {
            // Label it exactly as the workspace layer labels a degenerate
            // entry, so the alias never collides with a registry backend name.
            alias: crate::workspace::degenerate_alias_for(config, requested),
            backend: config.effective_backend_name().to_string(),
            vault: requested.to_string(),
            default: true,
        },
        None => workspace.default_entry()?.clone(),
    };
    let alias = configured.then(|| entry.alias.clone());
    Ok((entry, alias))
}

/// Construct only the selected backend and check it read-only.
///
/// Nothing here provisions: the probe registry opens an existing local store
/// rather than bootstrapping one, and the check itself is the same
/// `list_secrets` sweep `xv rotate --due` performs — so a target that
/// verifies here is a target the scheduled run can actually read. Listed
/// secret names are discarded; only the vault and backend names, which
/// already appear in ordinary CLI output, reach an error message.
async fn verify_selected_target(config: &Config, entry: &WorkspaceEntry) -> Result<()> {
    let mut probe = config.clone();
    probe.runtime_open_existing_local = true;

    let registry =
        crate::backend::BackendRegistry::with_lazy(&probe, std::slice::from_ref(&entry.backend))
            .map_err(|e| target_unavailable(entry, e))?;
    let backend = registry
        .materialize(&entry.backend)
        .map_err(|e| target_unavailable(entry, e))?;
    backend
        .health_check()
        .await
        .map_err(|e| target_unavailable(entry, e))?;
    backend
        .secrets()
        .list_secrets(&entry.vault, None)
        .await
        .map_err(|e| target_unavailable(entry, e))?;
    Ok(())
}

fn target_unavailable(
    entry: &WorkspaceEntry,
    error: crate::backend::error::BackendError,
) -> CrosstacheError {
    CrosstacheError::config(format!(
        "cannot verify the schedule target vault '{}' on backend '{}': {error}. \
         A schedule is only installed against a target this machine can already read — use the \
         vault once (for example 'xv list --vault {}') before scheduling rotation for it.",
        entry.vault, entry.backend, entry.vault
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::{AzureConfig, AzureCredentialType, LocalConfig};

    fn base_config() -> Config {
        Config::default()
    }

    // -----------------------------------------------------------------
    // Built-in backends
    // -----------------------------------------------------------------

    #[test]
    fn azure_builtin_digest_format_and_kind() {
        let mut config = base_config();
        config.azure = Some(AzureConfig {
            subscription_id: Some("sub-a".to_string()),
            tenant_id: Some("tenant-a".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });
        config.azure_credential_priority = AzureCredentialType::Cli;

        let identity = selected_backend_identity(&config, "azure").unwrap();
        assert_eq!(identity.name, "azure");
        assert_eq!(identity.kind, "azure");
        assert!(identity.digest.starts_with("sha256:"));
        assert_eq!(identity.digest.len(), "sha256:".len() + 64);
    }

    #[test]
    fn azure_builtin_digest_is_pinned_for_fixed_fixture() {
        let mut config = base_config();
        config.azure = Some(AzureConfig {
            subscription_id: Some("sub-fixed".to_string()),
            tenant_id: Some("tenant-fixed".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });
        config.azure_credential_priority = AzureCredentialType::Cli;

        let identity = selected_backend_identity(&config, "azure").unwrap();
        assert_eq!(
            identity.digest,
            "sha256:49433c9d763a99b6c78191f780d42c9412f9d3bae54a4e768b539174fdc2f8f7"
        );
    }

    #[test]
    fn azure_builtin_digest_changes_with_tenant() {
        let mut config_a = base_config();
        config_a.azure = Some(AzureConfig {
            subscription_id: Some("sub-x".to_string()),
            tenant_id: Some("tenant-a".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });

        let mut config_b = config_a.clone();
        config_b.azure = Some(AzureConfig {
            subscription_id: Some("sub-x".to_string()),
            tenant_id: Some("tenant-b".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });

        let identity_a = selected_backend_identity(&config_a, "azure").unwrap();
        let identity_b = selected_backend_identity(&config_b, "azure").unwrap();
        assert_ne!(identity_a.digest, identity_b.digest);
    }

    #[test]
    fn azure_builtin_digest_changes_with_subscription() {
        let mut config_a = base_config();
        config_a.azure = Some(AzureConfig {
            subscription_id: Some("sub-a".to_string()),
            tenant_id: Some("tenant-x".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });

        let mut config_b = config_a.clone();
        config_b.azure = Some(AzureConfig {
            subscription_id: Some("sub-b".to_string()),
            tenant_id: Some("tenant-x".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });

        let identity_a = selected_backend_identity(&config_a, "azure").unwrap();
        let identity_b = selected_backend_identity(&config_b, "azure").unwrap();
        assert_ne!(identity_a.digest, identity_b.digest);
    }

    #[test]
    fn azure_builtin_digest_changes_with_credential_priority() {
        let mut config_a = base_config();
        config_a.azure = Some(AzureConfig {
            subscription_id: Some("sub-x".to_string()),
            tenant_id: Some("tenant-x".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });
        config_a.azure_credential_priority = AzureCredentialType::Cli;

        let mut config_b = config_a.clone();
        config_b.azure_credential_priority = AzureCredentialType::ManagedIdentity;

        let identity_a = selected_backend_identity(&config_a, "azure").unwrap();
        let identity_b = selected_backend_identity(&config_b, "azure").unwrap();
        assert_ne!(identity_a.digest, identity_b.digest);
    }

    #[test]
    fn local_builtin_digest_changes_with_store_path() {
        let mut config_a = base_config();
        config_a.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/store-a".to_string()),
            key_file: None,
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let mut config_b = config_a.clone();
        config_b.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/store-b".to_string()),
            key_file: None,
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let identity_a = selected_backend_identity(&config_a, "local").unwrap();
        let identity_b = selected_backend_identity(&config_b, "local").unwrap();
        assert_ne!(identity_a.digest, identity_b.digest);
        assert_eq!(identity_a.kind, "local");
    }

    /// Unix-only: the digest covers the *normalized* store path, and
    /// `/tmp/...` is not absolute on Windows — it would be joined onto the
    /// test process's working directory and hash differently on every
    /// machine. The cross-platform guarantees (format, and that the digest
    /// changes with the store path) are covered by the tests around this one.
    #[cfg(unix)]
    #[test]
    fn local_builtin_digest_is_pinned_for_fixed_fixture() {
        let mut config = base_config();
        config.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/store-fixed".to_string()),
            key_file: None,
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let identity = selected_backend_identity(&config, "local").unwrap();
        assert_eq!(
            identity.digest,
            "sha256:42e179cdd7897f04bf449345624892db4cf4ffbe8f81a5a9c22527c98e187a81"
        );
    }

    #[test]
    fn local_builtin_key_file_does_not_affect_digest() {
        let mut config_a = base_config();
        config_a.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/store-same".to_string()),
            key_file: Some("/tmp/xv-schedule-target-test/key-a.txt".to_string()),
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let mut config_b = config_a.clone();
        config_b.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/store-same".to_string()),
            key_file: Some("/tmp/xv-schedule-target-test/key-b.txt".to_string()),
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let identity_a = selected_backend_identity(&config_a, "local").unwrap();
        let identity_b = selected_backend_identity(&config_b, "local").unwrap();
        assert_eq!(
            identity_a.digest, identity_b.digest,
            "key_file is credential-adjacent and must not participate in the identity digest"
        );
    }

    #[cfg(feature = "aws")]
    #[test]
    fn aws_builtin_digest_changes_with_region_profile_endpoint() {
        let mut config_a = base_config();
        config_a.aws = Some(AwsConfig {
            region: Some("us-east-1".to_string()),
            profile: Some("default".to_string()),
            endpoint_url: None,
            default_vault: Some("shared-vault".to_string()),
            s3_bucket: None,
        });

        let mut config_region = config_a.clone();
        config_region.aws = Some(AwsConfig {
            region: Some("us-west-2".to_string()),
            profile: Some("default".to_string()),
            endpoint_url: None,
            default_vault: Some("shared-vault".to_string()),
            s3_bucket: None,
        });

        let mut config_profile = config_a.clone();
        config_profile.aws = Some(AwsConfig {
            region: Some("us-east-1".to_string()),
            profile: Some("other".to_string()),
            endpoint_url: None,
            default_vault: Some("shared-vault".to_string()),
            s3_bucket: None,
        });

        let mut config_endpoint = config_a.clone();
        config_endpoint.aws = Some(AwsConfig {
            region: Some("us-east-1".to_string()),
            profile: Some("default".to_string()),
            endpoint_url: Some("http://localhost:4566".to_string()),
            default_vault: Some("shared-vault".to_string()),
            s3_bucket: None,
        });

        let base = selected_backend_identity(&config_a, "aws").unwrap();
        let region = selected_backend_identity(&config_region, "aws").unwrap();
        let profile = selected_backend_identity(&config_profile, "aws").unwrap();
        let endpoint = selected_backend_identity(&config_endpoint, "aws").unwrap();

        assert_ne!(base.digest, region.digest);
        assert_ne!(base.digest, profile.digest);
        assert_ne!(base.digest, endpoint.digest);
        assert_eq!(base.kind, "aws");
    }

    #[cfg(feature = "aws")]
    #[test]
    fn aws_builtin_digest_is_pinned_for_fixed_fixture() {
        let mut config = base_config();
        config.aws = Some(AwsConfig {
            region: Some("us-east-1".to_string()),
            profile: Some("default".to_string()),
            endpoint_url: None,
            default_vault: Some("shared-vault".to_string()),
            s3_bucket: None,
        });

        let identity = selected_backend_identity(&config, "aws").unwrap();
        assert_eq!(
            identity.digest,
            "sha256:ff832498c059975dba4363d28c975c7823209ada9c3a4557ccb47b164d957be1"
        );
    }

    #[cfg(not(feature = "aws"))]
    #[test]
    fn aws_builtin_without_feature_errors_naming_binary_support() {
        let mut config = base_config();
        config.aws = Some(AwsConfig {
            region: Some("us-east-1".to_string()),
            profile: Some("default".to_string()),
            endpoint_url: None,
            default_vault: Some("shared-vault".to_string()),
            s3_bucket: None,
        });

        let err = selected_backend_identity(&config, "aws").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("aws"), "message: {message}");
        assert!(
            message.to_lowercase().contains("feature")
                || message.to_lowercase().contains("compiled"),
            "message should explain the binary lacks AWS support: {message}"
        );
    }

    // -----------------------------------------------------------------
    // Named backends
    // -----------------------------------------------------------------

    #[test]
    fn named_local_digest_differs_from_builtin_local_same_store_path() {
        let mut config = base_config();
        config.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/shared-store".to_string()),
            key_file: None,
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });
        config.named_backends.insert(
            "local-b".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some("/tmp/xv-schedule-target-test/shared-store".to_string()),
                key_file: None,
                default_vault: Some("shared-vault".to_string()),
                encrypt_metadata: None,
                audit: None,
                git: None,
                opaque_filenames: None,
            }),
        );

        let builtin = selected_backend_identity(&config, "local").unwrap();
        let named = selected_backend_identity(&config, "local-b").unwrap();
        assert_ne!(
            builtin.digest, named.digest,
            "registry name participates in the identity, so same store path under a different registry name must differ"
        );
        assert_eq!(named.name, "local-b");
        assert_eq!(named.kind, "local");
    }

    #[test]
    fn named_local_digest_changes_with_store_path() {
        let mut config_a = base_config();
        config_a.named_backends.insert(
            "local-x".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some("/tmp/xv-schedule-target-test/named-a".to_string()),
                key_file: None,
                default_vault: Some("shared-vault".to_string()),
                encrypt_metadata: None,
                audit: None,
                git: None,
                opaque_filenames: None,
            }),
        );

        let mut config_b = base_config();
        config_b.named_backends.insert(
            "local-x".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some("/tmp/xv-schedule-target-test/named-b".to_string()),
                key_file: None,
                default_vault: Some("shared-vault".to_string()),
                encrypt_metadata: None,
                audit: None,
                git: None,
                opaque_filenames: None,
            }),
        );

        let identity_a = selected_backend_identity(&config_a, "local-x").unwrap();
        let identity_b = selected_backend_identity(&config_b, "local-x").unwrap();
        assert_ne!(identity_a.digest, identity_b.digest);
    }

    #[cfg(feature = "aws")]
    #[test]
    fn named_aws_digest_changes_with_profile() {
        let mut config_a = base_config();
        config_a.named_backends.insert(
            "aws-east".to_string(),
            NamedBackendEntry::Aws(AwsConfig {
                region: Some("us-east-1".to_string()),
                profile: Some("profile-a".to_string()),
                endpoint_url: None,
                default_vault: Some("shared-vault".to_string()),
                s3_bucket: None,
            }),
        );

        let mut config_b = base_config();
        config_b.named_backends.insert(
            "aws-east".to_string(),
            NamedBackendEntry::Aws(AwsConfig {
                region: Some("us-east-1".to_string()),
                profile: Some("profile-b".to_string()),
                endpoint_url: None,
                default_vault: Some("shared-vault".to_string()),
                s3_bucket: None,
            }),
        );

        let identity_a = selected_backend_identity(&config_a, "aws-east").unwrap();
        let identity_b = selected_backend_identity(&config_b, "aws-east").unwrap();
        assert_ne!(identity_a.digest, identity_b.digest);
        assert_eq!(identity_a.kind, "aws");
        assert_eq!(identity_a.name, "aws-east");
    }

    #[cfg(not(feature = "aws"))]
    #[test]
    fn named_aws_without_feature_errors_naming_binary_support() {
        let mut config = base_config();
        config.named_backends.insert(
            "aws-east".to_string(),
            NamedBackendEntry::Aws(AwsConfig {
                region: Some("us-east-1".to_string()),
                profile: Some("profile-a".to_string()),
                endpoint_url: None,
                default_vault: Some("shared-vault".to_string()),
                s3_bucket: None,
            }),
        );

        let err = selected_backend_identity(&config, "aws-east").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("aws-east"), "message: {message}");
        assert!(
            message.to_lowercase().contains("feature")
                || message.to_lowercase().contains("compiled"),
            "message should explain the binary lacks AWS support: {message}"
        );
    }

    // -----------------------------------------------------------------
    // Unrelated entries and unknown names
    // -----------------------------------------------------------------

    #[test]
    fn unrelated_named_backend_does_not_change_selected_digest() {
        let mut config = base_config();
        config.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/selected-store".to_string()),
            key_file: None,
            default_vault: Some("shared-vault".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let before = selected_backend_identity(&config, "local").unwrap();

        config.named_backends.insert(
            "unrelated-local".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some("/tmp/xv-schedule-target-test/unrelated-store".to_string()),
                key_file: None,
                default_vault: Some("other-vault".to_string()),
                encrypt_metadata: None,
                audit: None,
                git: None,
                opaque_filenames: None,
            }),
        );

        let after = selected_backend_identity(&config, "local").unwrap();
        assert_eq!(before.digest, after.digest);
    }

    #[test]
    fn digest_is_stable_across_repeated_calls() {
        let mut config = base_config();
        config.azure = Some(AzureConfig {
            subscription_id: Some("sub-stable".to_string()),
            tenant_id: Some("tenant-stable".to_string()),
            default_vault: Some("shared-vault".to_string()),
            resource_group: None,
            location: None,
        });

        let first = selected_backend_identity(&config, "azure").unwrap();
        let second = selected_backend_identity(&config, "azure").unwrap();
        assert_eq!(first.digest, second.digest);
    }

    #[test]
    fn unknown_registry_name_is_rejected_naming_it() {
        let config = base_config();
        let err = selected_backend_identity(&config, "totally-unknown").unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("totally-unknown"),
            "message should name the unknown registry name: {message}"
        );
    }

    // -----------------------------------------------------------------
    // Redaction regression
    // -----------------------------------------------------------------

    const CANARIES: &[&str] = &[
        "AKIAIOSFODNN7EXAMPLE",
        "aws-session-token-canary",
        "azure-client-secret-canary",
        "AGE-SECRET-KEY-1CANARY",
        "super-secret-value-canary",
    ];

    #[test]
    fn redaction_canaries_never_appear_in_serialized_identity_or_digest_input() {
        let mut config = base_config();
        config.azure = Some(AzureConfig {
            subscription_id: Some("sub-real".to_string()),
            tenant_id: Some("tenant-real".to_string()),
            default_vault: Some("azure-client-secret-canary".to_string()),
            resource_group: None,
            location: None,
        });
        config.azure_credential_priority = AzureCredentialType::Cli;
        config.local = Some(LocalConfig {
            store_path: Some("/tmp/xv-schedule-target-test/redaction-store".to_string()),
            key_file: Some(format!(
                "/tmp/xv-schedule-target-test/{}-key.txt",
                "AGE-SECRET-KEY-1CANARY"
            )),
            default_vault: Some("super-secret-value-canary".to_string()),
            encrypt_metadata: None,
            audit: None,
            git: None,
            opaque_filenames: None,
        });

        let azure_fields = AzureIdentityFields {
            registry_name: "azure",
            tenant_id: Some("tenant-real"),
            subscription_id: Some("sub-real"),
            credential_priority: "cli",
        };
        let azure_json = serde_json::to_string(&azure_fields).unwrap();
        let azure_identity = selected_backend_identity(&config, "azure").unwrap();

        let local_identity = selected_backend_identity(&config, "local").unwrap();
        let local_fields = LocalIdentityFields {
            registry_name: "local",
            store_path: "/tmp/xv-schedule-target-test/redaction-store",
        };
        let local_json = serde_json::to_string(&local_fields).unwrap();

        let manifest_snippet = format!(
            "{{\"backend_name\":\"local\",\"backend_kind\":\"local\",\"backend_identity\":\"{}\"}}",
            local_identity.digest
        );

        for canary in CANARIES {
            assert!(
                !azure_json.contains(canary),
                "canary '{canary}' leaked into azure identity JSON: {azure_json}"
            );
            assert!(
                !azure_identity.digest.contains(canary),
                "canary '{canary}' leaked into azure digest: {}",
                azure_identity.digest
            );
            assert!(
                !local_json.contains(canary),
                "canary '{canary}' leaked into local identity JSON: {local_json}"
            );
            assert!(
                !local_identity.digest.contains(canary),
                "canary '{canary}' leaked into local digest: {}",
                local_identity.digest
            );
            assert!(
                !manifest_snippet.contains(canary),
                "canary '{canary}' leaked into manifest snippet: {manifest_snippet}"
            );
        }
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crate::config::context::ContextManager;
    use crate::config::settings::{Config, LocalConfig, NamedBackendEntry};
    use crate::workspace::{WorkspaceEntryConfig, WorkspaceState};
    use std::collections::HashMap;

    /// A hermetic local backend config rooted under `root`, already
    /// initialized on disk: resolution verifies an existing store and must
    /// never create one.
    fn local_backend(root: &Path, name: &str) -> LocalConfig {
        let cfg = LocalConfig {
            store_path: Some(root.join(format!("{name}-store")).to_string_lossy().into()),
            key_file: Some(
                root.join(format!("{name}-key.txt"))
                    .to_string_lossy()
                    .into(),
            ),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
        };
        crate::backend::local::LocalBackend::new(Some(&cfg)).expect("initialize fixture store");
        cfg
    }

    /// Config with one built-in `local` backend plus two named local
    /// backends (`local-a`, `local-b`) over separate stores.
    fn config_with_two_named_locals(root: &Path) -> Config {
        let mut named_backends = HashMap::new();
        named_backends.insert(
            "local-a".to_string(),
            NamedBackendEntry::Local(local_backend(root, "a")),
        );
        named_backends.insert(
            "local-b".to_string(),
            NamedBackendEntry::Local(local_backend(root, "b")),
        );
        Config {
            backend: Some("local".to_string()),
            default_vault: "default".to_string(),
            local: Some(local_backend(root, "builtin")),
            named_backends,
            ..Default::default()
        }
    }

    fn entry_config(
        alias: &str,
        backend: &str,
        vault: &str,
        default: bool,
    ) -> WorkspaceEntryConfig {
        WorkspaceEntryConfig {
            vault: vault.to_string(),
            backend: Some(backend.to_string()),
            alias: Some(alias.to_string()),
            default,
        }
    }

    /// Write a real context file under `dir/.xv/context` and load it back
    /// through the same reader production uses, so `source_path`/
    /// `source_digest` describe a file that actually exists.
    async fn context_with_workspace(
        dir: &Path,
        entries: Vec<WorkspaceEntryConfig>,
    ) -> ContextManager {
        let context_dir = dir.join(".xv");
        std::fs::create_dir_all(&context_dir).unwrap();
        let manager = ContextManager {
            workspace: Some(WorkspaceState { entries }),
            ..Default::default()
        };
        let path = context_dir.join("context");
        std::fs::write(&path, serde_json::to_vec(&manager).unwrap()).unwrap();
        ContextManager::load_at(&path).await.unwrap()
    }

    /// Write a real context file carrying only a current vault (no workspace)
    /// and load it back through the production reader.
    async fn context_with_current_vault(dir: &Path, vault: &str) -> ContextManager {
        let context_dir = dir.join(".xv");
        std::fs::create_dir_all(&context_dir).unwrap();
        let manager = ContextManager {
            current: Some(crate::config::context::VaultContext::new(
                vault.to_string(),
                None,
                None,
            )),
            ..Default::default()
        };
        let path = context_dir.join("context");
        std::fs::write(&path, serde_json::to_vec(&manager).unwrap()).unwrap();
        ContextManager::load_at(&path).await.unwrap()
    }

    fn config_bytes() -> &'static [u8] {
        b"backend = \"local\"\n"
    }

    fn write_config(root: &Path) -> PathBuf {
        let path = root.join("xv.conf");
        std::fs::write(&path, config_bytes()).unwrap();
        path
    }

    #[tokio::test]
    async fn explicit_alias_selects_that_attached_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = context_with_workspace(
            root,
            vec![
                entry_config("work", "local-a", "work-vault", true),
                entry_config("stage", "local-b", "stage-vault", false),
            ],
        )
        .await;

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("stage"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "stage-vault");
        assert_eq!(resolved.target.workspace_alias.as_deref(), Some("stage"));
        assert_eq!(resolved.target.workspace_source, "context");
        assert_eq!(resolved.target.backend_name, "local-b");
        assert_eq!(resolved.target.backend_kind, "local");
        assert_eq!(resolved.entry.backend, "local-b");
        // Context participated, so its exact file is pinned.
        assert!(resolved.target.context_path.is_some());
        assert!(resolved
            .target
            .context_digest
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        // No `.xv.toml` governs a bare temp dir.
        assert_eq!(resolved.target.project_path, None);
        assert_eq!(resolved.target.project_digest, None);
        assert_eq!(resolved.target.environment, None);
        assert!(resolved.target.config_digest.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn no_vault_selects_the_workspace_default_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = context_with_workspace(
            root,
            vec![
                entry_config("work", "local-a", "work-vault", true),
                entry_config("stage", "local-b", "stage-vault", false),
            ],
        )
        .await;

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "work-vault");
        assert_eq!(resolved.target.workspace_alias.as_deref(), Some("work"));
        assert_eq!(resolved.target.backend_name, "local-a");
    }

    #[tokio::test]
    async fn same_vault_name_on_two_named_backends_resolves_to_distinct_identities() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = context_with_workspace(
            root,
            vec![
                entry_config("work", "local-a", "shared", true),
                entry_config("stage", "local-b", "shared", false),
            ],
        )
        .await;

        let a = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("work"),
            None,
            None,
        )
        .await
        .unwrap();
        let b = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("stage"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(a.target.vault, b.target.vault);
        assert_ne!(a.target.backend_name, b.target.backend_name);
        assert_ne!(
            a.target.backend_identity, b.target.backend_identity,
            "same vault name on two stores must not share an identity"
        );
    }

    #[tokio::test]
    async fn unknown_alias_is_rejected_with_the_attached_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = context_with_workspace(
            root,
            vec![
                entry_config("work", "local-a", "work-vault", true),
                entry_config("stage", "local-b", "stage-vault", false),
            ],
        )
        .await;

        let error = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("work-vault"),
            None,
            None,
        )
        .await
        .expect_err("a raw vault name must not resolve inside a configured workspace");

        let rendered = error.to_string();
        assert!(rendered.contains("work-vault"), "{rendered}");
        assert!(rendered.contains("work"), "{rendered}");
        assert!(rendered.contains("stage"), "{rendered}");
    }

    #[tokio::test]
    async fn degenerate_workspace_takes_an_explicit_vault_as_a_raw_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = ContextManager::default();

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("raw-vault"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "raw-vault");
        assert_eq!(resolved.target.workspace_alias, None);
        assert_eq!(resolved.target.workspace_source, "degenerate");
        assert_eq!(resolved.target.backend_name, "local");
        // Nothing was read from a context file, so nothing is pinned to one.
        assert_eq!(resolved.target.context_path, None);
        assert_eq!(resolved.target.context_digest, None);
    }

    #[tokio::test]
    async fn degenerate_workspace_without_a_vault_uses_the_configured_default() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = ContextManager::default();

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "default");
        assert_eq!(resolved.target.workspace_source, "degenerate");
        assert_eq!(resolved.target.workspace_alias, None);
    }

    #[tokio::test]
    async fn missing_working_directory_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = ContextManager::default();
        let missing = root.join("no-such-directory");

        let error = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            &missing,
            &context,
            None,
            None,
            None,
        )
        .await
        .expect_err("a working directory that does not exist cannot be pinned");

        assert!(error.to_string().contains("no-such-directory"), "{error}");
    }

    #[tokio::test]
    async fn project_environment_is_resolved_and_recorded() {
        // `resolve_env` lets `XV_ENV` beat `--env`, so this test must not run
        // beside one that sets it.
        let _env = crate::config::project::test_support::XvEnvGuard::acquire();
        std::env::remove_var("XV_ENV");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = ContextManager::default();

        std::fs::write(
            root.join(".xv.toml"),
            r#"default_env = "prod"

[env.prod]
vaults = [
  { vault = "prod-vault", backend = "local-a", alias = "prod", default = true },
]
"#,
        )
        .unwrap();

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            None,
            Some("prod"),
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.workspace_source, "project");
        assert_eq!(resolved.target.environment.as_deref(), Some("prod"));
        assert_eq!(resolved.target.vault, "prod-vault");
        assert_eq!(resolved.target.workspace_alias.as_deref(), Some("prod"));
        assert_eq!(resolved.target.backend_name, "local-a");
        assert!(resolved
            .target
            .project_path
            .as_deref()
            .unwrap()
            .ends_with(".xv.toml"));
        assert!(resolved
            .target
            .project_digest
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        // The project overlay replaced context, so no context file is pinned.
        assert_eq!(resolved.target.context_path, None);
    }

    #[tokio::test]
    async fn paths_containing_spaces_are_recorded_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("work dir with spaces");
        std::fs::create_dir_all(&root).unwrap();
        let config = config_with_two_named_locals(&root);
        let config_path = write_config(&root);
        let context = context_with_workspace(
            &root,
            vec![entry_config("work", "local-a", "work-vault", true)],
        )
        .await;

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            &root,
            &context,
            Some("work"),
            None,
            None,
        )
        .await
        .unwrap();

        assert!(
            resolved.target.config_path.contains("work dir with spaces"),
            "{}",
            resolved.target.config_path
        );
        assert!(resolved
            .target
            .context_path
            .as_deref()
            .unwrap()
            .contains("work dir with spaces"));
        assert!(resolved
            .working_directory
            .to_string_lossy()
            .contains("work dir with spaces"));
    }

    #[tokio::test]
    async fn degenerate_workspace_with_an_explicit_vault_pins_no_context_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut config = config_with_two_named_locals(root);
        // Nothing else supplies a vault, so the degenerate chain would take
        // the context's current vault — which an explicit `--vault` discards.
        config.default_vault = String::new();
        let config_path = write_config(root);
        let context = context_with_current_vault(root, "context-vault").await;
        assert!(context.source_path().is_some(), "fixture must read a file");

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("explicit-vault"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "explicit-vault");
        assert_eq!(resolved.target.workspace_source, "degenerate");
        // The context supplied nothing that survived into the target, so
        // pinning its digest would make the runner refuse on an edit that
        // cannot affect this schedule.
        assert_eq!(resolved.target.context_path, None);
        assert_eq!(resolved.target.context_digest, None);
    }

    #[tokio::test]
    async fn degenerate_workspace_without_a_vault_still_pins_the_context_it_used() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut config = config_with_two_named_locals(root);
        config.default_vault = String::new();
        let config_path = write_config(root);
        let context = context_with_current_vault(root, "context-vault").await;

        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "context-vault");
        assert!(resolved.target.context_path.is_some());
        assert!(resolved.target.context_digest.is_some());
    }

    #[tokio::test]
    async fn a_degenerate_alias_never_collides_with_a_registry_backend_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = ContextManager::default();

        // A raw vault literally named after a registry backend: the workspace
        // layer would never alias an entry that way.
        let resolved = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("local-a"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resolved.target.vault, "local-a");
        assert_eq!(resolved.target.workspace_alias, None);
        assert_ne!(resolved.entry.alias, "local-a");
        assert_eq!(
            resolved.entry.alias,
            crate::workspace::degenerate_alias_for(&config, "local-a")
        );
    }

    #[tokio::test]
    async fn an_ambient_backend_that_differs_from_the_saved_one_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let config = config_with_two_named_locals(root);
        let config_path = write_config(root);
        let context = ContextManager::default();

        let error = resolve_install_target_from(
            &config,
            &config_path,
            config_bytes(),
            root,
            &context,
            Some("raw-vault"),
            None,
            Some("azure"),
        )
        .await
        .expect_err("an unsaved ambient backend cannot be replayed by a scheduled run");

        let rendered = error.to_string();
        assert!(rendered.contains("azure"), "{rendered}");
        assert!(rendered.contains("local"), "{rendered}");
        assert!(rendered.contains("XV_BACKEND"), "{rendered}");
    }

    // -----------------------------------------------------------------
    // Canonical path shaping
    // -----------------------------------------------------------------

    use crate::utils::helpers::strip_verbatim_prefix;

    #[test]
    fn strip_verbatim_prefix_leaves_a_plain_path_unchanged() {
        let plain = if cfg!(windows) {
            PathBuf::from(r"C:\Users\alice\xv.conf")
        } else {
            PathBuf::from("/home/alice/.config/xv/xv.conf")
        };
        assert_eq!(strip_verbatim_prefix(plain.clone()), plain);
    }

    #[cfg(windows)]
    #[test]
    fn strip_verbatim_prefix_removes_windows_verbatim_prefixes() {
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\C:\Users\alice\xv.conf")),
            PathBuf::from(r"C:\Users\alice\xv.conf")
        );
        assert_eq!(
            strip_verbatim_prefix(PathBuf::from(r"\\?\UNC\server\share\xv.conf")),
            PathBuf::from(r"\\server\share\xv.conf")
        );
    }

    /// Whatever the platform, a path the manifest records must never carry a
    /// verbatim prefix — that is the spelling drift comparison uses.
    #[test]
    fn canonical_path_for_manifest_never_returns_a_verbatim_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("xv.conf");
        std::fs::write(&file, b"").unwrap();

        let recorded = canonical_path_for_manifest(&file).unwrap();
        assert!(
            !recorded.to_string_lossy().starts_with(r"\\?\"),
            "canonical_path_for_manifest leaked a verbatim prefix: {}",
            recorded.display()
        );
        crate::schedule::manifest::validate_absolute_normalized_path(
            "target.config_path",
            recorded.to_str().unwrap(),
        )
        .unwrap();
    }
}
