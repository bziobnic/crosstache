//! Backend lifecycle shared by `xv init` and `xv backend add|rm|ls`.
//!
//! See `docs/superpowers/specs/2026-08-09-backend-lifecycle-design.md`.

use crate::config::settings::Config;
use crate::config::setup::{apply_backend, SetupRequest};
use crate::error::{CrosstacheError, Result};

/// The backend types `xv backend` manages. One instance per type; named
/// multi-instance backends (`named_backends`) are out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Local,
    Azure,
    Aws,
}

impl BackendType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Azure => "azure",
            Self::Aws => "aws",
        }
    }
}

impl std::str::FromStr for BackendType {
    type Err = CrosstacheError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "local" => Ok(Self::Local),
            "azure" => Ok(Self::Azure),
            "aws" => Ok(Self::Aws),
            other => Err(CrosstacheError::invalid_argument(format!(
                "unknown backend '{other}'; expected one of: local, azure, aws"
            ))),
        }
    }
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which backend blocks are populated in `config`.
pub fn configured_backends(config: &Config) -> Vec<BackendType> {
    let mut found = Vec::new();
    if config.local.is_some() {
        found.push(BackendType::Local);
    }
    if config.azure.is_some() {
        found.push(BackendType::Azure);
    }
    if config.aws.is_some() {
        found.push(BackendType::Aws);
    }
    found
}

/// Validate `request`, stand the backend up for real, and fold it into `base`.
///
/// Returns the updated config **without** saving; the caller persists it. Any
/// failure returns `Err` with `base` unmodified, so a failed add writes nothing.
pub async fn add_backend(
    request: &SetupRequest,
    base: Config,
    make_active: bool,
) -> Result<Config> {
    let mut candidate = base;
    apply_backend(request, &mut candidate)?;
    initialize_backend(request, &candidate).await?;

    if make_active {
        match request {
            SetupRequest::Local { vault, .. } => {
                candidate.backend = Some("local".into());
                candidate.default_vault = vault.clone();
            }
            SetupRequest::Azure {
                vault,
                subscription_id,
                tenant_id,
                resource_group,
                location,
            } => {
                candidate.backend = None;
                candidate.default_vault = vault.clone();
                candidate.subscription_id = subscription_id.clone();
                candidate.tenant_id = tenant_id.clone();
                candidate.default_resource_group = resource_group.clone();
                candidate.default_location = location.clone();
            }
            SetupRequest::Aws { vault_prefix, .. } => {
                candidate.backend = Some("aws".into());
                candidate.default_vault = vault_prefix.clone();
            }
        }
        candidate.validate()?;
    }

    Ok(candidate)
}

/// Stand the backend up: create keys and directories, or probe credentials.
async fn initialize_backend(request: &SetupRequest, config: &Config) -> Result<()> {
    match request {
        SetupRequest::Local { .. } => {
            let local = config.local.as_ref().ok_or_else(|| {
                CrosstacheError::config("local setup did not produce a [local] block")
            })?;
            crate::backend::local::LocalBackend::new(Some(local)).map_err(|e| {
                CrosstacheError::config(format!("Failed to create local backend: {e}"))
            })?;
            Ok(())
        }
        // Azure and AWS validate their credentials lazily on first use; the
        // interactive flows already probe the CLI before building the request.
        SetupRequest::Azure { .. } | SetupRequest::Aws { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::Config;

    fn local_request(root: &std::path::Path) -> SetupRequest {
        SetupRequest::Local {
            store_path: root.join("store"),
            key_file: root.join("key.txt"),
            vault: "default".into(),
        }
    }

    /// Adding a backend without making it active must not move the write
    /// target — the whole point of `xv backend add` versus `xv init`.
    #[tokio::test]
    async fn add_backend_without_make_active_leaves_the_active_backend_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut base = Config::default();
        base.backend = Some("aws".into());
        base.default_vault = "aws-vault".into();

        let config = add_backend(&local_request(dir.path()), base, false)
            .await
            .unwrap();

        assert_eq!(config.backend.as_deref(), Some("aws"));
        assert_eq!(config.default_vault, "aws-vault");
        assert!(config.local.is_some(), "local block was still written");
    }

    #[tokio::test]
    async fn add_backend_with_make_active_switches_the_write_target() {
        let dir = tempfile::tempdir().unwrap();
        let config = add_backend(&local_request(dir.path()), Config::default(), true)
            .await
            .unwrap();

        assert_eq!(config.backend.as_deref(), Some("local"));
        assert_eq!(config.default_vault, "default");
    }

    #[test]
    fn configured_backends_lists_only_populated_blocks() {
        let mut config = Config::default();
        assert!(configured_backends(&config).is_empty());

        config.local = Some(Default::default());
        assert_eq!(configured_backends(&config), vec![BackendType::Local]);
    }

    #[test]
    fn backend_type_round_trips_through_str() {
        for name in ["local", "azure", "aws"] {
            assert_eq!(name.parse::<BackendType>().unwrap().as_str(), name);
        }
        assert!("postgres".parse::<BackendType>().is_err());
    }

    /// A failed add must leave the caller's config exactly as it was, so
    /// nothing partial is ever written. Here the store path is a *file*, so
    /// creating the store directory fails during initialization.
    #[tokio::test]
    async fn failed_add_returns_err_and_never_mutates_the_callers_config() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("store");
        std::fs::write(&blocker, "not a directory").unwrap();

        let mut base = Config::default();
        base.backend = Some("aws".into());
        base.aws = Some(Default::default());
        let before = toml::to_string(&base).unwrap();

        let result = add_backend(
            &SetupRequest::Local {
                store_path: blocker,
                key_file: dir.path().join("key.txt"),
                vault: "default".into(),
            },
            base.clone(),
            true,
        )
        .await;

        assert!(result.is_err(), "a store path that is a file must fail");
        assert_eq!(
            toml::to_string(&base).unwrap(),
            before,
            "the caller's config must be untouched by a failed add"
        );
    }
}
