# Attachment status and inventory implementation plan

> **Status:** ✅ Shipped in **v0.39.0** (PR #429). Operator guide:
> [`docs/attachments.md`](../../attachments.md#inspecting-key-status-and-file-references).

> **For agentic workers:** Use superpowers:subagent-driven-development for the domain task and review; CLI integration runs locally.

**Goal:** Add read-only CLI observations of the current attachment key and file metadata references.
**Architecture:** A typed, serializable report module consumes the existing custody and file interfaces. CLI resolves backend/vault with the existing current-vault resolver, then renders JSON/YAML or escaped human output.
**Tech Stack:** Rust, async traits, serde, clap, Local/AWS/Azure backends.
**Spec:** Contract below; ROADMAP.md's original 2026-09-03 lifecycle design file is not available locally.

## Contract and global constraints

- User approved continuing with status/inventory after PR #428.
- Commands: `xv attachment-key status` and `xv attachment-key inventory`.
- Status works without file storage. Never initialize, set, commit, delete, or rewrite custody records. Read the canonical pointer; report absent, v1, v2, or invalid. Validate identity-derived IDs using the existing custody interface. Domain problems appear as safe stable error codes in the report; provider permission/auth/transport failures propagate.
- Inventory is FILE REFERENCE inventory, not an enumeration of unreferenced retained records. List all files with no limit and request each file's metadata through get_file_info (S3 list omits user metadata). Never download/decrypt blobs or read key material for inventory.
- Report metadata classification as schema1, legacy_unversioned, invalid_reference, or unmanaged, following classify_download's ownership and schema rules assuming ciphertext only for classification. Do not claim ciphertext validity, decryptability, or retirement safety. Include unmanaged files to make scan scope explicit.
- Structured reports have schema_version=1. Status fields: mode, active_key_id, active_version, legacy_key_id, problem_code (optional fields serialized as null). Inventory fields: observation="metadata_only", files array, each with name, classification, key_id/slot/provider_version (only validated schema1 references; otherwise null).
- Sort file entries by name. Abort inventory on any list/info provider error; never emit a partial-success report. Entire scan is not atomic and can race uploads.
- Do not expose raw identities, arbitrary metadata, tags, or provider error bodies in reports.
- CLI envelope includes backend (registry name), vault, and report. Human output escapes controls; JSON/YAML machine output stays parseable. CSV rejected explicitly for nested reports.
- No new provider mutation requests, dependencies, Web UI, rotation, or retirement. Existing local read-time journal/file recovery remains active; this is not a zero-filesystem-write forensic mode.

## Task 1: Domain reports
Files: create src/secret/attachment_inventory.rs; add cfg(file-ops) module in src/secret/mod.rs.
Interfaces:
```rust
pub async fn key_status(keys: &dyn AttachmentKeyStore, vault: &str) -> Result<KeyStatus>;
pub async fn file_inventory(files: &dyn FileBackend, vault: &str) -> Result<FileInventory>;
```
Both report types derive Serialize and Debug and carry only safe public fields.

- [x] Write tests using real LocalBackend and minimal fake backends that count/refuse mutation/download. Demonstrate empty status does not create a pointer, valid v1/v2 status returns derived IDs, malformed/missing/mismatched keys return safe codes, provider errors propagate.
- [x] Run tests before implementation to observe failure, then implement read-only methods.
- [x] Verify metadata-only enumeration refreshes per-object metadata (list result can be empty metadata), includes ordinary and encrypted files, distinguishes malformed references/unknown schema, aborts errors, and never requests blob bytes.
- [x] Run focused tests, self-review, report changed files and test evidence.

## Task 2: CLI, end-to-end behavior, docs
Files: src/cli/attachment_key_ops.rs, src/cli/commands.rs, src/cli/mod.rs, tests/e2e_local_file_ops.rs, docs/attachments.md, ROADMAP.md, CHANGELOG.md.
- [x] Add command parsing regressions and real CLI test: status empty -> file encrypted upload -> v2 status and inventory with key/version match; inventory unmanaged alongside managed; JSON/YAML parse; malformed pointer reports code without leaking value; no key writes during observations.
- [x] Run CLI regression RED (unknown attachment-key command).
- [x] Wire subcommands to resolve_current_vault(config,None), keeping registry name/vault together, including workspace defaults. Status needs no files; inventory gates capability.
- [x] Format report envelopes and escape control chars for human output. Reject CSV before reads.
- [x] Document observation limits and cloud per-file metadata request cost; update roadmap to mark this slice while preserving remaining lifecycle tasks.

## Task 3: Review and validation
- [x] Independent domain and full diff review, resolve findings.
- [x] Run focused and full workspace tests, Clippy all targets with -D warnings, fmt/diff checks, no-default-features check.
- [x] Prepare the validated commit and sync with origin/main for feature-branch handoff.

## Validation and review

- CLI RED: unrecognized attachment-key subcommand on merged baseline.
- Domain RED: report types missing; GREEN: 9 domain tests.
- CLI GREEN: 3 end-to-end tests; full run also covers explicit alias routing.
- Full all-features workspace: 4,039 passed, 47 ignored, zero failures.
- Clippy all targets with warnings denied, formatting/diff checks, and no-default-features build passed. Minimal build retains existing dead-code warnings.
- Independent review cleared domain and CLI changes. Review corrections documented Local read-time recovery and cloud best-effort tag calls. Explicit --vault alias/literal routing received scoped re-review.
- No live cloud accounts were exercised. No Web code changed.
