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

/// Blob transfer settings for AWS S3 file storage, read from the global
/// `[blob]` config so `xv file` on AWS honors the same chunk-size and
/// concurrency knobs as Azure.
#[cfg(feature = "aws")]
fn aws_transfer_config(config: &Config) -> super::aws::TransferConfig {
    let blob = config.get_blob_config();
    super::aws::TransferConfig {
        chunk_size_mb: blob.chunk_size_mb,
        max_concurrent_uploads: blob.max_concurrent_uploads,
    }
}

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
    /// Config snapshot used for on-demand (lazy) construction of backends
    /// registered via [`with_lazy`](Self::with_lazy) but not yet built.
    /// `None` for registries built the eager way (`from_config`/`new`).
    lazy_config: Option<Config>,
    /// Names registered for lazy construction — a superset of what's been
    /// materialized so far. Registering a name here does NOT build it;
    /// [`materialize`](Self::materialize) builds (and caches) on first use.
    lazy_names: Vec<String>,
    /// Cache of backends materialized on demand, keyed by the *config*
    /// name (e.g. a `named_backends` key like `"local-a"`), which may
    /// differ from `Backend::name()` (the backend *kind*).
    lazy_cache: std::sync::Mutex<HashMap<String, Arc<dyn Backend>>>,
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
    #[cfg(test)]
    pub(crate) fn for_test(
        default: &'static str,
        backends: Vec<(&'static str, Arc<dyn Backend>)>,
    ) -> Self {
        Self {
            backends: backends.into_iter().collect(),
            default,
            azure_auth: None,
            lazy_config: None,
            lazy_names: Vec::new(),
            lazy_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

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
                // block_in_place is safe to call from inside a tokio multi-thread
                // runtime (unlike Handle::block_on which panics if a runtime is
                // already active on the current thread).
                let backend = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(super::aws::AwsBackend::new(
                        aws_cfg,
                        None,
                        None,
                        aws_transfer_config(config),
                    ))
                })?;
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
                // Named AWS entries carry no global `[blob]` coupling; use
                // the default transfer profile.
                let backend = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(super::aws::AwsBackend::new(
                        aws_cfg,
                        None,
                        None,
                        super::aws::TransferConfig::default(),
                    ))
                })?;
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
            lazy_config: None,
            lazy_names: Vec::new(),
            lazy_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Register backend names for on-demand (lazy) construction, without
    /// building any of them.
    ///
    /// Used for multi-vault workspaces: a workspace may attach vaults on
    /// several backends (e.g. `azure` + a named `aws-east` entry), but a
    /// command that only touches one of them must never authenticate the
    /// others. `names` may be built-in kind names (`"azure"`, `"local"`,
    /// `"aws"`) or `named_backends` keys — [`materialize`](Self::materialize)
    /// resolves either. This never fails: registration is pure bookkeeping;
    /// construction errors surface only when a name is actually
    /// materialized.
    pub fn with_lazy(config: &Config, names: &[String]) -> Result<Self, BackendError> {
        Ok(Self {
            backends: HashMap::new(),
            default: "",
            azure_auth: None,
            lazy_config: Some(config.clone()),
            lazy_names: names.to_vec(),
            lazy_cache: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// Get-or-construct a backend by name. The first call to materialize a
    /// given `name` builds it (and any auth it requires); later calls
    /// return the same cached `Arc`. Errors name the backend that failed.
    ///
    /// `name` must be one registered via [`with_lazy`](Self::with_lazy) (or
    /// already present as this registry's eagerly-built backend) —
    /// otherwise this returns `Err` without attempting construction.
    pub fn materialize(&self, name: &str) -> Result<Arc<dyn Backend>, BackendError> {
        // Fast path: already an eagerly-built backend (the degenerate,
        // single-backend registry case).
        if let Some(b) = self.backends.get(name) {
            return Ok(b.clone());
        }

        if !self.lazy_names.iter().any(|n| n == name) {
            return Err(BackendError::Internal(format!(
                "backend '{name}' is not attached to this workspace"
            )));
        }

        let mut cache = self
            .lazy_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(b) = cache.get(name) {
            return Ok(b.clone());
        }

        let config = self.lazy_config.as_ref().ok_or_else(|| {
            BackendError::Internal(
                "lazy registry has no config snapshot to construct backends from".into(),
            )
        })?;

        let backend = Self::construct_named(name, config)?;
        cache.insert(name.to_string(), backend.clone());
        Ok(backend)
    }

    /// Construct a single backend instance by its registry name (either a
    /// `named_backends` key or a built-in kind name), used by
    /// [`materialize`](Self::materialize).
    fn construct_named(name: &str, config: &Config) -> Result<Arc<dyn Backend>, BackendError> {
        if let Some(entry) = config.named_backends.get(name) {
            return match Self::from_named_entry(name, entry) {
                Ok(registry) => Ok(registry.backends[registry.default].clone()),
                Err(e) => Err(e),
            };
        }

        let kind: BackendKind = name
            .parse()
            .map_err(|e: String| BackendError::Internal(e))?;

        match kind {
            BackendKind::Azure => {
                let auth = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth)?;
                Ok(Arc::new(backend))
            }
            BackendKind::Local => {
                let backend = super::local::LocalBackend::new(config.local.as_ref())?;
                Ok(Arc::new(backend))
            }
            #[cfg(feature = "aws")]
            BackendKind::Aws => {
                let aws_cfg = config.aws.as_ref().ok_or_else(|| {
                    BackendError::Internal(
                        "[aws] config block missing — set backend = \"aws\" with [aws] block"
                            .into(),
                    )
                })?;
                let backend = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(super::aws::AwsBackend::new(
                        aws_cfg,
                        None,
                        None,
                        aws_transfer_config(config),
                    ))
                })?;
                Ok(Arc::new(backend))
            }
            #[cfg(not(feature = "aws"))]
            BackendKind::Aws => Err(BackendError::Internal(
                "AWS backend not compiled in: rebuild with --features aws".into(),
            )),
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

    /// Verify the active backend can connect and list the requested vault.
    ///
    /// Setup candidates call this before any configuration replacement. It
    /// intentionally returns no provider records or diagnostics.
    #[allow(dead_code)] // Consumed by the desktop setup adapter in Task 3.
    pub async fn verify_active_vault(&self, vault: &str) -> Result<(), BackendError> {
        let backend = self.active();
        backend.health_check().await?;
        backend.secrets().list_secrets(vault, None).await?;
        Ok(())
    }

    /// Create a fresh backend instance for the given kind using the provided config.
    ///
    /// Used for cross-backend operations such as resolving `xv://aws:prod/SECRET`
    /// while the active backend is Azure.
    pub async fn create_for_kind(
        kind: BackendKind,
        config: &Config,
    ) -> std::result::Result<std::sync::Arc<dyn Backend>, BackendError> {
        match kind {
            BackendKind::Azure => {
                let auth = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth)?;
                Ok(std::sync::Arc::new(backend))
            }
            BackendKind::Local => {
                let backend = super::local::LocalBackend::new(config.local.as_ref())?;
                Ok(std::sync::Arc::new(backend))
            }
            #[cfg(feature = "aws")]
            BackendKind::Aws => {
                let aws_cfg = config.aws.as_ref().ok_or_else(|| {
                    BackendError::Internal(
                        "[aws] config block missing — add an [aws] section to your config".into(),
                    )
                })?;
                let backend =
                    super::aws::AwsBackend::new(aws_cfg, None, None, aws_transfer_config(config))
                        .await?;
                Ok(std::sync::Arc::new(backend))
            }
            #[cfg(not(feature = "aws"))]
            BackendKind::Aws => Err(BackendError::Internal(
                "AWS backend not compiled in: rebuild with --features aws".into(),
            )),
        }
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

    #[tokio::test]
    #[cfg(feature = "ui")]
    async fn setup_verification_runs_health_check_before_vault_list() {
        use crate::web::testutil::stub::StubBackend;

        let health_failure = BackendRegistry::new(Arc::new(StubBackend::with_health_error(
            "stub",
            "health failed",
        )));
        let error = health_failure
            .verify_active_vault("default")
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::Internal(ref message) if message == "health failed"));

        let list_failure = BackendRegistry::new(Arc::new(StubBackend::with_list_error(
            "stub",
            "list failed",
        )));
        let error = list_failure
            .verify_active_vault("default")
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::Internal(ref message) if message == "list failed"));

        let success = BackendRegistry::new(Arc::new(StubBackend::new()));
        success.verify_active_vault("default").await.unwrap();
    }

    #[test]
    fn from_config_local_creates_backend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
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

    fn two_local_backends_config(tmp: &tempfile::TempDir) -> Config {
        use crate::config::settings::{LocalConfig, NamedBackendEntry};
        use std::collections::HashMap;

        let mut named_backends = HashMap::new();
        named_backends.insert(
            "local-a".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some(tmp.path().join("store-a").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key-a.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
        );
        named_backends.insert(
            "local-b".to_string(),
            NamedBackendEntry::Local(LocalConfig {
                store_path: Some(tmp.path().join("store-b").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key-b.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
        );

        Config {
            named_backends,
            ..Default::default()
        }
    }

    #[test]
    fn materialize_constructs_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = two_local_backends_config(&tmp);
        let registry =
            BackendRegistry::with_lazy(&config, &["local-a".to_string(), "local-b".to_string()])
                .expect("registration must not error");

        let first = registry
            .materialize("local-a")
            .expect("first materialize must succeed");
        let second = registry
            .materialize("local-a")
            .expect("second materialize must succeed");
        assert!(
            Arc::ptr_eq(&first, &second),
            "materialize must return the same cached Arc on repeated calls"
        );
    }

    #[test]
    fn materialize_unknown_name_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = two_local_backends_config(&tmp);
        let registry =
            BackendRegistry::with_lazy(&config, &["local-a".to_string()]).expect("must register");
        let result = registry.materialize("does-not-exist");
        assert!(result.is_err());
    }

    #[test]
    fn lazy_never_constructs_unreferenced_backend() {
        let tmp = tempfile::TempDir::new().unwrap();
        // `with_lazy` only records names — it must never dispatch into
        // backend construction at registration time. If it did, this
        // config (named_backends only; no top-level `[azure]`/credential
        // setup at all) would be a reasonable place for that to blow up.
        // It doesn't, because `with_lazy` never calls `construct_named`.
        let config = two_local_backends_config(&tmp);
        let registry =
            BackendRegistry::with_lazy(&config, &["azure".to_string(), "local-b".to_string()])
                .expect("registering azure + local-b must not construct either eagerly");

        // Touching only "local-b" must succeed and must not require ever
        // calling materialize("azure") — the command-level guarantee this
        // registry API exists to provide (a command touching only AWS/local
        // vaults in a workspace must never authenticate Azure).
        let local_b = registry.materialize("local-b");
        assert!(
            local_b.is_ok(),
            "local-b must materialize: {:?}",
            local_b.err()
        );

        // The "azure" name is registered but was never referenced above,
        // and nothing in this test touched Azure auth/config — the whole
        // point of lazy construction is that "registered" and "built" are
        // separate steps, so it is never even attempted here.
    }

    #[test]
    fn materialize_falls_back_to_eager_backend_map() {
        // A registry built the eager way (`new`/`from_config`) should still
        // answer `materialize` for its single active backend name, since
        // callers of the workspace resolver shouldn't need to special-case
        // "eager vs lazy" registries.
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
            ..Default::default()
        };
        let registry = BackendRegistry::from_config(&config).expect("must build");
        let materialized = registry
            .materialize("local")
            .expect("eager backend must be materializable by name");
        assert_eq!(materialized.name(), "local");
    }
}
