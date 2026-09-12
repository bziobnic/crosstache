//! Centralized, mutation-driven cache invalidation.
//!
//! Every command that mutates state (writing/updating/deleting a secret, a
//! file, or a vault) must drop the now-stale listing cache. This module is the
//! single seam through which that happens, so call sites no longer each build a
//! [`CacheManager`] and hand-assemble a [`CacheKey`]. Keeping it in one place
//! means the exact key set invalidated by each kind of mutation is defined once
//! and reviewed once.
//!
//! This is also the single seam where a future secret-*value* cache would hook
//! its own invalidation in — today the cache stores only listing metadata, and
//! nothing here (or anywhere in `crate::cache`) may be widened to cache secret
//! values.
//!
//! All functions are best-effort and non-fatal: a failure to invalidate is
//! logged inside [`CacheManager`] and never propagates. `backend` is the
//! resolved REGISTRY name actually written to (with a workspace, the entry's
//! `backend` field), not necessarily `config.effective_backend_name()`.
//!
//! Paths audited and deliberately NOT wired here: `xv cx rm` detaches a
//! workspace alias without touching vault data, so the cached listing stays
//! true; `xv init` refuses when a config exists and a new config has a new
//! identity fingerprint, so old entries are simply never read again.

use crate::cache::{CacheKey, CacheManager};
use crate::config::Config;

/// Invalidate the secrets-list cache for `(backend, vault)` after a secret
/// mutation (create/update/delete/rename/rotate/import).
pub fn on_secret_mutation(config: &Config, backend: &str, vault: &str) {
    let manager = CacheManager::from_config(config);
    manager.invalidate(&CacheKey::SecretsList {
        backend: backend.to_string(),
        vault_name: vault.to_string(),
    });
}

/// Invalidate both file-list cache entries (recursive and non-recursive) for
/// `(backend, vault)` after a file mutation (upload/delete/sync).
pub fn on_file_mutation(config: &Config, backend: &str, vault: &str) {
    let manager = CacheManager::from_config(config);
    for recursive in [true, false] {
        manager.invalidate(&CacheKey::FileList {
            backend: backend.to_string(),
            vault_name: vault.to_string(),
            recursive,
        });
    }
}

/// Invalidate the vault-list cache after a vault-set mutation (create, delete,
/// restore, purge, update) that changes which vaults exist or their metadata.
///
/// This mirrors the pre-refactor behavior exactly: only the vault list is
/// dropped. It does NOT clear a removed vault's per-vault secret/file
/// entries — for that, use `on_vault_removed`.
pub fn on_vault_mutation(config: &Config) {
    let manager = CacheManager::from_config(config);
    manager.invalidate(&CacheKey::VaultList);
}

/// Invalidate everything cached for one `(backend, vault)` after the vault
/// itself was deleted or purged, plus the vault list. Scoped to the registry
/// name so a same-named vault on another backend keeps its cache. Callers
/// invoke this only after the backend reported success; an aborted
/// confirmation or a failed delete must leave the cache untouched.
#[allow(dead_code)] // wired in vault_ops/backend_ops
pub fn on_vault_removed(config: &Config, backend: &str, vault: &str) {
    let manager = CacheManager::from_config(config);
    remove_vault_entries(&manager, backend, vault);
    manager.invalidate(&CacheKey::VaultList);
}

/// Invalidate every listing under one backend registry name after the
/// backend was removed from the configuration (`xv backend rm`), plus the
/// vault list. Must be called with the PRE-removal config: the identity
/// fingerprint includes the local store path, so once the block is gone the
/// manager would look under a different fingerprint.
#[allow(dead_code)] // wired in vault_ops/backend_ops
pub fn on_backend_removed(config: &Config, backend: &str) {
    let manager = CacheManager::from_config(config);
    remove_backend_entries(&manager, backend);
    manager.invalidate(&CacheKey::VaultList);
}

pub(crate) fn remove_vault_entries(manager: &CacheManager, backend: &str, vault: &str) {
    manager.invalidate(&CacheKey::SecretsList {
        backend: backend.to_string(),
        vault_name: vault.to_string(),
    });
    for recursive in [true, false] {
        manager.invalidate(&CacheKey::FileList {
            backend: backend.to_string(),
            vault_name: vault.to_string(),
            recursive,
        });
    }
}

pub(crate) fn remove_backend_entries(manager: &CacheManager, backend: &str) {
    manager.invalidate_backend(backend);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed(mgr: &CacheManager, backend: &str, vault: &str) {
        mgr.set(
            &CacheKey::SecretsList {
                backend: backend.into(),
                vault_name: vault.into(),
            },
            &vec![format!("{backend}:{vault}")],
        );
        for recursive in [true, false] {
            mgr.set(
                &CacheKey::FileList {
                    backend: backend.into(),
                    vault_name: vault.into(),
                    recursive,
                },
                &vec![format!("{backend}:{vault}:{recursive}")],
            );
        }
    }

    fn present(mgr: &CacheManager, backend: &str, vault: &str) -> (bool, bool, bool) {
        let s = mgr
            .get::<Vec<String>>(&CacheKey::SecretsList {
                backend: backend.into(),
                vault_name: vault.into(),
            })
            .is_some();
        let f = mgr
            .get::<Vec<String>>(&CacheKey::FileList {
                backend: backend.into(),
                vault_name: vault.into(),
                recursive: false,
            })
            .is_some();
        let r = mgr
            .get::<Vec<String>>(&CacheKey::FileList {
                backend: backend.into(),
                vault_name: vault.into(),
                recursive: true,
            })
            .is_some();
        (s, f, r)
    }

    #[test]
    fn vault_removed_drops_exactly_that_backend_vault_and_the_vault_list() {
        let dir = tempdir().unwrap();
        let mgr = CacheManager::new(dir.path().to_path_buf(), true, 300);
        seed(&mgr, "local-a", "default");
        seed(&mgr, "local-a", "other");
        seed(&mgr, "local-b", "default"); // same vault NAME on another backend
        mgr.set(&CacheKey::VaultList, &vec!["default".to_string()]);

        remove_vault_entries(&mgr, "local-a", "default");
        mgr.invalidate(&CacheKey::VaultList);

        assert_eq!(present(&mgr, "local-a", "default"), (false, false, false));
        assert_eq!(present(&mgr, "local-a", "other"), (true, true, true));
        assert_eq!(present(&mgr, "local-b", "default"), (true, true, true));
        assert!(mgr.get::<Vec<String>>(&CacheKey::VaultList).is_none());
    }

    #[test]
    fn vault_removed_is_idempotent_and_ignores_missing_entries() {
        let dir = tempdir().unwrap();
        let mgr = CacheManager::new(dir.path().to_path_buf(), true, 300);
        remove_vault_entries(&mgr, "local-a", "never");
        seed(&mgr, "local-a", "default");
        remove_vault_entries(&mgr, "local-a", "default");
        remove_vault_entries(&mgr, "local-a", "default");
        assert_eq!(present(&mgr, "local-a", "default"), (false, false, false));
    }

    #[test]
    fn backend_removed_drops_every_vault_under_that_backend_only() {
        let dir = tempdir().unwrap();
        let mgr = CacheManager::new(dir.path().to_path_buf(), true, 300);
        seed(&mgr, "local-a", "default");
        seed(&mgr, "local-a", "other");
        seed(&mgr, "local-b", "default");

        remove_backend_entries(&mgr, "local-a");

        assert_eq!(present(&mgr, "local-a", "default"), (false, false, false));
        assert_eq!(present(&mgr, "local-a", "other"), (false, false, false));
        assert_eq!(present(&mgr, "local-b", "default"), (true, true, true));
    }
}
