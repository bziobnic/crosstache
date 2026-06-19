//! Background cache refresh logic.
//!
//! Provides helpers for spawning background refresh processes and managing
//! lock files to prevent concurrent refreshes of the same cache entry.

use std::io::Write;
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
///
/// Acquisition is atomic: the lock file is created with
/// `OpenOptions::create_new(true)`, which fails if the file already exists.
/// This closes the check-then-create (TOCTOU) window that a separate
/// `is_locked()` test followed by `File::create()` would leave open — two
/// racing processes can no longer both observe "no lock" and then both create
/// it. If creation fails with `AlreadyExists`, the existing lock is examined:
/// a fresh lock means another refresh holds it (return `false`); a stale lock
/// is removed and acquisition is retried exactly once. The lock body records
/// the owning PID and a creation timestamp for stale-lock diagnostics.
pub fn acquire_lock(lock_path: &Path) -> bool {
    // Create parent directories if needed.
    if let Some(parent) = lock_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            debug!("acquire_lock: could not create parent dir: {e}");
            return false;
        }
    }

    // Try once, then — only if the failure was a pre-existing *stale* lock —
    // remove it and try a second time. Capped at one retry so a live
    // contender can never spin.
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut file) => {
                // Best-effort metadata for diagnosing stale locks; failure to
                // write the body does not invalidate the (already-held) lock.
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = writeln!(file, "pid={} created_at={}", std::process::id(), now);
                debug!("Lock acquired: {}", lock_path.display());
                return true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Something already holds the lock. If it's fresh, yield;
                // if it's stale, `is_locked` removes it so the next attempt
                // can win the create race.
                if is_locked(lock_path) {
                    debug!("acquire_lock: lock held and fresh: {}", lock_path.display());
                    return false;
                }
                if attempt == 1 {
                    // Removed a stale lock but still lost the re-create race to
                    // another contender — treat as locked rather than spin.
                    debug!(
                        "acquire_lock: lost re-create race after clearing stale lock: {}",
                        lock_path.display()
                    );
                    return false;
                }
                // Loop to retry the atomic create now that the stale lock is gone.
            }
            Err(e) => {
                debug!("acquire_lock: could not create lock file: {e}");
                return false;
            }
        }
    }
    false
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

    #[test]
    fn test_stale_lock_is_reclaimed() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("stale.lock");

        // Create a lock file and backdate its mtime well past the staleness
        // threshold so it looks abandoned.
        {
            let mut f = std::fs::File::create(&lock_path).unwrap();
            f.write_all(b"pid=999999 created_at=0\n").unwrap();
            let stale = SystemTime::now() - Duration::from_secs(LOCK_MAX_AGE_SECS + 30);
            f.set_modified(stale).unwrap();
        }

        // A stale lock must be reclaimed atomically, not block acquisition.
        assert!(
            acquire_lock(&lock_path),
            "stale lock should be reclaimed atomically"
        );
        // The new lock records our PID, not the stale 999999.
        let body = std::fs::read_to_string(&lock_path).unwrap();
        assert!(
            body.contains(&format!("pid={}", std::process::id())),
            "lock body should record acquiring PID, got: {body}"
        );
        release_lock(&lock_path);
    }

    #[test]
    fn test_lock_body_records_pid() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("meta.lock");
        assert!(acquire_lock(&lock_path));
        let body = std::fs::read_to_string(&lock_path).unwrap();
        assert!(body.contains("pid="), "lock body missing pid: {body}");
        assert!(
            body.contains("created_at="),
            "lock body missing created_at: {body}"
        );
        release_lock(&lock_path);
    }
}
