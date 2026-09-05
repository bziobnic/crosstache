# Attachment custody boundary implementation plan

> Execute inline using the executing-plans workflow. The user approved this continuation on 2026-09-05.

**Goal:** Make generic registry secret access guarded and preserve authorized attachment access through a narrow, audited key-store interface.

**Architecture:** Every registry construction path exposes a guarded backend. Its generic secret facade rejects reserved mutations, while a separate attachment key store provides only reads, exact-version reads, and writes of canonical custody records. The agent wrapper implements that interface with the existing policy evaluation, disclosure checks, and decision logging before provider access.

**Tech stack:** Rust, async-trait, Tokio, existing backend registry and local test stores.

**Design basis:** Existing `feat/attachment-key-p0` integrity foundation and its ROADMAP PR 1 boundary requirements. The referenced September 3 lifecycle design is not in the checked-out repository; this continuation is limited to the boundary requirements present in source and ROADMAP.

## Constraints

- No raw secret handle is returned from the attachment key store.
- Preserve default and named-backend routing and agent enforcement.
- Never log or serialize private keys in decision records.
- Reserved alias mutations fail before provider I/O; opaque backup restore remains refused.
- Provider commit protocols, download snapshots, key lifecycle commands, and local crash recovery remain separate chunks.

## Tasks

- [x] Prove registry bypass with `registry_blocks_reserved_mutations_on_every_resolution_path`; run `cargo test --lib registry_blocks_reserved_mutations_on_every_resolution_path --features ui` and confirm the unguarded local provider returns NotFound instead of PermissionDenied.
- [x] Add the owned guarded backend in `src/backend/guard.rs`; wrap eager/new, lazy, and cross-backend factory results in `src/backend/registry.rs`. Keep the existing borrowed facade for direct callers.
- [x] Add `AttachmentKeyStore` and its restricted raw adapter in `src/backend/attachment_keys.rs`, plus `Backend::attachment_keys`. Test that ordinary names cannot be read/written through it and that legitimate custody data survives a local upload/download.
- [x] Implement policy-aware custody access in `src/agent/enforce.rs`. Test denied reads/writes, raw-disclosure refusal, successful authorized round trips, and redacted decision records.
- [x] Route attachment CLI, file operations, Web uploads/downloads and archives through the narrow interface. Remove the unused legacy key-creation helper; preserve legacy attachment reads.
- [x] Exercise generic CRUD, alias protection, Web reserved moves, imports and migration, and named backends. Resolve error mapping or test fixtures that depended on raw registry access.
- [x] Run formatting, Rust default/all-feature checks and relevant tests, plus Web unit tests. Review the diff, update ROADMAP/operator docs, commit, pull --rebase, and push the branch per AGENTS.md.

## Validation evidence

- Registry regression failed against the original branch with provider NotFound instead of guard PermissionDenied, then passed with the registry boundary.
- Guarded attachment round trip initially failed at reserved-key mutation, then passed through the separate custody path.
- Web reserved folder move regression failed with HTTP 200 before the guard and passed with HTTP 400 afterward.
- `cargo test --all-features --workspace`: 3,946 passed, 47 ignored; no failures.
- `npm run test:unit`: 270 passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo check --no-default-features`: passed; existing dead-code warnings remain in the feature-disabled binary.
- `cargo fmt --check` and `git diff --check`: passed.
- Independent read-only review found no actionable regressions in registry routing, custody delegation, policy checks, or caller wiring.

The entire attachment integrity milestone is not complete: provider commit protocols, coherent file snapshots, and local crash recovery remain in ROADMAP.
