# Cache invalidation on vault, backend, and cross-vault removal — design

**Date:** 2026-09-11
**Baseline:** v0.39.0, `main` at `ea6bee7`
**Checklist items:** A04-01, A04-02, A04-03, A04-04 (xv Feature Priorities
checklist, 2026-09-08). A04-05/06/07 (encrypted listing metadata) are
deliberately deferred until the secret-domain model split lands, so the
cache has a stable, value-free payload type to wrap.
**Compatibility:** none required. Cache files are disposable; no on-disk
secret format changes.

## Problem

`cache::invalidation::on_vault_removed` exists but has no callers. Vault
deletion invalidates only the vault list (Azure path) or nothing at all
(local/AWS/named-backend trait path), so `xv ls --vault OLD` keeps serving
the deleted vault's cached listing until TTL. `docs/cache.md` documents this
as a known pitfall. The audit of other mutation paths found three more
removal-shaped paths with no invalidation: `xv backend rm`, `xv migrate`,
and `xv transfer --apply/--resume`.

## Decisions

1. **`on_vault_removed` takes the backend registry name.** Signature becomes
   `on_vault_removed(config, backend, vault)`. It drops exactly
   `SecretsList{backend,vault}`, both `FileList{backend,vault,recursive}`
   variants, and `VaultList`. It does not use `CacheManager::invalidate_vault`
   (which walks every backend directory for the vault name) because a
   same-named vault on another backend must keep its cache. The registry
   name is `config.effective_backend_name()` for every `vault delete|purge`
   path: vault verbs never resolve through a workspace, and `--backend` is
   folded into `config.backend` before dispatch.
2. **`on_backend_removed(config, backend)` is new.** It removes the whole
   `<entry_root>/<backend>/` directory via a new
   `CacheManager::invalidate_backend`, plus `VaultList`. Called from
   `xv backend rm` after the config save commits, using the pre-removal
   `Config` (the fingerprint includes the local store path, so the identity
   must be computed before the block disappears). Both `SecretsList` and
   `FileList` are backend-nested in v5, so removing the backend directory
   cannot touch another backend's entries.
3. **`migrate` and `transfer` invalidate on success only.** Migrate keys by
   `BackendKind` display name (it builds backends by kind, never by registry
   name, so that is the identity its writes land under). Transfer keys by the
   `(backend, vault)` identities `resolve_vault_ref_with_workspace` already
   returns. Both drop secret and file listings on every endpoint written
   (destination always; source when the operation is a move). Dry runs and
   previews invalidate nothing.
4. **Confirmation abort and backend failure invalidate nothing.** The seam
   is called only after the backend call returns `Ok`.
5. **`xv cx rm` and `xv init` need no invalidation.** `cx rm` detaches an
   alias without touching vault data, so the cached listing is still true.
   `init` refuses when a config exists, and a new config has a new
   fingerprint. Recorded in the invalidation module docs so the audit is not
   repeated.
6. **`XV_CACHE_STRICT` stays warning-only, now as a decided contract.** The
   cache is a read accelerator; a mode where a broken cache fails `xv ls`
   would turn a cache problem into an outage for the exact commands a
   scripted caller depends on. `docs/cache.md` already says "loud, never
   fatal"; the doc gains a sentence saying this is deliberate, and the
   roadmap does not carry a fail-nonzero option.
7. **Fix the fingerprint doc.** `docs/cache.md` claims the fingerprint
   includes the effective backend name; the code deliberately excludes it.
   The doc follows the code.

## Verification

- Unit tests on the seam and on `invalidate_backend` using
  `CacheManager::new(tempdir)` (no env mutation).
- CLI tests in `tests/e2e_workspaces.rs` using `WorkspaceEnv::with_cache_enabled`
  (hermetic `XV_CACHE_DIR`, two named local backends), asserting on-disk
  cache paths: removed vault's entries gone, same-named vault on the other
  backend intact, failed removal leaves entries, non-TTY unconfirmed delete
  leaves entries, repeated delete is a clean error.
- CLI tests for `backend rm`, `migrate local:a -> local:b`, and `transfer`.
- Docs: `docs/cache.md`, `CLAUDE.md` limitation list, `ROADMAP.md` P1 entry
  removed, `CHANGELOG.md` Unreleased entry.
