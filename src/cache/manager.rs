//! Cache manager for client-side caching of listing operations.
//!
//! Stores cache entries as JSON files on disk, organised by vault name.
//! All I/O errors are degraded gracefully — they are logged at debug level
//! and never propagate to callers.

use chrono::Utc;
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::cache::models::{CacheEntry, CacheEntryInfo, CacheKey, CacheStatus};
use crate::cache::refresh;

// ---------------------------------------------------------------------------
// CacheManager
// ---------------------------------------------------------------------------

/// Client-side disk cache for expensive listing operations.
pub struct CacheManager {
    cache_dir: PathBuf,
    enabled: bool,
    ttl_secs: u64,
}

impl CacheManager {
    /// Create a new `CacheManager`.
    ///
    /// * `cache_dir`  – root directory where cache files are stored.
    /// * `enabled`    – when `false` all operations become no-ops.
    /// * `ttl_secs`   – how long a cache entry is considered fresh.
    pub fn new(cache_dir: PathBuf, enabled: bool, ttl_secs: u64) -> Self {
        Self {
            cache_dir,
            enabled,
            ttl_secs,
        }
    }

    // TODO: uncomment when cache_enabled/cache_ttl_secs are added to Config
    //
    // pub fn from_config(config: &Config) -> Self {
    //     let cache_dir = dirs::cache_dir()
    //         .unwrap_or_else(|| PathBuf::from(".cache"))
    //         .join("xv");
    //     let enabled = config.cache_enabled && config.cache_ttl_secs > 0;
    //     let ttl_secs = config.cache_ttl_secs;
    //     Self::new(cache_dir, enabled, ttl_secs)
    // }

    // ------------------------------------------------------------------
    // Getters
    // ------------------------------------------------------------------

    /// Return the root cache directory.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Return `true` if caching is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    // ------------------------------------------------------------------
    // Core operations
    // ------------------------------------------------------------------

    /// Read a cached value.
    ///
    /// Returns `None` when:
    /// - caching is disabled,
    /// - the file does not exist,
    /// - the file cannot be parsed,
    /// - the entry has expired (age ≥ TTL).
    ///
    /// When the entry's age exceeds 80 % of the TTL a background refresh is
    /// triggered so the next read is likely to find fresh data.
    pub fn get<T: Serialize + DeserializeOwned>(&self, key: &CacheKey) -> Option<T> {
        if !self.enabled {
            debug!("Cache disabled — skipping get for {key}");
            return None;
        }

        let path = key.to_path(&self.cache_dir);
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                debug!("Cache miss ({key}): {e}");
                return None;
            }
        };

        let entry: CacheEntry<T> = match serde_json::from_str(&raw) {
            Ok(e) => e,
            Err(e) => {
                debug!("Cache parse error ({key}): {e} — treating as miss");
                return None;
            }
        };

        let age_secs = (Utc::now() - entry.created_at).num_seconds().max(0) as u64;

        if age_secs >= self.ttl_secs {
            debug!("Cache expired ({key}): age {age_secs}s ≥ ttl {}s", self.ttl_secs);
            return None;
        }

        // Trigger background refresh when we are past 80 % of the TTL.
        if self.ttl_secs > 0 && age_secs >= (self.ttl_secs * 80 / 100) {
            debug!(
                "Cache stale-while-revalidate ({key}): age {age_secs}s ≥ {}s — triggering refresh",
                self.ttl_secs * 80 / 100
            );
            let lock_path = self.lock_path(key);
            if refresh::acquire_lock(&lock_path) {
                refresh::trigger_background_refresh(key);
            }
        }

        debug!("Cache hit ({key}): age {age_secs}s");
        Some(entry.data)
    }

    /// Write a value to the cache (atomic: temp-file → rename).
    ///
    /// Errors are logged at debug level and silently ignored.
    pub fn set<T: Serialize + DeserializeOwned>(&self, key: &CacheKey, data: &T) {
        if !self.enabled {
            return;
        }

        let path = key.to_path(&self.cache_dir);

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                debug!("Cache set ({key}): failed to create directories: {e}");
                return;
            }
        }

        // Use a local serialisation-only struct so we can hold `&T` without
        // requiring `T: DeserializeOwned` (which `CacheEntry<T>` demands).
        #[derive(Serialize)]
        struct EntryRef<'a, D: Serialize> {
            created_at: chrono::DateTime<Utc>,
            ttl_secs: u64,
            vault_name: Option<String>,
            entry_type: crate::cache::models::CacheEntryType,
            data: &'a D,
        }

        let entry = EntryRef {
            created_at: Utc::now(),
            ttl_secs: self.ttl_secs,
            vault_name: key.vault_name().map(str::to_owned),
            entry_type: key.entry_type(),
            data,
        };

        let json = match serde_json::to_string_pretty(&entry) {
            Ok(s) => s,
            Err(e) => {
                debug!("Cache set ({key}): serialisation error: {e}");
                return;
            }
        };

        // Atomic write via a temp file in the same directory.
        let tmp_path = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp_path, &json) {
            debug!("Cache set ({key}): write temp error: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            debug!("Cache set ({key}): rename error: {e}");
            let _ = std::fs::remove_file(&tmp_path);
            return;
        }

        debug!("Cache set ({key}): written to {}", path.display());
    }

    /// Delete a single cache entry (and its lock file if present).
    pub fn invalidate(&self, key: &CacheKey) {
        let path = key.to_path(&self.cache_dir);
        remove_file_if_exists(&path, "invalidate cache entry");

        let lock_path = self.lock_path(key);
        remove_file_if_exists(&lock_path, "invalidate lock file");
    }

    /// Delete the entire vault-scoped cache directory.
    pub fn invalidate_vault(&self, vault_name: &str) {
        let vault_dir = self.cache_dir.join(vault_name);
        if vault_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&vault_dir) {
                debug!("invalidate_vault({vault_name}): {e}");
            } else {
                debug!("invalidate_vault({vault_name}): removed {}", vault_dir.display());
            }
        }
    }

    /// Clear cached data.
    ///
    /// * `vault = Some(name)` — clears only that vault's directory.
    /// * `vault = None`       — clears the entire cache directory.
    pub fn clear(&self, vault: Option<&str>) {
        match vault {
            Some(name) => self.invalidate_vault(name),
            None => {
                if self.cache_dir.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&self.cache_dir) {
                        debug!("clear all: {e}");
                    } else {
                        debug!("clear all: removed {}", self.cache_dir.display());
                    }
                }
            }
        }
    }

    /// Return a summary of the current cache state.
    pub fn status(&self) -> CacheStatus {
        let mut entry_count = 0usize;
        let mut total_size_bytes = 0u64;
        let mut entries = Vec::new();

        if self.cache_dir.exists() {
            collect_entries(
                &self.cache_dir,
                &self.cache_dir,
                self.ttl_secs,
                &mut entry_count,
                &mut total_size_bytes,
                &mut entries,
            );
        }

        CacheStatus {
            cache_dir: self.cache_dir.clone(),
            enabled: self.enabled,
            ttl_secs: self.ttl_secs,
            entry_count,
            total_size_bytes,
            entries,
        }
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn lock_path(&self, key: &CacheKey) -> PathBuf {
        key.to_path(&self.cache_dir).with_extension("lock")
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn remove_file_if_exists(path: &Path, context: &str) {
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            debug!("{context}: {e}");
        } else {
            debug!("{context}: removed {}", path.display());
        }
    }
}

/// Recursively walk `dir`, collecting metadata for every `.json` cache file.
fn collect_entries(
    dir: &Path,
    cache_root: &Path,
    ttl_secs: u64,
    entry_count: &mut usize,
    total_size_bytes: &mut u64,
    entries: &mut Vec<CacheEntryInfo>,
) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            debug!("status: read_dir({}): {e}", dir.display());
            return;
        }
    };

    for item in read_dir.flatten() {
        let path = item.path();
        let ft = match item.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if ft.is_dir() {
            collect_entries(&path, cache_root, ttl_secs, entry_count, total_size_bytes, entries);
            continue;
        }

        // Only count `.json` files (skip `.tmp`, `.lock`, etc.)
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = meta.len();
        *total_size_bytes += size;
        *entry_count += 1;

        // Build a human-readable key from the path relative to the cache root.
        let rel = path.strip_prefix(cache_root).unwrap_or(&path);
        let key_str = rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");

        // Parse timestamps from file contents if possible; fall back to epoch.
        let (created_at, expires_at, is_stale) =
            parse_entry_timestamps(&path, ttl_secs);

        entries.push(CacheEntryInfo {
            key: key_str,
            created_at,
            expires_at,
            size_bytes: size,
            is_stale,
        });
    }
}

/// Try to parse `created_at` from a JSON cache file.
fn parse_entry_timestamps(
    path: &Path,
    ttl_secs: u64,
) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>, bool) {
    use chrono::Duration;

    // Minimal struct so we can deserialise without needing the full `T`.
    #[derive(serde::Deserialize)]
    struct Header {
        created_at: chrono::DateTime<Utc>,
    }

    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let created_at = serde_json::from_str::<Header>(&raw)
        .map(|h| h.created_at)
        .unwrap_or_else(|_| Utc::now());

    let expires_at = created_at + Duration::seconds(ttl_secs as i64);
    let is_stale = Utc::now() >= expires_at;

    (created_at, expires_at, is_stale)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::models::CacheKey;
    use tempfile::tempdir;

    fn make_manager(dir: &Path, enabled: bool, ttl: u64) -> CacheManager {
        CacheManager::new(dir.to_path_buf(), enabled, ttl)
    }

    #[test]
    fn test_get_returns_none_for_missing_entry() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::VaultList;
        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_set_then_get_returns_data() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::VaultList;

        let data = vec!["vault-a".to_string(), "vault-b".to_string()];
        mgr.set(&key, &data);

        let retrieved: Option<Vec<String>> = mgr.get(&key);
        assert_eq!(retrieved, Some(data));
    }

    #[test]
    fn test_get_returns_none_for_expired_entry() {
        // TTL = 0 means every entry is immediately expired.
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 0);
        let key = CacheKey::VaultList;

        let data = vec!["vault-a".to_string()];
        mgr.set(&key, &data);

        let retrieved: Option<Vec<String>> = mgr.get(&key);
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_get_returns_none_when_disabled() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), false, 300);
        let key = CacheKey::VaultList;

        // Even if we manually write a file, get() should return None.
        let enabled_mgr = make_manager(dir.path(), true, 300);
        enabled_mgr.set(&key, &vec!["v1".to_string()]);

        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_invalidate_removes_entry() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::SecretsList { vault_name: "my-vault".to_string() };

        mgr.set(&key, &vec!["secret-1".to_string()]);
        assert!(mgr.get::<Vec<String>>(&key).is_some());

        mgr.invalidate(&key);
        assert!(mgr.get::<Vec<String>>(&key).is_none());
    }

    #[test]
    fn test_invalidate_vault_removes_all_entries() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        let key1 = CacheKey::SecretsList { vault_name: "my-vault".to_string() };
        let key2 = CacheKey::FileList { vault_name: "my-vault".to_string() };

        mgr.set(&key1, &vec!["s1".to_string()]);
        mgr.set(&key2, &vec!["f1".to_string()]);

        assert!(mgr.get::<Vec<String>>(&key1).is_some());
        assert!(mgr.get::<Vec<String>>(&key2).is_some());

        mgr.invalidate_vault("my-vault");

        assert!(mgr.get::<Vec<String>>(&key1).is_none());
        assert!(mgr.get::<Vec<String>>(&key2).is_none());
    }

    #[test]
    fn test_clear_all_removes_everything() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        mgr.set(&CacheKey::VaultList, &vec!["v1".to_string()]);
        mgr.set(
            &CacheKey::SecretsList { vault_name: "vlt".to_string() },
            &vec!["s1".to_string()],
        );

        mgr.clear(None);

        assert!(mgr.get::<Vec<String>>(&CacheKey::VaultList).is_none());
        assert!(
            mgr.get::<Vec<String>>(&CacheKey::SecretsList { vault_name: "vlt".to_string() })
                .is_none()
        );
    }

    #[test]
    fn test_clear_specific_vault() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        mgr.set(&CacheKey::VaultList, &vec!["v1".to_string()]);
        mgr.set(
            &CacheKey::SecretsList { vault_name: "target".to_string() },
            &vec!["s1".to_string()],
        );
        mgr.set(
            &CacheKey::SecretsList { vault_name: "other".to_string() },
            &vec!["s2".to_string()],
        );

        mgr.clear(Some("target"));

        // VaultList and "other" vault should remain.
        assert!(mgr.get::<Vec<String>>(&CacheKey::VaultList).is_some());
        assert!(
            mgr.get::<Vec<String>>(&CacheKey::SecretsList { vault_name: "other".to_string() })
                .is_some()
        );
        // "target" should be gone.
        assert!(
            mgr.get::<Vec<String>>(&CacheKey::SecretsList { vault_name: "target".to_string() })
                .is_none()
        );
    }

    #[test]
    fn test_corrupt_json_treated_as_miss() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::VaultList;
        let path = key.to_path(&mgr.cache_dir);

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not valid json at all {{{{").unwrap();

        let result: Option<Vec<String>> = mgr.get(&key);
        assert!(result.is_none());
    }

    #[test]
    fn test_set_creates_directories() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::SecretsList { vault_name: "brand-new-vault".to_string() };

        // Parent directory does not exist yet.
        assert!(!dir.path().join("brand-new-vault").exists());

        mgr.set(&key, &vec!["s".to_string()]);

        assert!(dir.path().join("brand-new-vault").exists());
        assert!(mgr.get::<Vec<String>>(&key).is_some());
    }

    #[test]
    fn test_status_reports_entries() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        // Empty cache.
        let s = mgr.status();
        assert_eq!(s.entry_count, 0);
        assert_eq!(s.total_size_bytes, 0);
        assert!(s.entries.is_empty());

        // Add two entries.
        mgr.set(&CacheKey::VaultList, &vec!["v1".to_string()]);
        mgr.set(
            &CacheKey::SecretsList { vault_name: "v1".to_string() },
            &vec!["s1".to_string()],
        );

        let s = mgr.status();
        assert_eq!(s.entry_count, 2);
        assert!(s.total_size_bytes > 0);
        assert_eq!(s.entries.len(), 2);
        assert!(s.enabled);
        assert_eq!(s.ttl_secs, 300);
    }
}
