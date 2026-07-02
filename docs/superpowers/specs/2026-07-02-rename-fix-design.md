# Rename Fix Design (`xv update --rename`, issue #295)

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-02 | **Author:** Claude + Scott
> Executed under Scott's standing autonomous instruction; adopted decisions recorded verbatim below. Scott is the sole user; backwards compatibility is a non-feature.
> Fixes [GitHub issue #295](https://github.com/bziobnic/crosstache/issues/295).

---

## Problem

`xv update --rename <new>` is broken on all three live backends (found during the legacy-cleanup review, pre-existing on `main`):

- **Azure**: `AzureSecretBackend::update_secret` (`src/backend/azure/secrets.rs:208-355`) builds the full-write compat request under `request.new_name` (`name: request.new_name.unwrap_or_else(...)`, line 332) and calls `inner.update_secret` — the secret is **created under the new name and the old one is never deleted**. Every rename leaves both copies and reports "Successfully updated secret".
- **Local** (`src/backend/local/secrets.rs:1451-1560`) and **AWS** (`src/backend/aws/secrets.rs:656-763`): `update_secret` never reads `request.new_name` — rename is a **silent no-op** that reports success.
- The only correct implementation (create-new → delete-old → `RenameIncomplete`/exit 43 on partial failure, shipped in #242) lived in the legacy `SecretManager::update_secret_enhanced` pipeline, which was dead on live paths and was deleted in the v0.17.0 legacy cleanup (#296, commit `1a05ff4`) together with the exit-43 row in `docs/exit-codes.md`. The `--rename` flag remains advertised (`src/cli/commands.rs:671-673`).

## Decisions

Adopted decisions (recorded verbatim):

1. Implement rename properly on ALL THREE backends at the trait level: read value + metadata, create under the new name (preserving tags/groups/note/folder/content-type/expiry where representable), delete the old name. Version history does not carry over — documented.
2. Partial failure (new created, old delete fails) surfaces a DEDICATED error variant + documented exit code 43 restored to docs/exit-codes.md (removed in v0.17.0 while unreachable). Error text names both copies and the recovery step.
3. Azure: the old name is left SOFT-DELETED (consistent with xv delete; visible in `ls --deleted`; renaming back within retention hits Azure's conflict — documented). No purge attempt.

Decisions made during design:

4. **Rename is a new provided trait method, not a per-backend reimplementation.** `SecretBackend::rename_secret(vault, name, new_name)` gets a **default implementation** in `src/backend/secret.rs` composed entirely from the trait's own required primitives: `get_secret(include_value=true)` → build a `SecretRequest` → `set_secret` under the new name → `delete_secret` of the old name. All three backends' `get_secret` expose groups/note/folder under the canonical tag keys `groups`/`note`/`folder` (Azure via the manager's tag parsing, local via `meta_to_properties` at `src/backend/local/secrets.rs:294-326`, AWS via `props_from_describe` at `src/backend/aws/secrets.rs:240-303`), so one implementation covers all three with zero backend-specific code. No overrides are needed.
5. **`SecretUpdateRequest.new_name` is deleted** (`src/secret/manager.rs:134`). With the field gone, a backend can never again silently ignore a rename — the compiler enforces it. The Azure `update_secret` rename branch (lines 224, 332) is removed with it.
6. **Combination semantics: `--rename` applies AFTER the other updates.** When `--rename` is combined with other update flags, `execute_secret_update_direct` (`src/cli/secret_ops.rs:1456`) first applies the in-place update under the old name via the existing (tested) `update_secret` path, then calls `rename_secret`. Rationale: reuses the update machinery unchanged, keeps error attribution clean (an update failure aborts before anything is copied), and preserves the advertised flag combination instead of rejecting it. `--rename` alone skips the no-op update round-trip. If the update succeeds and the rename then partially fails, the metadata changes are persisted on both copies (they were applied before the copy) — acceptable, and the `RenameIncomplete` recovery steps still hold.
7. **Destination guard.** Rename refuses to overwrite: if the destination name exists (`secret_exists`), it fails with `Conflict` (exit 41), mirroring `xv copy`'s target-exists behavior. Renaming a secret to its current name fails with `InvalidArgument`. Both guards run before anything is created.
8. **Delete-old uses each backend's normal `delete_secret`** — the same call `xv delete` makes. Azure: soft delete (REST DELETE). AWS: scheduled deletion with the 30-day recovery window (`recovery_window_in_days(30)`, `src/backend/aws/secrets.rs:643-654`); renaming back to the old name within the window fails (AWS blocks creating over a scheduled-deletion name; the `secret_exists` guard also reports scheduled-deletion secrets as existing because `DescribeSecret` still returns them) — documented, same shape as the Azure retention conflict. Local: trash entry (`delete_secret_at`, timestamped `@millis` trash dirs). No purge attempt anywhere.
9. **Metadata carried over:** value, content-type, enabled state, expires/not-before, user tags, groups, note, folder. `original_name` and `created_by` tags are stripped from the copied tag map and regenerated by each backend's `set_secret` for the new name (Azure: `prepare_secret_request`, `src/secret/manager.rs:400-431`; AWS: `set_secret` tag build; local: fresh `SecretMeta`). Version history starts fresh at the new name; old versions remain only wherever the backend keeps the deleted old name (Azure soft-delete, local trash).
10. **Disabled secrets (Azure): rename fails cleanly before mutating.** The read step uses `get_secret(include_value=true)`, which Azure rejects with 403 `SecretDisabled` on a disabled secret. Accepted limitation (re-enable first, then rename), documented here; local renames disabled secrets fine, AWS has no disabled state.

## Design

### Error plumbing (mirrors the v0.16.0 shape)

- New `BackendError::RenameIncomplete { source, destination, vault, cause: Box<BackendError> }` in `src/backend/error.rs` (the enum at lines 15-68), returned by the trait default method when the delete-old step fails after the new secret was created.
- `CrosstacheError::RenameIncomplete { source, destination, vault, #[source] cause: Box<CrosstacheError> }` restored **verbatim from `v0.16.0:src/error.rs`** (variant at old line 103): same display text naming both copies, the vault, and the recovery steps (`xv get <destination>`, then `xv delete <source>` or retry); code `xv-rename-incomplete`; exit code 43 (40-49 backend/API family, after `RateLimited` = 42); security-surface test entry with fields `source`/`destination`/`vault`/`cause`.
- `From<BackendError> for CrosstacheError` (`src/backend/error.rs:70-98`) maps the new variant field-for-field, converting the boxed cause recursively.
- `docs/exit-codes.md` gets its 43 row back verbatim: `rename created the new secret but failed to delete the original; both copies still exist (xv-rename-incomplete)`. No other exit-code machinery changes (`src/main.rs` already renders `exit_code()` and the JSON envelope generically).

### Trait method

`SecretBackend::rename_secret` (provided method in `src/backend/secret.rs`, placed after `update_secret`), plus a `pub(crate) fn rename_request_from_properties(new_name, &SecretProperties) -> Result<SecretRequest, BackendError>` helper that:

- fails with `Internal` if the properties carry no value (nothing has been created at that point);
- lifts `groups` (comma-split), `note`, `folder` out of the tag map into the first-class `SecretRequest` fields, and strips `original_name`/`created_by`;
- carries content-type, enabled, expires_on, not_before, and the remaining user tags.

The method: same-name guard → `secret_exists(new_name)` guard (`Conflict`) → get → build request → `set_secret` → `delete_secret(old)`; a delete failure wraps into `BackendError::RenameIncomplete` (the new secret is deliberately never rolled back — no secret material may be lost, matching #242).

### CLI wiring

`execute_secret_update_direct` (`src/cli/secret_ops.rs:1456-1563`): compute `has_other_updates` from the non-rename inputs; run `update_secret` (without any rename notion) when there are other updates **or** no rename at all (preserving today's bare-`xv update` behavior byte-for-byte); then, when `--rename` was given, call `rename_secret` and print `Successfully renamed secret '<old>' to '<new>'`. Cache invalidation (`invalidate_trait_secret_cache`) is unchanged and covers both steps.

### What does NOT change

- `xv copy` / `xv move` (`execute_secret_copy*`, `src/cli/secret_ops.rs:3459-3613`) stay on the legacy Azure-only `SecretManager` path. Their read+create machinery is **not reusable** for the trait-level rename (it is built on `get_secret_safe`/`set_secret_safe` + `get_azure_auth_provider`), but its metadata-preservation approach (carry the tag map; let the write path regenerate `original_name`) informed the helper above.
- Local backend needs **no rename-specific file handling**: `set_secret` and `delete_secret` already resolve active stems, maintain the encrypted name index, and move active pair + versions dir to trash in **both** plaintext and `[local].opaque_filenames` stores (`resolve_active_stem`, `ensure_opaque_layout`, `delete_secret_at`). Tests prove both modes.
- No machine-shape changes to shipped commands. User-visible changes: rename works, the restored error/exit code 43, the extra success line, docs.

## Testing

- **Unit (trait):** an in-memory stub `SecretBackend` in `src/backend/secret.rs` tests exercises the default method — metadata-preserving move, same-name/destination-exists guards, missing-value abort, and a `fail_delete` stub proving `RenameIncomplete` carries both names + vault + cause while both copies survive.
- **Unit (errors):** code/exit-code/display/security-surface tests in `src/error.rs`; mapping test in `src/backend/error.rs`.
- **Local backend:** rename roundtrip in the plaintext store and the opaque store (`test_backend()` / `test_backend_opaque()`, `src/backend/local/secrets.rs:1858/1892`): value + note/groups/folder preserved, old name `NotFound` and present in `list_deleted_secrets`, fresh version history, and no on-disk filename leaking either name in opaque mode.
- **e2e (hermetic local, `tests/e2e_local_backend.rs` `TestEnv`):** CLI rename roundtrip with metadata, rename combined with `--note` (update-then-rename semantics), destination-exists conflict, same-name error, missing-secret error.
- **AWS:** LocalStack-gated rename roundtrip in `tests/aws_localstack_tests.rs` (skips without `AWS_INTEGRATION_TESTS`); `cargo clippy --all-targets --features aws` gate.
- **Azure (live, `#[ignore]`):** rename roundtrip in `tests/e2e_azure_backend.rs` against the `heythere` vault with uniquely timestamped names; cleanup soft-deletes the fixture (the vault has purge protection, so purge is blocked by vault policy — acceptable per the existing harness contract).
- **Gates:** `cargo fmt`, `cargo clippy --all-targets` (+ `--features aws`) with 0 warnings, `cargo test --lib`, `cargo test --test e2e_local_backend`, full `cargo test`.

## Out of scope

- Porting `xv copy`/`xv move` to the backend trait (separate project; they still run the legacy Azure path).
- Rollback of the new secret on partial failure (deliberately never — matches #242), and any purge of the old name.
- Version-history carry-over to the new name.
- Cross-vault rename (that is `xv move`).
- A rename capability flag — all three backends support rename via the default method, so there is nothing to gate.
