# File/Blob CLI Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move file/blob command definitions and execution logic out of `src/cli/commands.rs` into focused `src/cli/file.rs` and `src/cli/file_ops.rs` modules without changing user-visible behavior.

**Architecture:** Keep CLI behavior and signatures stable while introducing a new internal module boundary for file/blob concerns. Do this in small, compiling passes: first extract definitions, then dispatch wiring, then execution helpers, then tests/cleanup.

**Tech Stack:** Rust, clap derive macros, tokio async, existing `BlobManager` APIs, existing `cargo test` and `cargo check` workflow.

---

## Status (2026-03-20)

| Item | State |
|------|--------|
| `src/cli/file.rs` with `FileCommands` + clap args | **Done** — wired from `commands.rs` under `#[cfg(feature = "file-ops")]` |
| `src/cli/helpers.rs` shared helpers | **Done** (exists independently of this plan) |
| `src/cli/file_ops.rs` + moved `execute_file_*` | **Done** — upload/list/delete/info/sync, quick upload/download, `display_file_list_items`, cache `refresh_file_list` |
| Integration tests import `cli::file::{FileCommands, …}` | **Done** (`tests/file_commands_tests.rs`) |
| Exit criterion: no file/blob ops in `commands.rs` | **Done** — only `crate::cli::file_ops::…` dispatch + `FileCommands` import for clap |

---


## Planned file structure

### New files

- `src/cli/file.rs` — `FileCommands` enum and file-related command argument structs.
- `src/cli/file_ops.rs` — `execute_file_command` and all `execute_file_*` helpers.

### Modified files

- `src/cli/mod.rs` — export new modules and stable re-exports.
- `src/cli/commands.rs` — remove moved file/blob code and import from new modules.
- `src/utils/resource_detector.rs` — switch `ResourceType` import to stable CLI re-export (avoid `commands` path coupling).
- `tests/*` (only if compile or path fallout appears) — update imports if any directly target `cli::commands`.

### Optional follow-up files

- `src/cli/helpers.rs` — if circular dependencies appear for shared helper utilities.

---

## Task 1: Establish module skeleton (no behavior change)

**Files:**

- Create: `src/cli/file.rs`
- Create: `src/cli/file_ops.rs`
- Modify: `src/cli/mod.rs`
- **Step 1: Create `src/cli/file.rs` with initial `FileCommands` and related structs**
  - Copy only definitions for `FileCommands` and its immediate clap argument types.
  - Keep derives and attributes identical.
- **Step 2: Create `src/cli/file_ops.rs` with placeholder public API**
  - Add function signatures for:
    - `execute_file_command`
    - `execute_file_upload_quick`
    - `execute_file_download_quick`
    - `execute_file_info_from_root`
  - Return explicit `todo!()` placeholders temporarily.
- **Step 3: Wire modules in `src/cli/mod.rs`**
  - Add `pub mod file;`
  - Add `pub mod file_ops;`
  - Re-export needed items (`pub use file::*;` and selected `file_ops` functions if needed).
- **Step 4: Run compile check**
  - Run: `cargo check`
  - Expected: may fail due to placeholders; no module/import errors.
- **Step 5: Commit skeleton**
  - Commit message: `refactor(cli): add file/blob module skeleton`

---

## Task 2: Migrate file command definitions

**Files:**

- Modify: `src/cli/commands.rs`
- Modify: `src/cli/file.rs`
- Modify: `src/cli/mod.rs`
- **Step 1: Move `FileCommands` definition block fully into `src/cli/file.rs`**
  - Include all feature-gated variants exactly as-is.
- **Step 2: Update `Commands::File` usage in `commands.rs`**
  - Import `FileCommands` from `crate::cli::file` (or from `crate::cli` re-export).
- **Step 3: Remove duplicate definitions from `commands.rs`**
  - Ensure no duplicate type definitions remain.
- **Step 4: Run compile check**
  - Run: `cargo check`
  - Expected: pass for definitions wiring; runtime behavior unchanged.
- **Step 5: Commit definitions extraction**
  - Commit message: `refactor(cli): extract FileCommands definitions`

---

## Task 3: Migrate dispatch entry points

**Files:**

- Modify: `src/cli/commands.rs`
- Modify: `src/cli/file_ops.rs`
- **Step 1: Move these functions from `commands.rs` to `file_ops.rs`**
  - `execute_file_command`
  - `execute_file_upload_quick`
  - `execute_file_download_quick`
  - `execute_file_info_from_root`
- **Step 2: Keep public signatures stable**
  - Do not change args/return types unless required for visibility.
  - Keep output/error messages byte-for-byte where possible.
- **Step 3: Replace old implementations with imports/calls**
  - In `commands.rs`, call into `file_ops` implementations.
- **Step 4: Verify command dispatch still compiles**
  - Run: `cargo check`
  - Run: `cargo test --lib`
  - Expected: no regressions in non-integration tests.
- **Step 5: Commit dispatch extraction**
  - Commit message: `refactor(cli): route file command dispatch through file_ops`

---

## Task 4: Migrate all file/blob helper operations

**Files:**

- Modify: `src/cli/commands.rs`
- Modify: `src/cli/file_ops.rs`
- Optional: `src/cli/helpers.rs`
- **Step 1: Move all `execute_file_*` helper functions**
  - Include upload/download/list/delete/info, recursive/multiple variants, sync helpers, and `execute_file_sync`.
- **Step 2: Co-locate local helper routines used only by file/blob path**
  - Keep private helper functions private to `file_ops.rs`.
- **Step 3: Resolve shared utility dependencies**
  - If `file` path needs helpers from `commands.rs`, either:
    - move helper to `helpers.rs`, or
    - pass dependencies as parameters.
  - Avoid broad `pub` visibility as a shortcut.
- **Step 4: Remove dead file/blob code from `commands.rs`**
  - Keep only non-file commands there.
- **Step 5: Run robust verification**
  - Run: `cargo check --all-targets`
  - Run: `cargo test --lib -- --test-threads=1`
  - Run: `cargo test --test file_commands_tests -- --nocapture` (if present and configured)
  - Expected: pass with no behavior change.
- **Step 6: Commit helper migration**
  - Commit message: `refactor(cli): move file/blob operation handlers to file_ops`

---

## Task 5: Stabilize external imports and API surface

**Files:**

- Modify: `src/utils/resource_detector.rs`
- Modify: `src/cli/mod.rs`
- Optional: other files from `rg "cli::commands::"` results
- **Step 1: Remove direct dependency on `cli::commands::ResourceType`**
  - Update imports to `crate::cli::ResourceType` via stable re-export.
- **Step 2: Ensure `cli::mod` is the only intended external boundary**
  - Keep `commands.rs` internals hidden where possible.
- **Step 3: Run compile + targeted tests**
  - Run: `cargo check`
  - Run: `cargo test --lib`
- **Step 4: Commit API stabilization**
  - Commit message: `refactor(cli): stabilize cli re-exports after file/blob split`

---

## Task 6: Regression test matrix and cleanup

**Files:**

- Modify: `src/cli/file_ops.rs` (if needed)
- Modify: `tests/file_commands_tests.rs` (if needed)
- Modify: `src/cli/commands.rs` (cleanup)
- **Step 1: Smoke test key file/blob flows manually**
  - `xv file list`
  - `xv file info <name>`
  - `xv file upload ...`
  - `xv file download ...`
  - `xv file sync --dry-run`
  - Note: run only in environment with Azure creds configured.
- **Step 2: Add/adjust tests only for changed module boundaries**
  - Prefer minimal tests proving dispatch and output consistency.
- **Step 3: Ensure no file/blob symbols remain in `commands.rs` except imports**
  - Confirm with search: `rg "execute_file_|FileCommands" src/cli/commands.rs`
- **Step 4: Final quality gate**
  - Run: `cargo fmt`
  - Run: `cargo clippy --all-targets`
  - Run: `cargo test`
- **Step 5: Commit cleanup**
  - Commit message: `refactor(cli): finalize file/blob extraction from commands.rs`

---

## Risks and mitigations

- **Risk:** circular dependencies between `commands.rs` and `file_ops.rs`.
  - **Mitigation:** move shared helpers to `helpers.rs` early; keep one-way dependency from `commands` -> `file_ops`.
- **Risk:** accidental behavior drift in error text/output formatting.
  - **Mitigation:** copy code first, refactor second; compare outputs for representative commands.
- **Risk:** feature-gate regressions under `file-ops`.
  - **Mitigation:** preserve `#[cfg(feature = "file-ops")]` boundaries and run checks with/without feature where possible.
- **Risk:** oversized PR becomes hard to review.
  - **Mitigation:** keep each task as a separate commit (or PR if preferred).

---

## Exit criteria

- `src/cli/commands.rs` no longer contains file/blob operation implementations.
- `FileCommands` and file execution logic live in dedicated modules.
- Public CLI behavior is unchanged (commands, flags, output/error semantics).
- `cargo check`, `cargo clippy --all-targets`, and `cargo test` pass.
- At least one focused test path validates file/blob dispatch still works.

