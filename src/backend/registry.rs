//! Backend registry — runtime backend resolution.
//!
//! [`BackendRegistry`] holds instantiated backends and dispatches
//! operations to the active one.  Created once at startup from the
//! application [`Config`](crate::config::Config).

use std::collections::HashMap;
use std::sync::Arc;

use super::error::BackendError;
use super::{Backend, BackendKind};
use crate::config::settings::Config;

/// Maps backend names to live [`Backend`] instances.
///
/// Created once at startup from the application config. The CLI and TUI
/// layers call [`active()`](Self::active) to get the current backend.
pub struct BackendRegistry {
    backends: HashMap<&'static str, Arc<dyn Backend>>,
    default: &'static str,
    /// The Azure auth provider, if the active backend is Azure.
    ///
    /// Stored separately because many CLI handlers still need the raw
    /// provider to construct `SecretManager` / `VaultManager` during the
    /// migration period. Will be removed once all handlers use the
    /// backend trait layer exclusively.
    azure_auth: Option<Arc<dyn crate::auth::provider::AzureAuthProvider>>,
}

impl std::fmt::Debug for BackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendRegistry")
            .field("default", &self.default)
            .field("backends", &self.backends.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl BackendRegistry {
    /// Build a registry from the loaded [`Config`].
    ///
    /// The active backend is determined by `config.backend` (defaulting to
    /// `"azure"` when absent). Named backends in `config.named_backends` are
    /// checked first; if a matching entry is found it is instantiated directly.
    ///
    /// [`AzureBackend`]: super::azure::AzureBackend
    pub fn from_config(config: &Config) -> Result<Self, BackendError> {
        let backend_name = config.effective_backend_name();

        // Resolve named-backend entry first if applicable
        if let Some(entry) = config.named_backends.get(backend_name) {
            return Self::from_named_entry(backend_name, entry);
        }

        let kind: BackendKind = backend_name
            .parse()
            .map_err(|e: String| BackendError::Internal(e))?;

        match kind {
            BackendKind::Azure => {
                let auth_provider = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth_provider.clone())?;
                let mut registry = Self::new(Arc::new(backend));
                registry.azure_auth = Some(auth_provider);
                Ok(registry)
            }
            BackendKind::Local => {
                let backend = super::local::LocalBackend::new(config.local.as_ref())?;
                Ok(Self::new(Arc::new(backend)))
            }
            #[cfg(feature = "aws")]
            BackendKind::Aws => {
                let aws_cfg = config.aws.as_ref().ok_or_else(|| {
                    BackendError::Internal(
                        "[aws] config block missing — set backend = \"aws\" with [aws] block"
                            .into(),
                    )
                })?;
                // We need an async runtime. The registry is sync; use a runtime handle.
                let backend = tokio::runtime::Handle::current()
                    .block_on(super::aws::AwsBackend::new(aws_cfg, None, None))?;
                Ok(Self::new(Arc::new(backend)))
            }
            #[cfg(not(feature = "aws"))]
            BackendKind::Aws => Err(BackendError::Internal(
                "AWS backend not compiled in: rebuild with --features aws".into(),
            )),
        }
    }

    fn from_named_entry(
        name: &str,
        entry: &crate::config::settings::NamedBackendEntry,
    ) -> Result<Self, BackendError> {
        use crate::config::settings::NamedBackendEntry as NBE;
        // `name` is used in the not(feature = "aws") error path below.
        // When aws is compiled in, Rust sees it unused — suppress the lint.
        let _ = name;
        match entry {
            #[cfg(feature = "aws")]
            NBE::Aws(aws_cfg) => {
                let backend = tokio::runtime::Handle::current()
                    .block_on(super::aws::AwsBackend::new(aws_cfg, None, None))?;
                Ok(Self::new(Arc::new(backend)))
            }
            #[cfg(not(feature = "aws"))]
            NBE::Aws(_) => Err(BackendError::Internal(format!(
                "named backend '{name}' is aws but binary built without --features aws"
            ))),
            NBE::Local(local_cfg) => {
                let backend = super::local::LocalBackend::new(Some(local_cfg))?;
                Ok(Self::new(Arc::new(backend)))
            }
        }
    }

    /// Create an Azure auth provider from the config's credential priority.
    pub fn create_azure_auth_provider(
        config: &Config,
    ) -> Result<Arc<dyn crate::auth::provider::AzureAuthProvider>, BackendError> {
        use crate::auth::provider::DefaultAzureCredentialProvider;

        let provider = DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| BackendError::AuthenticationFailed(e.to_string()))?;
        Ok(Arc::new(provider))
    }

    /// Create a new registry with a single backend.
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        let name = backend.name();
        let mut backends = HashMap::new();
        backends.insert(name, backend);
        Self {
            backends,
            default: name,
            azure_auth: None,
        }
    }

    /// Get the currently-active backend.
    #[allow(dead_code)] // Infrastructure for Phase 2 pluggability — called once dispatch is migrated.
    pub fn active(&self) -> &dyn Backend {
        self.backends[self.default].as_ref()
    }

    /// Get an `Arc` handle to the active backend (cloneable, `Send + Sync`).
    ///
    /// Useful when you need to move the backend into an async task (e.g. the
    /// TUI data-loading spawns).
    #[allow(dead_code)] // Used by the TUI feature gate; invisible to default builds.
    pub fn active_arc(&self) -> Arc<dyn Backend> {
        self.backends[self.default].clone()
    }

    /// Get a backend by name.
    #[allow(dead_code)] // Infrastructure for Phase 2 pluggability — used for multi-backend dispatch.
    pub fn get(&self, name: &str) -> Option<&dyn Backend> {
        self.backends.get(name).map(|b| b.as_ref())
    }

    /// List all registered backend names.
    #[allow(dead_code)] // Infrastructure for Phase 2 pluggability — used for multi-backend dispatch.
    pub fn names(&self) -> Vec<&'static str> {
        self.backends.keys().copied().collect()
    }

    /// The name of the default (active) backend.
    #[allow(dead_code)] // Infrastructure for Phase 2 pluggability — used for multi-backend dispatch.
    pub fn default_name(&self) -> &'static str {
        self.default
    }

    /// Try to extract the Azure auth provider from the active backend.
    ///
    /// During the migration period, many CLI handlers still need the raw
    /// `AzureAuthProvider` to construct `SecretManager` / `VaultManager`.
    /// This convenience method returns the provider that was created when
    /// the registry was built from config.
    ///
    /// Returns `None` if the active backend is not Azure.
    pub fn azure_auth_provider(&self) -> Option<Arc<dyn crate::auth::provider::AzureAuthProvider>> {
        self.azure_auth.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_local_creates_backend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
            }),
            ..Default::default()
        };
        let result = BackendRegistry::from_config(&config);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        let registry = result.unwrap();
        assert_eq!(registry.active().name(), "local");
    }

    #[test]
    fn from_config_unknown_backend_returns_error() {
        let config = Config {
            backend: Some("nosuchbackend".to_string()),
            ..Default::default()
        };
        let result = BackendRegistry::from_config(&config);
        assert!(result.is_err());
    }

    #[cfg(feature = "aws")]
    #[tokio::test]
    async fn from_config_aws_requires_aws_block() {
        let config = Config {
            backend: Some("aws".to_string()),
            aws: None,
            ..Default::default()
        };
        let result = BackendRegistry::from_config(&config);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("[aws]") || err_str.contains("aws"),
            "got: {err_str}"
        );
    }
}
