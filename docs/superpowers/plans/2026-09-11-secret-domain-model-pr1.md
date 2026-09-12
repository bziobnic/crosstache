# Secret Domain Model PR 1: Domain Module and `SecretValue` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the secret-domain types out of the legacy Azure manager into a backend-neutral `src/secret/domain/` module, wrap plaintext in a `SecretValue` newtype that cannot be serialized or `Display`ed and whose `Debug` is redacted, remove serde from every value-bearing type, and give the web layer a value-free `SecretMetadata` response type.

**Architecture:** `src/secret/domain/{value,metadata,secret,request,disclosure,mod}.rs` own the types. `crate::secret::manager` becomes an Azure-only implementation module that imports from `domain`. `SecretProperties` keeps its name and `value: Option<SecretValue>` in this PR (the trait split that makes the value non-optional is PR 2). `SecretMetadata` is `SecretProperties` minus `value`, produced by `SecretProperties::into_metadata()`; web handlers return it. Every plaintext read goes through `SecretValue::expose_secret()`.

**Tech Stack:** Rust 2021, `zeroize` (already a dependency), `serde`, `tabled`, existing web test harness `src/web/testutil.rs`.

**Spec:** `docs/superpowers/specs/2026-09-11-secret-domain-model-design.md`

## Global Constraints

- Never run `cargo` with `run_in_background`; foreground only, timeout up to 600000 ms, pipe through `tail -40`.
- Never use `git stash`. Never push.
- Behavior is unchanged in this PR: no CLI output, exit code, web response *metadata* field, cache filename, or on-disk format changes. Web responses that previously carried `"value": null` no longer carry a `value` key at all; that is the one intended wire change.
- `SecretValue` has no `Serialize`, no `Deserialize`, no `Display`, no `Deref`, no `From<SecretValue> for String`. The only read is `expose_secret(&self) -> &str`.
- `src/secret/domain/**` imports nothing from `crate::backend`, `crate::web`, `crate::cli`, or `crate::secret::manager`.
- `SecretSummary` and `DeletedSecretSummary` keep their exact fields and serde attributes so `secrets-list-v5.json` and the `SecuritySurface` allowlist in `src/error.rs` stay valid.
- Gates before finishing any task: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and the named tests. Task 5 runs the full workspace suite.

---

### Task 1: `SecretValue`

**Files:**
- Create: `src/secret/domain/mod.rs`, `src/secret/domain/value.rs`
- Modify: `src/secret/mod.rs` (add `pub mod domain;` after `pub mod attachments;`)

**Interfaces:**
- Produces: `crate::secret::domain::SecretValue` with `new`, `expose_secret`, `len`, `is_empty`, `Clone`, `PartialEq`, `Eq`, redacted `Debug`.

- [ ] **Step 1: Write the failing tests**

Create `src/secret/domain/value.rs` with only the tests module first:

```rust
//! Plaintext secret value. See module docs in `mod.rs`.

#[cfg(test)]
mod tests {
    use super::SecretValue;

    const CANARY: &str = "super-secret-value-canary";

    #[test]
    fn debug_is_redacted() {
        let v = SecretValue::new(CANARY);
        let dbg = format!("{v:?}");
        assert_eq!(dbg, "SecretValue([REDACTED])");
        assert!(!dbg.contains(CANARY));
        let opt = Some(SecretValue::new(CANARY));
        assert!(!format!("{opt:?}").contains(CANARY));
    }

    #[test]
    fn expose_returns_the_plaintext_and_only_that() {
        let v = SecretValue::new(CANARY);
        assert_eq!(v.expose_secret(), CANARY);
        assert_eq!(v.len(), CANARY.len());
        assert!(!v.is_empty());
        assert!(SecretValue::new("").is_empty());
    }

    #[test]
    fn equality_compares_plaintext() {
        assert_eq!(SecretValue::new("a"), SecretValue::new("a"));
        assert_ne!(SecretValue::new("a"), SecretValue::new("b"));
        let cloned = SecretValue::new("a").clone();
        assert_eq!(cloned.expose_secret(), "a");
    }
}
```

- [ ] **Step 2: Wire the module and run to verify it fails**

Create `src/secret/domain/mod.rs`:

```rust
//! Backend-neutral secret domain model.
//!
//! Plaintext lives only in [`SecretValue`], which cannot be serialized,
//! displayed, or dereferenced; the single read is
//! [`SecretValue::expose_secret`], so `grep expose_secret` lists every
//! plaintext read in the crate. Metadata types (`SecretMetadata`,
//! `SecretSummary`, `DeletedSecretSummary`) are value-free and serializable.
//! Value-bearing composites (`SecretProperties`, `SecretRequest`,
//! `SecretUpdateRequest`, `SecretSnapshot`) derive `Debug` (redacted through
//! `SecretValue`) but never serde.
//!
//! This module imports nothing from `crate::backend`, `crate::cli`,
//! `crate::web`, or `crate::secret::manager`; adapters translate provider
//! wire formats into these types at one place each.

pub mod value;

pub use value::SecretValue;
```

Add `pub mod domain;` to `src/secret/mod.rs` directly after `pub mod attachments;`.

Run: `cargo test --lib secret::domain::value 2>&1 | tail -20`
Expected: compile error, `SecretValue` not found.

- [ ] **Step 3: Implement `SecretValue`**

Replace the top of `src/secret/domain/value.rs` (above the tests module) with:

```rust
//! Plaintext secret value. See module docs in `mod.rs`.

use zeroize::Zeroizing;

/// A plaintext secret value.
///
/// - No `Serialize`/`Deserialize`: a value cannot reach JSON, YAML, TOML, the
///   listing cache, or a web body by accident. Disclosure boundaries convert
///   explicitly (`DisclosedSecret` in PR 3, `expose_secret` today).
/// - No `Display`/`Deref`: `format!("{v}")` and implicit `&str` coercion do
///   not compile.
/// - `Debug` prints `SecretValue([REDACTED])`, so any struct that derives
///   `Debug` and contains one stays safe to log.
/// - The buffer is zeroized on drop.
///
/// ```compile_fail
/// let v = crosstache::secret::domain::SecretValue::new("x");
/// let _ = serde_json::to_string(&v);
/// ```
///
/// ```compile_fail
/// let v = crosstache::secret::domain::SecretValue::new("x");
/// let _ = format!("{v}");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// The only plaintext read. Every caller is a disclosure boundary or an
    /// adapter writing to a provider; keep the call sites greppable.
    pub fn expose_secret(&self) -> &str {
        self.0.as_str()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}
```

- [ ] **Step 4: Run the tests, including the compile-fail doc tests**

Run: `cargo test --lib secret::domain::value 2>&1 | tail -10 && cargo test --doc secret::domain 2>&1 | tail -10`
Expected: 3 unit tests pass; 2 doc tests pass (as compile_fail).

- [ ] **Step 5: Commit**

```bash
git add src/secret/domain src/secret/mod.rs
git commit -m "secret: add SecretValue newtype with redacted Debug and no serde"
```

---

### Task 2: Move the domain types out of `manager.rs` (no field changes)

**Files:**
- Create: `src/secret/domain/metadata.rs`, `src/secret/domain/secret.rs`, `src/secret/domain/request.rs`, `src/secret/domain/disclosure.rs`
- Modify: `src/secret/domain/mod.rs`, `src/secret/manager.rs` (delete the moved definitions and import them), `src/backend/secret.rs` (move `SecretSnapshot` out, import it back), every file importing `crate::secret::manager::{…}` for these names (list below)

**Interfaces:**
- Produces, all under `crate::secret::domain`: `SecretProperties`, `SecretSummary`, `DeletedSecretSummary`, `SecretRequest`, `SecretUpdateRequest`, `SecretAttributesUpdate`, `FieldUpdate`, `SecretSnapshot`, `ConnectionComponent`, `parse_connection_components`, `connection_string_key_description`. Definitions are byte-for-byte the current ones (still `Zeroizing<String>` for values, still deriving serde) — this task is a pure move so its diff reviews as a move.

- [ ] **Step 1: Create the files by moving code**

`src/secret/domain/metadata.rs` receives, verbatim from `src/secret/manager.rs`: the `SecretSummary` struct (with its `display_optional_group` helper and doc comments), `DeletedSecretSummary`, `FieldUpdate<T>` and its `impl` block (`from_flags`, `apply`, `is_unchanged`), `SecretAttributesUpdate`, and `display_version_number`. Add the imports each needs (`chrono::{DateTime, Utc}`, `serde::{Deserialize, Serialize}`, `std::collections::HashMap`, `tabled::Tabled`, `crate::error::{CrosstacheError, Result}` if `from_flags` uses it). Mark `display_version_number` and `display_optional_group` `pub(crate)` so `secret.rs` can use them in `#[tabled(display_with = …)]` — tabled needs the function path to resolve from the struct's module, so pass the full path: `display_with = "crate::secret::domain::metadata::display_version_number"`.

`src/secret/domain/secret.rs` receives `SecretProperties` verbatim (keep `value: Option<Zeroizing<String>>` and all derives for this task; update the two `display_with` paths as above) and `SecretSnapshot` verbatim from `src/backend/secret.rs:20-25` including its `#[cfg_attr(not(feature = "ui"), allow(dead_code))]`.

`src/secret/domain/request.rs` receives `SecretRequest` and `SecretUpdateRequest` verbatim.

`src/secret/domain/disclosure.rs` receives `ConnectionComponent`, `connection_string_key_description`, and `parse_connection_components` verbatim; it imports `crate::utils::helpers::parse_connection_string`. Add a module doc: `//! Types that carry plaintext on purpose. Everything here is a reviewed disclosure boundary.`

`src/secret/domain/mod.rs` becomes:

```rust
pub mod disclosure;
pub mod metadata;
pub mod request;
pub mod secret;
pub mod value;

pub use disclosure::{
    connection_string_key_description, parse_connection_components, ConnectionComponent,
};
pub use metadata::{DeletedSecretSummary, FieldUpdate, SecretAttributesUpdate, SecretSummary};
pub use request::{SecretRequest, SecretUpdateRequest};
pub use secret::{SecretProperties, SecretSnapshot};
pub use value::SecretValue;
```

(keep the module doc comment from Task 1 above these lines).

- [ ] **Step 2: Update `manager.rs` and `backend/secret.rs`**

In `src/secret/manager.rs` delete the moved items and add:

```rust
use crate::secret::domain::{
    DeletedSecretSummary, SecretAttributesUpdate, SecretProperties, SecretRequest,
    SecretSummary, SecretUpdateRequest,
};
```

Remove now-unused imports (`tabled::Tabled`, possibly `Serialize`/`Deserialize` if nothing else in the file uses them; keep `Zeroizing` if `rollback_secret`/`parse_secret_properties_bundle` still construct values). Keep `FieldUpdate` imported only if manager.rs references it.

In `src/backend/secret.rs` replace the `use crate::secret::manager::{…}` block with `use crate::secret::domain::{DeletedSecretSummary, SecretProperties, SecretRequest, SecretSnapshot, SecretSummary, SecretUpdateRequest};` and delete the local `SecretSnapshot` definition.

- [ ] **Step 3: Rewrite every other import path**

Every `crate::secret::manager::` (and in `tests/`, `crosstache::secret::manager::`) import of a moved name becomes `crate::secret::domain::` / `crosstache::secret::domain::`. Imports of `AzureSecretOperations`, `SecretOperations`, or `SecretInfo` stay on their current paths. Find the sites with:

```bash
grep -rn "secret::manager::" src tests
```

Do not use a blind sed: the same `use` list can mix moved and unmoved names (e.g. `src/backend/azure/mod.rs:34` imports `AzureSecretOperations` alongside domain types). Open each file from `grep -rn "secret::manager::" src tests` and split the list into a `domain` import and, where needed, a remaining `manager` import. Files known to import moved names (from the inventory): `src/records/keeper.rs`, `src/records/conversion.rs`, `src/tui/message.rs`, `src/tui/app.rs`, `src/web/testutil.rs`, `src/web/api.rs`, `src/web/secrets.rs`, `src/web/context.rs`, `src/workspace/resolve.rs`, `src/utils/fuzzy.rs`, `src/backend/attachment_keys.rs`, `src/backend/attachment_key_aws_tests.rs`, `src/backend/guard.rs`, `src/backend/azure/mod.rs`, `src/backend/azure/secrets.rs`, `src/backend/local/transfer_namespace.rs`, `src/backend/local/secrets.rs`, `src/backend/aws/secrets.rs`, `src/agent/enforce.rs`, `src/cli/mv_ops.rs`, `src/cli/system_ops.rs`, `src/cli/config_ops.rs`, `src/cli/ls_view.rs`, `src/cli/migrate_ops.rs`, `src/cli/secret_ops.rs`, `src/cli/vault_ops.rs`, `src/secret/scheduled_rotation_tests.rs`, `src/secret/attachment_*.rs` (all), `src/cache/manager.rs`, `src/cache/models.rs`, `tests/aws_localstack_tests.rs`, `tests/local_audit_git_tests.rs`, `tests/e2e_azure_backend.rs`, `tests/aws_backend_tests.rs`, `tests/e2e_aws_backend.rs`, `tests/migration_round_trip_tests.rs`, `tests/e2e_record_types.rs`, `tests/e2e_local_file_ops.rs`, `tests/local_backend_integration.rs`, `tests/e2e_totp.rs`, `tests/e2e_local_backend.rs`. Also fix any fully-qualified inline paths (`crate::secret::manager::SecretSummary` appears in `src/cli/secret_ops.rs` around the union-ls cache reads).

- [ ] **Step 4: Compile everything**

Run: `cargo check --all-targets --all-features 2>&1 | tail -30`
Expected: clean. Then `grep -rn "secret::manager::" src tests | grep -vE "AzureSecretOperations|SecretOperations|SecretInfo"` must print nothing.

- [ ] **Step 5: Run the affected tests**

Run: `cargo test --lib secret:: 2>&1 | tail -10 && cargo test --lib backend:: 2>&1 | tail -10 && cargo test --lib web:: --features ui 2>&1 | tail -10`
Expected: all pass (no behavior changed).

- [ ] **Step 6: Commit**

```bash
git add -A src tests
git commit -m "secret: move domain types from the Azure manager into secret::domain"
```

---

### Task 3: Plaintext becomes `SecretValue`; value-bearing types lose serde; web returns `SecretMetadata`

**Files:**
- Modify: `src/secret/domain/secret.rs` (add `SecretMetadata`, conversions; change `value` type; drop serde derives), `src/secret/domain/request.rs` (change `value` types; drop serde derives)
- Modify: every `.value` read/write site the compiler reports (about 230 sites across `src/` and `tests/`; the inventory by file is in the spec's research: backends, `src/secret/attachment_*`, `src/records/conversion.rs`, `src/records/keeper.rs`, `src/cli/{secret_ops,mv_ops,system_ops,migrate_ops,vault_ops}.rs`, `src/web/{api,secrets,testutil}.rs`, `src/tui/{app,data}.rs`, `src/scan/orchestrator.rs`, `src/agent/enforce.rs`, `tests/*`)
- Modify: `src/web/api.rs` (`get_secret`, `put_secret`, `patch_secret`, `move_secret`), `src/web/secrets.rs` (`rename`, `restore`, `redact_conversion_properties`, `apply_conversion_route`), `src/web/mod.rs` only if a route signature changes

**Interfaces:**
- Produces: `SecretMetadata` (every `SecretProperties` field except `value`; derives `Debug, Clone, Serialize, Deserialize, Tabled`), `SecretProperties::into_metadata(self) -> SecretMetadata`, `SecretProperties::metadata(&self) -> SecretMetadata`.
- Changes: `SecretProperties.value: Option<SecretValue>`, `SecretRequest.value: SecretValue`, `SecretUpdateRequest.value: Option<SecretValue>`. `SecretProperties`, `SecretRequest`, `SecretUpdateRequest`, `SecretSnapshot` derive `Debug, Clone` only.

- [ ] **Step 1: Write the failing redaction tests**

Append to `src/secret/domain/secret.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::domain::{SecretRequest, SecretUpdateRequest, SecretValue};
    use std::collections::HashMap;

    const CANARY: &str = "super-secret-value-canary";

    fn props() -> SecretProperties {
        SecretProperties {
            name: "n".into(),
            original_name: "n".into(),
            value: Some(SecretValue::new(CANARY)),
            version: "v1".into(),
            version_number: Some(1),
            created_timestamp: 0,
            created_on: "2026-01-01".into(),
            updated_on: "2026-01-01".into(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::from([("k".to_string(), "v".to_string())]),
            content_type: "text/plain".into(),
            recovery_level: None,
        }
    }

    #[test]
    fn debug_of_every_value_bearing_type_is_redacted() {
        let p = props();
        assert!(!format!("{p:?}").contains(CANARY));
        assert!(format!("{p:?}").contains("[REDACTED]"));
        let snap = SecretSnapshot { properties: p.clone(), revision: "r".into() };
        assert!(!format!("{snap:?}").contains(CANARY));
        let req = SecretRequest {
            name: "n".into(),
            value: SecretValue::new(CANARY),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        };
        assert!(!format!("{req:?}").contains(CANARY));
        let upd = SecretUpdateRequest {
            name: "n".into(),
            expected_revision: None,
            value: Some(SecretValue::new(CANARY)),
            content_type: None,
            enabled: None,
            expires_on: Default::default(),
            not_before: Default::default(),
            tags: None,
            groups: None,
            note: Default::default(),
            folder: Default::default(),
            replace_tags: false,
            replace_groups: false,
        };
        assert!(!format!("{upd:?}").contains(CANARY));
    }

    #[test]
    fn metadata_serializes_every_field_but_the_value() {
        let m = props().into_metadata();
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains(CANARY));
        assert!(!json.contains("\"value\""));
        assert!(json.contains("\"name\":\"n\""));
        assert!(json.contains("\"tags\":{\"k\":\"v\"}"));
        assert!(json.contains("\"content_type\":\"text/plain\""));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib secret::domain::secret 2>&1 | tail -20`
Expected: compile errors (`SecretValue` not the field type, `into_metadata` missing).

- [ ] **Step 3: Change the domain types**

In `src/secret/domain/secret.rs`:

```rust
use crate::secret::domain::SecretValue;

/// Value-free view of a secret: everything in [`SecretProperties`] except the
/// plaintext. This is what listings, web metadata responses, caches, and
/// logs may carry.
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretMetadata {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub version: String,
    /// Human-readable sequential version number (1 = oldest). None when not in a version list context.
    #[tabled(rename = "Version", display_with = "crate::secret::domain::metadata::display_version_number")]
    pub version_number: Option<u32>,
    /// Raw Unix timestamp for sorting (not displayed)
    #[tabled(skip)]
    pub created_timestamp: i64,
    #[tabled(rename = "Created")]
    pub created_on: String,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(rename = "Enabled")]
    pub enabled: bool,
    #[tabled(skip)]
    pub expires_on: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub not_before: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub tags: HashMap<String, String>,
    #[tabled(rename = "Content Type")]
    pub content_type: String,
    #[tabled(skip)]
    pub recovery_level: Option<String>,
}

/// A secret with, optionally, its plaintext. Never serializable.
#[derive(Debug, Clone, Tabled)]
pub struct SecretProperties {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub value: Option<SecretValue>,
    #[tabled(skip)]
    pub version: String,
    #[tabled(rename = "Version", display_with = "crate::secret::domain::metadata::display_version_number")]
    pub version_number: Option<u32>,
    #[tabled(skip)]
    pub created_timestamp: i64,
    #[tabled(rename = "Created")]
    pub created_on: String,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(rename = "Enabled")]
    pub enabled: bool,
    #[tabled(skip)]
    pub expires_on: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub not_before: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub tags: HashMap<String, String>,
    #[tabled(rename = "Content Type")]
    pub content_type: String,
    #[tabled(skip)]
    pub recovery_level: Option<String>,
}

impl SecretProperties {
    /// Drop the plaintext, keeping every other field. The only way a
    /// `SecretProperties` becomes serializable.
    pub fn into_metadata(self) -> SecretMetadata {
        SecretMetadata {
            name: self.name,
            original_name: self.original_name,
            version: self.version,
            version_number: self.version_number,
            created_timestamp: self.created_timestamp,
            created_on: self.created_on,
            updated_on: self.updated_on,
            enabled: self.enabled,
            expires_on: self.expires_on,
            not_before: self.not_before,
            tags: self.tags,
            content_type: self.content_type,
            recovery_level: self.recovery_level,
        }
    }

    pub fn metadata(&self) -> SecretMetadata {
        self.clone().into_metadata()
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct SecretSnapshot {
    pub properties: SecretProperties,
    pub revision: String,
}
```

In `src/secret/domain/request.rs`: `SecretRequest` derives `Debug, Clone` with `pub value: SecretValue`; `SecretUpdateRequest` derives `Debug, Clone` with `pub value: Option<SecretValue>`; delete the `#[serde(skip)]` on `expected_revision` (no serde left). `SecretAttributesUpdate` is unchanged (metadata only, keeps serde).

Export `SecretMetadata` from `domain/mod.rs`.

- [ ] **Step 4: Migrate every site the compiler reports**

Rules, applied uniformly (do not invent alternatives):

| Old shape | New shape |
| --- | --- |
| `Zeroizing::new(s)` assigned to a `value` field | `SecretValue::new(s)` |
| `props.value.as_deref()` (yields `Option<&str>`) | `props.value.as_ref().map(SecretValue::expose_secret)` |
| `v.as_str()`, `&**v`, `&*v`, `v.to_string()` on a `Zeroizing<String>` value | `v.expose_secret()` / `v.expose_secret().to_string()` |
| `value.len()`, `value.is_empty()` | unchanged (methods exist) |
| `Some(v) => Zeroizing::new(v.clone())` re-wrapping | `v.clone()` |
| A `Zeroizing<String>` needed by an unrelated API (TUI messages, clipboard, `parse_sensitive_envelope`) | `Zeroizing::new(v.expose_secret().to_owned())` at that boundary |
| `props.value = None` followed by `Json(props)` in web handlers | `Json(props.into_metadata())` |

Web handlers: `get_secret`, `put_secret`, `patch_secret`, `move_secret` (`src/web/api.rs`), `rename` and `restore` (`src/web/secrets.rs`) return `Result<Json<SecretMetadata>, ApiError>` and end with `Ok(Json(props.into_metadata()))`. Where the old code also cleared tags (`renamed.tags.clear()` in `rename`; `redact_conversion_properties` in the conversion route), keep clearing tags on the metadata value before returning so the response shape is otherwise unchanged; delete `redact_conversion_properties` and set `ConversionResult.secret: SecretMetadata`. `reveal_secret` becomes `json!({ "value": props.value.as_ref().map(SecretValue::expose_secret) })`.

`src/backend/secret.rs::rename_request_from_properties` keeps `current.value.clone().ok_or_else(..)` (now a `SecretValue`). `transfer_metadata_revision` is unchanged (it never touched `value`).

Adapters (`src/backend/azure/secrets.rs`, `src/secret/manager.rs`, `src/backend/aws/secrets.rs`, `src/backend/local/secrets.rs`): wherever they hand plaintext to a provider client or crypto, use `request.value.expose_secret()`; wherever they build a `SecretProperties` from provider data, wrap with `SecretValue::new(..)`. Local `meta_to_properties(meta, value: Option<Zeroizing<String>>)` changes its parameter to `Option<SecretValue>` and its callers wrap accordingly.

Tests under `src/` and `tests/` that construct requests use `SecretValue::new("…")`; tests that assert on a value use `.expose_secret()`.

- [ ] **Step 5: Compile and iterate until clean**

Run: `cargo check --all-targets --all-features 2>&1 | grep -E "^error" | wc -l` repeatedly; then `cargo check --all-targets --all-features 2>&1 | tail -30` must be clean. Also `cargo check --no-default-features 2>&1 | tail -5` and `cargo check 2>&1 | tail -5` (default features) must be clean, since `SecretSnapshot` carries a `cfg_attr`.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib 2>&1 | tail -15 && cargo test --all-features --lib web:: 2>&1 | tail -10 && cargo test --test e2e_local_backend 2>&1 | tail -5 && cargo test --test e2e_record_types 2>&1 | tail -5`
Expected: all pass. Existing web tests that asserted `"value": null` on a metadata response must be updated to assert the key is absent (`json.get("value").is_none()`); that is the intended wire change and the only assertion change permitted.

- [ ] **Step 7: Commit**

```bash
git add -A src tests
git commit -m "secret: wrap plaintext in SecretValue and drop serde from value-bearing types"
```

---

### Task 4: Web canary tests, security-surface note, docs

**Files:**
- Modify: `src/web/api.rs` tests module, `src/web/secrets.rs` tests module (use the existing fake backend in `src/web/testutil.rs`)
- Modify: `src/error.rs` (comment above the `SecretSummary` `SecuritySurface` entry; add a `SecretMetadata` entry)
- Modify: `CLAUDE.md` (Records section pointer), `CHANGELOG.md` (Unreleased → Changed), `ROADMAP.md` (trim the P1 "Split secret-domain types" entry to what PR 2/3 still owe)

- [ ] **Step 1: Write the web canary tests**

In `src/web/api.rs` tests (mirror the style of `secret_list_exposes_canonical_expiry_without_value_disclosure` near line 1004):

```rust
    #[tokio::test]
    async fn metadata_responses_never_carry_the_value_but_reveal_does() {
        const CANARY: &str = "super-secret-value-canary";
        let (app, _state) = test_app_with_secret("leaky", CANARY).await; // use the existing testutil builder; adapt the name to what testutil exposes
        for (method, path, body) in [
            ("GET", "/api/secrets/leaky", None),
            ("PUT", "/api/secrets/leaky", Some(json!({ "value": CANARY }))),
            ("PATCH", "/api/secrets/leaky", Some(json!({ "note": "n" }))),
        ] {
            let (status, json) = request(&app, method, path, body).await;
            assert!(status.is_success(), "{method} {path}: {status}");
            let text = json.to_string();
            assert!(!text.contains(CANARY), "{method} {path} leaked: {text}");
            assert!(json.get("value").is_none(), "{method} {path} carries a value key: {text}");
        }
        let (status, json) = request(&app, "POST", "/api/secrets/leaky/value", None).await;
        assert!(status.is_success());
        assert_eq!(json["value"], CANARY);
    }
```

Use the actual helper names in `src/web/api.rs`'s test module (`request`/`send`/`test_app…`); the shape above is the requirement, the helper names are whatever the file already uses. Add the same kind of assertion for `rename` (`POST /api/secrets/{name}/rename` or the route `src/web/mod.rs` registers) and `restore` in `src/web/secrets.rs` tests: response contains no canary and no `value` key.

- [ ] **Step 2: Run to verify they fail or pass for the right reason**

Run: `cargo test --all-features --lib web::api::tests::metadata_responses 2>&1 | tail -15`
Expected: pass (Task 3 already removed the key). If it fails on a canary, that is a real leak: fix the handler, not the test.

- [ ] **Step 3: `src/error.rs` allowlist**

Above the `SecretSummary` entry, change the comment to:

```rust
            // Common structured output/cache payloads are metadata summaries.
            // SecretSummary and SecretMetadata are value-free by construction:
            // the plaintext type `SecretValue` has no serde impls, so the
            // value-bearing SecretProperties/SecretRequest cannot be
            // serialized at all and never need an entry here.
```

Add an entry after `SecretSummary`:

```rust
            SecuritySurface {
                category: "structured output",
                name: "SecretMetadata",
                fields: &[
                    "name",
                    "original_name",
                    "version",
                    "version_number",
                    "created_timestamp",
                    "created_on",
                    "updated_on",
                    "enabled",
                    "expires_on",
                    "not_before",
                    "tags",
                    "content_type",
                    "recovery_level",
                ],
                allowed_value_like_fields: &[],
            },
```

Run: `cargo test --lib error:: 2>&1 | tail -5` — the allowlist test must pass; if it checks fields against a struct definition, follow its mechanism.

- [ ] **Step 4: Docs**

`CLAUDE.md`, "Records, secrets, and attachments" section: add a first bullet `- `src/secret/domain/` — backend-neutral secret model: `SecretValue` (plaintext, no serde/Display, redacted Debug, read only via `expose_secret`), `SecretMetadata`, `SecretProperties`, requests, summaries` and change the Architecture note under "Backend layer" that says Azure delegates to `src/secret/` to add "`src/secret/manager.rs` is Azure-only implementation; domain types live in `src/secret/domain/`".

`CHANGELOG.md` under `## Unreleased`, add `### Changed` (or append to it):

```markdown
- **Secret plaintext is now a dedicated `SecretValue` type.** Every
  value-bearing internal model (`SecretProperties`, `SecretRequest`,
  `SecretUpdateRequest`, `SecretSnapshot`) lost its serde derives and prints
  `SecretValue([REDACTED])` under `{:?}`; the only plaintext read is
  `expose_secret()`. Web metadata responses (`GET/PUT/PATCH /secrets/{name}`,
  rename, move, restore, conversion) now return a value-free
  `SecretMetadata` body — the `value` key is absent instead of `null`;
  `POST /secrets/{name}/value` is unchanged. The domain types moved from the
  Azure-era `secret::manager` into `secret::domain`.
```

`ROADMAP.md`, the `### P1 — Split secret-domain types from provider/legacy manager types` section: rewrite the body to say the module split and the value type shipped, and what remains is the trait split (metadata vs value methods, no `include_value` bool) and explicit disclosure DTOs with canary coverage.

- [ ] **Step 5: Commit**

```bash
git add src/web src/error.rs CLAUDE.md CHANGELOG.md ROADMAP.md
git commit -m "secret: prove metadata responses are value-free; document the domain module"
```

---

### Task 5: Full gates

- [ ] **Step 1: Run**

```bash
cargo fmt --check 2>&1 | tail -5
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -20
cargo test --all-features --workspace 2>&1 | tail -30
cargo test --doc 2>&1 | tail -5
grep -rn "secret::manager::" src tests | grep -vE "AzureSecretOperations|SecretOperations|SecretInfo"   # must be empty
grep -rn "Zeroizing<String>" src/secret/domain                                                          # only inside value.rs
```

- [ ] **Step 2: Fix anything surfaced, amend into the responsible commit, re-run.**
