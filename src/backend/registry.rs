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

/// All fallible agent-policy work that must finish before a backend constructor
/// is allowed to create credentials, repositories, metadata, or store state.
struct AgentPolicyPreflight {
    identity: crate::agent::AgentIdentity,
    policy: crate::agent::policy::CompiledPolicy,
    decisions: crate::agent::DecisionLog,
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
            backends: backends
                .into_iter()
                .map(|(name, backend)| (name, super::guard::GuardedBackend::wrap(backend)))
                .collect(),
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
        Self::from_config_with_preflight(config, Self::agent_policy_preflight)
    }

    fn from_config_with_preflight<F>(config: &Config, preflight_fn: F) -> Result<Self, BackendError>
    where
        F: FnOnce(&Config) -> Result<Option<AgentPolicyPreflight>, BackendError>,
    {
        let preflight = preflight_fn(config)?;
        let backend_name = config.effective_backend_name();

        // Resolve named-backend entry first if applicable
        if let Some(entry) = config.named_backends.get(backend_name) {
            let mut registry =
                Self::from_named_entry(backend_name, entry, config.runtime_open_existing_local)?;
            registry.apply_agent_policy(preflight);
            return Ok(registry);
        }

        let kind: BackendKind = backend_name
            .parse()
            .map_err(|e: String| BackendError::Internal(e))?;

        let mut registry: Self = match kind {
            BackendKind::Azure => {
                let auth_provider = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth_provider.clone())?;
                let mut registry = Self::raw_registry(Arc::new(backend));
                registry.azure_auth = Some(auth_provider);
                Ok::<Self, BackendError>(registry)
            }
            BackendKind::Local => {
                let backend = if config.runtime_open_existing_local {
                    super::local::LocalBackend::open_existing(config.local.as_ref())?
                } else {
                    super::local::LocalBackend::new(config.local.as_ref())?
                };
                Ok::<Self, BackendError>(Self::raw_registry(Arc::new(backend)))
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
                Ok::<Self, BackendError>(Self::raw_registry(Arc::new(backend)))
            }
            #[cfg(not(feature = "aws"))]
            BackendKind::Aws => Err(BackendError::Internal(
                "AWS backend not compiled in: rebuild with --features aws".into(),
            )),
        }?;
        registry.apply_agent_policy(preflight);
        Ok(registry)
    }

    fn from_named_entry(
        name: &str,
        entry: &crate::config::settings::NamedBackendEntry,
        open_existing: bool,
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
                Ok(Self::raw_registry(Arc::new(backend)))
            }
            #[cfg(not(feature = "aws"))]
            NBE::Aws(_) => Err(BackendError::Internal(format!(
                "named backend '{name}' is aws but binary built without --features aws"
            ))),
            NBE::Local(local_cfg) => {
                let backend = if open_existing {
                    super::local::LocalBackend::open_existing(Some(local_cfg))?
                } else {
                    super::local::LocalBackend::new(Some(local_cfg))?
                };
                Ok(Self::raw_registry(Arc::new(backend)))
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
        Self::raw_registry(super::guard::GuardedBackend::wrap(backend))
    }

    fn raw_registry(backend: Arc<dyn Backend>) -> Self {
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

        let preflight = Self::agent_policy_preflight(config)?;
        let backend = Self::construct_named(name, config)?;
        let backend = Self::apply_preflight(backend, preflight);
        cache.insert(name.to_string(), backend.clone());
        Ok(backend)
    }

    fn apply_agent_policy(&mut self, preflight: Option<AgentPolicyPreflight>) {
        let backend = self.backends[self.default].clone();
        self.backends
            .insert(self.default, Self::apply_preflight(backend, preflight));
    }

    fn apply_preflight(
        backend: Arc<dyn Backend>,
        preflight: Option<AgentPolicyPreflight>,
    ) -> Arc<dyn Backend> {
        let backend: Arc<dyn Backend> = match preflight {
            Some(preflight) => Arc::new(crate::agent::enforce::PolicyEnforcedBackend::new(
                backend,
                preflight.identity,
                preflight.policy,
                preflight.decisions,
            )),
            None => backend,
        };
        super::guard::GuardedBackend::wrap(backend)
    }

    fn agent_policy_preflight(
        config: &Config,
    ) -> Result<Option<AgentPolicyPreflight>, BackendError> {
        // Enforcement protects the whole invocation, not only whichever identity
        // source wins resolution. A manually asserted identity can coexist with
        // GitHub Actions variables later consumed by Azure authentication, so
        // validate that bearer destination before any backend is constructed.
        if config.agent.as_ref().is_some_and(|agent| agent.enforce) {
            if let Ok(request_url) = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL") {
                crate::backend::azure::oidc::validate_github_actions_token_url(&request_url)
                    .map_err(|error| BackendError::AuthenticationFailed(error.to_string()))?;
            }
        }
        Self::agent_policy_preflight_with(
            config,
            crate::agent::resolve::current_resolution,
            crate::agent::DecisionLog::open_default,
        )
    }

    fn agent_policy_preflight_with<'a, F, L>(
        config: &Config,
        resolve: F,
        open_log: L,
    ) -> Result<Option<AgentPolicyPreflight>, BackendError>
    where
        F: FnOnce() -> &'a crate::agent::resolve::Resolution,
        L: FnOnce() -> Result<crate::agent::DecisionLog, BackendError>,
    {
        let Some(agent) = config.agent.as_ref().filter(|agent| agent.enforce) else {
            return Ok(None);
        };
        let policy = crate::agent::policy::CompiledPolicy::compile(agent)
            .map_err(BackendError::InvalidArgument)?;
        let identity = match resolve() {
            crate::agent::resolve::Resolution::Resolved(identity) => identity.as_ref().clone(),
            crate::agent::resolve::Resolution::Unresolved(attempts) => {
                return Err(BackendError::AuthenticationFailed(
                    crate::agent::resolve::unresolved_diagnostic(attempts),
                ));
            }
        };
        identity.validate().map_err(|error| {
            BackendError::InvalidArgument(format!("invalid agent identity: {error}"))
        })?;
        let decisions = open_log()?;
        Ok(Some(AgentPolicyPreflight {
            identity,
            policy,
            decisions,
        }))
    }

    #[cfg(test)]
    async fn create_for_kind_with_resolution(
        kind: BackendKind,
        config: &Config,
        resolution: &crate::agent::resolve::Resolution,
    ) -> Result<Arc<dyn Backend>, BackendError> {
        let preflight = Self::agent_policy_preflight_with(
            config,
            || resolution,
            || -> Result<crate::agent::DecisionLog, BackendError> {
                panic!("unresolved identity must fail before opening the decision log")
            },
        )?;
        let backend = Self::construct_for_kind(kind, config).await?;
        Ok(Self::apply_preflight(backend, preflight))
    }

    #[cfg(test)]
    async fn create_for_kind_with_resolution_and_log(
        kind: BackendKind,
        config: &Config,
        resolution: &crate::agent::resolve::Resolution,
        decision_path: std::path::PathBuf,
    ) -> Result<Arc<dyn Backend>, BackendError> {
        let preflight = Self::agent_policy_preflight_with(
            config,
            || resolution,
            move || Ok(crate::agent::DecisionLog::for_test(decision_path)),
        )?;
        let backend = Self::construct_for_kind(kind, config).await?;
        Ok(Self::apply_preflight(backend, preflight))
    }

    /// Construct a single backend instance by its registry name (either a
    /// `named_backends` key or a built-in kind name), used by
    /// [`materialize`](Self::materialize).
    fn construct_named(name: &str, config: &Config) -> Result<Arc<dyn Backend>, BackendError> {
        if let Some(entry) = config.named_backends.get(name) {
            return match Self::from_named_entry(name, entry, config.runtime_open_existing_local) {
                Ok(registry) => Ok(registry.backends[registry.default].clone()),
                Err(e) => Err(e),
            };
        }

        // Neither a `named_backends` key (checked above) nor a built-in kind:
        // the name does not exist at all. Report that as its own variant --
        // "unknown backend kind: X. Valid options: azure, local, aws" reads as
        // advice to correct a *kind*, when the caller most likely referenced a
        // named backend that has since been removed from the config.
        let kind: BackendKind = name
            .parse()
            .map_err(|_: String| BackendError::UnknownBackend {
                name: name.to_string(),
            })?;

        match kind {
            BackendKind::Azure => {
                let auth = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth)?;
                Ok(Arc::new(backend))
            }
            BackendKind::Local => {
                let backend = if config.runtime_open_existing_local {
                    super::local::LocalBackend::open_existing(config.local.as_ref())?
                } else {
                    super::local::LocalBackend::new(config.local.as_ref())?
                };
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
        let preflight = Self::agent_policy_preflight(config)?;
        let backend = Self::construct_for_kind(kind, config).await?;
        Ok(Self::apply_preflight(backend, preflight))
    }

    async fn construct_for_kind(
        kind: BackendKind,
        config: &Config,
    ) -> Result<Arc<dyn Backend>, BackendError> {
        let backend: Arc<dyn Backend> = match kind {
            BackendKind::Azure => {
                let auth = Self::create_azure_auth_provider(config)?;
                let backend = super::azure::AzureBackend::new(config, auth)?;
                std::sync::Arc::new(backend) as Arc<dyn Backend>
            }
            BackendKind::Local => {
                let backend = if config.runtime_open_existing_local {
                    super::local::LocalBackend::open_existing(config.local.as_ref())?
                } else {
                    super::local::LocalBackend::new(config.local.as_ref())?
                };
                std::sync::Arc::new(backend) as Arc<dyn Backend>
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
                std::sync::Arc::new(backend) as Arc<dyn Backend>
            }
            #[cfg(not(feature = "aws"))]
            BackendKind::Aws => {
                return Err(BackendError::Internal(
                    "AWS backend not compiled in: rebuild with --features aws".into(),
                ));
            }
        };
        Ok(backend)
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
    use crate::secret::domain::SecretValue;

    fn invalid_enforced_local_config(tmp: &tempfile::TempDir) -> Config {
        Config {
            backend: Some("local".into()),
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                git: Some(true),
                ..Default::default()
            }),
            agent: Some(crate::config::settings::AgentConfig {
                enforce: true,
                default_decision: "allow".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn assert_no_local_backend_state(tmp: &tempfile::TempDir) {
        assert!(!tmp.path().join("key.txt").exists());
        assert!(!tmp.path().join("store").exists());
    }

    #[test]
    fn from_config_compiles_enforced_policy_before_local_backend_construction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = BackendRegistry::from_config(&invalid_enforced_local_config(&tmp));
        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert_no_local_backend_state(&tmp);
    }

    #[test]
    fn from_config_opens_decision_log_before_local_backend_construction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = invalid_enforced_local_config(&tmp);
        config.agent.as_mut().unwrap().default_decision = "deny".into();
        let resolution = crate::agent::resolve::Resolution::Resolved(Box::new(
            crate::agent::AgentIdentity::new(
                crate::agent::IdentitySource::EnvAssertion,
                "test-agent",
            ),
        ));

        let result = BackendRegistry::from_config_with_preflight(&config, |config| {
            BackendRegistry::agent_policy_preflight_with(
                config,
                || &resolution,
                || {
                    Err(BackendError::Internal(
                        "injected decision-log failure".into(),
                    ))
                },
            )
        });

        assert!(
            matches!(result, Err(BackendError::Internal(ref message)) if message == "injected decision-log failure")
        );
        assert_no_local_backend_state(&tmp);
    }

    #[test]
    fn lazy_materialize_compiles_enforced_policy_before_local_backend_construction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = invalid_enforced_local_config(&tmp);
        let registry = BackendRegistry::with_lazy(&config, &["local".to_string()]).unwrap();
        let result = registry.materialize("local");
        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert_no_local_backend_state(&tmp);
    }

    #[tokio::test]
    async fn create_for_kind_compiles_enforced_policy_before_local_backend_construction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = BackendRegistry::create_for_kind(
            BackendKind::Local,
            &invalid_enforced_local_config(&tmp),
        )
        .await;
        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert_no_local_backend_state(&tmp);
    }

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
                audit: None,
                git: None,
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
                audit: None,
                git: None,
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
                audit: None,
                git: None,
            }),
        );

        Config {
            named_backends,
            ..Default::default()
        }
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn guarded_registry_preserves_attachment_round_trip() {
        use crate::secret::attachments::{download_decrypted, upload_encrypted};
        let tmp = tempfile::tempdir().unwrap();
        let mut config = two_local_backends_config(&tmp);
        config.backend = Some("local-a".into());
        let registry = BackendRegistry::from_config(&config).unwrap();
        let backend = registry.active();
        backend
            .secrets()
            .set_secret(
                "default",
                crate::secret::domain::SecretRequest {
                    name: "db".into(),
                    value: SecretValue::new("database password"),
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
        assert_eq!(
            backend
                .secrets()
                .get_secret("default", "db", true)
                .await
                .unwrap()
                .value
                .unwrap()
                .expose_secret(),
            "database password"
        );
        let keys = backend.attachment_keys();
        let files = backend.files().unwrap();
        upload_encrypted(
            keys.as_ref(),
            files,
            "default",
            crate::blob::models::FileUploadRequest {
                name: "attachments/db/cert.pem".into(),
                content: b"private certificate".to_vec(),
                content_type: None,
                groups: Vec::new(),
                metadata: Default::default(),
                tags: Default::default(),
            },
            None,
        )
        .await
        .unwrap();
        let content = download_decrypted(
            keys.as_ref(),
            files,
            "default",
            "attachments/db/cert.pem",
            None,
        )
        .await
        .unwrap();
        assert_eq!(content.as_slice(), b"private certificate");
        assert_eq!(
            backend
                .secrets()
                .list_secrets("default", None)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            keys.get_secret("default", "ordinary-secret", true).await,
            Err(BackendError::PermissionDenied(_))
        ));
    }

    // Removing the registry guard must make these operations reach the local
    // store and fail this test, even if every CLI handler keeps its own checks.
    #[tokio::test]
    async fn registry_blocks_reserved_mutations_on_every_resolution_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = two_local_backends_config(&tmp);
        config.backend = Some("local-a".into());
        let eager = BackendRegistry::from_config(&config).unwrap();
        let lazy = BackendRegistry::with_lazy(&config, &["local-b".into()]).unwrap();
        let lazy_backend = lazy.materialize("local-b").unwrap();
        let active_arc = eager.active_arc();
        for backend in [
            eager.active(),
            eager.get("local").unwrap(),
            active_arc.as_ref(),
            lazy_backend.as_ref(),
        ] {
            for name in ["xv-attachment-key", "XV-ATTACHMENT-KEY", "xv_attachment_key",
                "xv-attachment-key-ak1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"] {
                let error = backend.secrets().delete_secret("default", name).await.unwrap_err();
                assert!(matches!(error, BackendError::PermissionDenied(_)), "unguarded {name}: {error}");
            }
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

    #[tokio::test]
    async fn materialize_falls_back_to_eager_backend_map() {
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
                audit: None,
                git: None,
            }),
            ..Default::default()
        };
        let registry = BackendRegistry::from_config(&config).expect("must build");
        let original = registry.backends["local"].clone();
        let materialized = registry
            .materialize("local")
            .expect("eager backend must be materializable by name");
        assert_eq!(materialized.name(), "local");
        assert!(
            Arc::ptr_eq(&original, &materialized),
            "unenforced materialize must return the exact original Arc"
        );

        let raw: Arc<dyn Backend> =
            Arc::new(crate::backend::local::LocalBackend::new(config.local.as_ref()).unwrap());
        let preflight = BackendRegistry::agent_policy_preflight_with(
            &config,
            || panic!("unenforced mode must not resolve identity"),
            || panic!("unenforced mode must not initialize a decision log"),
        )
        .unwrap();
        let unchanged = BackendRegistry::apply_preflight(raw.clone(), preflight);
        assert!(matches!(
            unchanged
                .secrets()
                .delete_secret("default", "xv-attachment-key")
                .await,
            Err(BackendError::PermissionDenied(_))
        ));
    }

    #[tokio::test]
    async fn unenforced_policy_keeps_custody_guard_without_resolving_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let local = crate::config::settings::LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
            default_vault: Some("default".into()),
            ..Default::default()
        };
        let raw: Arc<dyn Backend> =
            Arc::new(crate::backend::local::LocalBackend::new(Some(&local)).unwrap());
        for config in [
            Config {
                backend: Some("local".into()),
                local: Some(local.clone()),
                ..Default::default()
            },
            Config {
                backend: Some("local".into()),
                local: Some(local.clone()),
                agent: Some(crate::config::settings::AgentConfig {
                    enforce: false,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ] {
            let preflight = BackendRegistry::agent_policy_preflight_with(
                &config,
                || panic!("identity resolution must not run while enforcement is disabled"),
                || panic!("decision-log state must not open while enforcement is disabled"),
            )
            .unwrap();
            let unchanged = BackendRegistry::apply_preflight(raw.clone(), preflight);
            assert!(matches!(
                unchanged
                    .secrets()
                    .delete_secret("default", "xv-attachment-key")
                    .await,
                Err(BackendError::PermissionDenied(_))
            ));
        }
    }

    #[tokio::test]
    async fn materialize_returns_enforcement_wrapper_and_denials_fire() {
        let tmp = tempfile::TempDir::new().unwrap();
        let raw_config = crate::config::settings::LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
        };
        let inner: Arc<dyn Backend> =
            Arc::new(crate::backend::local::LocalBackend::new(Some(&raw_config)).unwrap());
        let agent = crate::config::settings::AgentConfig {
            enforce: true,
            ..Default::default()
        };
        let policy = crate::agent::policy::CompiledPolicy::compile(&agent).unwrap();
        let wrapped: Arc<dyn Backend> =
            Arc::new(crate::agent::enforce::PolicyEnforcedBackend::for_test(
                inner,
                crate::agent::AgentIdentity::new(
                    crate::agent::IdentitySource::EnvAssertion,
                    "test-agent",
                ),
                policy,
                tmp.path().join("decisions.jsonl"),
            ));
        let registry = BackendRegistry::new(wrapped);
        let materialized = registry.materialize("local").unwrap();
        let error = materialized
            .secrets()
            .get_secret("default", "anything", true)
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn migration_and_cross_backend_factory_returns_an_enforced_backend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            backend: Some("local".into()),
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                ..Default::default()
            }),
            agent: Some(crate::config::settings::AgentConfig {
                enforce: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolution = crate::agent::resolve::Resolution::Resolved(Box::new(
            crate::agent::AgentIdentity::new(
                crate::agent::IdentitySource::GithubOidc,
                "github:o/r:.github/workflows/ci.yml@refs/heads/main",
            ),
        ));
        let backend = BackendRegistry::create_for_kind_with_resolution_and_log(
            BackendKind::Local,
            &config,
            &resolution,
            tmp.path().join("decisions.jsonl"),
        )
        .await
        .unwrap();

        let error = backend
            .secrets()
            .get_secret("default", "existing", false)
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn enforced_create_for_kind_fails_closed_on_identity_resolution_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            backend: Some("local".into()),
            local: Some(crate::config::settings::LocalConfig {
                store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
                key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
                default_vault: Some("default".into()),
                ..Default::default()
            }),
            agent: Some(crate::config::settings::AgentConfig {
                enforce: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolution = crate::agent::resolve::Resolution::Unresolved(vec![
            crate::agent::resolve::ResolverAttempt {
                source: crate::agent::IdentitySource::GithubOidc,
                reason: "test resolution failure".into(),
            },
        ]);
        let result = BackendRegistry::create_for_kind_with_resolution(
            BackendKind::Local,
            &config,
            &resolution,
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("enforced create_for_kind returned a raw backend"),
        };
        assert!(matches!(error, BackendError::AuthenticationFailed(_)));
        assert!(error.to_string().contains("failing closed"));
        assert!(!tmp.path().join("key.txt").exists());
        assert!(!tmp.path().join("store").exists());
    }
}
