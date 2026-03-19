# Cache Feature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a client-side caching layer for `xv ls`, `xv vault list`, and `xv file list` that stores results as flat JSON files, supports configurable TTL with background refresh, and eagerly invalidates on write operations.

**Architecture:** New `src/cache/` module with `manager.rs` (CacheManager API), `models.rs` (data types), and `refresh.rs` (background refresh via detached child process). Cache entries are per-vault flat JSON files stored in the platform cache directory. The existing `commands.rs` is modified to check cache before Azure API calls on reads, and invalidate cache after writes.

**Tech Stack:** Rust, serde_json, chrono, tokio, std::fs, std::process::Command. No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-03-19-cache-feature-design.md`

---

## File Structure

| Action | File | Responsibility |
|--------|------|----------------|
| Create | `src/cache/mod.rs` | Module declaration, public re-exports |
| Create | `src/cache/models.rs` | CacheEntry, CacheKey, CacheEntryType, CacheStatus, CacheEntryInfo |
| Create | `src/cache/manager.rs` | CacheManager struct with get/set/invalidate/clear/status methods |
| Create | `src/cache/refresh.rs` | Background refresh trigger and lock file logic |
| Create | `tests/cache_tests.rs` | Unit tests for cache module |
| Modify | `src/lib.rs` | Add `pub mod cache;` |
| Modify | `src/config/settings.rs` | Replace `cache_ttl: Duration` with `cache_enabled: bool` + `cache_ttl_secs: u64`, update env loading |
| Modify | `src/cli/commands.rs` | Add `--no-cache` flags, `Commands::Cache` variant, `CacheCommands` enum, cache integration in read/write functions, `execute_config_set` updates |

---

### Task 1: Create cache data models

**Files:**
- Create: `src/cache/models.rs`
- Create: `src/cache/mod.rs`
- Modify: `src/lib.rs:1-18`

- [ ] **Step 1: Write the failing test for CacheKey Display/FromStr**

Create `src/cache/models.rs` with test module:

```rust
// At bottom of models.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_display() {
        assert_eq!(
            CacheKey::SecretsList { vault_name: "myvault".to_string() }.to_string(),
            "secrets:myvault"
        );
        assert_eq!(CacheKey::VaultList.to_string(), "vaults");
        assert_eq!(
            CacheKey::FileList { vault_name: "myvault".to_string() }.to_string(),
            "files:myvault"
        );
    }

    #[test]
    fn test_cache_key_from_str() {
        let key: CacheKey = "secrets:myvault".parse().unwrap();
        assert!(matches!(key, CacheKey::SecretsList { vault_name } if vault_name == "myvault"));

        let key: CacheKey = "vaults".parse().unwrap();
        assert!(matches!(key, CacheKey::VaultList));

        let key: CacheKey = "files:myvault".parse().unwrap();
        assert!(matches!(key, CacheKey::FileList { vault_name } if vault_name == "myvault"));
    }

    #[test]
    fn test_cache_key_from_str_invalid() {
        assert!("invalid".parse::<CacheKey>().is_err());
        assert!("secrets:".parse::<CacheKey>().is_err());
        assert!("unknown:vault".parse::<CacheKey>().is_err());
    }

    #[test]
    fn test_cache_key_to_path() {
        let base = PathBuf::from("/cache");

        let key = CacheKey::SecretsList { vault_name: "myvault".to_string() };
        assert_eq!(key.to_path(&base), PathBuf::from("/cache/myvault/secrets-list.json"));

        let key = CacheKey::VaultList;
        assert_eq!(key.to_path(&base), PathBuf::from("/cache/vaults-list.json"));

        let key = CacheKey::FileList { vault_name: "myvault".to_string() };
        assert_eq!(key.to_path(&base), PathBuf::from("/cache/myvault/files-list.json"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cache::models::tests -- --nocapture`
Expected: FAIL — module doesn't exist yet

- [ ] **Step 3: Write the models implementation**

Create `src/cache/models.rs`:

```rust
//! Cache data models
//!
//! Data structures for cache entries, keys, and status reporting.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// A cached response stored on disk as JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct CacheEntry<T: Serialize + DeserializeOwned> {
    pub created_at: DateTime<Utc>,
    pub ttl_secs: u64,
    pub vault_name: Option<String>,
    pub entry_type: CacheEntryType,
    pub data: T,
}

/// The type of listing operation that produced this cache entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheEntryType {
    SecretsList,
    VaultList,
    FileList,
}

/// Identifies a cache entry and determines its file path.
#[derive(Debug, Clone)]
pub enum CacheKey {
    SecretsList { vault_name: String },
    VaultList,
    FileList { vault_name: String },
}

impl CacheKey {
    /// Resolve this key to its file path under the given cache directory.
    pub fn to_path(&self, cache_dir: &PathBuf) -> PathBuf {
        match self {
            CacheKey::SecretsList { vault_name } => {
                cache_dir.join(vault_name).join("secrets-list.json")
            }
            CacheKey::VaultList => cache_dir.join("vaults-list.json"),
            CacheKey::FileList { vault_name } => {
                cache_dir.join(vault_name).join("files-list.json")
            }
        }
    }

    /// Return the CacheEntryType for this key.
    pub fn entry_type(&self) -> CacheEntryType {
        match self {
            CacheKey::SecretsList { .. } => CacheEntryType::SecretsList,
            CacheKey::VaultList => CacheEntryType::VaultList,
            CacheKey::FileList { .. } => CacheEntryType::FileList,
        }
    }

    /// Return the vault name if this key is vault-scoped.
    pub fn vault_name(&self) -> Option<&str> {
        match self {
            CacheKey::SecretsList { vault_name } | CacheKey::FileList { vault_name } => {
                Some(vault_name)
            }
            CacheKey::VaultList => None,
        }
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheKey::SecretsList { vault_name } => write!(f, "secrets:{vault_name}"),
            CacheKey::VaultList => write!(f, "vaults"),
            CacheKey::FileList { vault_name } => write!(f, "files:{vault_name}"),
        }
    }
}

impl std::str::FromStr for CacheKey {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "vaults" {
            return Ok(CacheKey::VaultList);
        }
        if let Some(vault_name) = s.strip_prefix("secrets:") {
            if vault_name.is_empty() {
                return Err("Missing vault name after 'secrets:'".to_string());
            }
            return Ok(CacheKey::SecretsList {
                vault_name: vault_name.to_string(),
            });
        }
        if let Some(vault_name) = s.strip_prefix("files:") {
            if vault_name.is_empty() {
                return Err("Missing vault name after 'files:'".to_string());
            }
            return Ok(CacheKey::FileList {
                vault_name: vault_name.to_string(),
            });
        }
        Err(format!(
            "Invalid cache key: '{s}'. Expected 'secrets:<vault>', 'vaults', or 'files:<vault>'"
        ))
    }
}

/// Summary of the cache state, returned by `xv cache status`.
#[derive(Debug)]
pub struct CacheStatus {
    pub cache_dir: PathBuf,
    pub enabled: bool,
    pub ttl_secs: u64,
    pub entry_count: usize,
    pub total_size_bytes: u64,
    pub entries: Vec<CacheEntryInfo>,
}

/// Metadata about a single cache entry file.
#[derive(Debug)]
pub struct CacheEntryInfo {
    pub key: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub is_stale: bool,
}
```

- [ ] **Step 4: Create mod.rs and register the module**

Create `src/cache/mod.rs`:

```rust
//! Client-side cache for expensive listing operations.
//!
//! Caches responses from `xv ls`, `xv vault list`, and `xv file list`
//! as flat JSON files organized by vault. Supports configurable TTL,
//! background refresh, and eager invalidation on writes.

pub mod manager;
pub mod models;
pub mod refresh;

pub use manager::CacheManager;
pub use models::{CacheEntry, CacheEntryType, CacheKey, CacheStatus};
```

Add to `src/lib.rs` after `pub mod blob;`:

```rust
pub mod cache;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib cache::models::tests -- --nocapture`
Expected: PASS (all 4 tests)

- [ ] **Step 6: Commit**

```bash
git add src/cache/mod.rs src/cache/models.rs src/lib.rs
git commit -m "feat(cache): add cache data models (CacheKey, CacheEntry, CacheStatus)"
```

---

### Task 2: Create CacheManager with get/set/invalidate/clear/status

**Files:**
- Create: `src/cache/manager.rs`
- Create: `src/cache/refresh.rs` (stub for now — enough for `get` to compile)

- [ ] **Step 1: Write failing tests for CacheManager**

Add tests at the bottom of `src/cache/manager.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_manager(dir: &TempDir) -> CacheManager {
        CacheManager::new(dir.path().to_path_buf(), true, 900)
    }

    #[test]
    fn test_get_returns_none_for_missing_entry() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        let key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_set_then_get_returns_data() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        let key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        let data = vec!["secret1".to_string(), "secret2".to_string()];
        mgr.set(&key, &data);
        let result: Option<Vec<String>> = mgr.get(&key);
        assert_eq!(result, Some(data));
    }

    #[test]
    fn test_get_returns_none_for_expired_entry() {
        let dir = TempDir::new().unwrap();
        let mgr = CacheManager::new(dir.path().to_path_buf(), true, 0); // TTL=0 → immediately expired
        let key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        mgr.set(&key, &vec!["data".to_string()]);
        // TTL=0 means entry is already expired on read
        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_get_returns_none_when_disabled() {
        let dir = TempDir::new().unwrap();
        let mgr = CacheManager::new(dir.path().to_path_buf(), false, 900);
        let key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        mgr.set(&key, &vec!["data".to_string()]);
        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_invalidate_removes_entry() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        let key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        mgr.set(&key, &vec!["data".to_string()]);
        mgr.invalidate(&key);
        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_invalidate_vault_removes_all_entries() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        let secrets_key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        let files_key = CacheKey::FileList { vault_name: "v1".to_string() };
        mgr.set(&secrets_key, &vec!["s1".to_string()]);
        mgr.set(&files_key, &vec!["f1".to_string()]);
        mgr.invalidate_vault("v1");
        let s: Option<Vec<String>> = mgr.get(&secrets_key);
        let f: Option<Vec<String>> = mgr.get(&files_key);
        assert!(s.is_none());
        assert!(f.is_none());
    }

    #[test]
    fn test_clear_all_removes_everything() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        mgr.set(&CacheKey::SecretsList { vault_name: "v1".to_string() }, &vec!["a".to_string()]);
        mgr.set(&CacheKey::VaultList, &vec!["b".to_string()]);
        mgr.clear(None);
        let s: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "v1".to_string() });
        let v: Option<Vec<String>> = mgr.get(&CacheKey::VaultList);
        assert!(s.is_none());
        assert!(v.is_none());
    }

    #[test]
    fn test_clear_specific_vault() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        mgr.set(&CacheKey::SecretsList { vault_name: "v1".to_string() }, &vec!["a".to_string()]);
        mgr.set(&CacheKey::SecretsList { vault_name: "v2".to_string() }, &vec!["b".to_string()]);
        mgr.clear(Some("v1"));
        let v1: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "v1".to_string() });
        let v2: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "v2".to_string() });
        assert!(v1.is_none());
        assert_eq!(v2, Some(vec!["b".to_string()]));
    }

    #[test]
    fn test_corrupt_json_treated_as_miss() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        let key = CacheKey::SecretsList { vault_name: "v1".to_string() };
        let path = key.to_path(&dir.path().to_path_buf());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not valid json{{{").unwrap();
        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
        // File should be deleted after corrupt read
        assert!(!path.exists());
    }

    #[test]
    fn test_set_creates_directories() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        let key = CacheKey::SecretsList { vault_name: "deep-vault".to_string() };
        mgr.set(&key, &vec!["data".to_string()]);
        let path = key.to_path(&dir.path().to_path_buf());
        assert!(path.exists());
    }

    #[test]
    fn test_status_reports_entries() {
        let dir = TempDir::new().unwrap();
        let mgr = test_manager(&dir);
        mgr.set(&CacheKey::SecretsList { vault_name: "v1".to_string() }, &vec!["a".to_string()]);
        mgr.set(&CacheKey::VaultList, &vec!["b".to_string()]);
        let status = mgr.status();
        assert_eq!(status.entry_count, 2);
        assert!(status.total_size_bytes > 0);
        assert_eq!(status.entries.len(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib cache::manager::tests -- --nocapture`
Expected: FAIL — `CacheManager` doesn't exist yet

- [ ] **Step 3: Create refresh.rs stub**

Create `src/cache/refresh.rs` with enough to compile:

```rust
//! Background cache refresh via detached child process.
//!
//! When a cache entry is within its refresh window (80% of TTL elapsed),
//! a detached `xv cache refresh --key <key>` child process is spawned
//! to update the cache entry without blocking the user.

use crate::cache::models::CacheKey;

/// Spawn a detached background process to refresh a cache entry.
///
/// The child process runs `xv cache refresh --key <key>`, which
/// authenticates with Azure, fetches fresh data, and updates the cache.
/// Failures are silent — the existing cache entry serves until TTL expires.
pub fn trigger_background_refresh(key: &CacheKey) {
    let key_str = key.to_string();

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!("Failed to get current exe for cache refresh: {e}");
            return;
        }
    };

    match std::process::Command::new(exe)
        .args(["cache", "refresh", "--key", &key_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            tracing::debug!("Spawned background cache refresh for key: {key_str}");
        }
        Err(e) => {
            tracing::debug!("Failed to spawn cache refresh process: {e}");
        }
    }
}

/// Check whether a lock file exists and is fresh (< 60 seconds old).
pub fn is_locked(lock_path: &std::path::Path) -> bool {
    match std::fs::metadata(lock_path) {
        Ok(meta) => {
            if let Ok(modified) = meta.modified() {
                if let Ok(elapsed) = modified.elapsed() {
                    if elapsed.as_secs() < 60 {
                        return true; // Lock is fresh, another refresh is in progress
                    }
                    // Stale lock — previous refresh crashed. Remove it.
                    tracing::debug!("Removing stale lock file: {}", lock_path.display());
                    let _ = std::fs::remove_file(lock_path);
                }
            }
            false
        }
        Err(_) => false,
    }
}

/// Create a lock file. Returns true on success.
pub fn acquire_lock(lock_path: &std::path::Path) -> bool {
    if is_locked(lock_path) {
        return false;
    }
    // Create parent dirs if needed
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(lock_path, "locked").is_ok()
}

/// Remove the lock file.
pub fn release_lock(lock_path: &std::path::Path) {
    let _ = std::fs::remove_file(lock_path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_lock_acquire_and_release() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("test.lock");
        assert!(!is_locked(&lock_path));
        assert!(acquire_lock(&lock_path));
        assert!(is_locked(&lock_path));
        release_lock(&lock_path);
        assert!(!is_locked(&lock_path));
    }

    #[test]
    fn test_lock_prevents_double_acquire() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("test.lock");
        assert!(acquire_lock(&lock_path));
        assert!(!acquire_lock(&lock_path)); // Second acquire should fail
        release_lock(&lock_path);
    }
}
```

- [ ] **Step 4: Write CacheManager implementation**

Create `src/cache/manager.rs`:

```rust
//! Cache manager for reading, writing, and invalidating cache entries.
//!
//! All operations degrade gracefully — cache failures never cause
//! command failures. Errors are logged at debug level only.

use crate::cache::models::{
    CacheEntry, CacheEntryInfo, CacheKey, CacheStatus,
};
use crate::cache::refresh;
use chrono::Utc;
use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

/// Manages client-side cache storage for listing operations.
pub struct CacheManager {
    cache_dir: PathBuf,
    enabled: bool,
    ttl_secs: u64,
}

impl CacheManager {
    /// Create a new CacheManager.
    pub fn new(cache_dir: PathBuf, enabled: bool, ttl_secs: u64) -> Self {
        Self {
            cache_dir,
            enabled,
            ttl_secs,
        }
    }

    /// Create a CacheManager from the application Config.
    pub fn from_config(config: &crate::config::Config) -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("xv");
        let enabled = config.cache_enabled && config.cache_ttl_secs > 0;
        Self::new(cache_dir, enabled, config.cache_ttl_secs)
    }

    /// Read a cached entry. Returns `None` on miss, expiry, or error.
    ///
    /// If the entry is within its refresh window (80% of TTL elapsed),
    /// triggers a background refresh and returns the cached data.
    pub fn get<T: DeserializeOwned>(&self, key: &CacheKey) -> Option<T> {
        if !self.enabled {
            return None;
        }

        let path = key.to_path(&self.cache_dir);
        if !path.exists() {
            return None;
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Cache read error for {key}: {e}");
                let _ = std::fs::remove_file(&path);
                return None;
            }
        };

        let entry: CacheEntry<T> = match serde_json::from_str(&contents) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("Cache deserialize error for {key}: {e}");
                let _ = std::fs::remove_file(&path);
                return None;
            }
        };

        let now = Utc::now();
        let age_secs = (now - entry.created_at).num_seconds().max(0) as u64;

        // Expired — remove and return None
        if age_secs >= entry.ttl_secs {
            tracing::debug!("Cache entry expired for {key}");
            let _ = std::fs::remove_file(&path);
            return None;
        }

        // Within refresh window (80% of TTL elapsed) — trigger background refresh
        let refresh_threshold = (entry.ttl_secs as f64 * 0.8) as u64;
        if age_secs >= refresh_threshold {
            tracing::debug!("Cache entry in refresh window for {key}, triggering background refresh");
            refresh::trigger_background_refresh(key);
        }

        Some(entry.data)
    }

    /// Write a cache entry to disk. Failures are silent.
    pub fn set<T: Serialize>(&self, key: &CacheKey, data: &T) {
        if !self.enabled {
            return;
        }

        let path = key.to_path(&self.cache_dir);

        // Create parent directories
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::debug!("Failed to create cache directory: {e}");
                return;
            }
        }

        let entry = CacheEntry {
            created_at: Utc::now(),
            ttl_secs: self.ttl_secs,
            vault_name: key.vault_name().map(|s| s.to_string()),
            entry_type: key.entry_type(),
            data,
        };

        let json = match serde_json::to_string_pretty(&entry) {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!("Failed to serialize cache entry for {key}: {e}");
                return;
            }
        };

        // Atomic write: write to temp file in same directory, then rename
        let tmp_path = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp_path, &json) {
            tracing::debug!("Failed to write cache temp file for {key}: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            tracing::debug!("Failed to rename cache file for {key}: {e}");
            let _ = std::fs::remove_file(&tmp_path);
        }
    }

    /// Delete a specific cache entry and its lock file.
    pub fn invalidate(&self, key: &CacheKey) {
        let path = key.to_path(&self.cache_dir);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::debug!("Failed to invalidate cache entry for {key}: {e}");
            }
        }
        let lock_path = path.with_extension("lock");
        let _ = std::fs::remove_file(&lock_path);
    }

    /// Delete all cache entries for a specific vault.
    pub fn invalidate_vault(&self, vault_name: &str) {
        let vault_dir = self.cache_dir.join(vault_name);
        if vault_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&vault_dir) {
                tracing::debug!("Failed to invalidate vault cache for {vault_name}: {e}");
            }
        }
    }

    /// Delete cache entries. If vault is Some, only that vault's cache.
    /// If None, delete all cache contents.
    pub fn clear(&self, vault: Option<&str>) {
        if let Some(vault_name) = vault {
            self.invalidate_vault(vault_name);
        } else if self.cache_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let _ = std::fs::remove_dir_all(&path);
                    } else {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }

    /// Collect status information about the cache.
    pub fn status(&self) -> CacheStatus {
        let mut entries = Vec::new();
        let mut total_size: u64 = 0;

        if self.cache_dir.exists() {
            self.walk_cache_dir(&self.cache_dir, &mut entries, &mut total_size);
        }

        CacheStatus {
            cache_dir: self.cache_dir.clone(),
            enabled: self.enabled,
            ttl_secs: self.ttl_secs,
            entry_count: entries.len(),
            total_size_bytes: total_size,
            entries,
        }
    }

    /// Return the cache directory path.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Return whether caching is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn walk_cache_dir(
        &self,
        dir: &std::path::Path,
        entries: &mut Vec<CacheEntryInfo>,
        total_size: &mut u64,
    ) {
        let read_dir = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.walk_cache_dir(&path, entries, total_size);
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(meta) = std::fs::metadata(&path) {
                    let size = meta.len();
                    *total_size += size;

                    // Try to read the entry to get timestamps
                    if let Ok(contents) = std::fs::read_to_string(&path) {
                        if let Ok(raw) =
                            serde_json::from_str::<CacheEntry<serde_json::Value>>(&contents)
                        {
                            let now = Utc::now();
                            let expires_at = raw.created_at
                                + chrono::Duration::seconds(raw.ttl_secs as i64);
                            let is_stale = now > expires_at;

                            // Derive key string from path
                            let key = path
                                .strip_prefix(&self.cache_dir)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .to_string();

                            entries.push(CacheEntryInfo {
                                key,
                                created_at: raw.created_at,
                                expires_at,
                                size_bytes: size,
                                is_stale,
                            });
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib cache -- --nocapture`
Expected: PASS (all manager and refresh tests)

- [ ] **Step 6: Commit**

```bash
git add src/cache/manager.rs src/cache/refresh.rs
git commit -m "feat(cache): add CacheManager with get/set/invalidate/clear/status"
```

---

### Task 3: Update Config to support cache settings

**Files:**
- Modify: `src/config/settings.rs:76-139` (Config struct and Default impl)
- Modify: `src/config/settings.rs:357-435` (load_from_env)
- Modify: `src/config/settings.rs:469-599` (tests)

- [ ] **Step 1: Write failing test for new config fields**

Add to the existing test module in `src/config/settings.rs`:

```rust
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
        function_app_url = ""
        output_json = false
        no_color = false
    "#;
    let config: Config = toml::from_str(toml).unwrap();
    assert!(config.cache_enabled);
    assert_eq!(config.cache_ttl_secs, 900);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config::settings::tests::test_cache_config -- --nocapture`
Expected: FAIL — `cache_enabled` and `cache_ttl_secs` fields don't exist

- [ ] **Step 3: Update Config struct**

In `src/config/settings.rs`, replace the `cache_ttl: Duration` field with two new fields. In the `Config` struct (around line 92-93):

Replace:
```rust
    #[tabled(skip)]
    pub cache_ttl: Duration,
```

With:
```rust
    /// Whether client-side caching is enabled for listing operations
    #[tabled(rename = "Cache Enabled")]
    #[serde(default = "default_cache_enabled")]
    pub cache_enabled: bool,
    /// Cache time-to-live in seconds (0 to disable)
    #[tabled(rename = "Cache TTL")]
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
```

Add default functions after `default_clipboard_timeout()`:

```rust
fn default_cache_enabled() -> bool {
    true
}

fn default_cache_ttl_secs() -> u64 {
    900
}
```

Update `Default for Config` (around line 131): replace `cache_ttl: Duration::from_secs(300),` with:

```rust
            cache_enabled: default_cache_enabled(),
            cache_ttl_secs: default_cache_ttl_secs(),
```

Remove the `use std::time::Duration;` import — it is only used for `cache_ttl` in this file, so once replaced it is safe to remove.

- [ ] **Step 4: Update load_from_env**

In `load_from_env` (around line 382-386), replace the existing `CACHE_TTL` handler:

```rust
    if let Ok(value) = std::env::var("CACHE_TTL") {
        if let Ok(seconds) = value.parse::<u64>() {
            config.cache_ttl = Duration::from_secs(seconds);
        }
    }
```

With:

```rust
    if let Ok(value) = std::env::var("CACHE_ENABLED") {
        config.cache_enabled = value.to_lowercase() == "true" || value == "1";
    }

    if let Ok(value) = std::env::var("CACHE_TTL") {
        if let Ok(seconds) = value.parse::<u64>() {
            config.cache_ttl_secs = seconds;
        }
    }
```

- [ ] **Step 5: Fix any existing test that references cache_ttl**

The test `test_gen_default_charset_absent_in_toml_is_none` (around line 582-598) references `cache_ttl = { secs = 300, nanos = 0 }` in its TOML fixture. Remove that line from the TOML string — the new fields have `serde(default)` so they don't need to be in the fixture.

- [ ] **Step 6: Fix compilation errors in commands.rs**

Search for all references to `config.cache_ttl` in `src/cli/commands.rs` and update them. The known references are:

- Line ~3905 in `execute_config_show`: change `format!("{}s", config.cache_ttl.as_secs())` to `format!("{}s", config.cache_ttl_secs)`
- Line ~4012-4016 in `execute_config_set`: update the `"cache_ttl"` branch to set `config.cache_ttl_secs = seconds;` instead of `config.cache_ttl = Duration::from_secs(seconds);`

Also add two new branches in `execute_config_set` for `"cache_enabled"` and `"cache_ttl_secs"`:

```rust
        "cache_enabled" => {
            config.cache_enabled = value.to_lowercase() == "true" || value == "1";
        }
        "cache_ttl" | "cache_ttl_secs" => {
            let seconds = value.parse::<u64>().map_err(|_| {
                CrosstacheError::config(format!("Invalid value for cache_ttl_secs: {value}"))
            })?;
            config.cache_ttl_secs = seconds;
        }
```

Update the error message in the `_` catch-all to include `cache_enabled` and `cache_ttl_secs` in the list of available keys.

Update the `ConfigItem` for cache in `execute_config_show` to show both new fields:

```rust
        ConfigItem {
            key: "cache_enabled".to_string(),
            value: config.cache_enabled.to_string(),
            source: "config".to_string(),
        },
        ConfigItem {
            key: "cache_ttl_secs".to_string(),
            value: format!("{}s", config.cache_ttl_secs),
            source: "config".to_string(),
        },
```

- [ ] **Step 7: Run all tests**

Run: `cargo test --lib -- --nocapture`
Expected: PASS

Run: `cargo clippy --all-targets`
Expected: No errors

- [ ] **Step 8: Commit**

```bash
git add src/config/settings.rs src/cli/commands.rs
git commit -m "feat(cache): add cache_enabled and cache_ttl_secs config fields, replace cache_ttl Duration"
```

---

### Task 4: Add CLI commands (--no-cache flag, xv cache subcommand)

**Files:**
- Modify: `src/cli/commands.rs` — add `no_cache` fields to List commands, add `Commands::Cache` variant, add `CacheCommands` enum, add `execute_cache_command`

- [ ] **Step 1: Add --no-cache flag to Commands::List**

In `src/cli/commands.rs`, add to the `Commands::List` variant (around line 254-267):

```rust
        /// Bypass cache and fetch fresh data
        #[arg(long)]
        no_cache: bool,
```

- [ ] **Step 2: Add --no-cache flag to VaultCommands::List**

In the `VaultCommands::List` variant (around line 588-596):

```rust
        /// Bypass cache and fetch fresh data
        #[arg(long)]
        no_cache: bool,
```

- [ ] **Step 3: Add --no-cache flag to FileCommands::List**

In the `FileCommands::List` variant (find `FileCommands` enum, the `List` variant):

```rust
        /// Bypass cache and fetch fresh data
        #[arg(long)]
        no_cache: bool,
```

- [ ] **Step 4: Add CacheCommands enum and Commands::Cache variant**

Add the `CacheCommands` enum near the other subcommand enums (e.g., after `ConfigCommands`):

```rust
#[derive(Subcommand)]
pub enum CacheCommands {
    /// Remove cached data
    Clear {
        /// Clear cache for a specific vault only
        #[arg(long)]
        vault: Option<String>,
    },
    /// Show cache status and statistics
    Status,
    /// Internal: refresh a cache entry in the background
    #[command(hide = true)]
    Refresh {
        /// Cache key to refresh (e.g., secrets:myvault)
        #[arg(long)]
        key: String,
    },
}
```

Add `Commands::Cache` variant in the `Commands` enum (e.g., after `Config`):

```rust
    /// Cache management commands
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
```

- [ ] **Step 5: Add dispatch in Cli::execute**

In the `Cli::execute` match block (around line 1143, near `Commands::Config`):

```rust
            Commands::Cache { command } => execute_cache_command(command, config).await,
```

- [ ] **Step 6: Implement execute_cache_command**

Add the handler function:

```rust
async fn execute_cache_command(command: CacheCommands, config: Config) -> Result<()> {
    use crate::cache::CacheManager;

    let cache_manager = CacheManager::from_config(&config);

    match command {
        CacheCommands::Clear { vault } => {
            cache_manager.clear(vault.as_deref());
            if let Some(vault_name) = vault {
                output::success(&format!("Cache cleared for vault '{vault_name}'"));
            } else {
                output::success("All cache entries cleared");
            }
            Ok(())
        }
        CacheCommands::Status => {
            let status = cache_manager.status();
            println!("Cache directory: {}", status.cache_dir.display());
            println!("Enabled: {}", status.enabled);
            println!("TTL: {}s", status.ttl_secs);
            println!("Entries: {}", status.entry_count);
            println!(
                "Total size: {}",
                format_cache_size(status.total_size_bytes)
            );

            if !status.entries.is_empty() {
                println!();
                for entry in &status.entries {
                    let stale_marker = if entry.is_stale { " (stale)" } else { "" };
                    println!(
                        "  {} — created: {}, expires: {}, size: {}{}",
                        entry.key,
                        entry.created_at.format("%Y-%m-%d %H:%M:%S"),
                        entry.expires_at.format("%Y-%m-%d %H:%M:%S"),
                        format_cache_size(entry.size_bytes),
                        stale_marker,
                    );
                }
            }
            Ok(())
        }
        CacheCommands::Refresh { key } => {
            execute_cache_refresh(&key, &config).await
        }
    }
}

fn format_cache_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

async fn execute_cache_refresh(key_str: &str, config: &Config) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    use crate::cache::refresh::{acquire_lock, release_lock};

    let key: CacheKey = key_str
        .parse()
        .map_err(|e: String| CrosstacheError::config(e))?;

    let cache_manager = CacheManager::from_config(config);
    let lock_path = key.to_path(cache_manager.cache_dir()).with_extension("lock");

    if !acquire_lock(&lock_path) {
        tracing::debug!("Cache refresh already in progress for {key_str}");
        return Ok(());
    }

    let result = match &key {
        CacheKey::SecretsList { vault_name } => {
            refresh_secrets_list(vault_name, &cache_manager, &key, config).await
        }
        CacheKey::VaultList => {
            refresh_vault_list(&cache_manager, &key, config).await
        }
        CacheKey::FileList { vault_name } => {
            refresh_file_list(vault_name, &cache_manager, &key, config).await
        }
    };

    release_lock(&lock_path);

    if let Err(e) = result {
        tracing::debug!("Background cache refresh failed for {key_str}: {e}");
    }

    Ok(())
}

async fn refresh_secrets_list(
    vault_name: &str,
    cache_manager: &crate::cache::CacheManager,
    key: &crate::cache::CacheKey,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Use the underlying list API via the public secret_ops() accessor
    // to get raw SecretSummary data without printing to stdout. This
    // matches the type that the read path deserializes: Vec<SecretSummary>.
    let secrets = secret_manager
        .secret_ops()
        .list_secrets(vault_name, None)
        .await?;

    cache_manager.set(key, &secrets);
    Ok(())
}

async fn refresh_vault_list(
    cache_manager: &crate::cache::CacheManager,
    key: &crate::cache::CacheKey,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::{AzureAuthProvider, DefaultAzureCredentialProvider};
    use std::sync::Arc;

    let auth_provider: Arc<dyn AzureAuthProvider> = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    let vault_manager = VaultManager::new(
        auth_provider,
        config.subscription_id.clone(),
        config.no_color,
    )?;

    let vaults = vault_manager
        .list_vaults_formatted(
            Some(&config.subscription_id),
            None,
            crate::utils::format::OutputFormat::Json,
        )
        .await?;

    cache_manager.set(key, &vaults);
    Ok(())
}

#[cfg(feature = "file-ops")]
async fn refresh_file_list(
    _vault_name: &str,
    cache_manager: &crate::cache::CacheManager,
    key: &crate::cache::CacheKey,
    config: &Config,
) -> Result<()> {
    use crate::blob::manager::create_blob_manager;
    use crate::blob::models::FileListRequest;

    let blob_manager = create_blob_manager(config)?;
    let list_request = FileListRequest {
        prefix: None,
        groups: None,
        limit: None,
        delimiter: None,
        recursive: true,
    };
    let files = blob_manager.list_files(list_request).await?;
    cache_manager.set(key, &files);
    Ok(())
}

#[cfg(not(feature = "file-ops"))]
async fn refresh_file_list(
    _vault_name: &str,
    _cache_manager: &crate::cache::CacheManager,
    _key: &crate::cache::CacheKey,
    _config: &Config,
) -> Result<()> {
    Ok(())
}
```

- [ ] **Step 7: Update dispatch match arms to pass no_cache**

Update `Commands::List` match arm (around line 1032-1037) to pass `no_cache`:

```rust
            Commands::List {
                group,
                all,
                expiring,
                expired,
                no_cache,
            } => execute_secret_list_direct(group, all, expiring, expired, no_cache, config).await,
```

Update `VaultCommands::List` match arm (around line 1429-1433) to pass `no_cache`:

```rust
        VaultCommands::List {
            resource_group,
            format,
            no_cache,
        } => {
            execute_vault_list(&vault_manager, resource_group, format, no_cache, &config).await?;
        }
```

Update `FileCommands::List` match arm (around line 1339-1354) to pass `no_cache`:

```rust
        FileCommands::List {
            prefix,
            group,
            limit,
            recursive,
            no_cache,
        } => {
            execute_file_list(
                &blob_manager,
                prefix,
                group,
                limit,
                recursive,
                no_cache,
                &config,
            )
            .await?;
        }
```

- [ ] **Step 8: Update function signatures to accept no_cache**

Update `execute_secret_list_direct` signature to accept `no_cache: bool` and pass it through.

Update `execute_vault_list` signature to accept `no_cache: bool`.

Update `execute_file_list` signature to accept `no_cache: bool`.

(Do NOT add the cache logic yet — that's the next task. Just accept the parameter.)

- [ ] **Step 9: Verify compilation**

Run: `cargo check`
Expected: compiles without errors

Run: `cargo clippy --all-targets`
Expected: no errors

- [ ] **Step 10: Commit**

```bash
git add src/cli/commands.rs
git commit -m "feat(cache): add --no-cache flags, xv cache subcommand, and cache refresh handler"
```

---

### Task 5: Integrate cache into listing commands (read path)

> **IMPLEMENTER NOTE:** This task explores multiple approaches inline. **Skip to the section marked "This is the approach we'll take"** for the final implementation. The preceding code blocks are exploratory and should be ignored.

**Files:**
- Modify: `src/cli/commands.rs` — update `execute_secret_list`, `execute_secret_list_direct`, `execute_vault_list`, `execute_file_list`

**Approach:** The key challenge is that `list_secrets_formatted` both fetches data AND prints to stdout. To avoid double-display or extra API calls:

1. Change `execute_secret_list` to return `Result<Vec<SecretSummary>>` — it already has the data, just return it instead of discarding. Its only caller is `execute_secret_list_direct` (verified by grep — no other call sites).
2. On cache hit: filter and display from cache via a new `display_cached_secret_list` helper.
3. On cache miss: call the existing flow (which displays), capture the returned data, cache it.

- [ ] **Step 1: Modify execute_secret_list return type**

Change `execute_secret_list` (line 5433) from `Result<()>` to `Result<Vec<crate::secret::manager::SecretSummary>>`.

Right after `list_secrets_formatted` returns `secrets`, clone it before any filtering:

```rust
    let all_secrets = secrets.clone(); // Save pre-filter copy for cache
```

At the end of the function, replace `Ok(())` with `Ok(all_secrets)`.

This returns the **full unfiltered list** (before `--group`, `--expiring`, `--expired` filters), which is what we cache. The function still displays the filtered results as before.

- [ ] **Step 2: Add display_cached_secret_list helper**
PLACEHOLDER_DELETE_START
            let mut secrets = cached_secrets;
            if !all {
                secrets.retain(|s| s.enabled);
            }
            if let Some(ref g) = group {
                secrets.retain(|s| {
                    s.groups
                        .as_ref()
                        .map(|groups| groups.contains(g))
                        .unwrap_or(false)
                });
            }

            // Display using the same formatting as live path
            let output_format = if config.output_json {
                crate::utils::format::OutputFormat::Json
            } else {
                crate::utils::format::OutputFormat::Table
            };

            // Display vault header
            if output_format == crate::utils::format::OutputFormat::Table {
                use crate::utils::format::format_table;
                use tabled::Table;

                // Show vault header
                println!();
                if config.no_color {
                    println!("Vault: {}", vault_name);
                } else {
                    println!("\x1b[36mVault: {}\x1b[0m", vault_name);
                }
                println!();

                if secrets.is_empty() {
                    crate::utils::output::info(if all {
                        "No secrets found in vault."
                    } else {
                        "No enabled secrets found in vault. Use --all to show disabled secrets."
                    });
                } else {
                    let table = Table::new(&secrets);
                    println!("{}", format_table(table, config.no_color));
                    println!("\n{} secret(s) in vault '{}'", secrets.len(), vault_name);
                }
            } else {
                let json_output = serde_json::to_string_pretty(&secrets).map_err(|e| {
                    CrosstacheError::serialization(format!("Failed to serialize secrets: {e}"))
                })?;
                println!("{json_output}");
            }
            return Ok(());
        }
    }

    // Cache miss or cache disabled — fetch from Azure
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );

    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // For caching, we always fetch the full unfiltered list
    let all_secrets = secret_manager
        .list_secrets_formatted(
            &vault_name,
            None,    // no group filter for cache
            if config.output_json {
                crate::utils::format::OutputFormat::Json
            } else {
                crate::utils::format::OutputFormat::Table
            },
            false,
            true,    // show all for complete cache
        )
        .await?;

    // Cache the full unfiltered result
    if use_cache {
        cache_manager.set(&cache_key, &all_secrets);
    }

    // If the user requested filters, we need to re-display with filters applied
    // But since list_secrets_formatted already displayed output, we only need
    // to handle the case where we fetched all (for caching) but user wanted filtered
    // This is only needed if group was specified or all was false
    // Since list_secrets_formatted already displayed, and we called it with
    // show_all=true and no group filter, we may need to call it again with
    // the user's actual filters.
    //
    // Actually, this would double-display. The cleaner approach is:
    // just call the original execute_secret_list with the user's filters.
    // The cache write already happened above.

    // For the non-cache path, just use the existing flow
    // We need to call the original function for proper display with filters
    if group.is_some() || !all || expiring.is_some() || expired {
        // The all_secrets call above already printed output.
        // We need to suppress that and re-display with filters.
        // This is tricky because list_secrets_formatted prints directly.
        //
        // Simpler approach: skip the cache-write optimization and just
        // call the original flow, then cache separately.
    }

    Ok(())
}
```

**IMPORTANT NOTE:** The above has a design issue — `list_secrets_formatted` prints directly to stdout, so we can't easily "capture" the output for caching while also displaying filtered results. The cleaner implementation is:

1. If cache hit: apply filters client-side, display.
2. If cache miss: call the original `execute_secret_list` flow (which displays), then separately fetch the unfiltered list for caching in a background write.

Actually, the simplest correct approach: always call `execute_secret_list` with the user's filters for display. For caching, after a miss, save the result of the API call. Since `list_secrets_formatted` returns `Vec<SecretSummary>`, we can cache whatever comes back. However, if the user passed `--group`, the returned list is already filtered.

**The actual clean implementation:**

```rust
async fn execute_secret_list_direct(
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    no_cache: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    let cache_manager = CacheManager::from_config(&config);
    let vault_name = config.resolve_vault_name(None).await?;
    let cache_key = CacheKey::SecretsList { vault_name: vault_name.clone() };
    let use_cache = cache_manager.is_enabled() && !no_cache;

    // Try cache first
    if use_cache {
        if let Some(cached) = cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key) {
            // Apply filters and display from cache
            return display_cached_secret_list(cached, group, all, expiring, expired, &vault_name, &config).await;
        }
    }

    // Cache miss — create auth provider and secret manager
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Fetch full unfiltered list for caching
    // We suppress display here by using Json format and not printing
    let all_secrets = secret_manager
        .secret_ops
        .list_secrets(&vault_name, None)
        .await?;

    // Cache the full list
    if use_cache {
        cache_manager.set(&cache_key, &all_secrets);
    }

    // Now call the original display flow with user's filters
    execute_secret_list(
        &secret_manager,
        None,
        group,
        all,
        expiring,
        expired,
        &config,
    )
    .await
}
```

Wait — `secret_ops` is private. The cleanest approach that doesn't require changing the `SecretManager` API is to just do two things:

1. On cache hit: filter and display from cache (new function).
2. On cache miss: call original `execute_secret_list` (which displays), then the result already comes back as `Vec<SecretSummary>` from `list_secrets_formatted`. But `execute_secret_list` doesn't return the data to us.

The pragmatic fix: modify `execute_secret_list` to return the unfiltered secrets list so we can cache it. Or, add a method to `SecretManager` that lists without displaying.

**Cleanest approach:** Just check `SecretManager::list_secrets_formatted` — it already returns `Vec<SecretSummary>`. We need to capture that return value from `execute_secret_list`. Currently `execute_secret_list` returns `Result<()>`. We can change it to `Result<Vec<SecretSummary>>` or have the caller call `list_secrets_formatted` directly and handle display.

Let me revise to the simplest correct approach:

```rust
async fn execute_secret_list_direct(
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    no_cache: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    let cache_manager = CacheManager::from_config(&config);

    // Resolve vault name early — needed for both cache key and API call
    let vault_name = config.resolve_vault_name(None).await?;
    let cache_key = CacheKey::SecretsList { vault_name: vault_name.clone() };
    let use_cache = cache_manager.is_enabled() && !no_cache;

    // Try cache first (only for non-expiry-filtered queries since expiry
    // filtering requires per-secret API calls in the current implementation)
    if use_cache && expiring.is_none() && !expired {
        if let Some(cached) = cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key) {
            return display_cached_secret_list(cached, group, all, &vault_name, &config);
        }
    }

    // Cache miss — use the existing flow
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Update context
    let mut context_manager = crate::config::ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Call original execute_secret_list (handles display)
    execute_secret_list(
        &secret_manager,
        None,
        group.clone(),
        all,
        expiring,
        expired,
        &config,
    )
    .await?;

    // After successful API call, cache the full unfiltered list
    if use_cache {
        // Fetch unfiltered list for cache (this is an extra API call on miss,
        // but only happens on cache miss which is infrequent)
        if let Ok(all_secrets) = secret_manager
            .list_secrets_formatted(
                &vault_name,
                None,
                crate::utils::format::OutputFormat::Json,
                false,
                true,
            )
            .await
        {
            cache_manager.set(&cache_key, &all_secrets);
        }
    }

    Ok(())
}
```

This makes an extra API call on cache miss. A better approach: since `execute_secret_list` already calls `list_secrets_formatted` which returns the data, we should modify it to also return the data so we can cache it. Let's do that:

Modify `execute_secret_list` to return `Result<Vec<SecretSummary>>` instead of `Result<()>`. Then in `execute_secret_list_direct`, capture the return value and cache it.

**This is the approach we'll take.** Here's the final implementation:

- [ ] **Step 1: Modify execute_secret_list return type**

Change `execute_secret_list` (line 5433) return type from `Result<()>` to `Result<Vec<crate::secret::manager::SecretSummary>>`. At the end of the function, instead of `Ok(())`, return `Ok(secrets)` (the secrets variable is already in scope — it's what gets displayed).

Ensure the function returns the **pre-filter** list (before `--group`, `--expiring`, `--expired` are applied) so the cache stores the complete data. This means: capture the initial `secrets` from `list_secrets_formatted` before any filtering, clone it for return, then proceed with filtering for display.

- [ ] **Step 2: Add display_cached_secret_list helper**

```rust
fn display_cached_secret_list(
    secrets: Vec<crate::secret::manager::SecretSummary>,
    group: Option<String>,
    all: bool,
    vault_name: &str,
    config: &Config,
) -> Result<()> {
    use crate::utils::format::format_table;
    use tabled::Table;

    let mut filtered = secrets;

    // Apply filters
    if !all {
        filtered.retain(|s| s.enabled);
    }
    if let Some(ref g) = group {
        filtered.retain(|s| {
            s.groups
                .as_ref()
                .map(|groups| groups.contains(g))
                .unwrap_or(false)
        });
    }

    // Display
    let output_format = if config.output_json {
        crate::utils::format::OutputFormat::Json
    } else {
        crate::utils::format::OutputFormat::Table
    };

    if output_format == crate::utils::format::OutputFormat::Table {
        println!();
        if config.no_color {
            println!("Vault: {}", vault_name);
        } else {
            println!("\x1b[36mVault: {}\x1b[0m", vault_name);
        }
        println!();

        if filtered.is_empty() {
            output::info(if all {
                "No secrets found in vault."
            } else {
                "No enabled secrets found in vault. Use --all to show disabled secrets."
            });
        } else {
            let table = Table::new(&filtered);
            println!("{}", format_table(table, config.no_color));
            println!(
                "\n{} secret(s) in vault '{}'",
                filtered.len(),
                vault_name
            );
        }
    } else {
        let json = serde_json::to_string_pretty(&filtered).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize secrets: {e}"))
        })?;
        println!("{json}");
    }

    Ok(())
}
```

- [ ] **Step 3: Update execute_secret_list_direct with cache integration**

```rust
async fn execute_secret_list_direct(
    group: Option<String>,
    all: bool,
    expiring: Option<String>,
    expired: bool,
    no_cache: bool,
    config: Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::cache::{CacheKey, CacheManager};
    use crate::secret::manager::SecretManager;
    use std::sync::Arc;

    let cache_manager = CacheManager::from_config(&config);
    let vault_name = config.resolve_vault_name(None).await?;
    let cache_key = CacheKey::SecretsList { vault_name: vault_name.clone() };
    let use_cache = cache_manager.is_enabled() && !no_cache;

    // Try cache (skip for expiry filters — they need per-secret API calls)
    if use_cache && expiring.is_none() && !expired {
        if let Some(cached) = cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key) {
            return display_cached_secret_list(cached, group, all, &vault_name, &config);
        }
    }

    // Cache miss — fetch from Azure using existing flow
    let auth_provider = Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    let secrets = execute_secret_list(
        &secret_manager,
        None,
        group,
        all,
        expiring,
        expired,
        &config,
    )
    .await?;

    // Cache the full unfiltered result
    if use_cache {
        cache_manager.set(&cache_key, &secrets);
    }

    Ok(())
}
```

- [ ] **Step 4: Add cache integration to execute_vault_list**

Update `execute_vault_list`:

```rust
async fn execute_vault_list(
    vault_manager: &VaultManager,
    resource_group: Option<String>,
    format: OutputFormat,
    no_cache: bool,
    config: &Config,
) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    use crate::vault::models::VaultSummary;

    let cache_manager = CacheManager::from_config(config);
    let cache_key = CacheKey::VaultList;
    let use_cache = cache_manager.is_enabled() && !no_cache;

    // Try cache (only if no resource_group filter)
    if use_cache && resource_group.is_none() {
        if let Some(cached) = cache_manager.get::<Vec<VaultSummary>>(&cache_key) {
            // Display cached vaults
            if cached.is_empty() {
                output::info("No vaults found.");
            } else {
                use crate::utils::format::format_table;
                use tabled::Table;

                if format == OutputFormat::Table {
                    let table = Table::new(&cached);
                    println!("{}", format_table(table, config.no_color));
                } else {
                    let json = serde_json::to_string_pretty(&cached).map_err(|e| {
                        CrosstacheError::serialization(format!("Failed to serialize: {e}"))
                    })?;
                    println!("{json}");
                }
            }
            return Ok(());
        }
    }

    // Cache miss — fetch from Azure
    let vaults = vault_manager
        .list_vaults_formatted(
            Some(&config.subscription_id),
            resource_group.as_deref(),
            format,
        )
        .await?;

    // Cache unfiltered results
    if use_cache && resource_group.is_none() {
        cache_manager.set(&cache_key, &vaults);
    }

    Ok(())
}
```

- [ ] **Step 5: Add cache integration to execute_file_list**

Update `execute_file_list` (line 7054) with the same cache-before-fetch, write-after-fetch pattern. Add `no_cache: bool` to the signature (already done in Task 4). Key details:

- Cache key: `CacheKey::FileList { vault_name }` where vault_name comes from `config.resolve_vault_name(None).await.unwrap_or_default()`
- Only cache unfiltered queries — skip cache if `prefix`, `group`, or `limit` are set
- Cache the `Vec<BlobListItem>` returned by the fetch
- On cache hit, display using the existing table/JSON display logic (copy the display block from the existing function body)
- On cache miss, run the existing fetch and display logic, then `cache_manager.set(&cache_key, &items)` after display

The pattern is identical to `execute_vault_list` above — create `CacheManager`, check `is_enabled() && !no_cache`, try `get()`, fall through to fetch, `set()` after fetch.

- [ ] **Step 6: Verify compilation and run tests**

Run: `cargo check`
Expected: compiles

Run: `cargo test --lib -- --nocapture`
Expected: PASS

Run: `cargo clippy --all-targets`
Expected: no errors

- [ ] **Step 7: Commit**

```bash
git add src/cli/commands.rs
git commit -m "feat(cache): integrate cache read/write into listing commands"
```

---

### Task 6: Add cache invalidation to write commands

**Files:**
- Modify: `src/cli/commands.rs` — add invalidation calls after successful write operations

- [ ] **Step 1: Add invalidation to secret write commands**

After each successful secret write operation, add cache invalidation. The pattern for each function is:

```rust
// At the top of the function, create the cache manager:
let cache_manager = crate::cache::CacheManager::from_config(&config);

// After the successful write operation:
let vault_name = /* however the vault name is resolved in this function */;
cache_manager.invalidate(&crate::cache::CacheKey::SecretsList { vault_name: vault_name.clone() });
```

Functions to update (with their approximate line numbers):

1. `execute_secret_set` (line 4093) — invalidate `SecretsList` for the vault
2. `execute_secret_delete` (line 5560) — invalidate `SecretsList`
3. `execute_secret_update` (line 5601) — invalidate `SecretsList`
4. `execute_secret_purge` (line 5806) — invalidate `SecretsList`
5. `execute_secret_rotate` (line 4824) — invalidate `SecretsList`
6. `execute_secret_rollback` (line 4658) — invalidate `SecretsList`
7. `execute_secret_restore` (line 5844) — invalidate `SecretsList`
8. `execute_secret_copy` (line 5883) — invalidate `SecretsList` for **both** source and destination vaults
9. `execute_secret_move` (line 5960) — invalidate `SecretsList` for **both** source and destination vaults

For the `_direct` wrapper functions (`execute_secret_set_direct`, `execute_secret_delete_direct`, etc.), add invalidation at the `_direct` level since that's where the config is available and the vault name is resolved.

- [ ] **Step 2: Add invalidation to vault write commands**

In `execute_vault_command` (line 1400), after the match arms for vault write operations, add:

```rust
// After VaultCommands::Create, Delete, Purge, Restore, Update:
let cache_manager = crate::cache::CacheManager::from_config(&config);
cache_manager.invalidate(&crate::cache::CacheKey::VaultList);
```

The cleanest approach: add invalidation after each relevant match arm inside the existing match block.

- [ ] **Step 3: Add invalidation to file write commands**

In `execute_file_command` (line 1192), after `FileCommands::Upload` and `FileCommands::Delete` match arms:

```rust
let cache_manager = crate::cache::CacheManager::from_config(&config);
// vault_name needs to be resolved from config for the cache key
if let Ok(vault_name) = config.resolve_vault_name(None).await {
    cache_manager.invalidate(&crate::cache::CacheKey::FileList { vault_name });
}
```

- [ ] **Step 4: Add invalidation to vault import**

`execute_vault_import` (line 6408) imports secrets into a vault — invalidate `SecretsList` for the target vault.

- [ ] **Step 5: Verify compilation**

Run: `cargo check`
Expected: compiles

Run: `cargo clippy --all-targets`
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs
git commit -m "feat(cache): add cache invalidation to all write commands"
```

---

### Task 7: Write integration tests

**Files:**
- Create: `tests/cache_tests.rs`

- [ ] **Step 1: Write cache integration tests**

```rust
//! Integration tests for the cache module.
//!
//! These tests verify CacheManager behavior without requiring Azure credentials.

use crosstache::cache::{CacheKey, CacheManager};
use tempfile::TempDir;

#[test]
fn test_cache_roundtrip_secrets_list() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::SecretsList {
        vault_name: "test-vault".to_string(),
    };

    let data = vec![
        serde_json::json!({"name": "secret1", "updated_on": "2026-03-19"}),
        serde_json::json!({"name": "secret2", "updated_on": "2026-03-18"}),
    ];

    mgr.set(&key, &data);

    let cached: Option<Vec<serde_json::Value>> = mgr.get(&key);
    assert!(cached.is_some());
    assert_eq!(cached.unwrap().len(), 2);
}

#[test]
fn test_cache_roundtrip_vault_list() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::VaultList;

    let data = vec![
        serde_json::json!({"name": "vault1", "location": "eastus"}),
    ];

    mgr.set(&key, &data);

    let cached: Option<Vec<serde_json::Value>> = mgr.get(&key);
    assert!(cached.is_some());
    assert_eq!(cached.unwrap().len(), 1);
}

#[test]
fn test_cache_no_cache_flag_behavior() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::SecretsList {
        vault_name: "test-vault".to_string(),
    };

    mgr.set(&key, &vec!["data".to_string()]);

    // Simulating --no-cache: caller skips cache_manager.get()
    // When no_cache is true, the caller should not call mgr.get() at all
    // This test verifies the cache file exists but would be skipped
    let path = key.to_path(&dir.path().to_path_buf());
    assert!(path.exists());
}

#[test]
fn test_cache_disabled_behavior() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), false, 900);
    let key = CacheKey::SecretsList {
        vault_name: "test-vault".to_string(),
    };

    // set should be a no-op when disabled
    mgr.set(&key, &vec!["data".to_string()]);

    let path = key.to_path(&dir.path().to_path_buf());
    assert!(!path.exists());

    // get should return None when disabled
    let result: Option<Vec<String>> = mgr.get(&key);
    assert!(result.is_none());
}

#[test]
fn test_cache_clear_specific_vault() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);

    mgr.set(
        &CacheKey::SecretsList { vault_name: "vault-a".to_string() },
        &vec!["a"],
    );
    mgr.set(
        &CacheKey::SecretsList { vault_name: "vault-b".to_string() },
        &vec!["b"],
    );

    mgr.clear(Some("vault-a"));

    let a: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "vault-a".to_string() });
    let b: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "vault-b".to_string() });
    assert!(a.is_none());
    assert!(b.is_some());
}

#[test]
fn test_cache_clear_all() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);

    mgr.set(
        &CacheKey::SecretsList { vault_name: "vault-a".to_string() },
        &vec!["a"],
    );
    mgr.set(&CacheKey::VaultList, &vec!["v"]);

    mgr.clear(None);

    let a: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "vault-a".to_string() });
    let v: Option<Vec<String>> = mgr.get(&CacheKey::VaultList);
    assert!(a.is_none());
    assert!(v.is_none());
}

#[test]
fn test_cache_invalidation() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::SecretsList { vault_name: "v1".to_string() };

    mgr.set(&key, &vec!["data"]);
    assert!(mgr.get::<Vec<String>>(&key).is_some());

    mgr.invalidate(&key);
    assert!(mgr.get::<Vec<String>>(&key).is_none());
}

#[test]
fn test_cache_invalidate_vault_removes_all_entries() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);

    mgr.set(
        &CacheKey::SecretsList { vault_name: "v1".to_string() },
        &vec!["s"],
    );
    mgr.set(
        &CacheKey::FileList { vault_name: "v1".to_string() },
        &vec!["f"],
    );

    mgr.invalidate_vault("v1");

    let s: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList { vault_name: "v1".to_string() });
    let f: Option<Vec<String>> = mgr.get(&CacheKey::FileList { vault_name: "v1".to_string() });
    assert!(s.is_none());
    assert!(f.is_none());
}

#[test]
fn test_cache_status() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);

    let status = mgr.status();
    assert_eq!(status.entry_count, 0);
    assert_eq!(status.total_size_bytes, 0);

    mgr.set(&CacheKey::VaultList, &vec!["v"]);
    mgr.set(
        &CacheKey::SecretsList { vault_name: "v1".to_string() },
        &vec!["s"],
    );

    let status = mgr.status();
    assert_eq!(status.entry_count, 2);
    assert!(status.total_size_bytes > 0);
    assert!(status.enabled);
    assert_eq!(status.ttl_secs, 900);
}

#[test]
fn test_cache_ttl_zero_disables_cache() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 0);
    let key = CacheKey::VaultList;

    mgr.set(&key, &vec!["data"]);
    // TTL=0 means the entry is considered disabled
    // CacheManager::new with ttl_secs=0 sets enabled=false... wait no,
    // from_config does that, but the direct constructor doesn't.
    // Actually per the spec, cache_ttl_secs=0 should be treated as disabled.
    // The from_config method handles this. For the direct constructor,
    // set would still write (enabled=true), but get would find it expired.
    // Actually TTL=0 means age >= ttl_secs is always true (age is >= 0).
    let result: Option<Vec<String>> = mgr.get(&key);
    assert!(result.is_none());
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test --test cache_tests -- --nocapture`
Expected: PASS (all tests)

- [ ] **Step 3: Commit**

```bash
git add tests/cache_tests.rs
git commit -m "test(cache): add integration tests for cache module"
```

---

### Task 8: Final verification and cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test --lib -- --nocapture`
Expected: PASS

- [ ] **Step 2: Run integration tests**

Run: `cargo test --test cache_tests -- --nocapture`
Expected: PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No errors or warnings (related to cache changes)

- [ ] **Step 4: Run formatter**

Run: `cargo fmt`
Expected: No changes (code already formatted)

- [ ] **Step 5: Verify build**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 6: Manual smoke test (if Azure credentials available)**

```bash
# Enable cache (should be on by default)
xv config set cache_enabled true

# First list — should hit Azure API
xv ls

# Second list — should be instant (from cache)
xv ls

# List with --no-cache — should hit Azure API
xv ls --no-cache

# Check cache status
xv cache status

# Set a secret — should invalidate cache
xv set test-cache-secret=value

# List again — should hit Azure API (cache was invalidated)
xv ls

# Clear cache
xv cache clear

# Verify status
xv cache status
```

- [ ] **Step 7: Final commit (if any formatting/cleanup needed)**

```bash
git add -A
git commit -m "chore(cache): final cleanup and formatting"
```
