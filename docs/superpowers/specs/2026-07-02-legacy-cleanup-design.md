# Legacy Code Cleanup Design

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-02 | **Author:** Claude + Scott
> Executed under Scott's standing autonomous instruction; recommended scope adopted at the option prompt. Scott is the sole user; backwards compatibility is a non-feature.

---

## Problem

The codebase carries pre-backend-trait "legacy" code and deprecation shims. A survey categorized them; two categories are safe to delete now, two are deliberately deferred.

## In scope

### 1. Dead legacy CLI functions (zero callers, `#[allow(dead_code)]`-marked)

`src/cli/secret_ops.rs`: `execute_secret_set` (~1996), `execute_secret_get` (~2081), `execute_secret_delete` (~3608), `execute_secret_set_bulk` (~4285), `execute_secret_delete_group` (~4451) — plus any helpers/imports/tests orphaned by their removal. Also `src/config/context.rs` `migrate_from_config` (~363, dead) and `src/config/settings.rs` `init_default_config` (~726, verify zero callers first).

### 2. The near-dead legacy update path (closes the recorded tag-drop bug)

- `execute_secret_update` (`src/cli/secret_ops.rs` ~3649, ~210 LOC) is reachable only via the registry-None fallback at ~1573. Delete it; make `execute_secret_update_direct` error like set/get do ("No backend registry available…").
- Its manager-side pipeline `SecretManager::update_secret_enhanced` (`src/secret/manager.rs` ~2238-2416) — the root of the tag-drop bug — is deleted IF the legacy fn was its only caller (verify; if other callers exist, stop and report instead).

### 3. Deprecated aliases (remove outright)

- `vault share list --fmt` (commands.rs ~1155 + warn at vault_ops.rs ~1243)
- `audit --raw` (commands.rs ~833 + warn/handling at system_ops.rs ~278)
- `context envs` subcommand (commands.rs ~1316 + delegate/warn at config_ops.rs ~1230) — its e2e deprecation test is deleted/replaced with an unknown-subcommand assertion
- `migrate --overwrite` (commands.rs ~957 + `legacy_overwrite` shim at migrate_ops.rs ~247-258)

### 4. Comment/doc cleanup

Where the above deletions make wording stale: section banners in secret_ops.rs ("Azure legacy path", "shared by the trait and legacy paths" — only where the legacy half is gone), helpers.rs doc comments, `build_patched_tags`' "mirroring the legacy full-write pipeline" phrase, CHANGELOG entry describing the removals.

## Explicitly deferred (recorded, do not touch)

- **Live Azure "legacy" paths**: `execute_secret_rollback_legacy`, `execute_secret_purge_legacy`, `execute_secret_find` (Azure branch), `execute_secret_share`, and the audit Activity-Log fallback — these RUN on Azure today; removing them requires porting rollback/purge/find/share to the backend trait first (its own project).
- **On-disk read-compat shims**: old name-tag fallback (`get_original_name`), AWS legacy `,` group separator, missing-`backend` config default, legacy-context warning, mixed-encryption tolerance, `[local].opaque_filenames` default — Scott's existing vault data may rely on these.
- **"Phase 2 pluggability" dead scaffolding** — intentional future surface, separate decision.

## Testing

`cargo test --lib` + `cargo test --test e2e_local_backend` + full `cargo test` green; clippy 0 warnings (deletions often orphan imports); behavioral: `xv vault share list <v> --fmt json` now errors with clap unexpected-argument; `xv audit --raw` errors; `xv context envs` errors; `xv migrate --overwrite` errors; `xv update` still works via the trait path (hermetic e2e).

## Out of scope

Everything in "Explicitly deferred". No behavior changes to live paths beyond the alias removals.
