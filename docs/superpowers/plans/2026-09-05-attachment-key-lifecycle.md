# Retained-key enumeration, upgrade, and recovery

> **Status:** ✅ Shipped in **v0.39.0** (PRs #429, #430). Operator guide:
> [`docs/attachments.md`](../../attachments.md#inspecting-key-status-and-file-references).

**Goal:** Enumerate retained custody records, upgrade a V1 vault without breaking old attachments, and repair a missing/broken pointer using existing retained keys.
**Architecture:** Expand the narrow custody API with metadata-only retained enumeration, retaining policy enforcement. A lifecycle module verifies exact versions and identities before publishing pointers. CLI previews changes by default; --apply --offline acknowledges paused writers and applies.
**Spec:** This contract supplements the existing attachment_key types and ROADMAP; the original external lifecycle design is unavailable.
**Execution:** superpowers:subagent-driven-development; independent enumeration task delegated while parent implements lifecycle and CLI.

## Constraints
- No generic access to reserved records; no deletion, rotation to a newly generated key, blob rewrap, or provider-version rewriting.
- No identity/ciphertext/raw provider bodies in reports. Never overwrite unmarked strict-name collisions.
- Enumeration lists visible current marked retained records, not value material or every historical version. Policy-filtered results are not proof of custody completeness.
- Offline upgrade/recovery requires users to stop all writers/old clients; prewrite rereads detect changes but do not claim provider-portable CAS.
- Existing Local read-time recovery still applies.
- Preview performs reads only; actual application requires explicit --apply --offline. The tool implementation is authorized; do not run migrations on user's real vaults.
- Recovery scope: pointer repair from existing retained keys (recommended scope proceeded after optional preference received no reply). Cross-vault encrypted backup/restore needs a separate design for provider-version portability.

## Task 1: Enumeration
Files: src/backend/attachment_keys.rs, src/agent/enforce.rs and tests there.
Interface:
```rust
#[derive(Debug, Clone, Serialize)]
pub struct RetainedKeySummary { pub name: String, pub key_id: String, pub enabled: bool }
async fn list_retained_keys(&self, vault: &str) -> Result<Vec<RetainedKeySummary>, BackendError>;
```
Default Unsupported. Raw implementation uses list_secrets(vault,None), filters strictly canonical retained names AND exact KEY_RECORD_CONTENT_TYPE, maps only safe fields and sorts by name. Do not fetch values/versions. Policy wrapper authorizes list scope before I/O, delegates custody enumeration, and filters each entry with list_item_allowed; normal generic lists remain unchanged.
- [x] RED regression for mixed marked retained, unmarked collision, broad prefix, pointer, ordinary.
- [x] GREEN implementation, real LocalBackend test, provider error propagation, policy deny-before-I/O and per-item filtering.
- [x] Independent review. Parent owns final commit and builds.

## Task 2: Safe legacy reads and upgrade
Files: src/secret/attachments.rs, new src/secret/attachment_lifecycle.rs, src/secret/mod.rs.
- [x] Fix no-schema download resolution: V1 parses raw identity; V2 requires explicit legacy ID, reads ONLY that retained record, validates ID; direct V2 without fallback fails. Schema1 pinned reads unchanged.
- [x] Upgrade preview validates current V1 identity/version; derives ID; checks target retained record absence or marked identical identity. No fresh key generated.
- [x] Apply retains original identity under marked deterministic name, verifies exact committed/adopted version and ID. Before pointer publication re-read V1 source and verify version/value match; never publish after failed verification. Publish V2(active=old ID, legacy=old ID), exact readback confirmation; ensure original V1 provider version remains readable. Retain all committed keys on failure. Already matching upgraded V2 is idempotent.
- [x] Tests prove old no-schema blobs, schema1 legacy-version blobs, and new retained-version blobs decrypt across upgrade; collision refusal; interruption/retry; provider failures; preview no mutations.

## Task 3: Recovery and CLI
Files: lifecycle module; src/cli/attachment_key_ops.rs; tests/e2e_local_file_ops.rs.
- [x] keys subcommand returns schema_version=1, observation=visible_retained_records, keys array from narrow custody API.
- [x] upgrade and recover preview by default; --apply requires --offline; CSV/template rejected before I/O; preserve current --vault alias/literal routing.
- [x] recover requires selected --key-id and explicit --legacy-key-id or --no-legacy. Validate marked retained identities/versions before repair. Refuse V1 (use upgrade), refuse altering an already valid V2 pointer except exact no-op; preserve a parsable V2 legacy binding. Broken/missing pointer can be repaired only after unchanged prewrite reread and exact readback confirmation. No key material created/generated.
- [x] Real CLI tests for keys/upgrade/recover previews, explicit application gate, structured output, no secrets, and continued downloads.

## Task 4: Review, docs, validation, handoff
- [x] Document offline requirement, preview/apply workflow, recovery limits, policy visibility, and Local recovery side effects. Reconcile roadmap.
- [x] Independent review; focused and full workspace tests; Clippy -D warnings, fmt/diff checks, minimal build.
- [x] Prepare validated commits for origin/main sync and feature-branch handoff.

## Validation
- Enumeration: 23 focused tests and 5 backend module tests passed.
- Lifecycle RED: absent upgrade API; then old no-schema download failed with KeyInvalid after conversion.
- Lifecycle GREEN: 103 attachment-focused tests including distinct active/legacy fallback, retry, collision, pointer drift, and exact-version failures.
- CLI RED: unknown keys subcommand. GREEN: preview, offline apply gate, key enumeration, recovery and continued pinned downloads.
- Final full workspace: 4,064 passed, 47 ignored, zero failures.
- Clippy all features/workspace/targets with -D warnings, fmt/diff checks, and minimal-feature build passed (existing minimal dead-code warnings remain).
- Independent review: no blockers under paused-writers contract; final docs and explicit-legacy regression reviewed.
- No live cloud migration or user-vault mutation performed.
