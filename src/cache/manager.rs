//! Cache manager for client-side caching of listing operations.
//!
//! Stores cache entries as JSON files on disk, organised by vault name.
//! All I/O errors are degraded gracefully — they are logged at debug level
//! and never propagate to callers.

use chrono::Utc;
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::cache::models::{
    validate_cache_vault_name, CacheEntry, CacheEntryInfo, CacheKey, CacheStatus,
};
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

    pub fn from_config(config: &crate::config::Config) -> Self {
        Self::from_config_with_dir(config, Self::resolve_cache_dir())
    }

    /// Create a `CacheManager` from config, rooted at an explicit directory
    /// rather than the resolved `XV_CACHE_DIR`/`dirs::cache_dir()` location.
    ///
    /// Intended for tests that need an isolated cache directory (e.g. a
    /// `tempfile::TempDir`) without touching the real OS cache path.
    pub fn from_config_with_dir(config: &crate::config::Config, cache_dir: PathBuf) -> Self {
        let enabled = config.cache_enabled && config.cache_ttl_secs > 0;
        Self::new(cache_dir, enabled, config.cache_ttl_secs)
    }

    /// Resolve the root cache directory: `XV_CACHE_DIR` env var override
    /// (if set and non-empty), else the OS cache directory joined with
    /// `xv`, else `/tmp/xv` as a last resort. A relative `XV_CACHE_DIR` is
    /// resolved against the process's current working directory, which can
    /// shift under `cd`/`chdir` — an absolute path is recommended.
    fn resolve_cache_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("XV_CACHE_DIR") {
            if !dir.is_empty() {
                return PathBuf::from(dir);
            }
        }
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("xv")
    }

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
            debug!(
                "Cache expired ({key}): age {age_secs}s ≥ ttl {}s",
                self.ttl_secs
            );
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

    /// Delete every cache entry scoped to `vault_name`, across both on-disk
    /// layouts:
    /// - depth-1 (`cache_dir/<vault>/...`) — `FileList` entries, unaffected
    ///   by the Phase B `(backend, vault)` cache-key change.
    /// - depth-2 (`cache_dir/<backend>/<vault>/...`) — `SecretsList` v3
    ///   entries, nested one level deeper under a `backend` directory since
    ///   two workspace entries can share a vault NAME on different
    ///   backends. Every backend directory is walked so `xv cache clear
    ///   <vault>` still finds and removes the entry regardless of which
    ///   backend it belongs to.
    ///
    /// Guard (Bugbot review MINOR): a vault can be literally named the same
    /// as a backend (a built-in kind like `"local"`/`"azure"`/`"aws"`, or a
    /// `named_backends` key like `"local-a"`) — in that case `cache_dir/
    /// <vault_name>` is ALSO the v3 backend directory holding every OTHER
    /// vault's `secrets-list-v3.json` for that backend. Blindly
    /// `remove_dir_all`-ing the depth-1 path in that situation would nuke
    /// every unrelated vault's cache under that backend, not just the one
    /// vault named `vault_name`. [`looks_like_v3_backend_dir`] detects this
    /// (any immediate child directory contains `secrets-list-v3.json`) and,
    /// instead of removing the whole directory, selectively removes only
    /// the depth-1 `FileList` entries that actually belong to
    /// `vault_name` (`files-list.json`/`files-list-recursive.json` and
    /// their lock files — the only `CacheKey` variants that ever write at
    /// depth 1) — leaving the child directories that form the v3 backend
    /// layout untouched (Bugbot review LOW: an earlier version of this
    /// guard skipped the depth-1 cleanup entirely, leaving stale
    /// `FileList` entries behind forever in the collision case). The
    /// depth-2 walk below still correctly removes this vault's OWN
    /// `SecretsList` entries from every backend directory (including, as a
    /// harmless edge case, one nested under itself).
    pub fn invalidate_vault(&self, vault_name: &str) {
        if let Err(reason) = validate_cache_vault_name(vault_name) {
            debug!("invalidate_vault({vault_name}): rejected — {reason}");
            return;
        }

        let vault_dir = self.cache_dir.join(vault_name);
        if vault_dir.starts_with(&self.cache_dir) && vault_dir.exists() {
            if looks_like_v3_backend_dir(&vault_dir) {
                // `cache_dir/<vault_name>` is actually a v3 BACKEND directory
                // (a backend happens to be named like this vault) — removing it
                // whole would wipe other vaults' entries. Both SecretsList and
                // (now) FileList are backend-nested (`cache_dir/<backend>/<vault>/…`),
                // so the read_dir loop below removes this vault's real entries
                // under every backend; nothing to do here but skip.
                debug!(
                    "invalidate_vault({vault_name}): {} looks like a v3 backend directory \
                     — skipping whole-directory removal; the per-backend loop below \
                     removes this vault's nested entries",
                    vault_dir.display(),
                );
            } else if let Err(e) = std::fs::remove_dir_all(&vault_dir) {
                debug!("invalidate_vault({vault_name}): {e}");
            } else {
                debug!(
                    "invalidate_vault({vault_name}): removed {}",
                    vault_dir.display()
                );
            }
        }

        if let Ok(read_dir) = std::fs::read_dir(&self.cache_dir) {
            for entry in read_dir.flatten() {
                let backend_dir = entry.path();
                if !backend_dir.is_dir() {
                    continue;
                }
                let nested = backend_dir.join(vault_name);
                if nested.starts_with(&self.cache_dir) && nested.is_dir() {
                    if let Err(e) = std::fs::remove_dir_all(&nested) {
                        debug!("invalidate_vault({vault_name}): {e}");
                    } else {
                        debug!(
                            "invalidate_vault({vault_name}): removed {}",
                            nested.display()
                        );
                    }
                }
            }
        }
    }

    /// Clear cached data.
    ///
    /// * `vault = Some(name)` — clears only that vault's directory.
    /// * `vault = None`       — clears the entire cache directory.
    pub fn clear(&self, vault: Option<&str>) {
        match vault {
            Some(name) => {
                if let Err(reason) = validate_cache_vault_name(name) {
                    debug!("clear({name}): rejected — {reason}");
                    return;
                }
                self.invalidate_vault(name);
            }
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

/// True if `dir` has any immediate child DIRECTORY that itself contains a
/// `secrets-list-v3.json` file — i.e. `dir` is (also) serving as a v3
/// backend directory (`cache_dir/<backend>/<vault>/secrets-list-v3.json`),
/// per [`CacheManager::invalidate_vault`]'s collision guard.
fn looks_like_v3_backend_dir(dir: &Path) -> bool {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in read_dir.flatten() {
        let child = entry.path();
        if child.is_dir()
            && child
                .join(crate::cache::models::SECRETS_LIST_FILENAME)
                .exists()
        {
            return true;
        }
    }
    false
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
            collect_entries(
                &path,
                cache_root,
                ttl_secs,
                entry_count,
                total_size_bytes,
                entries,
            );
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
        let key_str = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");

        // Parse timestamps from file contents if possible; fall back to epoch.
        let (created_at, expires_at, is_stale) = parse_entry_timestamps(&path, ttl_secs);

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

    /// Bugbot MEDIUM review (record-types plan): `SecretSummary` gained a
    /// `tags` field with `#[serde(default)]`, so a v1-schema secrets-list
    /// cache entry written before that change would deserialize
    /// successfully but with an empty `tags` map — silently hiding every
    /// typed secret's `xv-type`/`f.*` tags from `ls --type`/the TYPE column
    /// until TTL expiry. The fix renames the on-disk filename
    /// (`secrets-list.json` -> `secrets-list-v2.json`, see
    /// `SECRETS_LIST_FILENAME` in `cache::models`) so a pre-existing v1
    /// entry simply misses instead of silently deserializing into
    /// incomplete data. This test writes a legacy-shaped entry at the OLD
    /// path directly (bypassing `CacheManager::set`, which only ever
    /// writes the current path) and asserts `get` treats it as absent.
    #[test]
    fn test_get_misses_legacy_pre_v2_secrets_list_cache_file() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "myvault".to_string(),
        };

        // Simulate a cache entry written by a pre-Task-10 binary: same
        // vault directory, but the OLD filename and a payload shaped like
        // the OLD (tags-less) SecretSummary.
        let legacy_dir = dir.path().join("myvault");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("secrets-list.json");
        let legacy_json = serde_json::json!({
            "created_at": chrono::Utc::now().to_rfc3339(),
            "ttl_secs": 300,
            "vault_name": "myvault",
            "entry_type": "SecretsList",
            "data": [{"name": "cred", "note": null, "folder": null, "groups": null, "updated_on": "", "original_name": "cred"}]
        });
        std::fs::write(&legacy_path, legacy_json.to_string()).unwrap();

        // The new code path never looks at the old filename — a cache
        // miss, not a hit with an empty `tags` map masking a typed secret.
        let result: Option<Vec<crate::secret::manager::SecretSummary>> = mgr.get(&key);
        assert!(
            result.is_none(),
            "legacy pre-v2 cache entry must miss, not silently deserialize: {result:?}"
        );

        // The current writer never touches the legacy path either.
        let data = vec![];
        mgr.set::<Vec<crate::secret::manager::SecretSummary>>(&key, &data);
        assert!(
            legacy_path.exists(),
            "set() must not overwrite/consume the legacy file"
        );
        assert!(
            key.to_path(dir.path()).ends_with("secrets-list-v3.json"),
            "current schema must resolve to the v3 filename"
        );
    }

    /// Multi-vault workspaces plan (Phase B, Task 7): `CacheKey::SecretsList`
    /// gained a `backend` field so the on-disk path nests under a `backend`
    /// directory (`cache_dir/<backend>/<vault>/secrets-list-v3.json`)
    /// instead of `cache_dir/<vault>/secrets-list-v2.json`. A pre-existing
    /// v2-era entry (written before this change, one directory level
    /// shallower) must miss cleanly rather than being read as if it were the
    /// new schema — mirrors `test_get_misses_legacy_pre_v2_secrets_list_cache_file`
    /// above for the v2 -> v3 bump.
    #[test]
    fn test_get_misses_legacy_pre_v3_secrets_list_cache_file() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "myvault".to_string(),
        };

        // Simulate a v2-era entry: `cache_dir/myvault/secrets-list-v2.json`
        // — no `backend` directory component at all.
        let legacy_dir = dir.path().join("myvault");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("secrets-list-v2.json");
        let legacy_json = serde_json::json!({
            "created_at": chrono::Utc::now().to_rfc3339(),
            "ttl_secs": 300,
            "vault_name": "myvault",
            "entry_type": "SecretsList",
            "data": [{"name": "cred", "note": null, "folder": null, "groups": null, "updated_on": "", "original_name": "cred", "enabled": true, "content_type": "", "tags": {}}]
        });
        std::fs::write(&legacy_path, legacy_json.to_string()).unwrap();

        let result: Option<Vec<crate::secret::manager::SecretSummary>> = mgr.get(&key);
        assert!(
            result.is_none(),
            "legacy pre-v3 cache entry must miss, not be read as the new (backend, vault) schema: {result:?}"
        );

        assert_eq!(
            key.to_path(dir.path()),
            dir.path()
                .join("azure")
                .join("myvault")
                .join("secrets-list-v3.json"),
            "v3 schema must nest under a backend directory"
        );
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
        let key = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "my-vault".to_string(),
        };

        mgr.set(&key, &vec!["secret-1".to_string()]);
        assert!(mgr.get::<Vec<String>>(&key).is_some());

        mgr.invalidate(&key);
        assert!(mgr.get::<Vec<String>>(&key).is_none());
    }

    #[test]
    fn test_invalidate_vault_removes_all_entries() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        let key1 = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "my-vault".to_string(),
        };
        let key2 = CacheKey::FileList {
            backend: "azure".to_string(),
            vault_name: "my-vault".to_string(),
            recursive: false,
        };

        mgr.set(&key1, &vec!["s1".to_string()]);
        mgr.set(&key2, &vec!["f1".to_string()]);

        assert!(mgr.get::<Vec<String>>(&key1).is_some());
        assert!(mgr.get::<Vec<String>>(&key2).is_some());

        mgr.invalidate_vault("my-vault");

        assert!(mgr.get::<Vec<String>>(&key1).is_none());
        assert!(mgr.get::<Vec<String>>(&key2).is_none());
    }

    /// Bugbot review MINOR: a vault can be literally named the same as a
    /// backend (a built-in kind or a `named_backends` key) — `cache_dir/
    /// <that name>` is then simultaneously a depth-1 `FileList` directory
    /// AND the v3 `SecretsList` backend directory holding every OTHER
    /// vault's cache for that backend. `invalidate_vault` must not
    /// `remove_dir_all` that shared directory wholesale, or invalidating
    /// the colliding-named vault would silently destroy unrelated vaults'
    /// cached listings too.
    #[test]
    fn test_invalidate_vault_does_not_nuke_backend_dir_when_vault_name_collides() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        // A SecretsList entry for a DIFFERENT vault under backend "azure":
        // cache_dir/azure/other-vault/secrets-list-v3.json.
        let other_vault_key = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "other-vault".to_string(),
        };
        mgr.set(&other_vault_key, &vec!["s1".to_string()]);

        // A vault literally named "azure" (on backend "local") — its FileList
        // entry lands at `cache_dir/local/azure/files-list.json`. Invalidating
        // vault "azure" must clear it (via the per-backend loop) without
        // touching the `cache_dir/azure/...` backend directory above.
        let colliding_file_key = CacheKey::FileList {
            backend: "local".to_string(),
            vault_name: "azure".to_string(),
            recursive: false,
        };
        mgr.set(&colliding_file_key, &vec!["f1".to_string()]);

        assert!(mgr.get::<Vec<String>>(&other_vault_key).is_some());
        assert!(mgr.get::<Vec<String>>(&colliding_file_key).is_some());

        mgr.invalidate_vault("azure");

        // The critical guarantee: an unrelated vault's cache must survive
        // just because its backend directory shares a name with the vault
        // being invalidated.
        assert!(
            mgr.get::<Vec<String>>(&other_vault_key).is_some(),
            "unrelated vault's cache must survive a same-named-as-backend invalidate_vault call"
        );
        // The vault's OWN depth-1 FileList entry must still be cleared
        // (Bugbot review LOW follow-up: the collision guard must not leave
        // this vault's own file-list cache stale just because it skips the
        // whole-directory removal).
        assert!(
            mgr.get::<Vec<String>>(&colliding_file_key).is_none(),
            "the colliding vault's own FileList entry must still be invalidated"
        );
    }

    /// Bugbot review LOW: the collision guard above must not leave THIS
    /// vault's own depth-1 `FileList` entries stale — only the
    /// whole-directory `remove_dir_all` is skipped (to protect other
    /// vaults' `SecretsList` entries); the vault's own `files-list.json`
    /// must still be removed via the selective cleanup.
    #[test]
    fn test_invalidate_vault_clears_own_file_list_entries_under_collision_guard() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        // A vault named "local-a" also doubles as a v3 backend directory:
        // cache_dir/local-a/default/secrets-list-v3.json (backend
        // "local-a", vault "default").
        let nested_secrets_key = CacheKey::SecretsList {
            backend: "local-a".to_string(),
            vault_name: "default".to_string(),
        };
        mgr.set(&nested_secrets_key, &vec!["s1".to_string()]);

        // The vault literally named "local-a" (here on backend "azure") has its
        // OWN FileList entries at cache_dir/azure/local-a/files-list.json and
        // files-list-recursive.json — a different tree from the
        // cache_dir/local-a/... backend directory above.
        let file_key = CacheKey::FileList {
            backend: "azure".to_string(),
            vault_name: "local-a".to_string(),
            recursive: false,
        };
        let file_key_recursive = CacheKey::FileList {
            backend: "azure".to_string(),
            vault_name: "local-a".to_string(),
            recursive: true,
        };
        mgr.set(&file_key, &vec!["f1".to_string()]);
        mgr.set(&file_key_recursive, &vec!["f2".to_string()]);

        assert!(mgr.get::<Vec<String>>(&nested_secrets_key).is_some());
        assert!(mgr.get::<Vec<String>>(&file_key).is_some());
        assert!(mgr.get::<Vec<String>>(&file_key_recursive).is_some());

        mgr.invalidate_vault("local-a");

        // The nested v3 SecretsList entry (a DIFFERENT vault, "default",
        // under backend "local-a") must survive.
        assert!(
            mgr.get::<Vec<String>>(&nested_secrets_key).is_some(),
            "nested secrets-list-v3.json for a different vault must survive"
        );
        // But "local-a"'s own FileList entries (both recursive and not)
        // must be gone — no longer left stale forever.
        assert!(
            mgr.get::<Vec<String>>(&file_key).is_none(),
            "the colliding vault's own files-list.json must be removed"
        );
        assert!(
            mgr.get::<Vec<String>>(&file_key_recursive).is_none(),
            "the colliding vault's own files-list-recursive.json must be removed"
        );
    }

    #[test]
    fn test_looks_like_v3_backend_dir_detects_nested_secrets_list_file() {
        let dir = tempdir().unwrap();
        let backend_dir = dir.path().join("azure");
        let vault_dir = backend_dir.join("some-vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        std::fs::write(vault_dir.join("secrets-list-v3.json"), b"{}").unwrap();

        assert!(looks_like_v3_backend_dir(&backend_dir));
    }

    #[test]
    fn test_looks_like_v3_backend_dir_false_for_plain_vault_dir() {
        let dir = tempdir().unwrap();
        let vault_dir = dir.path().join("my-vault");
        std::fs::create_dir_all(&vault_dir).unwrap();
        std::fs::write(vault_dir.join("files-list.json"), b"[]").unwrap();

        assert!(!looks_like_v3_backend_dir(&vault_dir));
    }

    #[test]
    fn test_clear_all_removes_everything() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        mgr.set(&CacheKey::VaultList, &vec!["v1".to_string()]);
        mgr.set(
            &CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "vlt".to_string(),
            },
            &vec!["s1".to_string()],
        );

        mgr.clear(None);

        assert!(mgr.get::<Vec<String>>(&CacheKey::VaultList).is_none());
        assert!(mgr
            .get::<Vec<String>>(&CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "vlt".to_string()
            })
            .is_none());
    }

    #[test]
    fn test_clear_specific_vault() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);

        mgr.set(&CacheKey::VaultList, &vec!["v1".to_string()]);
        mgr.set(
            &CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "target".to_string(),
            },
            &vec!["s1".to_string()],
        );
        mgr.set(
            &CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "other".to_string(),
            },
            &vec!["s2".to_string()],
        );

        mgr.clear(Some("target"));

        // VaultList and "other" vault should remain.
        assert!(mgr.get::<Vec<String>>(&CacheKey::VaultList).is_some());
        assert!(mgr
            .get::<Vec<String>>(&CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "other".to_string()
            })
            .is_some());
        // "target" should be gone.
        assert!(mgr
            .get::<Vec<String>>(&CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "target".to_string()
            })
            .is_none());
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
        let key = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "brand-new-vault".to_string(),
        };

        // Parent directory does not exist yet.
        assert!(!dir.path().join("azure").join("brand-new-vault").exists());

        mgr.set(&key, &vec!["s".to_string()]);

        assert!(dir.path().join("azure").join("brand-new-vault").exists());
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
            &CacheKey::SecretsList {
                backend: "azure".to_string(),
                vault_name: "v1".to_string(),
            },
            &vec!["s1".to_string()],
        );

        let s = mgr.status();
        assert_eq!(s.entry_count, 2);
        assert!(s.total_size_bytes > 0);
        assert_eq!(s.entries.len(), 2);
        assert!(s.enabled);
        assert_eq!(s.ttl_secs, 300);
    }

    #[test]
    fn test_invalidate_vault_rejects_traversal_name() {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Create a directory outside the cache root that an attacker would
        // want to delete.
        let outside = dir.path().join("outside_target");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("important.txt"), b"do not delete").unwrap();

        let mgr = make_manager(&cache_dir, true, 300);

        // Attempt to invalidate with a traversal name.
        mgr.invalidate_vault("../outside_target");

        // The outside directory must still exist.
        assert!(
            outside.exists(),
            "invalidate_vault traversal should NOT delete outside directory"
        );
        assert!(outside.join("important.txt").exists());
    }

    #[test]
    fn test_clear_vault_rejects_traversal_name() {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let outside = dir.path().join("outside_target2");
        std::fs::create_dir_all(&outside).unwrap();

        let mgr = make_manager(&cache_dir, true, 300);
        mgr.clear(Some("../outside_target2"));

        assert!(
            outside.exists(),
            "clear(Some(traversal)) should NOT delete outside directory"
        );
    }
}
