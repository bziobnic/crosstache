//! Vault context management for crosstache
//!
//! This module provides smart vault context detection, allowing users to work
//! with vaults without repeatedly specifying --vault flags.

use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info};

static LEGACY_CONTEXT_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

fn maybe_warn_legacy_context(path: &std::path::Path) {
    if LEGACY_CONTEXT_WARN_EMITTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        eprintln!(
            "warning: legacy .xv/context loaded from {}; consider migrating to .xv.toml — see docs/env-profiles.md",
            path.display()
        );
    }
}

/// Represents a vault context with associated metadata
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultContext {
    /// Current vault name
    pub vault_name: String,
    /// Resource group for the vault
    pub resource_group: Option<String>,
    /// Subscription ID (optional override)
    pub subscription_id: Option<String>,
    /// Storage container name for blob operations (optional override)
    pub storage_container: Option<String>,
    /// Last used timestamp
    pub last_used: chrono::DateTime<chrono::Utc>,
    /// Usage count for prioritization
    // Note: usage_count is best-effort and non-atomic. This is acceptable for a CLI tool
    // (single process) but would need atomic operations in a server context.
    #[serde(default)]
    pub usage_count: u32,
}

impl VaultContext {
    /// Create a new vault context
    pub fn new(
        vault_name: String,
        resource_group: Option<String>,
        subscription_id: Option<String>,
    ) -> Self {
        Self {
            vault_name,
            resource_group,
            subscription_id,
            storage_container: None,
            last_used: chrono::Utc::now(),
            usage_count: 1,
        }
    }

    /// Create a new vault context with storage container
    #[allow(dead_code)]
    pub fn with_storage_container(
        vault_name: String,
        resource_group: Option<String>,
        subscription_id: Option<String>,
        storage_container: Option<String>,
    ) -> Self {
        Self {
            vault_name,
            resource_group,
            subscription_id,
            storage_container,
            last_used: chrono::Utc::now(),
            usage_count: 1,
        }
    }

    /// Update usage timestamp and increment count
    pub fn update_usage(&mut self) {
        self.last_used = chrono::Utc::now();
        self.usage_count = self.usage_count.saturating_add(1);
    }

    /// Check if this context matches the given vault name
    #[allow(dead_code)]
    pub fn matches_vault(&self, vault_name: &str) -> bool {
        self.vault_name == vault_name
    }
}

/// Manages vault contexts with local and global persistence
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextManager {
    /// Current active context
    pub current: Option<VaultContext>,
    /// Recently used contexts (max 10)
    pub recent: Vec<VaultContext>,
    /// Context file path
    #[serde(skip)]
    pub context_file: Option<PathBuf>,
    /// Whether this is a local context (directory-specific)
    #[serde(skip)]
    pub is_local: bool,
    /// Multi-vault workspace state (`xv cx add/rm/default`), if any.
    /// `#[serde(default)]` so pre-workspace context files (missing this
    /// field entirely) still load without error.
    #[serde(default)]
    pub workspace: Option<crate::workspace::WorkspaceState>,
}

impl ContextManager {
    /// Load context from local directory or global config.
    ///
    /// `XV_CONTEXT_DIR` (see `global_context_path`'s doc comment), when set
    /// and non-empty, skips the local (`cwd/.xv/context`) check entirely and
    /// goes straight to the global path — the override means "my context
    /// store lives HERE, full stop," so a `.xv/context` that happens to
    /// exist in the test process's cwd must never take precedence over it.
    pub async fn load() -> Result<Self> {
        // A `current_dir()` failure (e.g. a deleted cwd) degrades to "no
        // local context" here, same as the pre-refactor behavior where
        // `load_local_context`'s own `?` on this call was swallowed by
        // `load`'s `if let Ok(..)` below.
        let cwd = std::env::current_dir().ok();
        Self::load_from(cwd.as_deref()).await
    }

    /// Core of [`Self::load`], parameterized over `cwd` so it's testable
    /// without changing the process-global working directory (which unit
    /// tests can't safely do under parallel `cargo test`). Production code
    /// always goes through [`Self::load`].
    async fn load_from(cwd: Option<&std::path::Path>) -> Result<Self> {
        let context_dir_overridden = std::env::var("XV_CONTEXT_DIR")
            .map(|d| !d.is_empty())
            .unwrap_or(false);
        if !context_dir_overridden {
            // 1. Check for .xv/context in current directory
            if let Some(cwd) = cwd {
                if let Ok(local_context) = Self::load_local_context_from(cwd).await {
                    debug!("Loaded local vault context");
                    return Ok(local_context);
                }
            }
        }

        // 2. Fall back to global context
        debug!("No local context found, loading global context");
        Self::load_global_context().await
    }

    /// Load context from `cwd/.xv/context`.
    async fn load_local_context_from(cwd: &std::path::Path) -> Result<Self> {
        let context_path = cwd.join(".xv").join("context");
        if !context_path.exists() {
            return Err(CrosstacheError::config("No local context found"));
        }

        let content = tokio::fs::read_to_string(&context_path).await?;
        let mut context: ContextManager = serde_json::from_str(&content)?;
        maybe_warn_legacy_context(&context_path);
        context.context_file = Some(context_path);
        context.is_local = true;

        if let Some(ref path) = context.context_file {
            debug!("Loaded local context from: {}", path.display());
        }
        Ok(context)
    }

    /// Load context from global config directory
    async fn load_global_context() -> Result<Self> {
        let context_path = Self::global_context_path()?;
        if !context_path.exists() {
            debug!("No global context file found, using default");
            return Ok(Self {
                context_file: Some(context_path),
                is_local: false,
                ..Default::default()
            });
        }

        let content = tokio::fs::read_to_string(&context_path).await?;
        let mut context: ContextManager = serde_json::from_str(&content)?;
        context.context_file = Some(context_path);
        context.is_local = false;

        if let Some(ref path) = context.context_file {
            debug!("Loaded global context from: {}", path.display());
        }
        Ok(context)
    }

    /// Create a new local context manager
    pub fn new_local() -> Result<Self> {
        let context_path = std::env::current_dir()?.join(".xv").join("context");
        Ok(Self {
            context_file: Some(context_path),
            is_local: true,
            ..Default::default()
        })
    }

    /// Create a new global context manager
    pub fn new_global() -> Result<Self> {
        Ok(Self {
            context_file: Some(Self::global_context_path()?),
            is_local: false,
            ..Default::default()
        })
    }

    /// Save current context
    pub async fn save(&self) -> Result<()> {
        if let Some(ref path) = self.context_file {
            // Ensure parent directory exists with private (0700) permissions —
            // context files hold the user's active vault/subscription state and
            // are treated as user-private config.
            if let Some(parent) = path.parent() {
                crate::utils::helpers::create_private_dir(parent)?;
            }

            let content = serde_json::to_string_pretty(self)?;
            // Route through the sensitive-file writer: atomic 0600 create with
            // O_NOFOLLOW, so the context file is never group/world-readable and
            // a symlinked path cannot redirect the write.
            crate::utils::helpers::write_sensitive_file_async(path, content.as_bytes()).await?;

            debug!("Saved context to: {}", path.display());
        }
        Ok(())
    }

    /// Set current context and update recent list
    pub async fn set_context(&mut self, context: VaultContext) -> Result<()> {
        let vault_name = context.vault_name.clone();

        // Update recent contexts - remove existing entry for this vault
        self.recent.retain(|c| c.vault_name != vault_name);

        // Add current context to recent if we're changing contexts
        if let Some(ref current) = self.current {
            if current.vault_name != vault_name {
                self.recent.insert(0, current.clone());
            }
        }

        // Keep only 10 recent contexts
        self.recent.truncate(10);

        self.current = Some(context);
        self.save().await?;

        info!("Set vault context to: {}", vault_name);
        Ok(())
    }

    /// Update usage timestamp for current context
    pub async fn update_usage(&mut self, vault_name: &str) -> Result<()> {
        let mut updated = false;

        if let Some(ref mut context) = self.current {
            if context.vault_name == vault_name {
                context.update_usage();
                updated = true;
            }
        }

        if updated {
            self.save().await?;
            debug!("Updated usage for vault: {}", vault_name);
        }

        Ok(())
    }

    /// Clear current context
    pub async fn clear_context(&mut self) -> Result<()> {
        if let Some(ref context) = self.current {
            info!("Cleared vault context: {}", context.vault_name);

            // Move current context to recent
            self.recent.insert(0, context.clone());
            self.recent.truncate(10);
        }

        self.current = None;
        self.save().await?;
        Ok(())
    }

    /// Get current vault name from context
    pub fn current_vault(&self) -> Option<&str> {
        self.current.as_ref().map(|c| c.vault_name.as_str())
    }

    /// Get current resource group from context
    pub fn current_resource_group(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.resource_group.as_deref())
    }

    /// Get current subscription ID from context
    pub fn current_subscription_id(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.subscription_id.as_deref())
    }

    /// Get current storage container from context
    #[allow(dead_code)]
    pub fn current_storage_container(&self) -> Option<&str> {
        self.current
            .as_ref()
            .and_then(|c| c.storage_container.as_deref())
    }

    /// List recent contexts sorted by usage
    pub fn list_recent(&self) -> Vec<&VaultContext> {
        let mut recent = self.recent.iter().collect::<Vec<_>>();
        recent.sort_by(|a, b| {
            // Sort by usage count (descending), then by last used (descending)
            b.usage_count
                .cmp(&a.usage_count)
                .then_with(|| b.last_used.cmp(&a.last_used))
        });
        recent
    }

    /// Find context by vault name in recent list
    #[allow(dead_code)]
    pub fn find_recent_context(&self, vault_name: &str) -> Option<&VaultContext> {
        self.recent.iter().find(|c| c.vault_name == vault_name)
    }

    /// Get global context file path.
    ///
    /// `XV_CONTEXT_DIR` (if set and non-empty) overrides the resolved
    /// directory entirely — mirrors `CacheManager::resolve_cache_dir`'s
    /// `XV_CACHE_DIR` precedent (#318). Intended for tests that need an
    /// isolated context store (e.g. a `tempfile::TempDir`) without ever
    /// touching the real `$XDG_CONFIG_HOME`/`$HOME/.config` context file — a
    /// unit test that calls into `resolve_workspace`/`ContextManager::load`
    /// without this override reads whatever workspace happens to be
    /// attached on the machine running the test (#342). A relative
    /// `XV_CONTEXT_DIR` is resolved against the process's current working
    /// directory, which can shift under `cd`/`chdir` — an absolute path is
    /// recommended.
    ///
    /// This override also makes `ContextManager::load` skip the LOCAL
    /// (`cwd/.xv/context`) check entirely, not just the global path — see
    /// `load`'s own doc comment (#342 code review, MINOR).
    fn global_context_path() -> Result<PathBuf> {
        if let Ok(dir) = std::env::var("XV_CONTEXT_DIR") {
            if !dir.is_empty() {
                return Ok(PathBuf::from(dir).join("context"));
            }
        }

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
            Ok(config_dir.join("xv").join("context"))
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Use platform-appropriate config directory for other platforms
            let config_dir = dirs::config_dir()
                .ok_or_else(|| CrosstacheError::config("Could not determine config directory"))?;
            Ok(config_dir.join("xv").join("context"))
        }
    }

    /// Check if local context directory exists
    pub fn local_context_exists() -> bool {
        std::env::current_dir()
            .map(|dir| dir.join(".xv").join("context").exists())
            .unwrap_or(false)
    }

    /// Initialize local context directory
    #[allow(dead_code)]
    pub async fn init_local_context() -> Result<PathBuf> {
        let context_dir = std::env::current_dir()?.join(".xv");
        // 0700 dir — the local context lives under the project's .xv/ but holds
        // the user's active vault state, treated as user-private.
        crate::utils::helpers::create_private_dir(&context_dir)?;

        let context_path = context_dir.join("context");

        // Create empty context file if it doesn't exist
        if !context_path.exists() {
            let empty_context = ContextManager::default();
            let content = serde_json::to_string_pretty(&empty_context)?;
            // 0600, O_NOFOLLOW, atomic — never group/world-readable.
            crate::utils::helpers::write_sensitive_file_async(&context_path, content.as_bytes())
                .await?;
        }

        Ok(context_path)
    }

    /// Get context scope description for display
    pub fn scope_description(&self) -> &'static str {
        if self.is_local {
            "Local (current directory)"
        } else {
            "Global"
        }
    }
}

/// Test-only helpers shared across `#[cfg(test)] mod tests` blocks in
/// DIFFERENT modules of this crate (this module's own tests, plus
/// `crate::cli::secret_ops::tests`) that need to mutate the process-global
/// `XV_CONTEXT_DIR` env var.
///
/// `pub(crate)` (not nested inside a private `mod tests`) so it's visible
/// crate-wide when compiled for test. A single shared lock is required, not
/// one per module: two independently-defined mutexes guarding the SAME env
/// var give each caller a false sense of exclusivity while a test in the
/// other module can still be mutating that var concurrently — cargo runs
/// lib tests in parallel threads by default, so two per-module locks race on
/// `XV_CONTEXT_DIR` exactly like having no lock at all (Bugbot review, LOW,
/// PR #343).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::OnceLock;
    use tokio::sync::Mutex as AsyncMutex;

    /// Serializes tests that mutate the process-global `XV_CONTEXT_DIR` env
    /// var (#342). Any test whose call path reaches
    /// `crate::workspace::resolve_workspace` or
    /// `ContextManager::load`/`load_from`/`new_global` — directly, or
    /// transitively via `Config::resolve_vault_name`/`resolve_vault_for_trait`/
    /// `resolve_workspace_or_default` — reads the REAL global context file
    /// (`$XDG_CONFIG_HOME/xv/context` or `$HOME/.config/xv/context`) unless
    /// `XV_CONTEXT_DIR` is overridden: on a machine with a multi-vault
    /// workspace attached (e.g. the maintainer's, per #341/#342), that
    /// context leaks into the test process and silently changes which
    /// vault/backend these tests resolve against.
    pub(crate) fn context_dir_env_lock() -> &'static AsyncMutex<()> {
        static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| AsyncMutex::new(()))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::context_dir_env_lock;
    use super::*;
    use tempfile::TempDir;
    use tokio;

    #[tokio::test]
    async fn test_vault_context_creation() {
        let context = VaultContext::new(
            "test-vault".to_string(),
            Some("test-rg".to_string()),
            Some("test-sub".to_string()),
        );

        assert_eq!(context.vault_name, "test-vault");
        assert_eq!(context.resource_group, Some("test-rg".to_string()));
        assert_eq!(context.subscription_id, Some("test-sub".to_string()));
        assert_eq!(context.usage_count, 1);
    }

    #[tokio::test]
    async fn test_context_update_usage() {
        let mut context = VaultContext::new("test-vault".to_string(), None, None);

        let initial_count = context.usage_count;
        let initial_time = context.last_used;

        // Small delay to ensure timestamp changes
        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

        context.update_usage();

        assert_eq!(context.usage_count, initial_count + 1);
        assert!(context.last_used > initial_time);
    }

    #[tokio::test]
    async fn test_context_manager_set_and_clear() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().join("context");

        let mut manager = ContextManager {
            context_file: Some(context_path.clone()),
            ..Default::default()
        };

        let context = VaultContext::new("test-vault".to_string(), None, None);

        // Set context
        manager.set_context(context.clone()).await.unwrap();
        assert_eq!(manager.current_vault(), Some("test-vault"));
        assert!(context_path.exists());

        // Clear context
        manager.clear_context().await.unwrap();
        assert_eq!(manager.current_vault(), None);
        assert_eq!(manager.recent.len(), 1);
        assert_eq!(manager.recent[0].vault_name, "test-vault");
    }

    #[tokio::test]
    async fn test_recent_contexts_limit() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().join("context");

        let mut manager = ContextManager {
            context_file: Some(context_path),
            ..Default::default()
        };

        // Add 12 contexts (more than the limit of 10)
        for i in 0..12 {
            let context = VaultContext::new(format!("vault-{i}"), None, None);
            manager.set_context(context).await.unwrap();
        }

        // Should only keep 10 recent contexts
        assert!(manager.recent.len() <= 10);
    }

    #[test]
    fn test_context_matching() {
        let context = VaultContext::new("my-vault".to_string(), None, None);

        assert!(context.matches_vault("my-vault"));
        assert!(!context.matches_vault("other-vault"));
    }

    #[tokio::test]
    async fn test_vault_context_with_storage_container() {
        let context = VaultContext::with_storage_container(
            "test-vault".to_string(),
            Some("test-rg".to_string()),
            Some("test-sub".to_string()),
            Some("my-container".to_string()),
        );

        assert_eq!(context.vault_name, "test-vault");
        assert_eq!(context.resource_group, Some("test-rg".to_string()));
        assert_eq!(context.subscription_id, Some("test-sub".to_string()));
        assert_eq!(context.storage_container, Some("my-container".to_string()));
        assert_eq!(context.usage_count, 1);
    }

    #[tokio::test]
    async fn test_current_storage_container() {
        let temp_dir = TempDir::new().unwrap();
        let context_path = temp_dir.path().join("context");

        let mut manager = ContextManager {
            context_file: Some(context_path),
            ..Default::default()
        };

        // Test with no context
        assert_eq!(manager.current_storage_container(), None);

        // Test with context without storage container
        let context1 = VaultContext::new("test-vault".to_string(), None, None);
        manager.set_context(context1).await.unwrap();
        assert_eq!(manager.current_storage_container(), None);

        // Test with context with storage container
        let context2 = VaultContext::with_storage_container(
            "test-vault-2".to_string(),
            None,
            None,
            Some("my-container".to_string()),
        );
        manager.set_context(context2).await.unwrap();
        assert_eq!(manager.current_storage_container(), Some("my-container"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_saved_context_file_is_private_0600() {
        use std::os::unix::fs::PermissionsExt;
        let temp_dir = TempDir::new().unwrap();
        // Nest under a subdir so the private-dir creation path is exercised too.
        let context_path = temp_dir.path().join("nested").join("context");

        let manager = ContextManager {
            context_file: Some(context_path.clone()),
            ..Default::default()
        };
        manager.save().await.unwrap();

        let mode = std::fs::metadata(&context_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "context file must be owner-only (0600), got {:03o}",
            mode & 0o777
        );
        // Parent directory must be 0700.
        let dir_mode = std::fs::metadata(context_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o777,
            0o700,
            "context dir must be owner-only (0700), got {:03o}",
            dir_mode & 0o777
        );
    }

    /// A pre-workspace context JSON file (no `workspace` key at all) must
    /// still load cleanly — `#[serde(default)]` on the new field.
    #[test]
    fn legacy_context_json_without_workspace_field_loads() {
        let legacy = r#"{
            "current": {
                "vault_name": "myvault",
                "resource_group": null,
                "subscription_id": null,
                "storage_container": null,
                "last_used": "2024-01-01T00:00:00Z",
                "usage_count": 1
            },
            "recent": []
        }"#;
        let manager: ContextManager =
            serde_json::from_str(legacy).expect("legacy context JSON must still deserialize");
        assert_eq!(manager.current_vault(), Some("myvault"));
        assert!(manager.workspace.is_none());
    }

    /// RAII guard that sets an env var for its lifetime and restores the
    /// previous value (or removes it, if previously unset) on drop.
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// #342 code review (MINOR): `XV_CONTEXT_DIR` must override the LOCAL
    /// (`cwd/.xv/context`) check too, not just the global path — the
    /// override means "my context store lives HERE, full stop." Uses
    /// `load_from` (the cwd-parameterized testable core) instead of
    /// `load()` + `set_current_dir`, so this never touches the real
    /// process-global cwd.
    #[tokio::test]
    async fn xv_context_dir_override_skips_local_context_check() {
        let _env_guard = context_dir_env_lock().lock().await;

        // A `.xv/context` planted in what would be the "local" cwd, with a
        // distinct current vault — if this were ever read, the assertion
        // below would see "local-current" instead of the override's vault.
        let local_root = TempDir::new().unwrap();
        let local_xv_dir = local_root.path().join(".xv");
        std::fs::create_dir_all(&local_xv_dir).unwrap();
        std::fs::write(
            local_xv_dir.join("context"),
            r#"{"current":{"vault_name":"local-current","resource_group":null,"subscription_id":null,"storage_container":null,"last_used":"2024-01-01T00:00:00Z","usage_count":1},"recent":[]}"#,
        )
        .unwrap();

        // The XV_CONTEXT_DIR override target, with its OWN context file.
        let override_dir = TempDir::new().unwrap();
        std::fs::write(
            override_dir.path().join("context"),
            r#"{"current":{"vault_name":"override-current","resource_group":null,"subscription_id":null,"storage_container":null,"last_used":"2024-01-01T00:00:00Z","usage_count":1},"recent":[]}"#,
        )
        .unwrap();

        let _context_dir_guard = EnvVarGuard::set("XV_CONTEXT_DIR", override_dir.path());

        let loaded = ContextManager::load_from(Some(local_root.path()))
            .await
            .expect("load_from must succeed");

        assert_eq!(
            loaded.current_vault(),
            Some("override-current"),
            "XV_CONTEXT_DIR must win over a .xv/context present in cwd, not just supplement it"
        );
        assert!(
            !loaded.is_local,
            "a context loaded via the XV_CONTEXT_DIR override must be reported as global, \
             never local"
        );
    }
}
