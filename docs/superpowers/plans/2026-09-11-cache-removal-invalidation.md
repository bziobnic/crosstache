# Cache Removal Invalidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every removal-shaped mutation (vault delete/purge, backend rm, migrate, transfer) drops the listing-cache entries it makes stale, scoped to the exact `(backend, vault)` written, and the strict-mode and fingerprint docs match the code.

**Architecture:** All invalidation goes through `src/cache/invalidation.rs`. Two seam functions are (re)defined there — `on_vault_removed(config, backend, vault)` and `on_backend_removed(config, backend)` — backed by one new `CacheManager::invalidate_backend`. Call sites in `vault_ops.rs`, `backend_ops.rs`, `migrate_ops.rs`, and `transfer_ops.rs` call the seam only after the backend operation returns `Ok`. Tests assert on hermetic on-disk cache paths.

**Tech Stack:** Rust 2021, existing `CacheManager`/`CacheKey`, `tempfile`, CLI e2e harness `WorkspaceEnv` in `tests/e2e_workspaces.rs`.

**Spec:** `docs/superpowers/specs/2026-09-11-cache-removal-invalidation-design.md`

## Global Constraints

- Never run `cargo` with `run_in_background`; foreground only, timeout up to 600000 ms, pipe through `tail -40`.
- Never use `git stash`.
- Do not commit unless the task says to; never push.
- Cache on-disk layout is `<cache_dir>/<fingerprint>/<backend>/<vault>/{secrets-list-v5.json,files-list-v5.json,files-list-recursive-v5.json}` and `<cache_dir>/<fingerprint>/vaults-list.json`. Do not change filenames or the layout.
- `backend` passed to the seam is always the REGISTRY name (`config.effective_backend_name()`, a workspace entry's `backend`, or a `BackendKind` display name for migrate), never `Backend::name()`.
- stdout is data; status text goes to stderr via `output::*`.
- Gate before finishing any task: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and the named tests.

---

### Task 1: Seam functions and `invalidate_backend`

**Files:**
- Modify: `src/cache/manager.rs` (add `invalidate_backend` after `invalidate_vault`, ~line 366; tests in `mod tests` at ~line 665)
- Modify: `src/cache/invalidation.rs` (replace `on_vault_removed`, add `on_backend_removed`, add tests module)

**Interfaces:**
- Produces: `CacheManager::invalidate_backend(&self, backend: &str)`
- Produces: `pub fn on_vault_removed(config: &Config, backend: &str, vault: &str)`
- Produces: `pub fn on_backend_removed(config: &Config, backend: &str)`
- Produces (crate-private, for tests): `pub(crate) fn remove_vault_entries(manager: &CacheManager, backend: &str, vault: &str)` and `pub(crate) fn remove_backend_entries(manager: &CacheManager, backend: &str)`

- [ ] **Step 1: Write the failing manager unit test**

Append inside `mod tests` in `src/cache/manager.rs`:

```rust
    #[test]
    fn test_invalidate_backend_removes_only_that_backend_directory() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let a_secrets = CacheKey::SecretsList {
            backend: "local-a".into(),
            vault_name: "default".into(),
        };
        let a_files = CacheKey::FileList {
            backend: "local-a".into(),
            vault_name: "default".into(),
            recursive: true,
        };
        let b_secrets = CacheKey::SecretsList {
            backend: "local-b".into(),
            vault_name: "default".into(),
        };
        mgr.set(&a_secrets, &vec!["x".to_string()]);
        mgr.set(&a_files, &vec!["f".to_string()]);
        mgr.set(&b_secrets, &vec!["y".to_string()]);
        mgr.set(&CacheKey::VaultList, &vec!["default".to_string()]);

        mgr.invalidate_backend("local-a");

        assert!(!dir.path().join("local-a").exists(), "backend dir must be gone");
        assert!(mgr.get::<Vec<String>>(&a_secrets).is_none());
        assert!(mgr.get::<Vec<String>>(&a_files).is_none());
        assert_eq!(mgr.get::<Vec<String>>(&b_secrets), Some(vec!["y".to_string()]));
        // The vault list is the caller's job (the seam drops it), not this method's.
        assert!(mgr.get::<Vec<String>>(&CacheKey::VaultList).is_some());
    }

    #[test]
    fn test_invalidate_backend_rejects_traversal_and_missing() {
        let dir = tempdir().unwrap();
        let mgr = make_manager(dir.path(), true, 300);
        let key = CacheKey::SecretsList {
            backend: "local-a".into(),
            vault_name: "default".into(),
        };
        mgr.set(&key, &vec!["x".to_string()]);
        mgr.invalidate_backend("../local-a");
        mgr.invalidate_backend("");
        mgr.invalidate_backend("never-existed");
        assert!(mgr.get::<Vec<String>>(&key).is_some());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib cache::manager::tests::test_invalidate_backend -- --nocapture 2>&1 | tail -20`
Expected: compile error, no method `invalidate_backend`.

- [ ] **Step 3: Implement `invalidate_backend`**

Add to `impl CacheManager` in `src/cache/manager.rs`, directly after `invalidate_vault`:

```rust
    /// Delete every cache entry under `<entry_root>/<backend>/` — all vaults
    /// of one backend registry name. Used when a backend is removed from the
    /// configuration (`xv backend rm`): every listing under it is
    /// unreachable afterwards. Both `SecretsList` and `FileList` are
    /// backend-nested in the v5 layout, so this cannot touch another
    /// backend's entries. The vault list lives at the entry root and is not
    /// touched here; callers drop it through the invalidation seam.
    pub fn invalidate_backend(&self, backend: &str) {
        if let Err(reason) = validate_cache_vault_name(backend) {
            debug!("invalidate_backend({backend}): rejected — {reason}");
            return;
        }
        let root = self.entry_root();
        let backend_dir = root.join(backend);
        if !backend_dir.starts_with(&root) || !backend_dir.is_dir() {
            return;
        }
        match std::fs::remove_dir_all(&backend_dir) {
            Ok(()) => debug!(
                "invalidate_backend({backend}): removed {}",
                backend_dir.display()
            ),
            Err(e) => debug!("invalidate_backend({backend}): {e}"),
        }
    }
```

`validate_cache_vault_name` is already imported in manager.rs (used by `invalidate_vault`); it validates a single normal path component, which is the same shape a backend directory name must have.

- [ ] **Step 4: Run manager tests**

Run: `cargo test --lib cache::manager::tests -- --nocapture 2>&1 | tail -20`
Expected: all pass, including the two new tests.

- [ ] **Step 5: Write the failing invalidation seam tests**

Append to `src/cache/invalidation.rs`:

```rust
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
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test --lib cache::invalidation -- --nocapture 2>&1 | tail -20`
Expected: compile error, `remove_vault_entries` / `remove_backend_entries` not found.

- [ ] **Step 7: Replace `on_vault_removed` and add `on_backend_removed`**

In `src/cache/invalidation.rs`, delete the existing `on_vault_removed` (with its `#[allow(dead_code)]` attribute and doc comment, lines ~54-69) and append:

```rust
/// Invalidate everything cached for one `(backend, vault)` after the vault
/// itself was deleted or purged, plus the vault list. Scoped to the registry
/// name so a same-named vault on another backend keeps its cache. Callers
/// invoke this only after the backend reported success; an aborted
/// confirmation or a failed delete must leave the cache untouched.
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
```

Also update the doc comment on `on_vault_mutation` so its last sentence reads: "It does NOT clear a removed vault's per-vault secret/file entries — for that, use `on_vault_removed`." and extend the module doc with:

```rust
//! Paths audited and deliberately NOT wired here: `xv cx rm` detaches a
//! workspace alias without touching vault data, so the cached listing stays
//! true; `xv init` refuses when a config exists and a new config has a new
//! identity fingerprint, so old entries are simply never read again.
```

- [ ] **Step 8: Run the seam tests and the whole cache module**

Run: `cargo test --lib cache:: 2>&1 | tail -20`
Expected: all pass. Then `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -20` — expect a dead-code warning for `on_vault_removed`/`on_backend_removed` only if nothing calls them yet; that is resolved in Tasks 2 and 3, so for this task add `#[allow(dead_code)] // wired in vault_ops/backend_ops` above each of the two pub fns and remove it in the task that wires them.

- [ ] **Step 9: Commit**

```bash
git add src/cache/manager.rs src/cache/invalidation.rs
git commit -m "cache: scope vault-removal invalidation to (backend, vault) and add backend removal"
```

---

### Task 2: Wire `vault delete` and `vault purge`

**Files:**
- Modify: `src/cli/vault_ops.rs:171-181` (trait path Delete arm), `:264-277` (second-dispatch Delete arm), `:289-303` (Purge arm)
- Modify: `src/cache/invalidation.rs` (remove the temporary `#[allow(dead_code)]` on `on_vault_removed`)
- Test: `tests/e2e_workspaces.rs` (new helper on `WorkspaceEnv` + four tests)

**Interfaces:**
- Consumes: `crate::cache::invalidation::on_vault_removed(config, backend, vault)`
- Registry name for every vault verb: `config.effective_backend_name()`.

- [ ] **Step 1: Add a cache-path helper to `WorkspaceEnv`**

In `tests/e2e_workspaces.rs`, inside `impl WorkspaceEnv` (after `fn ok`), add:

```rust
    /// The on-disk cache entry for `(backend, vault, file)` under this
    /// environment's single identity fingerprint. Panics if the cache dir
    /// holds anything other than exactly one fingerprint directory, which
    /// would mean the test ran commands under two config identities.
    fn cache_entry(&self, backend: &str, vault: &str, file: &str) -> PathBuf {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&self.cache_dir)
            .expect("read cache dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(dirs.len(), 1, "expected one fingerprint dir, found {dirs:?}");
        dirs.remove(0).join(backend).join(vault).join(file)
    }
```

- [ ] **Step 2: Write the failing e2e tests**

Append to `tests/e2e_workspaces.rs`:

The local backend refuses to delete a vault that still holds secrets
(`src/backend/local/vaults.rs::delete_vault`, `Conflict`), so every test
deletes an EMPTY vault named `victim`. Empty listings are cached
unconditionally (`cache_manager.set(&cache_key, &fetched)` in
`src/cli/secret_ops.rs` and `file_ops.rs`), and a cache hit on `ls --vault X`
returns before the backend is asked whether `X` exists — so "stale" is
observable as `ls --vault victim` still SUCCEEDING after the vault is gone.

```rust
/// A04-01/02: deleting a vault on a named backend drops that vault's cached
/// secret and file listings (both recursive variants) and the vault list,
/// while a same-named vault on the other backend keeps its cache.
#[test]
fn vault_delete_drops_only_that_backends_vault_cache() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok_with_backend("local-a", &["vault", "create", "victim"]);
    env.ok_with_backend("local-b", &["vault", "create", "victim"]);
    env.ok_with_backend("local-a", &["set", "KEEP_A", "--value", "va"]); // in default, stays

    // Populate secret, file (both variants), and vault-list caches.
    env.ok_with_backend("local-a", &["ls", "--vault", "victim"]);
    env.ok_with_backend("local-a", &["file", "list", "--vault", "victim"]);
    env.ok_with_backend("local-a", &["file", "list", "--vault", "victim", "--recursive"]);
    env.ok_with_backend("local-a", &["ls"]);
    env.ok_with_backend("local-a", &["vault", "list"]);
    env.ok_with_backend("local-b", &["ls", "--vault", "victim"]);
    env.ok_with_backend("local-b", &["file", "list", "--vault", "victim"]);

    let a_secrets = env.cache_entry("local-a", "victim", "secrets-list-v5.json");
    let a_files = env.cache_entry("local-a", "victim", "files-list-v5.json");
    let a_files_rec = env.cache_entry("local-a", "victim", "files-list-recursive-v5.json");
    let a_default = env.cache_entry("local-a", "default", "secrets-list-v5.json");
    let b_secrets = env.cache_entry("local-b", "victim", "secrets-list-v5.json");
    let b_files = env.cache_entry("local-b", "victim", "files-list-v5.json");
    let vault_list = a_secrets
        .parent().unwrap().parent().unwrap().parent().unwrap()
        .join("vaults-list.json");
    for p in [&a_secrets, &a_files, &a_files_rec, &a_default, &b_secrets, &b_files, &vault_list] {
        assert!(p.exists(), "precondition: {} must be cached", p.display());
    }

    env.ok_with_backend("local-a", &["vault", "delete", "victim", "--force"]);

    assert!(!a_secrets.exists(), "deleted vault's secret listing must be dropped");
    assert!(!a_files.exists(), "deleted vault's file listing must be dropped");
    assert!(!a_files_rec.exists(), "deleted vault's recursive file listing must be dropped");
    assert!(!vault_list.exists(), "vault list must be dropped");
    assert!(a_default.exists(), "another vault on the same backend must keep its cache");
    assert!(b_secrets.exists(), "same-named vault on local-b must keep its cache");
    assert!(b_files.exists(), "same-named vault on local-b must keep its file cache");

    // The listing no longer serves the deleted vault from cache: without the
    // entry, `ls` asks the backend, which reports the vault as missing.
    let out = env.run_with_backend("local-a", &["ls", "--vault", "victim"]);
    assert!(!out.status.success(), "ls must not serve a deleted vault from cache");
    let b = env.ok_with_backend("local-b", &["ls", "--vault", "victim"]);
    let _ = b; // local-b's victim still lists (empty) without error
}

/// A failed removal (unknown vault) must leave every cache entry alone.
#[test]
fn vault_delete_failure_leaves_cache_intact() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok_with_backend("local-a", &["vault", "create", "victim"]);
    env.ok_with_backend("local-a", &["ls", "--vault", "victim"]);
    let a_secrets = env.cache_entry("local-a", "victim", "secrets-list-v5.json");
    assert!(a_secrets.exists());

    let out = env.run_with_backend("local-a", &["vault", "delete", "no-such-vault", "--force"]);
    assert!(!out.status.success());
    assert!(a_secrets.exists(), "failed delete must not invalidate");

    // A refused delete (vault still holds a secret) must not invalidate either.
    env.ok_with_backend("local-a", &["set", "IN_VICTIM", "--value", "v", "--vault", "victim"]);
    env.ok_with_backend("local-a", &["ls", "--vault", "victim"]);
    assert!(a_secrets.exists());
    let out = env.run_with_backend("local-a", &["vault", "delete", "victim", "--force"]);
    assert!(!out.status.success(), "local refuses to delete a non-empty vault");
    assert!(a_secrets.exists(), "refused delete must not invalidate");
}

/// Non-TTY delete without --force refuses before touching the backend and
/// must not invalidate either.
#[test]
fn vault_delete_unconfirmed_leaves_cache_intact() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok_with_backend("local-a", &["vault", "create", "victim"]);
    env.ok_with_backend("local-a", &["ls", "--vault", "victim"]);
    let a_secrets = env.cache_entry("local-a", "victim", "secrets-list-v5.json");

    let out = env.run_with_backend("local-a", &["vault", "delete", "victim"]);
    assert!(!out.status.success(), "non-interactive delete must refuse without --force");
    assert!(a_secrets.exists(), "refused delete must not invalidate");
    env.ok_with_backend("local-a", &["ls", "--vault", "victim"]);
}

/// Repeated cleanup: a second delete of the same vault is a clean error and
/// the cache stays empty for it.
#[test]
fn vault_delete_twice_is_clean() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok_with_backend("local-a", &["vault", "create", "victim"]);
    env.ok_with_backend("local-a", &["ls", "--vault", "victim"]);
    let a_secrets = env.cache_entry("local-a", "victim", "secrets-list-v5.json");

    env.ok_with_backend("local-a", &["vault", "delete", "victim", "--force"]);
    assert!(!a_secrets.exists());
    let out = env.run_with_backend("local-a", &["vault", "delete", "victim", "--force"]);
    assert!(!out.status.success(), "second delete of a removed vault must fail");
    assert!(!a_secrets.exists());
}
```

If `set`/`ls`/`file list` do not accept `--vault` in that position, move the flag before the subcommand (`xv --vault victim ls`); check `xv ls --help`.

If `PathBuf` is not already imported at the top of the file, add `use std::path::PathBuf;` (it is used by `WorkspaceEnv` fields, so it should be).

- [ ] **Step 3: Run to verify the first test fails**

Run: `cargo test --test e2e_workspaces vault_delete_ -- --nocapture 2>&1 | tail -30`
Expected: `vault_delete_drops_only_that_backends_vault_cache` FAILS on "deleted vault's secret listing must be dropped". The other three may pass already; that is fine.

If `file list` on a local backend fails in the precondition, check `tests/e2e_local_file_ops.rs` for the exact `file list` invocation and adjust the args (the local backend supports file ops under the default `file-ops` feature).

- [ ] **Step 4: Wire the three call sites**

`src/cli/vault_ops.rs`, trait-path Delete arm (currently):

```rust
                vaults_backend.delete_vault(&name, None).await?;
                output::success(&format!("Successfully deleted vault '{name}'"));
```

becomes:

```rust
                vaults_backend.delete_vault(&name, None).await?;
                crate::cache::invalidation::on_vault_removed(
                    &config,
                    config.effective_backend_name(),
                    &name,
                );
                output::success(&format!("Successfully deleted vault '{name}'"));
```

Second-dispatch Delete arm: replace `crate::cache::invalidation::on_vault_mutation(&config);` after `execute_vault_delete(...)` with:

```rust
            crate::cache::invalidation::on_vault_removed(
                &config,
                config.effective_backend_name(),
                &name,
            );
```

Purge arm: replace `crate::cache::invalidation::on_vault_mutation(&config);` after `execute_vault_purge(...)` with the same three-argument call using `&name`.

Leave `Create`, `Restore`, and `Update` on `on_vault_mutation` (they add or change a vault; nothing per-vault is stale).

Remove the temporary `#[allow(dead_code)]` from `on_vault_removed` in `src/cache/invalidation.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --test e2e_workspaces vault_delete_ 2>&1 | tail -20`
Expected: 4 passed. Then `cargo test --lib cache:: 2>&1 | tail -5` still green.

- [ ] **Step 6: Commit**

```bash
git add src/cli/vault_ops.rs src/cache/invalidation.rs tests/e2e_workspaces.rs
git commit -m "vault: drop a removed vault's listing cache on delete and purge"
```

---

### Task 3: Wire `xv backend rm`

**Files:**
- Modify: `src/cli/backend_ops.rs:226-415` (`execute_backend_rm`)
- Modify: `src/cache/invalidation.rs` (remove the temporary `#[allow(dead_code)]` on `on_backend_removed`)
- Test: `tests/e2e_workspaces.rs`

**Interfaces:**
- Consumes: `crate::cache::invalidation::on_backend_removed(config, backend)`; `backend.as_str()` on `BackendType`.

- [ ] **Step 1: Write the failing test**

`xv backend rm` acts on the canonical `[local]`/`[azure]`/`[aws]` blocks, not `named_backends`, so this test uses the top-level `local` backend (`WorkspaceEnv`'s default active backend, registry name `"local"`). Removing the active backend while others remain is refused, and `WorkspaceEnv` configures only `[local]` as a canonical block, so `rm local` is allowed there. Append to `tests/e2e_workspaces.rs`:

```rust
/// A04-01: removing a backend from the config drops every listing cached
/// under its registry name and the vault list, leaving named backends alone.
#[test]
fn backend_rm_drops_that_backends_listing_cache() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok(&["set", "DEFAULT_SECRET", "--value", "v"]);
    env.ok(&["ls"]);
    env.ok(&["vault", "list"]);
    env.ok_with_backend("local-a", &["set", "A_SECRET", "--value", "va"]);
    env.ok_with_backend("local-a", &["ls"]);

    let local_secrets = env.cache_entry("local", "default", "secrets-list-v5.json");
    let a_secrets = env.cache_entry("local-a", "default", "secrets-list-v5.json");
    let vault_list = local_secrets
        .parent().unwrap() // <fp>/local/default
        .parent().unwrap() // <fp>/local
        .parent().unwrap() // <fp>
        .join("vaults-list.json");
    assert!(local_secrets.exists());
    assert!(a_secrets.exists());
    assert!(vault_list.exists());

    let out = env.run(&["backend", "rm", "local", "--yes"]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!local_secrets.exists(), "removed backend's listings must be dropped");
    assert!(!local_secrets.parent().unwrap().parent().unwrap().exists(), "backend dir must be gone");
    assert!(!vault_list.exists(), "vault list must be dropped");
    assert!(a_secrets.exists(), "named backend local-a must keep its cache");
}
```

If `backend rm local --yes` is refused because `XV_BACKEND=local` is set in the harness env (the command reads `config.effective_backend_name()` to decide whether the active backend is being removed, and refuses only when *other* canonical backends remain), read the refusal text and, if needed, run the removal via `env.xv().env_remove("XV_BACKEND")`. Record what was needed in the commit message.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test e2e_workspaces backend_rm_drops -- --nocapture 2>&1 | tail -30`
Expected: FAIL on "removed backend's listings must be dropped".

- [ ] **Step 3: Wire the call**

In `src/cli/backend_ops.rs::execute_backend_rm`, after the `purge_local_store` block and before the success messages:

```rust
    // The config on disk no longer claims this backend; every listing cached
    // under its registry name is unreachable now. Use the PRE-removal
    // `config` so the identity fingerprint still resolves to the directory
    // those entries were written under.
    crate::cache::invalidation::on_backend_removed(&config, backend.as_str());
```

`config` is the function's `config: Config` parameter (the loaded, pre-removal config). Remove the temporary `#[allow(dead_code)]` from `on_backend_removed`.

- [ ] **Step 4: Run the test and the existing backend CLI tests**

Run: `cargo test --test e2e_workspaces backend_rm_drops 2>&1 | tail -10 && cargo test --test backend_cli_tests 2>&1 | tail -10`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/cli/backend_ops.rs src/cache/invalidation.rs tests/e2e_workspaces.rs
git commit -m "backend: drop a removed backend's listing cache on rm"
```

---

### Task 4: Wire `xv migrate`

**Files:**
- Modify: `src/cli/migrate_ops.rs:338-736` (`execute_migrate`, success branch at the end)
- Test: `tests/e2e_workspaces.rs`

**Interfaces:**
- Consumes: `crate::cache::invalidation::{on_secret_mutation, on_file_mutation}`; `to_kind: BackendKind` (has `Display`, prints `local`/`azure`/`aws`); `target_vault: String`.

- [ ] **Step 1: Write the failing test**

Append to `tests/e2e_workspaces.rs`:

```rust
/// A04-03: migrate writes into the destination vault under the backend
/// KIND name (it builds backends by kind), so that is the cache identity it
/// must invalidate. A pre-populated destination listing must not survive.
#[test]
fn migrate_drops_destination_listing_cache() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok(&["set", "MIGRATE_ME", "--value", "v"]);
    env.ok(&["vault", "create", "other"]);
    env.ok(&["ls", "--vault", "other"]); // populate (empty) destination listing
    env.ok(&["ls"]);                      // populate source listing
    let dest = env.cache_entry("local", "other", "secrets-list-v5.json");
    let src = env.cache_entry("local", "default", "secrets-list-v5.json");
    assert!(dest.exists(), "precondition: destination listing cached");
    assert!(src.exists());

    let out = env.run(&["migrate", "--from", "local:default", "--to", "local:other"]);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!dest.exists(), "destination listing must be dropped after migrate");
    let after = env.ok(&["ls", "--vault", "other"]);
    assert!(after.contains("MIGRATE_ME"), "{after}");
}

#[test]
fn migrate_dry_run_leaves_cache_intact() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok(&["set", "MIGRATE_ME", "--value", "v"]);
    env.ok(&["vault", "create", "other"]);
    env.ok(&["ls", "--vault", "other"]);
    let dest = env.cache_entry("local", "other", "secrets-list-v5.json");
    assert!(dest.exists());
    env.ok(&["migrate", "--from", "local:default", "--to", "local:other", "--dry-run"]);
    assert!(dest.exists(), "dry run must not invalidate");
}
```

If `ls --vault other` on an empty vault does not write a cache entry, replace the precondition with `env.ok(&["set", "PRE", "--value", "p", "--vault", "other"]); env.ok(&["ls", "--vault", "other"]);` — `--vault` is accepted by `set` and `ls`; check `xv set --help` output if unsure.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test e2e_workspaces migrate_ -- --nocapture 2>&1 | tail -30`
Expected: `migrate_drops_destination_listing_cache` FAILS at "destination listing must be dropped".

- [ ] **Step 3: Wire the call**

In `execute_migrate`, in the success branch (the `else` that prints `Migrated {} secret(s) ({} skipped)`), before `Ok(())`, and only when `!dry_run` (verify `dry_run` is still in scope; it is a parameter). Insert directly after the `output::success(...)` call:

```rust
        if !dry_run && migrated > 0 {
            let target_backend = to_kind.to_string();
            crate::cache::invalidation::on_secret_mutation(&config, &target_backend, &target_vault);
            #[cfg(feature = "file-ops")]
            crate::cache::invalidation::on_file_mutation(&config, &target_backend, &target_vault);
        }
```

Check that `to_kind` and `target_vault` are still live at that point (they are declared near the top of the function and not moved; if `target_vault` was moved into a struct, clone it where it is declared). `on_file_mutation` covers attachments copied by `--with-attachments`; calling it when no attachments moved is a harmless miss.

- [ ] **Step 4: Run the tests**

Run: `cargo test --test e2e_workspaces migrate_ 2>&1 | tail -10`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add src/cli/migrate_ops.rs tests/e2e_workspaces.rs
git commit -m "migrate: drop the destination listing cache after a successful migration"
```

---

### Task 5: Wire `xv transfer` (and the generic `copy`/`move --with-attachments` path)

**Files:**
- Modify: `src/cli/transfer_ops.rs:44-134` (`execute`)
- Test: `tests/e2e_workspaces.rs`

**Interfaces:**
- Consumes: `on_secret_mutation`, `on_file_mutation`; `source_backend`, `source_vault`, `destination_backend`, `destination_vault` (all `String`, already resolved in `execute`); `options.move_source: bool`.

- [ ] **Step 1: Write the failing test**

Same-vault rename with `--move` is the offline-safe transfer route (`tests/e2e_transfer.rs` uses it). Append to `tests/e2e_workspaces.rs`:

```rust
/// A04-03: an applied transfer rewrites the destination (and, for a move,
/// the source) so both listing caches must be dropped; a preview must not.
#[test]
fn transfer_apply_drops_endpoint_listing_caches_but_preview_does_not() {
    let env = WorkspaceEnv::with_cache_enabled(300);
    env.ok(&["set", "cert", "--value", "private-secret-value"]);
    let proof = env.home.join("proof.txt");
    std::fs::write(&proof, b"private-attachment-content").unwrap();
    env.ok(&["attach", "cert", proof.to_str().unwrap()]);
    env.ok(&["ls"]);
    env.ok(&["file", "list"]);
    let secrets = env.cache_entry("local", "default", "secrets-list-v5.json");
    let files = env.cache_entry("local", "default", "files-list-v5.json");
    assert!(secrets.exists() && files.exists());

    let recovery = env.home.join("recovery");
    let base = [
        "transfer", "cert", "--from", "default", "--to", "default",
        "--new-name", "renamed", "--move",
    ];
    // Preview: no invalidation.
    env.ok(&base);
    assert!(secrets.exists() && files.exists(), "preview must not invalidate");

    let out = env
        .xv()
        .args(base)
        .args(["--apply", "--offline", "--recovery-dir"])
        .arg(&recovery)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!secrets.exists(), "applied transfer must drop the secret listing");
    assert!(!files.exists(), "applied transfer must drop the file listing");

    let after = env.ok(&["ls"]);
    assert!(after.contains("renamed") && !after.contains("cert"), "{after}");
}
```

`env.home` is a private field of `WorkspaceEnv` in the same file, so it is accessible. If `attach` needs the attachment key initialized first on a fresh local vault, look at how `tests/e2e_transfer.rs` prepares it (it does not; `attach` initializes the key on first use) and mirror that.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test e2e_workspaces transfer_apply_drops -- --nocapture 2>&1 | tail -30`
Expected: FAIL at "applied transfer must drop the secret listing".

- [ ] **Step 3: Wire the call**

In `src/cli/transfer_ops.rs::execute`, the `intent` struct moves `source_backend`/`source_vault`/`destination_backend`/`destination_vault` into it. Before building `intent`, clone the four strings and the move flag:

```rust
    let cache_source = (source_backend.clone(), source_vault.clone());
    let cache_destination = (destination_backend.clone(), destination_vault.clone());
    let is_move = options.move_source;
```

Then, inside `if options.apply || options.resume.is_some() { ... }`, after the `println!("{}", serde_json::to_string_pretty(&report)?);` line (i.e. only once `apply`/`resume` returned `Ok`):

```rust
        let (dest_backend, dest_vault) = &cache_destination;
        crate::cache::invalidation::on_secret_mutation(&config, dest_backend, dest_vault);
        crate::cache::invalidation::on_file_mutation(&config, dest_backend, dest_vault);
        if is_move {
            let (src_backend, src_vault) = &cache_source;
            crate::cache::invalidation::on_secret_mutation(&config, src_backend, src_vault);
            crate::cache::invalidation::on_file_mutation(&config, src_backend, src_vault);
        }
```

Do not add anything to the preview branch.

- [ ] **Step 4: Run the tests**

Run: `cargo test --test e2e_workspaces transfer_apply_drops 2>&1 | tail -10 && cargo test --test e2e_transfer 2>&1 | tail -5 && cargo test --test e2e_transfer_self_target 2>&1 | tail -5`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/cli/transfer_ops.rs tests/e2e_workspaces.rs
git commit -m "transfer: drop endpoint listing caches after an applied transfer"
```

---

### Task 6: Documentation, roadmap, changelog

**Files:**
- Modify: `docs/cache.md` (fingerprint bullets ~line 58-66, strict section ~84-91, pitfalls row ~105)
- Modify: `CLAUDE.md` ("Current known limitations" list; the `cache invalidation on vault removal` bullet)
- Modify: `ROADMAP.md` (remove `### P1 — Finish cache invalidation on vault removal`, lines 83-88)
- Modify: `CHANGELOG.md` (Unreleased → Fixed)

- [ ] **Step 1: `docs/cache.md`**

Fingerprint list: delete the bullet `- the effective backend name, and` and join the remaining two bullets so they read:

```markdown
- the resolved global config path, and
- the active backend's identity fields — Azure: tenant + subscription; AWS:
  region + profile + endpoint URL; Local: resolved store path.

The backend *name* is deliberately not part of the fingerprint: it is already
the next path component, and two configs that differ only in a named backend
already differ by config path.
```

Strict section: append after "Loud, never fatal.":

```markdown
This is a deliberate contract, not a gap: the cache is a read accelerator, and
a mode that failed `xv ls` because a cache file was unwritable would turn a
cache problem into an outage for exactly the commands scripts depend on. There
is no fail-nonzero variant and none is planned; use `xv doctor` or
`xv cache status` to surface cache problems in CI.
```

Pitfalls row: replace the "Vault list is gone after delete..." row with:

```markdown
| A listing looks stale after a vault, backend, migrate, or transfer removed or rewrote it | Vault delete/purge, `backend rm`, `migrate`, and an applied `transfer` drop the affected `(backend, vault)` listings eagerly. If you still see stale data, the write came from outside `xv` (portal, another machine): `xv ls --no-cache` or `xv cache clear --vault NAME`. |
```

Add a short subsection before "Common pitfalls":

```markdown
### What invalidates what

| Mutation | Dropped |
| --- | --- |
| secret set/update/delete/restore/rename/rotate/import/copy/move | `secrets:<backend>:<vault>` on every vault written |
| file upload/delete/sync | both `files:*` variants for that vault |
| vault create/restore/update | `vaults` |
| vault delete/purge | `vaults` plus that `(backend, vault)`'s secret and file listings |
| `backend rm` | `vaults` plus every listing under that backend name |
| `migrate` (not dry-run) | destination `(kind, vault)` secret and file listings |
| `transfer`/`copy`/`move` with `--apply`/`--resume` | destination listings; source listings too for a move |
| `cx rm`, `init` | nothing — no vault data changes |

`<backend>` is always the registry name (`local`, `azure`, `aws`, or a
`named_backends` key); `migrate` addresses backends by kind, so its entries
live under the kind name.
```

- [ ] **Step 2: `CLAUDE.md`**

In "Current known limitations", delete the line `- cache invalidation on vault removal (v5 filesystem hardening shipped)`.

- [ ] **Step 3: `ROADMAP.md`**

Delete the whole `### P1 — Finish cache invalidation on vault removal` section (heading plus its paragraph). Leave the surrounding sections untouched.

- [ ] **Step 4: `CHANGELOG.md`**

Under `## Unreleased`, add (create the `### Fixed` heading after `### Added` if it does not exist):

```markdown
### Fixed

- **Listing caches no longer outlive the vault, backend, or transfer that
  made them stale.** `xv vault delete|purge` now drops the removed vault's
  cached secret and file listings for exactly that backend (a same-named
  vault on another backend keeps its cache), `xv backend rm` drops every
  listing under the removed backend, and `xv migrate` and an applied
  `xv transfer`/`copy`/`move` drop the listings for the vaults they wrote.
  Previously only the vault list was invalidated on the Azure path and
  nothing at all on local/AWS/named-backend vault deletes, so `xv ls
  --vault OLD` kept serving a deleted vault's listing until the TTL
  expired. Dry runs, previews, refused confirmations, and failed removals
  leave the cache untouched. `docs/cache.md` now lists what each mutation
  invalidates, states that `XV_CACHE_STRICT` is warning-only by design,
  and no longer claims the identity fingerprint includes the backend name.
```

- [ ] **Step 5: Commit**

```bash
git add docs/cache.md CLAUDE.md ROADMAP.md CHANGELOG.md
git commit -m "docs: record cache invalidation coverage and the strict-mode contract"
```

---

### Task 7: Full gates

- [ ] **Step 1: Run the gates**

```bash
cargo fmt --check 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -20
cargo test --all-features --workspace 2>&1 | tail -30
```

Expected: no formatting diffs, no clippy warnings, all tests pass. If disk space drops below 4 GB during the run, prune with the command in the cargo-target memory (never `cargo clean`).

- [ ] **Step 2: Fix anything the gates surface, amend into the responsible commit, and re-run.**
