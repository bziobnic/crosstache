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
    /// `"azure"` when absent). For Azure, an [`AzureBackend`] is created
    /// using the existing auth provider. For Local, an error is returned
    /// because the local backend is not yet implemented.
    ///
    /// [`AzureBackend`]: super::azure::AzureBackend
    pub fn from_config(config: &Config) -> Result<Self, BackendError> {
        let kind: BackendKind = config
            .effective_backend_name()
            .parse()
            .map_err(|e: String| BackendError::Internal(e))?;

        match kind {
            BackendKind::Azure => {
                let auth_provider = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth_provider)?;
                Ok(Self::new(Arc::new(backend)))
            }
            BackendKind::Local => Err(BackendError::Unsupported(
                "local backend is not yet implemented — coming in a future release".into(),
            )),
        }
    }

    /// Create an Azure auth provider from the config's credential priority.
    fn create_azure_auth_provider(
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
        }
    }

    /// Get the currently-active backend.
    pub fn active(&self) -> &dyn Backend {
        self.backends[self.default].as_ref()
    }

    /// Get a backend by name.
    pub fn get(&self, name: &str) -> Option<&dyn Backend> {
        self.backends.get(name).map(|b| b.as_ref())
    }

    /// List all registered backend names.
    pub fn names(&self) -> Vec<&'static str> {
        self.backends.keys().copied().collect()
    }

    /// The name of the default (active) backend.
    pub fn default_name(&self) -> &'static str {
        self.default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_local_returns_unsupported() {
        let config = Config {
            backend: Some("local".to_string()),
            ..Default::default()
        };
        let result = BackendRegistry::from_config(&config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, BackendError::Unsupported(_)),
            "expected Unsupported, got: {err:?}"
        );
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
}
