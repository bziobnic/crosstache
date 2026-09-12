# Secret Domain Model PR 2: Trait Split — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make "does this call return the value?" a property of the method signature: remove every `include_value: bool` from `SecretBackend` and the legacy `SecretOperations`, add metadata-only getters, make writes return `SecretMetadata`, and make `Secret.value` non-optional.

**Architecture:** `SecretProperties` is renamed `Secret` with `value: SecretValue` (always present). `get_secret`/`get_secret_version` return `Secret`; new `get_secret_metadata`/`get_secret_version_metadata` return `SecretMetadata`. Every write (`set_secret`, `update_secret`, `update_secret_if_revision`, `create_secret_if_absent`, `rename_secret`, `rename_secret_if_revision`, `rollback`, `restore_secret`, `restore_from_backup`, `validate_secret_revision`) returns `SecretMetadata`; `list_versions` returns `Vec<SecretMetadata>`. `SecretSnapshot` keeps `value: Option<SecretValue>` but its getter takes `SnapshotValue::{Omit, Include}` instead of a bool. Adapters build `SecretMetadata` at one place each and wrap it into `Secret` only where a value was fetched.

**Tech Stack:** Rust 2021; PR 1's `src/secret/domain/`.

**Spec:** `docs/superpowers/specs/2026-09-11-secret-domain-model-design.md` ("Trait split" section)

## Global Constraints

- Never run `cargo` with `run_in_background`; foreground only, timeout up to 600000 ms, `| tail -40`. `CARGO_TARGET_DIR=/Users/scottzionic/crosstache/target`.
- Never `git stash`. Never push.
- No `bool` parameter anywhere in `SecretBackend`/`SecretOperations` selects whether a value is returned. `grep -rn "include_value" src tests` is empty at the end.
- `expose_secret()` call sites may only shrink or move; no new disclosure boundary.
- Behavior unchanged for the CLI (output, exit codes), web (responses already `SecretMetadata`), TUI, cache, and on-disk formats. Backends must not fetch a value where they previously did not (no extra Key Vault/Secrets Manager `GET` of a value on a metadata path) and must not skip fetching one where they did.
- `SecretSummary`/`DeletedSecretSummary`/`SecretMetadata` unchanged.
- Gates before finishing any task: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo check`, `cargo check --no-default-features`, and the named tests. Task 5 runs the full workspace suite.

---

### Task 1: `Secret`, `SnapshotValue`, and the new trait surface (compiles with adapters stubbed)

**Files:**
- Modify: `src/secret/domain/secret.rs` (rename `SecretProperties` → `Secret`; `value: SecretValue`; add `SnapshotValue`; `Secret::into_metadata`/`metadata`; `Secret::into_parts(self) -> (SecretMetadata, SecretValue)`; `SecretSnapshot { metadata: SecretMetadata, value: Option<SecretValue>, revision }`)
- Modify: `src/secret/domain/mod.rs` (exports)
- Modify: `src/backend/secret.rs` (trait signatures), `src/secret/manager.rs` (`SecretOperations` signatures)

**Interfaces (produced, used by every later task):**

```rust
pub enum SnapshotValue { Omit, Include }

pub struct Secret { pub metadata: SecretMetadata, pub value: SecretValue }
impl std::ops::Deref for Secret { type Target = SecretMetadata; … }
impl std::ops::DerefMut for Secret { … }
impl Secret {
    pub fn into_metadata(self) -> SecretMetadata;
    pub fn metadata(&self) -> SecretMetadata;
    pub fn into_parts(self) -> (SecretMetadata, SecretValue);
}

pub struct SecretSnapshot { pub metadata: SecretMetadata, pub value: Option<SecretValue>, pub revision: String }
```

`SecretBackend` (all `async`, `Result<_, BackendError>`):

| Method | Signature |
| --- | --- |
| `get_secret_metadata` | `(&self, vault: &str, name: &str) -> Result<SecretMetadata>` **required** |
| `get_secret` | `(&self, vault: &str, name: &str) -> Result<Secret>` **required** |
| `get_secret_version_metadata` | `(&self, vault, name, version: &str) -> Result<SecretMetadata>` **required** |
| `get_secret_version` | `(&self, vault, name, version) -> Result<Secret>` **required** |
| `set_secret` | `(&self, vault, request: SecretRequest) -> Result<SecretMetadata>` |
| `update_secret` | `(&self, vault, name, request: SecretUpdateRequest) -> Result<SecretMetadata>` |
| `update_secret_if_revision` | `… -> Result<SecretMetadata>` |
| `validate_secret_revision` | `… -> Result<SecretMetadata>` |
| `create_secret_if_absent` | `… -> Result<SecretMetadata>` |
| `rename_secret`, `rename_secret_if_revision` | `… -> Result<SecretMetadata>` |
| `list_versions` | `… -> Result<Vec<SecretMetadata>>` |
| `rollback`, `restore_secret`, `restore_from_backup` | `… -> Result<SecretMetadata>` |
| `get_secret_snapshot` | `(&self, vault, name, with_value: SnapshotValue) -> Result<SecretSnapshot>` |
| `get_transfer_snapshot` | same shape, default delegates |
| `secret_exists` | default calls `get_secret_metadata` |
| unchanged | `delete_secret`, `purge_secret`, `list_secrets`, `list_deleted_secrets`, `backup_secret`, `native_rotate`, `delete_secret_if_revision`, `validate_transfer_*`, `supports_*` |

`SecretOperations` (legacy, Azure-only) gets the identical split for the methods it has; `update_secret_attributes` returns `SecretMetadata`.

- [ ] **Step 1: Write the failing domain tests** in `src/secret/domain/secret.rs` tests: `into_parts` round-trips; `Deref` gives `secret.name`; `Debug` of `Secret` and `SecretSnapshot` is redacted (reuse the canary); `SnapshotValue` is `Copy + Eq`.
- [ ] **Step 2: Run** `cargo test --lib secret::domain::secret` → compile errors.
- [ ] **Step 3: Change the domain types and both traits** as specified. Rename `SecretProperties` → `Secret` crate-wide with a mechanical replace of the identifier only (`grep -rlw SecretProperties src tests`), then fix construction sites in Task 2. Do not keep a `SecretProperties` alias.
- [ ] **Step 4:** `cargo check --all-targets --all-features 2>&1 | grep -c "^error"` — expect errors only in adapters, wrappers, and consumers (Tasks 2–4). Commit the domain + trait change even though the crate does not compile yet? **No.** Tasks 1–4 are one compile unit; commit at the end of Task 4. Use `git add -p`-free single commits per task only where the tree compiles; here, keep Task 1's edits uncommitted and proceed.

---

### Task 2: Adapters and wrappers

**Files:**
- Modify: `src/backend/local/secrets.rs` (`meta_to_properties` → `meta_to_metadata(meta) -> SecretMetadata` and `meta_to_secret(meta, value) -> Secret`; each trait method), `src/backend/aws/secrets.rs` (`props_from_describe` → `metadata_from_describe`, `props_from_value` → `secret_from_value`), `src/backend/azure/secrets.rs` + `src/secret/manager.rs` (`parse_secret_properties_bundle` returns `SecretMetadata` plus `Option<SecretValue>`; `get_secret` errors with `BackendError::Internal("provider returned no value")` if the bundle lacks one), `src/backend/guard.rs`, `src/agent/enforce.rs`, `src/backend/secret.rs` helpers (`rename_request_from_properties(new_name, current: &Secret)`, `transfer_metadata_revision(&SecretMetadata)`), `src/backend/attachment_keys.rs`.

**Rules:**
- A metadata method must issue the same provider calls the old `include_value: false` path issued; a value method the same as `include_value: true`. Diff the old bodies; do not merge them into one body with a branch.
- Writes return `metadata` from the same provider response they used before (`.into_metadata()` where the old code returned a `Secret` with `value: None`, or the metadata half of `into_parts()` where a value was present).
- Wrappers (`GuardedSecretBackend`, `PolicyEnforcedBackend`) delegate method-for-method; policy preflight for `get_secret_metadata` uses the same permission as the old `get_secret(.., false)` path, and `get_secret` the same as `(.., true)` — read `src/agent/enforce.rs` to confirm which permission each path checked and preserve it.
- Test-only `SecretBackend` impls (`src/web/testutil.rs`, `src/tui/app.rs`, `src/records/conversion.rs`, `src/backend/guard.rs`, `src/backend/secret.rs`, `src/agent/enforce.rs`, `src/cli/{secret_ops,mv_ops,system_ops}.rs`, `src/secret/{attachments,attachment_transfer_execution_tests}.rs`, `src/backend/attachment_keys.rs`) get the same split.

- [ ] **Step 1:** Migrate adapters; `cargo check --all-features --lib 2>&1 | grep -c "^error"` decreasing to only consumer errors.
- [ ] **Step 2:** `cargo test --lib backend:: 2>&1 | tail -10` once consumers compile (Task 3); until then proceed.

---

### Task 3: Consumers

**Files:** everything else the compiler reports: `src/cli/{secret_ops,mv_ops,system_ops,migrate_ops,vault_ops,config_ops,transfer_support,ls_view}.rs`, `src/records/{conversion,keeper}.rs`, `src/secret/attachment_*.rs`, `src/secret/scheduled_rotation.rs`, `src/web/{api,secrets,context}.rs`, `src/tui/{app,data}.rs`, `src/scan/orchestrator.rs`, `src/workspace/resolve.rs`, `tests/*`.

**Rules:**
- `get_secret(v, n, false)` → `get_secret_metadata(v, n)`; the result is `SecretMetadata`, so `.value` reads disappear (there were none on that path by construction; if one exists it was a latent bug — report it).
- `get_secret(v, n, true)` → `get_secret(v, n)`; `props.value.as_ref().map(SecretValue::expose_secret)` / `.ok_or(..)` unwrapping becomes `secret.value.expose_secret()`; remove the now-impossible "no value" error branches.
- Sites that called `get_secret(.., true)` and never read `.value` (the reviewer in PR 1 found these exist) become `get_secret_metadata` — this removes a value fetch, which is a behavior improvement, not a change to preserve. List every such site in the report.
- Writes: callers that formatted or inspected the returned properties keep working on `SecretMetadata` (all display fields are there). Callers that read `.value` from a write result (none expected; report any).
- `get_secret_snapshot(.., true/false)` → `SnapshotValue::Include`/`Omit`.
- Web: `get_secret` handler → `get_secret_metadata`; `reveal_secret` → `get_secret` then `secret.value.expose_secret()`; `put/patch/move/rename/restore` return the `SecretMetadata` the trait now gives directly (delete the `into_metadata()` calls PR 1 added).

- [ ] **Step 1:** Iterate `cargo check --all-targets --all-features` to zero errors; then `cargo check` and `cargo check --no-default-features`.
- [ ] **Step 2:** `grep -rn "include_value" src tests` → empty. `grep -rnw "SecretProperties" src tests docs/*.md CLAUDE.md` → empty (update docs mentions to `Secret`/`SecretMetadata`).
- [ ] **Step 3:** `cargo test --all-features --lib 2>&1 | tail -15` and the e2e groups: `e2e_local_backend`, `e2e_record_types`, `e2e_workspaces`, `e2e_transfer`, `e2e_local_file_ops`, `e2e_totp`, `local_backend_integration`, `tui_view_tests`, `aws_backend_tests`.
- [ ] **Step 4: Commit** everything from Tasks 1–3: `secret: split SecretBackend getters by value disclosure and return metadata from writes`.

---

### Task 4: Tests that pin the split

**Files:**
- Modify: `src/backend/secret.rs` tests (fake backend: `get_secret_metadata` must not be able to observe a value — the fake returns `SecretMetadata` built without ever holding a `SecretValue`), `src/backend/local/secrets.rs` tests (metadata getter does not open the value file: assert via a store where the value file is deleted after write — `get_secret_metadata` succeeds, `get_secret` fails with `NotFound`/decryption error), `src/agent/enforce.rs` tests (policy permission for metadata vs value getters unchanged from the old bool paths), `src/web/api.rs` tests (canary test from PR 1 still passes; `GET /secrets/{name}` handler now cannot even name a value).
- Doc-test in `src/backend/secret.rs`: `compile_fail` showing `backend.get_secret(v, n, true)` no longer compiles.

- [ ] **Step 1:** Write the tests; run the named modules; commit `secret: pin metadata getters to never touch values`.

---

### Task 5: Docs, changelog, roadmap, full gates

- `CLAUDE.md` Records section: `Secret` (value always present) vs `SecretMetadata`; `get_secret_metadata` vs `get_secret`; `SnapshotValue`.
- `CHANGELOG.md` Unreleased → Changed: trait split; metadata paths no longer fetch values on any backend; web `GET /secrets/{name}` served from the metadata getter.
- `ROADMAP.md` P1 entry: only PR 3 (disclosure DTOs + canary suite) remains.
- Full gates: fmt, clippy, `cargo check`, `--no-default-features`, `cargo test --all-features --workspace`, `cargo test --doc`.
- Commit `docs: describe the value-vs-metadata getter split`.
