//! Configuration settings management
//!
//! This module handles loading configuration from multiple sources,
//! validation, and persistence.

use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use tabled::Tabled;

// ---------------------------------------------------------------------------
// Local backend configuration (Phase 2)
// ---------------------------------------------------------------------------

/// Configuration for the local age-encrypted file backend.
///
/// Lives under `[local]` in `xv.conf`. Only relevant when `backend = "local"`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalConfig {
    /// Root directory for the encrypted secret store.
    /// Defaults to `$XDG_DATA_HOME/xv/store` (or `~/.local/share/xv/store`).
    #[serde(default)]
    pub store_path: Option<String>,

    /// Path to the age identity (private key) file.
    /// Defaults to `$XDG_CONFIG_HOME/xv/age-key.txt`.
    #[serde(default)]
    pub key_file: Option<String>,

    /// Default vault name used when no `--vault` / context is set.
    #[serde(default)]
    pub default_vault: Option<String>,

    /// Encrypt secret metadata (`.meta.json`) at rest using the same age
    /// recipients as secret values. When `false` (the default, for backward
    /// compatibility), metadata — note, tags, folder, expiry, content-type —
    /// is stored as plaintext JSON. Secret *names* remain visible as on-disk
    /// filenames regardless of this setting. After enabling, run
    /// `xv local encrypt-metadata` to convert existing plaintext metadata.
    #[serde(default)]
    pub encrypt_metadata: Option<bool>,

    /// Make on-disk filenames opaque so a directory listing reveals no secret
    /// names. When `false` (the default, for backward compatibility), secret
    /// names are stored verbatim as URL-encoded filenames. When `true`, each
    /// secret's files are named by a keyed hash (HMAC-SHA256 over the secret
    /// name, base32) and an age-encrypted `.index.age` maps stems back to
    /// names. After enabling, run `xv local migrate` to convert an existing
    /// store; new writes upgrade each secret's layout on touch.
    #[serde(default)]
    pub opaque_filenames: Option<bool>,
}

/// Configuration for the AWS Secrets Manager backend.
///
/// Lives under `[aws]` in `xv.conf`. Only relevant when `backend = "aws"`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AwsConfig {
    /// AWS region, e.g. `us-east-1`. Falls through to `AWS_REGION` env var.
    #[serde(default)]
    pub region: Option<String>,

    /// AWS profile name. Falls through to `AWS_PROFILE`. Defaults to "default".
    #[serde(default)]
    pub profile: Option<String>,

    /// Optional endpoint URL override. Used for LocalStack and other AWS-compatible APIs.
    #[serde(default)]
    pub endpoint_url: Option<String>,

    /// Default vault name (= prefix) used when no `--vault` / context is set.
    #[serde(default)]
    pub default_vault: Option<String>,

    /// S3 bucket used for `xv file` storage. Files are stored under
    /// `<vault>/files/<name>` so vaults stay isolated. Falls through to the
    /// `XV_AWS_S3_BUCKET` env var. File operations error with a setup hint
    /// when unset; the bucket is never auto-created.
    #[serde(default)]
    pub s3_bucket: Option<String>,
}

/// A named backend entry in `Config.named_backends`. Each entry is a
/// fully-self-contained backend configuration tagged with its type.
///
/// Used for multi-region AWS, multi-tenant Azure, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NamedBackendEntry {
    Aws(AwsConfig),
    Local(LocalConfig),
    // Azure intentionally omitted from this enum for now; existing
    // top-level Azure fields handle the single-instance case.
}

/// Azure credential type priority for authentication
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AzureCredentialType {
    /// Use Azure CLI credentials first
    Cli,
    /// Use Managed Identity credentials first
    ManagedIdentity,
    /// Use environment variable credentials first
    Environment,
    /// Use the default credential chain order
    #[default]
    Default,
}

impl fmt::Display for AzureCredentialType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli => write!(f, "cli"),
            Self::ManagedIdentity => write!(f, "managed_identity"),
            Self::Environment => write!(f, "environment"),
            Self::Default => write!(f, "default"),
        }
    }
}

impl std::str::FromStr for AzureCredentialType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cli" | "azure-cli" | "az" => Ok(Self::Cli),
            "managed_identity" | "managed-identity" | "msi" => Ok(Self::ManagedIdentity),
            "environment" | "env" => Ok(Self::Environment),
            "default" => Ok(Self::Default),
            _ => Err(format!("Invalid credential type: {s}. Valid options: cli, managed_identity, environment, default")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobConfig {
    pub storage_account: String,
    pub container_name: String,
    pub endpoint: Option<String>,
    pub enable_large_file_support: bool,
    pub chunk_size_mb: usize,
    pub max_concurrent_uploads: usize,
    #[serde(default = "default_progress_threshold_mb")]
    pub progress_threshold_mb: usize,
}

fn default_progress_threshold_mb() -> usize {
    5
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            storage_account: String::new(),
            container_name: "crosstache-files".to_string(),
            endpoint: None,
            enable_large_file_support: true,
            chunk_size_mb: 4,
            max_concurrent_uploads: 3,
            progress_threshold_mb: default_progress_threshold_mb(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct Config {
    /// Active backend: `"azure"` (default) or `"local"`.
    /// Missing / `None` is treated as `"azure"` for backward compatibility.
    #[tabled(skip)]
    #[serde(default)]
    pub backend: Option<String>,

    #[tabled(rename = "Debug")]
    pub debug: bool,
    #[tabled(rename = "Subscription ID")]
    pub subscription_id: String,
    #[tabled(rename = "Default Vault")]
    pub default_vault: String,
    #[tabled(rename = "Default Resource Group")]
    pub default_resource_group: String,
    #[tabled(rename = "Default Location")]
    pub default_location: String,
    #[tabled(skip)]
    pub tenant_id: String,
    /// Whether client-side caching is enabled for listing operations
    #[tabled(rename = "Cache Enabled")]
    #[serde(default = "default_cache_enabled")]
    pub cache_enabled: bool,
    /// Cache time-to-live in seconds (0 to disable)
    #[tabled(rename = "Cache TTL")]
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    #[tabled(rename = "JSON Output")]
    pub output_json: bool,
    /// Resolved global `--format` after `auto` / TTY handling (set in `Cli::execute`, not persisted).
    #[serde(skip)]
    #[tabled(skip)]
    pub runtime_output_format: OutputFormat,
    /// Custom template string for `--format template` (set in `Cli::execute`, not persisted).
    #[serde(skip)]
    #[tabled(skip)]
    pub template: Option<String>,
    /// True when the user passed an explicit `--format` (not `auto`).
    /// Set in `Cli::execute`, not persisted.
    #[serde(skip)]
    #[tabled(skip)]
    pub format_explicit: bool,
    /// Parsed global `--columns` selection (set in `Cli::execute`, not persisted).
    /// Applies to table/plain/csv renders of every `TableFormatter` consumer.
    #[serde(skip)]
    #[tabled(skip)]
    pub runtime_columns: Option<Vec<String>>,
    #[tabled(rename = "No Color")]
    pub no_color: bool,
    #[tabled(skip)]
    pub blob_config: Option<BlobConfig>,
    /// Azure credential type to use first for authentication
    /// Controls the order in which credentials are attempted
    #[tabled(rename = "Credential Priority")]
    #[serde(default)]
    pub azure_credential_priority: AzureCredentialType,
    /// Configuration for the local age-encrypted file backend.
    /// Only relevant when `backend = "local"`.
    #[tabled(skip)]
    #[serde(default)]
    pub local: Option<LocalConfig>,
    /// Configuration for the AWS Secrets Manager backend.
    /// Only relevant when `backend = "aws"`.
    #[tabled(skip)]
    #[serde(default)]
    pub aws: Option<AwsConfig>,
    /// Named backend instances for multi-region / multi-tenant use.
    /// Active backend selected via `Config.backend` matching a key here.
    #[tabled(skip)]
    #[serde(default)]
    pub named_backends: std::collections::HashMap<String, NamedBackendEntry>,
    /// Seconds before clipboard is automatically cleared (0 to disable)
    #[tabled(rename = "Clipboard Timeout")]
    #[serde(default = "default_clipboard_timeout")]
    pub clipboard_timeout: u64,
    /// Default character set for the `gen` command.
    /// Valid values: alphanumeric, alphanumeric-symbols, hex, base64, numeric, uppercase, lowercase
    /// If absent, `gen` defaults to alphanumeric.
    #[tabled(skip)]
    #[serde(default)]
    pub gen_default_charset: Option<String>,
    /// CLI `--env` flag override for active env in `.xv.toml`. Set
    /// once in main.rs from `cli.env`. Lower priority than the
    /// `XV_ENV` env var.
    #[serde(skip)]
    #[tabled(skip)]
    pub env_flag: Option<String>,

    /// Original `--backend` CLI flag value, BEFORE `resolve_effective_backend`
    /// collapses every layer into `self.backend`. Set once in main.rs from
    /// `cli.backend.clone()`. Used only by `xv config show --resolved` to
    /// attribute the winning layer correctly when CLI/profile/env values
    /// happen to coincide.
    ///
    /// Note: clap auto-populates `cli.backend` from `XV_BACKEND` too, so this
    /// alone cannot distinguish CLI from env var — `cli_backend_was_arg`
    /// is required for that.
    #[serde(skip)]
    #[tabled(skip)]
    pub cli_backend: Option<String>,

    /// `true` when `--backend` was passed on the command line (vs. populated
    /// from `XV_BACKEND` via clap's `env = ...`). Set in main.rs by checking
    /// `std::env::args_os()`. Lets `--resolved` distinguish "won by CLI flag"
    /// from "won by env var" even when both supply the same string.
    #[serde(skip)]
    #[tabled(skip)]
    pub cli_backend_was_arg: bool,

    /// Backend value loaded from the on-disk global config (`xv.conf`), BEFORE
    /// main.rs overwrites `self.backend` with the resolved value. Set once
    /// in main.rs by snapshotting `config.backend` immediately after
    /// `Config::load_or_create`. Used only by `xv config show --resolved`
    /// to detect the "no source set anything → built-in default" case.
    #[serde(skip)]
    #[tabled(skip)]
    pub disk_backend: Option<String>,
}

fn default_clipboard_timeout() -> u64 {
    30
}

fn default_cache_enabled() -> bool {
    true
}

fn default_cache_ttl_secs() -> u64 {
    900
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: None,
            debug: false,
            subscription_id: String::new(),
            default_vault: String::new(),
            default_resource_group: "Vaults".to_string(),
            default_location: "eastus".to_string(),
            tenant_id: String::new(),
            cache_enabled: default_cache_enabled(),
            cache_ttl_secs: default_cache_ttl_secs(),
            output_json: false,
            runtime_output_format: OutputFormat::Auto,
            template: None,
            format_explicit: false,
            runtime_columns: None,
            no_color: false,
            blob_config: None,
            azure_credential_priority: AzureCredentialType::Default,
            local: None,
            aws: None,
            named_backends: std::collections::HashMap::new(),
            clipboard_timeout: default_clipboard_timeout(),
            gen_default_charset: None,
            env_flag: None,
            cli_backend: None,
            cli_backend_was_arg: false,
            disk_backend: None,
        }
    }
}

impl Config {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        let backend = self.effective_backend_name();
        if backend == "azure" {
            if self.subscription_id.is_empty() {
                return Err(CrosstacheError::config("Subscription ID is required"));
            }
            if self.tenant_id.is_empty() {
                return Err(CrosstacheError::config("Tenant ID is required"));
            }
        }
        if backend == "aws" {
            let aws = self.aws.as_ref().ok_or_else(|| {
                CrosstacheError::config("[aws] config block is required when backend = \"aws\"")
            })?;
            if aws.region.is_none()
                && std::env::var("AWS_REGION").is_err()
                && std::env::var("AWS_DEFAULT_REGION").is_err()
            {
                return Err(CrosstacheError::config(
                    "AWS region required: set [aws].region in config or AWS_REGION env var",
                ));
            }
        }
        Ok(())
    }

    pub fn get_config_path() -> Result<PathBuf> {
        // Use XDG Base Directory specification on Linux and macOS
        // On Windows, use the platform-appropriate config directory
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::env;
            let config_dir = if let Ok(xdg_config_home) = env::var("XDG_CONFIG_HOME") {
                PathBuf::from(xdg_config_home)
            } else {
                let home_dir = env::var("HOME")
                    .map_err(|_| CrosstacheError::config("HOME environment variable not set"))?;
                PathBuf::from(home_dir).join(".config")
            };
            Ok(config_dir.join("xv").join("xv.conf"))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Use platform-appropriate config directory for other platforms
            let config_dir = dirs::config_dir()
                .ok_or_else(|| CrosstacheError::config("Unable to determine config directory"))?;
            Ok(config_dir.join("xv").join("xv.conf"))
        }
    }

    #[allow(dead_code)]
    pub async fn load() -> Result<Self> {
        load_config().await
    }

    pub async fn save(&self) -> Result<()> {
        save_config(self).await
    }

    /// Return the effective backend name as a string.
    ///
    /// Resolves `self.backend` (an `Option<String>`) to a definite value.
    /// `None` is treated as `"azure"` for backward compatibility.
    #[allow(dead_code)]
    pub fn effective_backend_name(&self) -> &str {
        self.backend.as_deref().unwrap_or("azure")
    }

    /// Resolve vault name with context awareness
    /// Priority: CLI argument > .xv.toml env profile > context > config default
    pub async fn resolve_vault_name(&self, vault_arg: Option<String>) -> Result<String> {
        use crate::config::{project, ContextManager};

        // 1. Command line argument takes precedence
        if let Some(vault) = vault_arg {
            return Ok(vault);
        }

        // 2. Project config (.xv.toml) — walk up from cwd
        let cwd = std::env::current_dir()?;
        if let Ok(Some((path, cfg))) = project::find_project_config(&cwd).await {
            // resolve_env returns Err on unknown-env — propagate so the
            // user sees the helpful EnvNotDefined message with the list
            // of available envs.
            let (name, profile) = project::resolve_env(&cfg, self.env_flag.as_deref())?;
            // Emit the cross-boundary notice if the .xv.toml lives
            // above cwd. Suppressed by XV_NO_PARENT_CONFIG=1 since
            // walk-up wouldn't have reached the ancestor anyway —
            // but keep this branch defensive.
            if path.parent().map(|p| p != cwd.as_path()).unwrap_or(false) {
                if let Some(line) = project::capture_cross_boundary_notice(&path, name) {
                    eprintln!("{line}");
                }
            }
            if let Some(v) = profile.vault.as_deref() {
                return Ok(v.to_string());
            }
            // Profile defines no vault — fall through to context/config.
        }

        // 3. Check local/global context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(vault_name) = context_manager.current_vault() {
            return Ok(vault_name.to_string());
        }

        // 4. Fall back to config default
        if !self.default_vault.is_empty() {
            return Ok(self.default_vault.clone());
        }

        Err(CrosstacheError::config(
            "No vault specified. Use --vault, set context with 'xv context use', or configure default_vault"
        ))
    }

    /// Resolve resource group with context awareness
    /// Priority: CLI argument > .xv.toml env profile > context > config default
    #[allow(dead_code)]
    pub async fn resolve_resource_group(&self, rg_arg: Option<String>) -> Result<String> {
        use crate::config::{project, ContextManager};

        // 1. Command line argument takes precedence
        if let Some(rg) = rg_arg {
            return Ok(rg);
        }

        // 2. Project config (.xv.toml) — walk up from cwd
        let cwd = std::env::current_dir()?;
        if let Ok(Some((path, cfg))) = project::find_project_config(&cwd).await {
            let (name, profile) = project::resolve_env(&cfg, self.env_flag.as_deref())?;
            // Emit the cross-boundary notice if the .xv.toml lives
            // above cwd. Suppressed by XV_NO_PARENT_CONFIG=1 since
            // walk-up wouldn't have reached the ancestor anyway —
            // but keep this branch defensive.
            if path.parent().map(|p| p != cwd.as_path()).unwrap_or(false) {
                if let Some(line) = project::capture_cross_boundary_notice(&path, name) {
                    eprintln!("{line}");
                }
            }
            if let Some(rg) = profile.resource_group.as_deref() {
                return Ok(rg.to_string());
            }
        }

        // 3. Check context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(rg) = context_manager.current_resource_group() {
            return Ok(rg.to_string());
        }

        // 4. Fall back to config default
        if !self.default_resource_group.is_empty() {
            return Ok(self.default_resource_group.clone());
        }

        Err(CrosstacheError::config("No resource group specified"))
    }

    /// Resolve subscription ID with context awareness
    /// Priority: CLI argument > context > config default
    #[allow(dead_code)]
    pub async fn resolve_subscription_id(&self, sub_arg: Option<String>) -> Result<String> {
        use crate::config::ContextManager;

        // 1. Command line argument takes precedence
        if let Some(sub) = sub_arg {
            return Ok(sub);
        }

        // 2. Check context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(sub) = context_manager.current_subscription_id() {
            return Ok(sub.to_string());
        }

        // 3. Fall back to config default
        if !self.subscription_id.is_empty() {
            return Ok(self.subscription_id.clone());
        }

        Err(CrosstacheError::config("No subscription ID specified"))
    }

    /// Get blob storage configuration, creating default if not present
    pub fn get_blob_config(&self) -> BlobConfig {
        self.blob_config.clone().unwrap_or_default()
    }

    /// Set blob storage configuration
    pub fn set_blob_config(&mut self, blob_config: BlobConfig) {
        self.blob_config = Some(blob_config);
    }

    /// Check if blob storage is configured
    #[allow(dead_code)]
    pub fn is_blob_storage_configured(&self) -> bool {
        self.blob_config
            .as_ref()
            .map(|config| !config.storage_account.is_empty())
            .unwrap_or(false)
    }

    /// Get storage account endpoint URL
    #[allow(dead_code)]
    pub fn get_storage_endpoint(&self) -> Option<String> {
        self.blob_config.as_ref().and_then(|config| {
            config.endpoint.clone().or_else(|| {
                if !config.storage_account.is_empty() {
                    Some(format!(
                        "https://{}.blob.core.windows.net",
                        config.storage_account
                    ))
                } else {
                    None
                }
            })
        })
    }
}

/// Load configuration from multiple sources with priority order:
/// 1. Command-line flags (handled by clap)
/// 2. Environment variables
/// 3. Configuration file
/// 4. Default values
pub async fn load_config() -> Result<Config> {
    let config = load_config_no_validation().await?;

    // Validate configuration
    config.validate()?;

    Ok(config)
}

/// Load configuration without validation (for init and config commands)
pub async fn load_config_no_validation() -> Result<Config> {
    let mut config = Config::default();

    // Load from configuration file if it exists
    let config_path = Config::get_config_path()?;
    if config_path.exists() {
        config = load_from_file(&config_path).await?;
    }

    // Override with environment variables
    load_from_env(&mut config);

    Ok(config)
}

async fn load_from_file(path: &PathBuf) -> Result<Config> {
    let contents = tokio::fs::read_to_string(path).await?;

    // Try to parse as TOML first, then JSON as fallback.
    // If both fail, return the TOML error — the file is likely TOML with a syntax error.
    let toml_err = match toml::from_str::<Config>(&contents) {
        Ok(config) => return Ok(config),
        Err(e) => {
            tracing::debug!("TOML parse failed: {}, trying JSON", e);
            e
        }
    };

    serde_json::from_str::<Config>(&contents).map_err(|_json_err| {
        CrosstacheError::config(format!(
            "Failed to parse config file '{}': {}. Run 'xv init' to create a valid configuration.",
            path.display(),
            toml_err
        ))
    })
}

fn load_from_env(config: &mut Config) {
    // Backend override from environment variable
    if let Ok(value) = std::env::var("XV_BACKEND") {
        config.backend = Some(value);
    }

    if let Ok(value) = std::env::var("DEBUG") {
        config.debug = value.to_lowercase() == "true" || value == "1";
    }

    if std::env::var("NO_COLOR").is_ok() {
        config.no_color = true;
    }

    if let Ok(value) = std::env::var("AZURE_SUBSCRIPTION_ID") {
        config.subscription_id = value;
    }

    if let Ok(value) = std::env::var("DEFAULT_VAULT") {
        config.default_vault = value;
    }

    if let Ok(value) = std::env::var("DEFAULT_RESOURCE_GROUP") {
        config.default_resource_group = value;
    }

    if let Ok(value) = std::env::var("AZURE_TENANT_ID") {
        config.tenant_id = value;
    }

    if let Ok(value) = std::env::var("CACHE_ENABLED") {
        config.cache_enabled = value.to_lowercase() == "true" || value == "1";
    }

    if let Ok(value) = std::env::var("CACHE_TTL") {
        if let Ok(seconds) = value.parse::<u64>() {
            config.cache_ttl_secs = seconds;
        }
    }

    // Load Azure credential priority from environment variable
    if let Ok(value) = std::env::var("AZURE_CREDENTIAL_PRIORITY") {
        if let Ok(cred_type) = value.parse::<AzureCredentialType>() {
            config.azure_credential_priority = cred_type;
        }
    }

    // Load blob storage configuration from environment variables
    let mut blob_config = config.blob_config.clone().unwrap_or_default();
    let mut blob_config_updated = false;

    // Check if we have existing config from file
    let had_existing_config = config.blob_config.is_some();

    if let Ok(value) = std::env::var("AZURE_STORAGE_ACCOUNT") {
        blob_config.storage_account = value;
        blob_config_updated = true;
    }

    if let Ok(value) = std::env::var("AZURE_STORAGE_CONTAINER") {
        blob_config.container_name = value;
        blob_config_updated = true;
    }

    if let Ok(value) = std::env::var("AZURE_STORAGE_ENDPOINT") {
        blob_config.endpoint = Some(value);
        blob_config_updated = true;
    }

    if let Ok(value) = std::env::var("BLOB_CHUNK_SIZE_MB") {
        if let Ok(chunk_size) = value.parse::<usize>() {
            blob_config.chunk_size_mb = chunk_size;
            blob_config_updated = true;
        }
    }

    if let Ok(value) = std::env::var("BLOB_MAX_CONCURRENT_UPLOADS") {
        if let Ok(max_uploads) = value.parse::<usize>() {
            blob_config.max_concurrent_uploads = max_uploads;
            blob_config_updated = true;
        }
    }

    if let Ok(value) = std::env::var("PROGRESS_THRESHOLD_MB") {
        if let Ok(threshold) = value.parse::<usize>() {
            blob_config.progress_threshold_mb = threshold;
            blob_config_updated = true;
        }
    }

    // Set blob_config if we have existing config OR if updated by env vars
    if had_existing_config || blob_config_updated {
        config.blob_config = Some(blob_config);
    }
}

pub async fn save_config(config: &Config) -> Result<()> {
    let config_path = Config::get_config_path()?;

    // Create parent directories if they don't exist
    if let Some(parent) = config_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Serialize to TOML format
    let contents = toml::to_string_pretty(config)
        .map_err(|e| CrosstacheError::serialization(e.to_string()))?;

    crate::utils::helpers::write_sensitive_file_async(&config_path, contents.as_bytes()).await?;

    Ok(())
}

#[allow(dead_code)]
pub async fn init_default_config() -> Result<()> {
    let config_path = Config::get_config_path()?;

    // Don't overwrite existing configuration
    if config_path.exists() {
        return Ok(());
    }

    let config = Config::default();
    save_config(&config).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{NamedBackendEntry, *};

    #[test]
    fn test_azure_credential_type_from_str() {
        use std::str::FromStr;

        // Test valid credential types
        assert_eq!(
            AzureCredentialType::from_str("cli").unwrap(),
            AzureCredentialType::Cli
        );
        assert_eq!(
            AzureCredentialType::from_str("CLI").unwrap(),
            AzureCredentialType::Cli
        );
        assert_eq!(
            AzureCredentialType::from_str("azure-cli").unwrap(),
            AzureCredentialType::Cli
        );
        assert_eq!(
            AzureCredentialType::from_str("az").unwrap(),
            AzureCredentialType::Cli
        );

        assert_eq!(
            AzureCredentialType::from_str("managed_identity").unwrap(),
            AzureCredentialType::ManagedIdentity
        );
        assert_eq!(
            AzureCredentialType::from_str("managed-identity").unwrap(),
            AzureCredentialType::ManagedIdentity
        );
        assert_eq!(
            AzureCredentialType::from_str("msi").unwrap(),
            AzureCredentialType::ManagedIdentity
        );

        assert_eq!(
            AzureCredentialType::from_str("environment").unwrap(),
            AzureCredentialType::Environment
        );
        assert_eq!(
            AzureCredentialType::from_str("env").unwrap(),
            AzureCredentialType::Environment
        );

        assert_eq!(
            AzureCredentialType::from_str("default").unwrap(),
            AzureCredentialType::Default
        );

        // Test invalid credential types
        assert!(AzureCredentialType::from_str("invalid").is_err());
        assert!(AzureCredentialType::from_str("unknown").is_err());
        assert!(AzureCredentialType::from_str("").is_err());
    }

    #[test]
    fn test_azure_credential_type_display() {
        assert_eq!(AzureCredentialType::Cli.to_string(), "cli");
        assert_eq!(
            AzureCredentialType::ManagedIdentity.to_string(),
            "managed_identity"
        );
        assert_eq!(AzureCredentialType::Environment.to_string(), "environment");
        assert_eq!(AzureCredentialType::Default.to_string(), "default");
    }

    #[test]
    fn test_azure_credential_type_default() {
        assert_eq!(AzureCredentialType::default(), AzureCredentialType::Default);
    }

    #[test]
    fn test_config_with_credential_priority() {
        let mut config = Config::default();
        assert_eq!(
            config.azure_credential_priority,
            AzureCredentialType::Default
        );

        config.azure_credential_priority = AzureCredentialType::Cli;
        assert_eq!(config.azure_credential_priority, AzureCredentialType::Cli);
    }

    #[test]
    fn test_gen_default_charset_defaults_to_none() {
        let config = Config::default();
        assert!(config.gen_default_charset.is_none());
    }

    #[test]
    fn test_gen_default_charset_serde_round_trip() {
        let config = Config {
            gen_default_charset: Some("alphanumeric-symbols".to_string()),
            ..Default::default()
        };

        let serialized = toml::to_string_pretty(&config).unwrap();
        assert!(
            serialized.contains("gen_default_charset"),
            "field must be present in serialized output: {serialized}"
        );

        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized.gen_default_charset.as_deref(),
            Some("alphanumeric-symbols")
        );
    }

    #[test]
    fn test_gen_default_charset_absent_in_toml_is_none() {
        let toml = r#"
            debug = false
            subscription_id = ""
            default_vault = ""
            default_resource_group = "Vaults"
            default_location = "eastus"
            tenant_id = ""

            output_json = false
            no_color = false
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.gen_default_charset.is_none());
    }

    #[test]
    fn test_cache_config_defaults() {
        let config = Config::default();
        assert!(config.cache_enabled);
        assert_eq!(config.cache_ttl_secs, 900);
    }

    #[test]
    fn test_cache_config_serde_round_trip() {
        let config = Config {
            cache_enabled: false,
            cache_ttl_secs: 600,
            ..Default::default()
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        assert!(serialized.contains("cache_enabled"));
        assert!(serialized.contains("cache_ttl_secs"));
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert!(!deserialized.cache_enabled);
        assert_eq!(deserialized.cache_ttl_secs, 600);
    }

    #[test]
    fn test_cache_config_absent_in_toml_uses_defaults() {
        let toml = r#"
            debug = false
            subscription_id = ""
            default_vault = ""
            default_resource_group = "Vaults"
            default_location = "eastus"
            tenant_id = ""

            output_json = false
            no_color = false
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.cache_enabled);
        assert_eq!(config.cache_ttl_secs, 900);
    }

    #[test]
    fn validate_requires_aws_block_when_backend_is_aws() {
        let cfg = Config {
            backend: Some("aws".into()),
            aws: None,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("aws"), "got: {err}");
    }

    #[test]
    fn validate_passes_when_aws_block_present_with_region() {
        let cfg = Config {
            backend: Some("aws".into()),
            aws: Some(AwsConfig {
                region: Some("us-east-1".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn named_backends_deserializes_aws_entry() {
        let toml_str = r#"
backend = "aws-east"
debug = false
subscription_id = ""
default_vault = ""
default_resource_group = "Vaults"
default_location = "eastus"
tenant_id = ""
output_json = false
no_color = false

[named_backends.aws-east]
type = "aws"
region = "us-east-1"
profile = "prod"
default_vault = "myproj-kv"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.backend.as_deref(), Some("aws-east"));
        let entry = cfg.named_backends.get("aws-east").unwrap();
        match entry {
            NamedBackendEntry::Aws(aws) => {
                assert_eq!(aws.region.as_deref(), Some("us-east-1"));
                assert_eq!(aws.profile.as_deref(), Some("prod"));
            }
            _ => panic!("expected Aws variant"),
        }
    }
}
