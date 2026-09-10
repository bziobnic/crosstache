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
//! This is P1 Task 2 of the scheduled-target-manifest work: the identity
//! digest only. `resolve_install_target`, the rest of canonical target
//! resolution, is a later task in the same module — kept separate so this
//! file stays focused.
#![allow(dead_code)]

use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::backend::local::config::ResolvedLocalConfig;
use crate::backend::BackendKind;
use crate::config::settings::{AwsConfig, Config, NamedBackendEntry};
use crate::error::{CrosstacheError, Result};

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

fn aws_not_compiled_error(registry_name: &str) -> CrosstacheError {
    CrosstacheError::config(format!(
        "schedule backend '{registry_name}' is aws, but this binary lacks AWS support (rebuild with --features aws)"
    ))
}

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
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| {
                CrosstacheError::config(format!(
                    "cannot resolve local store path '{}' from the current directory: {e}",
                    path.display()
                ))
            })?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
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
