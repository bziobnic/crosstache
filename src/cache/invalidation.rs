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
/// dropped. It does NOT also clear a removed vault's per-vault secret/file
/// entries — for that, use [`on_vault_removed`].
pub fn on_vault_mutation(config: &Config) {
    let manager = CacheManager::from_config(config);
    manager.invalidate(&CacheKey::VaultList);
}

/// Invalidate the vault-list cache AND every cached listing scoped to a removed
/// `vault`, across every backend and file-list variant.
///
/// Provided for callers that need a fully consistent cache after a vault is
/// deleted/purged. (The legacy delete/purge paths only dropped the vault list;
/// they are preserved as-is via [`on_vault_mutation`] to keep this refactor
/// behavior-preserving — see the module and PR notes.)
#[allow(dead_code)] // completes the invalidation seam; not yet wired to a call site
pub fn on_vault_removed(config: &Config, vault: &str) {
    let manager = CacheManager::from_config(config);
    manager.invalidate_vault(vault);
    manager.invalidate(&CacheKey::VaultList);
}
