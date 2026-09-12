# Secret domain model: values apart from metadata — design

**Date:** 2026-09-11
**Baseline:** v0.39.0 plus the cache-removal-invalidation branch
**Checklist items:** A05-01 through A05-08 (xv Feature Priorities checklist,
2026-09-08); ROADMAP "P1 — Split secret-domain types from provider/legacy
manager types".
**Compatibility:** none required. Internal types only; no on-disk secret
format, CLI output contract, or web API response *shape for metadata*
changes. Serialized value-bearing types were never read back anywhere
(verified: no `from_str`/`from_slice`/`Json<T>` over `SecretRequest`,
`SecretProperties`, or `SecretUpdateRequest`), so removing their serde
impls breaks nothing that runs.

## Problem

`SecretProperties`, `SecretRequest`, and `SecretUpdateRequest` in
`src/secret/manager.rs` carry plaintext in `Zeroizing<String>` fields and
derive `Debug`, `Serialize`, and `Deserialize`. `Zeroizing` protects memory
at drop, not formatting: `{:?}` prints the value and `serde_json::to_*`
emits it as a plain string (zeroize's `serde` feature is on). Safety today
rests on discipline at each site: the web layer has three different
redaction idioms (`include_value: false`, an inline two-liner, a named
helper), and `put_secret`/`patch_secret`/`move_secret`/`restore` return
`Json<SecretProperties>` relying on every backend leaving `value` as `None`.
The backend-neutral `SecretBackend` trait imports these types from the
legacy Azure module, which itself imports Azure types: a cycle that keeps
provider coupling in otherwise generic code.

## Goals

1. Generic `Debug`/serialization of a domain object cannot emit a secret
   value. Enforced by the type system, not by review.
2. "Does this call return the value?" is answered by the method signature,
   not by a boolean argument.
3. Intentional disclosure is explicit, grep-able, and tested in both
   directions (allowed path returns the value; every other path does not).
4. Domain types live in a backend-neutral module with no provider imports.

## Non-goals

- Changing what is stored in backend tags (typed-record metadata fields
  stay tags by design).
- Changing the cache payload shape (`SecretSummary` keeps its fields, so
  `secrets-list-v5.json` stays v5).
- Removing the legacy `SecretOperations` trait or `AzureSecretOperations`;
  they stay as the Azure adapter's implementation detail.
- Constant-time comparison of values (no such dependency exists; add one
  only if a real timing surface appears).

## Design

### Module

New module `src/secret/domain/` with:

- `value.rs` — `SecretValue`
- `metadata.rs` — `SecretMetadata`, `SecretSummary`, `DeletedSecretSummary`,
  `SecretVersion` alias, `FieldUpdate<T>`
- `secret.rs` — `Secret`, `SecretSnapshot`
- `request.rs` — `SecretRequest`, `SecretUpdateRequest`,
  `SecretAttributesUpdate`
- `disclosure.rs` — `DisclosedSecret`, `ConnectionComponent`,
  `parse_connection_components`
- `mod.rs` — re-exports

`src/secret/domain` imports nothing from `crate::backend::*`. The Azure
manager keeps importing from it, never the reverse.

### `SecretValue`

```rust
pub struct SecretValue(Zeroizing<String>);
```

- `SecretValue::new(impl Into<String>)`.
- `expose_secret(&self) -> &str` is the only read. The name is the single
  grep target for every plaintext read in the codebase.
- `Clone` (transfer, rollback, and rename legitimately copy), `PartialEq`,
  `Eq`.
- `Debug` prints `SecretValue([REDACTED])`. No `Display`. No `Serialize`,
  no `Deserialize`, no `From<SecretValue> for String`.
- Doc-test with `compile_fail` proving `serde_json::to_string(&SecretValue)`
  and `format!("{}", value)` do not compile.

### Metadata vs secret

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretMetadata { /* every SecretProperties field except value */ }

#[derive(Debug, Clone)]
pub struct Secret {
    pub metadata: SecretMetadata,
    pub value: SecretValue,
}

#[derive(Debug, Clone)]
pub struct SecretSnapshot {
    pub metadata: SecretMetadata,
    pub value: Option<SecretValue>,
    pub revision: String,
}
```

`Secret` implements `Deref<Target = SecretMetadata>` and `DerefMut` so
existing `props.name`, `props.tags`, `props.content_type` reads keep
compiling; only `props.value` sites change. `Secret` derives no serde.
`SecretSnapshot` keeps an optional value because transfer and CAS paths
snapshot metadata without a value; it is never serialized.

`SecretSummary` and `DeletedSecretSummary` move unchanged (field-for-field)
so the cache filename and the `SecuritySurface` allowlist in `src/error.rs`
stay valid apart from the import path.

### Requests

```rust
#[derive(Debug, Clone)]
pub struct SecretRequest { pub value: SecretValue, /* rest unchanged */ }

#[derive(Debug, Clone)]
pub struct SecretUpdateRequest { pub value: Option<SecretValue>, /* rest unchanged */ }
```

No serde on either. `SecretAttributesUpdate` (metadata only) keeps serde.
Web request bodies keep their own DTOs (`PutSecretBody` etc.) and convert.

### Trait split (the signature is the disclosure)

`SecretBackend` drops every `include_value: bool`:

| Before | After |
| --- | --- |
| `get_secret(vault, name, include_value) -> SecretProperties` | `get_secret_metadata(vault, name) -> SecretMetadata` and `get_secret(vault, name) -> Secret` |
| `get_secret_version(vault, name, version, include_value) -> SecretProperties` | `get_secret_version_metadata(..) -> SecretMetadata` and `get_secret_version(..) -> Secret` |
| `get_secret_snapshot(vault, name, include_value)` / `get_transfer_snapshot(..)` | keep one `bool`-free pair: `get_secret_snapshot(vault, name, with_value: SnapshotValue)` where `enum SnapshotValue { Omit, Include }` — an enum, not a bool, so call sites read as intent |
| `set_secret / update_secret / update_secret_if_revision / create_secret_if_absent / rename_secret* / rollback / restore_secret / restore_from_backup / validate_secret_revision -> SecretProperties` | `-> SecretMetadata` (writes never echo the value) |
| `list_versions -> Vec<SecretProperties>` | `-> Vec<SecretMetadata>` |
| `secret_exists` default | calls `get_secret_metadata` |

The legacy `SecretOperations` trait gets the same split so the Azure adapter
is a pure delegation again. `GuardedSecretBackend` and
`PolicyEnforcedBackend` delegate method-for-method.

Consumers pick the metadata method unless they need the value. Sites that
today pass `true` and then never read `.value` become metadata calls; the
compiler finds the rest.

### Disclosure boundaries

```rust
#[derive(Debug, Serialize)]
pub struct DisclosedSecret { pub name: String, pub value: String }

impl Secret {
    /// The only way a value becomes a serializable string. Every caller is a
    /// reviewed disclosure boundary; grep `.disclose(` to list them.
    pub fn disclose(self) -> DisclosedSecret;
}
```

Reviewed boundaries after this change, and the only ones:

- CLI `xv get` / `xv get --field` raw print and clipboard copy
  (`expose_secret`).
- CLI `xv get --record` envelope printers (fields come from
  `parse_sensitive_envelope` over `expose_secret`).
- CLI `vault export` with `--include-values` (`disclose` per secret; keeper
  export takes `&str` from it).
- Web `POST /secrets/{name}/value` (`disclose` → `{"value": ..}`).
- TUI reveal keystroke (`expose_secret` into the existing
  `Zeroizing<String>` message; unchanged behavior).
- `ConnectionComponent` table for parsed connection strings (already a
  deliberate reveal; documented as such).
- Scan orchestrator `SecretRef` (needs plaintext to match; `expose_secret`).

Everything else (`ls` JSON, cache, error contexts, tracing, web metadata
responses, `xv get` without `--raw`) is typed so it cannot reach a value.

### Provider snapshots

Adapters keep provider identifiers (Azure bundle `id`, AWS ARN, local
`SecretMeta`) private and build `SecretMetadata`/`Secret` at one place each:
Azure `parse_secret_properties_bundle`, AWS `props_from_describe`/
`props_from_value`, local `meta_to_properties`. The trait
`transfer_metadata_revision` helper keeps its explicit field tuple; it takes
`&SecretMetadata` now, which makes "value excluded by construction" a type
fact.

## Delivery

Three stacked PRs, each green on the full local gate:

1. **Domain module and value type.** Create `src/secret/domain/`, move the
   types, introduce `SecretValue`, drop serde from value-bearing types,
   redact `Debug`, update every import path. Behavior unchanged; `Secret`
   still has `value: Option<SecretValue>` at this step so the change is
   mechanical.
2. **Trait split.** Remove `include_value`; add metadata methods; writes
   return `SecretMetadata`; `Secret.value` becomes non-optional. Migrate
   backends, wrappers, and all consumers; delete the three ad-hoc web
   redaction idioms.
3. **Disclosure boundaries and proof.** `DisclosedSecret`/`disclose`,
   canary tests across Debug, serde, cache bytes, `ls --format json`, web
   metadata bodies, error Display, and tracing; positive tests for each
   boundary; `SecuritySurface` allowlist and docs updated; ROADMAP P1 entry
   removed; CHANGELOG entry.

## Verification

- Doc-test `compile_fail` for `SecretValue` serialization and `Display`.
- Unit: `Debug` of `Secret`, `SecretRequest`, `SecretUpdateRequest`,
  `SecretSnapshot` with a canary value contains `[REDACTED]` and not the
  canary.
- Web (`src/web/testutil.rs` fake backend): put/patch/move/rename/restore/
  convert response bodies and `GET /secrets/{name}` contain no canary;
  `POST /secrets/{name}/value` returns it.
- CLI e2e (local backend): `ls --format json`, cache file bytes, and stderr
  of a failing command contain no canary; `get --raw` prints it; `vault
  export --include-values` contains it and without the flag does not.
- Existing suites: `cargo test --all-features --workspace`, web unit tests,
  browser tests unchanged.
