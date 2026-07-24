//! Shared non-interactive configuration setup models and persistence.

use crate::backend::{Backend, BackendKind, BackendRegistry};
use crate::config::settings::{AwsConfig, Config, LocalConfig};
use crate::error::{CrosstacheError, Result};
use async_trait::async_trait;
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

/// Backend verification boundary used by setup orchestration and isolated
/// desktop tests.
#[allow(dead_code)] // Consumed by the desktop setup adapter in Task 3.
#[async_trait]
pub trait SetupVerifier: Send + Sync {
    async fn verify(&self, config: &Config) -> Result<SetupVerification>;
}

#[allow(dead_code)] // Consumed through setup_and_save by Task 3.
struct DefaultSetupVerifier;

#[allow(dead_code)] // Consumed through setup_and_save by Task 3.
#[async_trait]
impl SetupVerifier for DefaultSetupVerifier {
    async fn verify(&self, config: &Config) -> Result<SetupVerification> {
        let backend_name = config.effective_backend_name();
        let kind: BackendKind = backend_name.parse().map_err(CrosstacheError::config)?;
        let vault = config.default_vault.as_str();

        match kind {
            BackendKind::Local => {
                // Construction intentionally initializes the configured age
                // identity and vault directories before the list operation.
                let backend = crate::backend::local::LocalBackend::new(config.local.as_ref())?;
                backend.health_check().await?;
                backend.secrets().list_secrets(vault, None).await?;
            }
            BackendKind::Azure | BackendKind::Aws => {
                let registry = BackendRegistry::from_config(config)?;
                registry.verify_active_vault(vault).await?;
            }
        }

        Ok(SetupVerification {
            operation: "list-secrets".into(),
            backend: backend_name.into(),
            vault: vault.into(),
        })
    }
}

/// Verify a validated setup candidate through the standard provider chain.
#[allow(dead_code)] // Consumed by the desktop setup adapter in Task 3.
pub async fn verify_setup(config: &Config) -> Result<SetupVerification> {
    config.validate()?;
    DefaultSetupVerifier.verify(config).await
}

fn setup_preview(config: &Config) -> SetupPreview {
    SetupPreview {
        backend: config.effective_backend_name().into(),
        vault: config.default_vault.clone(),
    }
}

async fn build_and_verify_candidate(
    request: SetupRequest,
    base: Config,
    verifier: &dyn SetupVerifier,
) -> Result<(Config, SetupOutcome)> {
    let candidate = build_setup_config(&request, base)?;
    let preview = setup_preview(&candidate);
    let reported = verifier.verify(&candidate).await?;
    if reported.operation != "list-secrets"
        || reported.backend != preview.backend
        || reported.vault != preview.vault
    {
        return Err(CrosstacheError::config(
            "Setup verifier returned an inconsistent verification summary",
        ));
    }
    // Reconstruct from trusted candidate scope after consistency validation;
    // a custom verifier cannot inject provider diagnostics into the outcome.
    let verification = SetupVerification {
        operation: "list-secrets".into(),
        backend: preview.backend.clone(),
        vault: preview.vault.clone(),
    };
    Ok((
        candidate,
        SetupOutcome {
            preview,
            verification,
        },
    ))
}

/// Build and verify a setup candidate without persisting it.
///
/// Desktop preview/tests use this exact three-argument boundary. Production
/// setup must use [`setup_and_save`] so verification cannot be reordered
/// after replacement.
#[allow(dead_code)] // Consumed by the desktop setup adapter in Task 3.
pub async fn setup_with_verifier(
    request: SetupRequest,
    base: Config,
    verifier: &dyn SetupVerifier,
) -> Result<SetupOutcome> {
    let (_, outcome) = build_and_verify_candidate(request, base, verifier).await?;
    Ok(outcome)
}

/// Build, verify, then atomically persist a setup candidate.
///
/// No configuration replacement is attempted unless candidate verification
/// succeeds. Build or verification failures therefore preserve prior bytes.
#[allow(dead_code)] // Consumed by the desktop setup adapter in Task 3.
pub async fn setup_and_save_with_verifier(
    request: SetupRequest,
    base: Config,
    path: &Path,
    verifier: &dyn SetupVerifier,
) -> Result<SetupOutcome> {
    let (candidate, outcome) = build_and_verify_candidate(request, base, verifier).await?;
    atomic_save_config(&candidate, path).await?;
    Ok(outcome)
}

/// Production setup orchestration for Task 3's desktop adapter.
#[allow(dead_code)] // Consumed by the desktop setup adapter in Task 3.
pub async fn setup_and_save(
    request: SetupRequest,
    base: Config,
    path: &Path,
) -> Result<SetupOutcome> {
    setup_and_save_with_verifier(request, base, path, &DefaultSetupVerifier).await
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

            let local = LocalConfig {
                store_path: Some(store_path),
                key_file: Some(key_file),
                default_vault: Some(vault.clone()),
                encrypt_metadata: None,
                opaque_filenames: None,
            };
            crate::backend::local::config::ResolvedLocalConfig::from_raw(Some(&local))
                .validate()?;

            base.backend = Some("local".into());
            base.local = Some(local);
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
    use crate::error::SafeSetupError;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn local_request(root: &std::path::Path) -> SetupRequest {
        SetupRequest::Local {
            store_path: root.join("store"),
            key_file: root.join("key.txt"),
            vault: "default".into(),
        }
    }

    struct InspectingVerifier {
        expected_backend: &'static str,
        expected_vault: &'static str,
        seen: AtomicBool,
        fail: bool,
    }

    struct UnsafeSummaryVerifier;

    #[async_trait]
    impl SetupVerifier for UnsafeSummaryVerifier {
        async fn verify(&self, _config: &Config) -> Result<SetupVerification> {
            Ok(SetupVerification {
                operation: "Bearer verifier-token".into(),
                backend: "client_secret=verifier-secret".into(),
                vault: "/Users/alice/private-vault".into(),
            })
        }
    }

    impl InspectingVerifier {
        fn success(expected_backend: &'static str, expected_vault: &'static str) -> Self {
            Self {
                expected_backend,
                expected_vault,
                seen: AtomicBool::new(false),
                fail: false,
            }
        }

        fn failure(expected_backend: &'static str, expected_vault: &'static str) -> Self {
            Self {
                expected_backend,
                expected_vault,
                seen: AtomicBool::new(false),
                fail: true,
            }
        }
    }

    #[async_trait]
    impl SetupVerifier for InspectingVerifier {
        async fn verify(&self, config: &Config) -> Result<SetupVerification> {
            assert_eq!(config.effective_backend_name(), self.expected_backend);
            assert_eq!(config.default_vault, self.expected_vault);
            self.seen.store(true, Ordering::SeqCst);
            if self.fail {
                return Err(CrosstacheError::authentication(
                    "Authorization: Bearer verification-token",
                ));
            }
            Ok(SetupVerification {
                operation: "list-secrets".into(),
                backend: self.expected_backend.into(),
                vault: self.expected_vault.into(),
            })
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

    #[tokio::test]
    async fn verify_local_setup_uses_the_candidate_config() {
        let root = tempfile::tempdir().unwrap();
        let verifier = InspectingVerifier::success("local", "default");

        let outcome = setup_with_verifier(local_request(root.path()), Config::default(), &verifier)
            .await
            .unwrap();

        assert!(verifier.seen.load(Ordering::SeqCst));
        assert_eq!(outcome.verification.operation, "list-secrets");
        assert_eq!(outcome.verification.backend, "local");
        assert_eq!(outcome.preview.vault, "default");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn verify_setup_initializes_local_backend_and_lists_requested_vault() {
        let root = tempfile::tempdir().unwrap();
        let config = build_setup_config(&local_request(root.path()), Config::default()).unwrap();

        let verification = verify_setup(&config).await.unwrap();

        assert_eq!(verification.operation, "list-secrets");
        assert_eq!(verification.backend, "local");
        assert_eq!(verification.vault, "default");
        assert!(root
            .path()
            .join("store/vaults/default/.vault.json")
            .is_file());
        assert!(root.path().join("key.txt").is_file());
    }

    #[tokio::test]
    async fn verify_azure_and_aws_setup_use_provider_candidates() {
        let cases = [
            (
                SetupRequest::Azure {
                    subscription_id: "subscription".into(),
                    tenant_id: "tenant".into(),
                    vault: "vault".into(),
                    resource_group: "group".into(),
                    location: "eastus".into(),
                },
                InspectingVerifier::success("azure", "vault"),
            ),
            (
                SetupRequest::Aws {
                    region: "us-east-1".into(),
                    profile: Some("work".into()),
                    vault_prefix: "team".into(),
                },
                InspectingVerifier::success("aws", "team"),
            ),
        ];

        for (request, verifier) in cases {
            let outcome = setup_with_verifier(request, Config::default(), &verifier)
                .await
                .unwrap();
            assert!(verifier.seen.load(Ordering::SeqCst));
            assert_eq!(outcome.verification.operation, "list-secrets");
        }
    }

    #[tokio::test]
    async fn verifier_cannot_inject_unsafe_summary_fields() {
        let root = tempfile::tempdir().unwrap();

        let error = setup_with_verifier(
            local_request(root.path()),
            Config::default(),
            &UnsafeSummaryVerifier,
        )
        .await
        .unwrap_err();
        let rendered = error.to_string();

        assert!(!rendered.contains("verifier-token"));
        assert!(!rendered.contains("verifier-secret"));
        assert!(!rendered.contains("/Users/alice"));
    }

    #[tokio::test]
    async fn verify_failure_preserves_prior_config_bytes_and_never_saves_candidate() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let prior = b"this exact prior configuration is intentionally untouched";
        tokio::fs::write(&path, prior).await.unwrap();
        let verifier = InspectingVerifier::failure("local", "default");

        let result = setup_and_save_with_verifier(
            local_request(root.path()),
            Config::default(),
            &path,
            &verifier,
        )
        .await;

        assert!(result.is_err());
        assert!(verifier.seen.load(Ordering::SeqCst));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), prior);
        assert!(!root.path().join("store").exists());
        assert!(!root.path().join("key.txt").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn build_failure_preserves_prior_config_bytes_and_skips_verification() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let prior = b"prior bytes survive candidate validation";
        tokio::fs::write(&path, prior).await.unwrap();
        let verifier = InspectingVerifier::success("local", "default");
        let invalid = SetupRequest::Local {
            store_path: PathBuf::new(),
            key_file: root.path().join("key.txt"),
            vault: "default".into(),
        };

        let result =
            setup_and_save_with_verifier(invalid, Config::default(), &path, &verifier).await;

        assert!(result.is_err());
        assert!(!verifier.seen.load(Ordering::SeqCst));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), prior);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_local_vault_fails_before_store_key_or_config_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let prior = b"prior config must survive invalid local vault syntax";
        tokio::fs::write(&path, prior).await.unwrap();
        let request = SetupRequest::Local {
            store_path: root.path().join("store"),
            key_file: root.path().join("key.txt"),
            vault: "../invalid".into(),
        };

        let result = setup_and_save(request, Config::default(), &path).await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), prior);
        assert!(!root.path().join("store").exists());
        assert!(!root.path().join("key.txt").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsafe_local_path_components_fail_before_any_side_effect() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("xv.conf");
        let prior = b"prior config must survive unsafe candidate paths";
        tokio::fs::write(&path, prior).await.unwrap();
        let request = SetupRequest::Local {
            store_path: root.path().join("nested").join("..").join("store"),
            key_file: root.path().join("key.txt"),
            vault: "default".into(),
        };

        let result = setup_and_save(request, Config::default(), &path).await;

        assert!(result.is_err());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), prior);
        assert!(!root.path().join("nested").exists());
        assert!(!root.path().join("store").exists());
        assert!(!root.path().join("key.txt").exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn valid_local_vault_and_path_edge_cases_build_without_side_effects() {
        let root = tempfile::tempdir().unwrap();
        for vault in ["default", "work-secrets", "team_1", "Vault123"] {
            let request = SetupRequest::Local {
                store_path: root.path().join(vault).join("store"),
                key_file: PathBuf::from("keys").join(format!("{vault}.txt")),
                vault: vault.into(),
            };
            let config = build_setup_config(&request, Config::default()).unwrap();
            assert_eq!(config.default_vault, vault);
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn local_store_and_key_lexical_aliases_are_rejected_without_lookup() {
        let entry = "xv-setup-candidate-alias-never-created";
        let request = SetupRequest::Local {
            store_path: PathBuf::from(entry),
            key_file: std::env::current_dir().unwrap().join(entry),
            vault: "default".into(),
        };

        assert!(build_setup_config(&request, Config::default()).is_err());
        assert!(!PathBuf::from(entry).exists());
    }

    #[test]
    #[cfg(unix)]
    fn local_store_and_key_aliases_through_existing_parents_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        let alias = root.path().join("alias");
        std::fs::create_dir(&real).unwrap();
        symlink(&real, &alias).unwrap();
        let request = SetupRequest::Local {
            store_path: real.join("not-created"),
            key_file: alias.join("not-created"),
            vault: "default".into(),
        };

        assert!(build_setup_config(&request, Config::default()).is_err());
        assert!(!real.join("not-created").exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn save_failure_after_verification_preserves_prior_target_bytes() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("prior.conf");
        let path = root.path().join("xv.conf");
        let prior = b"prior target bytes survive atomic save refusal";
        tokio::fs::write(&target, prior).await.unwrap();
        symlink(&target, &path).unwrap();
        let verifier = InspectingVerifier::success("local", "default");

        let result = setup_and_save_with_verifier(
            local_request(root.path()),
            Config::default(),
            &path,
            &verifier,
        )
        .await;

        assert!(result.is_err());
        assert!(verifier.seen.load(Ordering::SeqCst));
        assert_eq!(tokio::fs::read(&target).await.unwrap(), prior);
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn setup_and_save_verifies_local_candidate_before_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config").join("xv.conf");

        let outcome = setup_and_save(local_request(root.path()), Config::default(), &path)
            .await
            .unwrap();

        let saved: Config =
            toml::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(saved.effective_backend_name(), "local");
        assert_eq!(saved.default_vault, "default");
        assert_eq!(outcome.verification.operation, "list-secrets");
        assert!(root
            .path()
            .join("store/vaults/default/.vault.json")
            .is_file());
    }

    #[test]
    fn diagnostics_redact_auth_material_paths_urls_and_identifiers() {
        let raw = concat!(
            "AUTHORIZATION: Bearer abc\n",
            "Proxy-Authorization: Basic dXNlcjpwYXNz\n",
            "client_secret=hunter2; ",
            "access_token=token-value; https://user:pass@example.test/path?sig=query-secret; ",
            "client_secret%3Dpercent-secret%26scope%3Dvault; ",
            "config=/Users/alice/.config/xv/xv.conf; ",
            "tenant=123e4567-e89b-12d3-a456-426614174000; account=123456789012"
        );

        let safe = SafeSetupError::from_message(raw);
        let serialized = serde_json::to_string(&safe).unwrap();

        for forbidden in [
            "abc",
            "hunter2",
            "dXNlcjpwYXNz",
            "token-value",
            "user:pass",
            "query-secret",
            "percent-secret",
            "/Users/alice",
            "123e4567-e89b-12d3-a456-426614174000",
            "123456789012",
        ] {
            assert!(
                !safe.diagnostics.contains(forbidden),
                "diagnostics leaked {forbidden}: {}",
                safe.diagnostics
            );
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn diagnostics_are_bounded_after_redaction() {
        let raw = format!(
            "Authorization: Bearer never-visible; {} client_secret=also-never-visible",
            "provider-detail ".repeat(400)
        );

        let safe = SafeSetupError::from_message(&raw);

        assert!(safe.diagnostics.chars().count() <= 2048);
        assert!(!safe.diagnostics.contains("never-visible"));
        assert!(!safe.diagnostics.contains("also-never-visible"));
    }

    #[test]
    fn diagnostics_redact_adversarial_encodings_and_remain_valid_json() {
        let raw = concat!(
            "Authorization :\r\n  Basic folded-basic-secret\r\n",
            "x-api-key:\tapi-header-secret\n",
            "Cookie: session=cookie-secret\n",
            "https://user:password@example.test/vault?X-Amz-Signature=query-secret#fragment-secret ",
            "CLIENT_SECRET%3dencoded-lower-secret%26next%3dok ",
            "ACCESS_TOKEN%3Dencoded-upper-secret%26next%3Dok ",
            "config=C:\\Users\\alice\\AppData\\xv.conf ",
            "account=AKIAABCDEFGHIJKLMNOP ",
            "abcdefgh.ijklmnop.qrstuvwx ",
            "this_is_a_very_long_opaque_token_value_1234567890 ",
            "\u{1b}[31mcontrol"
        );

        let safe = SafeSetupError::from_message(raw);
        let json = serde_json::to_string(&safe).unwrap();
        let round_trip: SafeSetupError = serde_json::from_str(&json).unwrap();
        let debug = format!("{safe:?}");

        for forbidden in [
            "folded-basic-secret",
            "api-header-secret",
            "cookie-secret",
            "user:password",
            "query-secret",
            "fragment-secret",
            "encoded-lower-secret",
            "encoded-upper-secret",
            "C:\\Users\\alice",
            "AKIAABCDEFGHIJKLMNOP",
            "abcdefgh.ijklmnop.qrstuvwx",
            "this_is_a_very_long_opaque_token_value_1234567890",
        ] {
            assert!(!safe.diagnostics.contains(forbidden));
            assert!(!json.contains(forbidden));
            assert!(!debug.contains(forbidden));
        }
        assert_eq!(round_trip, safe);
        assert!(safe.diagnostics.chars().count() <= 2048);
        assert!(!safe.diagnostics.contains('\u{1b}'));
    }

    #[test]
    fn diagnostics_redact_encoded_urls_unc_opaque_tokens_and_zero_guid() {
        let raw = concat!(
            "callback=https%253A%252F%252Fuser%253Apassword%2540example.test",
            "%252Fvault%253Fsig%253Dquery-secret%2523fragment-secret; ",
            "config=\\\\server\\private\\config\\xv.conf; ",
            "opaque.token/value+secret==; ",
            "tenant=00000000-0000-0000-0000-000000000000"
        );

        let safe = SafeSetupError::from_message(raw);
        let serialized = serde_json::to_string(&safe).unwrap();
        serde_json::from_str::<SafeSetupError>(&serialized).unwrap();

        for forbidden in [
            "user",
            "password",
            "query-secret",
            "fragment-secret",
            "server",
            "private",
            "opaque.token/value+secret==",
            "00000000",
        ] {
            assert!(
                !safe.diagnostics.contains(forbidden),
                "diagnostics partially leaked {forbidden}: {}",
                safe.diagnostics
            );
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn diagnostics_preserve_ordinary_safe_provider_guidance() {
        let raw =
            "Connection timed out while listing the selected vault. Check the network and retry.";

        let safe = SafeSetupError::from_message(raw);

        assert_eq!(safe.diagnostics, raw);
        assert!(!safe.message.contains("[REDACTED]"));
        assert!(!safe.hint.contains("[REDACTED]"));
    }

    #[test]
    fn diagnostics_redact_adjacent_opaque_tokens_without_partial_overlap() {
        let first = "opaque.token/value+first-secret==";
        let second = "opaque.token/value+second-secret==";
        let safe = SafeSetupError::from_message(&format!("{first} {second}"));

        assert!(!safe.diagnostics.contains(first));
        assert!(!safe.diagnostics.contains(second));
        assert!(!safe.diagnostics.contains("opaque.token"));
        assert!(!safe.diagnostics.contains("first-secret"));
        assert!(!safe.diagnostics.contains("second-secret"));
    }

    #[test]
    fn diagnostics_handle_malformed_percent_input_and_preserve_safe_versions() {
        let raw = "Azure CLI 2.61.0; TLS/1.3; region us-east-1; malformed=%ZZ%E0%A4%A";

        let safe = SafeSetupError::from_message(raw);
        let serialized = serde_json::to_string(&safe).unwrap();
        serde_json::from_str::<SafeSetupError>(&serialized).unwrap();

        assert!(safe.diagnostics.contains("Azure CLI 2.61.0"));
        assert!(safe.diagnostics.contains("TLS/1.3"));
        assert!(safe.diagnostics.contains("region us-east-1"));
        assert!(safe.diagnostics.chars().count() <= 2048);
    }

    #[test]
    fn diagnostics_classify_failures_with_provider_safe_hints() {
        let cases = [
            (
                CrosstacheError::authentication("expired token"),
                "azure",
                "xv-auth-failed",
                "az login",
            ),
            (
                CrosstacheError::config("invalid field"),
                "local",
                "xv-config-invalid",
                "setup fields",
            ),
            (
                CrosstacheError::permission_denied("denied"),
                "aws",
                "xv-permission-denied",
                "IAM",
            ),
            (
                CrosstacheError::network("offline"),
                "azure",
                "xv-network",
                "network",
            ),
            (
                CrosstacheError::unknown("provider failure"),
                "aws",
                "xv-backend-internal",
                "AWS",
            ),
        ];

        for (error, backend, code, hint_fragment) in cases {
            let safe = SafeSetupError::from_error("list-secrets", backend, "display-vault", &error);
            assert_eq!(safe.code, code);
            assert_eq!(safe.operation, "list-secrets");
            assert_eq!(safe.backend, backend);
            assert_eq!(safe.vault, "display-vault");
            assert!(safe.hint.contains(hint_fragment), "{safe:?}");
            assert!(!safe.message.contains(&error.to_string()));
        }
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
