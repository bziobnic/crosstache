//! Shared non-interactive configuration setup models and persistence.

use crate::config::settings::{AwsConfig, Config, LocalConfig};
use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Non-interactive values accepted by the shared setup service.
///
/// Provider credentials are intentionally absent. Azure and AWS setup use
/// their standard credential chains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "backend", rename_all = "lowercase", deny_unknown_fields)]
pub enum SetupRequest {
    Local {
        store_path: PathBuf,
        key_file: PathBuf,
        vault: String,
    },
    Azure {
        subscription_id: String,
        tenant_id: String,
        vault: String,
        resource_group: String,
        location: String,
    },
    Aws {
        region: String,
        profile: Option<String>,
        vault_prefix: String,
    },
}

/// Display-safe setup scope returned before a candidate is applied.
// Consumed by the desktop verification/orchestration tasks that follow this
// shared-model task. The binary currently compiles this module independently.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupPreview {
    pub backend: String,
    pub vault: String,
}

/// Display-safe evidence that a candidate backend operation succeeded.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupVerification {
    pub operation: String,
    pub backend: String,
    pub vault: String,
}

/// Display-safe result returned after setup and verification.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetupOutcome {
    pub preview: SetupPreview,
    pub verification: SetupVerification,
}

fn required(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(CrosstacheError::config(format!("{field} is required")));
    }
    Ok(())
}

fn persisted_path(path: &Path, field: &str) -> Result<String> {
    if path.as_os_str().is_empty() {
        return Err(CrosstacheError::config(format!("{field} is required")));
    }
    let value = path.to_str().ok_or_else(|| {
        CrosstacheError::config(format!(
            "{field} must be valid Unicode for TOML persistence"
        ))
    })?;
    required(value, field)?;
    Ok(value.to_string())
}

/// Build a validated candidate configuration without filesystem or backend
/// side effects.
pub fn build_setup_config(request: &SetupRequest, mut base: Config) -> Result<Config> {
    match request {
        SetupRequest::Local {
            store_path,
            key_file,
            vault,
        } => {
            let store_path = persisted_path(store_path, "Local store path")?;
            let key_file = persisted_path(key_file, "Local key file")?;
            required(vault, "Local vault")?;

            base.backend = Some("local".into());
            base.local = Some(LocalConfig {
                store_path: Some(store_path),
                key_file: Some(key_file),
                default_vault: Some(vault.clone()),
                encrypt_metadata: None,
                opaque_filenames: None,
            });
            base.aws = None;
            base.subscription_id.clear();
            base.tenant_id.clear();
            base.default_vault = vault.clone();
            base.default_resource_group.clear();
            base.default_location.clear();
            base.blob_config = None;
        }
        SetupRequest::Azure {
            subscription_id,
            tenant_id,
            vault,
            resource_group,
            location,
        } => {
            required(subscription_id, "Azure subscription ID")?;
            required(tenant_id, "Azure tenant ID")?;
            required(vault, "Azure vault")?;
            required(resource_group, "Azure resource group")?;
            required(location, "Azure location")?;

            // Preserve the CLI initializer's legacy representation: no
            // explicit backend means Azure.
            base.backend = None;
            base.subscription_id = subscription_id.clone();
            base.tenant_id = tenant_id.clone();
            base.default_vault = vault.clone();
            base.default_resource_group = resource_group.clone();
            base.default_location = location.clone();
            base.local = None;
            base.aws = None;
        }
        SetupRequest::Aws {
            region,
            profile,
            vault_prefix,
        } => {
            required(region, "AWS region")?;
            required(vault_prefix, "AWS vault prefix")?;
            if let Some(profile) = profile {
                required(profile, "AWS profile")?;
            }

            base.backend = Some("aws".into());
            base.aws = Some(AwsConfig {
                region: Some(region.clone()),
                profile: profile.clone(),
                endpoint_url: None,
                default_vault: Some(vault_prefix.clone()),
                s3_bucket: None,
            });
            base.local = None;
            base.subscription_id.clear();
            base.tenant_id.clear();
            base.default_vault = vault_prefix.clone();
            base.default_resource_group.clear();
            base.default_location.clear();
            base.blob_config = None;
        }
    }

    base.validate()?;
    Ok(base)
}

/// Serialize, parse, validate, and atomically persist a setup candidate.
///
/// No directory or temporary file is created until every in-memory check has
/// passed. The hardened writer owns sibling-temp creation, private modes,
/// syncing, cross-platform replacement, and failure cleanup.
pub async fn atomic_save_config(config: &Config, path: &Path) -> Result<()> {
    let contents = toml::to_string_pretty(config)
        .map_err(|error| CrosstacheError::serialization(error.to_string()))?;
    let parsed: Config = toml::from_str(&contents).map_err(|error| {
        CrosstacheError::serialization(format!(
            "Serialized setup configuration did not parse: {error}"
        ))
    })?;
    parsed.validate()?;
    crate::utils::helpers::atomic_write_file_no_follow_async(path, contents.as_bytes(), true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::{AzureCredentialType, Config};
    use std::path::PathBuf;

    fn local_request(root: &std::path::Path) -> SetupRequest {
        SetupRequest::Local {
            store_path: root.join("store"),
            key_file: root.join("key.txt"),
            vault: "default".into(),
        }
    }

    #[test]
    fn local_request_builds_the_same_config_shape_as_cli_init() {
        let request = SetupRequest::Local {
            store_path: "/tmp/store".into(),
            key_file: "/tmp/key.txt".into(),
            vault: "default".into(),
        };
        let config = build_setup_config(&request, Config::default()).unwrap();

        assert_eq!(config.backend.as_deref(), Some("local"));
        assert_eq!(
            config.local.as_ref().unwrap().store_path.as_deref(),
            Some("/tmp/store")
        );
        assert_eq!(
            config.local.as_ref().unwrap().key_file.as_deref(),
            Some("/tmp/key.txt")
        );
        assert_eq!(
            config.local.as_ref().unwrap().default_vault.as_deref(),
            Some("default")
        );
        assert_eq!(config.default_vault, "default");
        assert!(config.aws.is_none());
        assert!(config.subscription_id.is_empty());
        assert!(config.tenant_id.is_empty());
        assert_eq!(config.clipboard_timeout, 30);
    }

    #[test]
    fn azure_request_builds_the_same_config_shape_as_cli_init() {
        let request = SetupRequest::Azure {
            subscription_id: "subscription".into(),
            tenant_id: "tenant".into(),
            vault: "vault".into(),
            resource_group: "group".into(),
            location: "eastus".into(),
        };
        let config = build_setup_config(&request, Config::default()).unwrap();

        assert_eq!(config.backend, None);
        assert_eq!(config.subscription_id, "subscription");
        assert_eq!(config.tenant_id, "tenant");
        assert_eq!(config.default_vault, "vault");
        assert_eq!(config.default_resource_group, "group");
        assert_eq!(config.default_location, "eastus");
        assert_eq!(
            config.azure_credential_priority,
            AzureCredentialType::Default
        );
        assert!(config.local.is_none());
        assert!(config.aws.is_none());
    }

    #[test]
    fn aws_request_builds_the_same_config_shape_as_cli_init() {
        let request = SetupRequest::Aws {
            region: "us-east-1".into(),
            profile: Some("work".into()),
            vault_prefix: "team".into(),
        };
        let config = build_setup_config(&request, Config::default()).unwrap();

        assert_eq!(config.backend.as_deref(), Some("aws"));
        assert_eq!(config.default_vault, "team");
        let aws = config.aws.as_ref().unwrap();
        assert_eq!(aws.region.as_deref(), Some("us-east-1"));
        assert_eq!(aws.profile.as_deref(), Some("work"));
        assert_eq!(aws.default_vault.as_deref(), Some("team"));
        assert!(aws.endpoint_url.is_none());
        assert!(aws.s3_bucket.is_none());
        assert!(config.local.is_none());
    }

    #[test]
    fn request_models_are_tagged_and_never_accept_provider_secrets() {
        let serialized = serde_json::to_value(SetupRequest::Aws {
            region: "us-east-1".into(),
            profile: None,
            vault_prefix: "team".into(),
        })
        .unwrap();
        assert_eq!(serialized["backend"], "aws");
        let object = serialized.as_object().unwrap();
        for forbidden in [
            "access_key",
            "secret_key",
            "client_secret",
            "password",
            "token",
        ] {
            assert!(!object.contains_key(forbidden));
        }

        for forbidden_request in [
            serde_json::json!({
                "backend": "aws",
                "region": "us-east-1",
                "profile": null,
                "vault_prefix": "team",
                "access_key": "secret"
            }),
            serde_json::json!({
                "backend": "azure",
                "subscription_id": "subscription",
                "tenant_id": "tenant",
                "vault": "vault",
                "resource_group": "group",
                "location": "eastus",
                "client_secret": "secret"
            }),
        ] {
            assert!(serde_json::from_value::<SetupRequest>(forbidden_request).is_err());
        }
    }

    #[test]
    fn setup_models_are_serializable_safe_summaries() {
        let preview = SetupPreview {
            backend: "local".into(),
            vault: "default".into(),
        };
        let verification = SetupVerification {
            operation: "list-secrets".into(),
            backend: "local".into(),
            vault: "default".into(),
        };
        let outcome = SetupOutcome {
            preview: preview.clone(),
            verification: verification.clone(),
        };

        assert_eq!(
            serde_json::to_value(&outcome).unwrap()["preview"]["backend"],
            "local"
        );
        assert_eq!(outcome.preview, preview);
        assert_eq!(outcome.verification, verification);
    }

    #[test]
    fn invalid_requests_fail_before_mutating_the_base() {
        let base = Config {
            debug: true,
            ..Config::default()
        };
        let invalid = [
            SetupRequest::Local {
                store_path: PathBuf::new(),
                key_file: "/tmp/key".into(),
                vault: "default".into(),
            },
            SetupRequest::Local {
                store_path: "/tmp/store".into(),
                key_file: PathBuf::new(),
                vault: "default".into(),
            },
            SetupRequest::Local {
                store_path: "/tmp/store".into(),
                key_file: "/tmp/key".into(),
                vault: " ".into(),
            },
            SetupRequest::Azure {
                subscription_id: " ".into(),
                tenant_id: "tenant".into(),
                vault: "vault".into(),
                resource_group: "group".into(),
                location: "eastus".into(),
            },
            SetupRequest::Azure {
                subscription_id: "subscription".into(),
                tenant_id: String::new(),
                vault: "vault".into(),
                resource_group: "group".into(),
                location: "eastus".into(),
            },
            SetupRequest::Azure {
                subscription_id: "subscription".into(),
                tenant_id: "tenant".into(),
                vault: String::new(),
                resource_group: "group".into(),
                location: "eastus".into(),
            },
            SetupRequest::Azure {
                subscription_id: "subscription".into(),
                tenant_id: "tenant".into(),
                vault: "vault".into(),
                resource_group: String::new(),
                location: "eastus".into(),
            },
            SetupRequest::Azure {
                subscription_id: "subscription".into(),
                tenant_id: "tenant".into(),
                vault: "vault".into(),
                resource_group: "group".into(),
                location: String::new(),
            },
            SetupRequest::Aws {
                region: String::new(),
                profile: None,
                vault_prefix: "team".into(),
            },
            SetupRequest::Aws {
                region: "us-east-1".into(),
                profile: None,
                vault_prefix: " ".into(),
            },
            SetupRequest::Aws {
                region: "us-east-1".into(),
                profile: Some(String::new()),
                vault_prefix: "team".into(),
            },
        ];

        for request in invalid {
            assert!(build_setup_config(&request, base.clone()).is_err());
            assert!(base.debug);
            assert!(base.backend.is_none());
        }
    }

    #[tokio::test]
    async fn failed_atomic_save_preserves_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xv.conf");
        let missing_parent = dir.path().join("not-created");
        let missing_path = missing_parent.join("xv.conf");
        tokio::fs::write(&path, b"backend = \"local\"\n")
            .await
            .unwrap();

        assert!(atomic_save_config(&Config::default(), &path).await.is_err());
        assert!(atomic_save_config(&Config::default(), &missing_path)
            .await
            .is_err());
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            b"backend = \"local\"\n"
        );
        assert!(!missing_parent.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn atomic_save_round_trips_valid_config_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private").join("xv.conf");
        let config = build_setup_config(&local_request(dir.path()), Config::default()).unwrap();

        atomic_save_config(&config, &path).await.unwrap();
        let bytes = tokio::fs::read(&path).await.unwrap();
        let parsed: Config = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.backend.as_deref(), Some("local"));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .count(),
            1
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn atomic_save_refuses_a_symlink_and_preserves_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.conf");
        let path = dir.path().join("xv.conf");
        tokio::fs::write(&target, b"original").await.unwrap();
        symlink(&target, &path).unwrap();
        let config = build_setup_config(&local_request(dir.path()), Config::default()).unwrap();

        assert!(atomic_save_config(&config, &path).await.is_err());
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"original");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
