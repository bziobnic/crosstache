//! Background cache refresh logic.
//!
//! Provides helpers for spawning background refresh processes and managing
//! lock files to prevent concurrent refreshes of the same cache entry.

use std::path::Path;
use std::time::{Duration, SystemTime};
use tracing::debug;

use crate::cache::models::CacheKey;

/// Age threshold for lock file staleness (60 seconds).
const LOCK_MAX_AGE_SECS: u64 = 60;

/// Spawn a detached `xv cache refresh --key <key>` child process.
///
/// The child runs independently; any error spawning it is logged at debug
/// level and silently ignored so the caller is never affected.
pub fn trigger_background_refresh(key: &CacheKey) {
    let key_str = key.to_string();
    debug!("Triggering background refresh for cache key: {key_str}");

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            debug!("background refresh: could not determine executable path: {e}");
            return;
        }
    };

    match std::process::Command::new(exe)
        .args(["cache", "refresh", "--key", &key_str])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => {
            debug!("Spawned background refresh for key: {key_str}");
        }
        Err(e) => {
            debug!("Failed to spawn background refresh for key {key_str}: {e}");
        }
    }
}

/// Returns `true` if a fresh (< 60 s old) lock file exists at `lock_path`.
///
/// If the lock file exists but is stale, it is removed and `false` is
/// returned so the caller may proceed.
pub fn is_locked(lock_path: &Path) -> bool {
    match lock_path.metadata() {
        Err(_) => false, // file does not exist or inaccessible
        Ok(meta) => {
            let age = meta
                .modified()
                .ok()
                .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
                .unwrap_or(Duration::MAX);

            if age < Duration::from_secs(LOCK_MAX_AGE_SECS) {
                true
            } else {
                // Stale lock — remove it.
                debug!("Removing stale lock file: {}", lock_path.display());
                let _ = std::fs::remove_file(lock_path);
                false
            }
        }
    }
}

/// Attempt to create the lock file at `lock_path`.
///
/// Returns `true` if the lock was successfully acquired, `false` if the
/// entry is already locked (or an error occurred creating the file).
pub fn acquire_lock(lock_path: &Path) -> bool {
    if is_locked(lock_path) {
        return false;
    }

    // Create parent directories if needed.
    if let Some(parent) = lock_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            debug!("acquire_lock: could not create parent dir: {e}");
            return false;
        }
    }

    match std::fs::File::create(lock_path) {
        Ok(_) => {
            debug!("Lock acquired: {}", lock_path.display());
            true
        }
        Err(e) => {
            debug!("acquire_lock: could not create lock file: {e}");
            false
        }
    }
}

/// Remove the lock file at `lock_path` (errors silently ignored).
pub fn release_lock(lock_path: &Path) {
    if lock_path.exists() {
        let _ = std::fs::remove_file(lock_path);
        debug!("Lock released: {}", lock_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_lock_acquire_and_release() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        // Initially not locked.
        assert!(!is_locked(&lock_path));

        // Acquire succeeds.
        assert!(acquire_lock(&lock_path));
        assert!(lock_path.exists());
        assert!(is_locked(&lock_path));

        // Release clears the lock.
        release_lock(&lock_path);
        assert!(!lock_path.exists());
        assert!(!is_locked(&lock_path));
    }

    #[test]
    fn test_lock_prevents_double_acquire() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("double.lock");

        // First acquire succeeds.
        assert!(acquire_lock(&lock_path));

        // Second acquire fails while first is still held.
        assert!(!acquire_lock(&lock_path));

        // Release and confirm the lock can be re-acquired.
        release_lock(&lock_path);
        assert!(acquire_lock(&lock_path));
        release_lock(&lock_path);
    }
}
