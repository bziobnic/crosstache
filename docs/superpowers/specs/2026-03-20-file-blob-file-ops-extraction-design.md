# Design: File/blob CLI extraction (`file_ops.rs`)

> **Status:** Implemented (2026-03-20) — `src/cli/file_ops.rs` + thin dispatch in `commands.rs`  
> **Date:** 2026-03-20  
> **Scope:** Mechanical refactor only — no user-visible behavior change  
> **Related:** `dev/FILE-BLOB-REFACTOR-PLAN.md`, `dev/REFACTOR.md`

---

## 1. Goal

Complete **Tasks 3–6** of the file/blob refactor: move all file/blob **execution** out of `src/cli/commands.rs` into `src/cli/file_ops.rs`, leaving `commands.rs` with **strict** dispatch only (plus `use` lines and `#[cfg(feature = "file-ops")]` wiring).

**Exit criterion:** `commands.rs` must not contain file/blob *behavior* — only thin delegation to `file_ops` for `Commands::File` and any top-level file quick-commands (`Upload` / `Download` / etc.), with no remaining `execute_file_*`, `display_file_list_items`, or file-only helper implementations in `commands.rs`.

---

## 2. Non-goals

- Pushing domain logic into `blob/` or changing `BlobManager` APIs (that is a separate refactor).
- Splitting `file_ops.rs` into submodules in the **first** PR (optional follow-up if the single file is unwieldy).
- Changing CLI flags, output text, error messages, or Azure call patterns (byte-for-byte behavior preservation).

---

## 3. Architecture

### 3.1 Modules

| Module | Responsibility |
|--------|----------------|
| `src/cli/file.rs` | Clap types only (`FileCommands`, `SyncDirection`, …) — **already done** |
| `src/cli/file_ops.rs` | All async/sync handlers for file commands: `execute_file_command`, `execute_file_*`, `display_file_list_items`, file sync entrypoints, and helpers used **only** by those paths |
| `src/cli/helpers.rs` | Shared CLI helpers (existing); extend only if a helper is currently private to `commands.rs` but needed by `file_ops` |
| `src/cli/commands.rs` | Dispatch: `FileCommands` → `file_ops::execute_file_command`; quick file commands → one-line `file_ops::…` calls |

### 3.2 Feature gating

- `file_ops` is compiled with **`#[cfg(feature = "file-ops")]`** (module-level or file contents), consistent with `file.rs` and existing `Commands::File` gates.

### 3.3 Dependency direction (no cycles)

```
commands.rs  →  file_ops.rs  →  blob, config, utils, cli::helpers, cli::file (types)
     ↓
   (must not import file_ops in a way that file_ops imports commands)
```

`file_ops` **must not** depend on `commands.rs`. Shared symbols today in `commands.rs` that file code needs move to `helpers.rs` or stay in `file_ops` if file-only.

---

## 4. What moves

Move wholesale (names indicative; discover with `rg` before editing):

- `execute_file_command` and every `execute_file_*` helper.
- `display_file_list_items` and file-list–only formatting.
- `execute_file_sync` and helpers used **exclusively** from file/sync flows.
- Any private function in `commands.rs` that is **only** referenced from the above.

**Verification:** Before merge, search `commands.rs` for `BlobManager`, `execute_file_`, `display_file`, `BlobListItem`, and `FileCommands` — expect matches only on imports, `#[cfg]` blocks, and dispatch lines.

---

## 5. Public API surface (`file_ops`)

Recommended entry points (exact signatures follow existing code):

- `pub(crate) async fn execute_file_command(command: FileCommands, config: Config) -> Result<()>`
- Additional `pub(crate)` functions only if top-level `Commands::Upload` / `Download` (or similar) must call into `file_ops` without wrapping `FileCommands`.

Keep visibility minimal (`pub(crate)`) unless integration tests require otherwise.

---

## 6. Migration sequence

1. Add `file_ops.rs` + `mod file_ops` in `cli/mod.rs`; empty or stub that compiles.
2. Move the dispatch target (`execute_file_command` + callees) in **one cohesive change** — prefer avoiding a long-lived “half in commands, half in file_ops” state.
3. Move leaf helpers before callers if needed for compile order.
4. Run `cargo check --features file-ops`, `cargo test --features file-ops` (and `cargo test --lib` as usual).
5. Update `dev/FILE-BLOB-REFACTOR-PLAN.md` status table; optional one-line note in `REFACTOR.md`.

---

## 7. Testing

| Check | Command / action |
|-------|------------------|
| Default feature | `cargo test --features file-ops` (default includes `file-ops` in this crate) |
| Existing | `tests/file_commands_tests.rs` (struct wiring) |
| Smoke | `tests/cli_integration_tests.rs` — `file --help` |
| Manual (optional) | With Azure: `xv file list`, `xv file sync --dry-run` |

No new tests are **required** for merge if behavior is move-only; optional: narrow integration test for `file` subcommand parse path.

---

## 8. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| Large diff | Single focused PR; review as “move-only”; avoid drive-by edits |
| Missed symbol left in `commands.rs` | Pre-merge `rg` checklist (section 4) |
| Circular dependency | Never have `file_ops` import `commands`; lift shared bits to `helpers` |
| Oversized `file_ops.rs` | Acceptable for v1; split into `file_ops/*.rs` in a **second** PR if >~1.5k lines |

---

## 9. Follow-up (optional, not part of this spec)

- Split `file_ops.rs` into submodules by concern (upload / list / sync / delete).
- Deduplicate any helpers lifted to `helpers.rs` for other domains later.

---

## 10. Approval

Design approved by project owner on 2026-03-20 (strict exit criterion, Approach A: single `file_ops.rs` first).

**Next step:** Use the **writing-plans** skill to produce a step-by-step implementation plan with file-level checkpoints, then implement.
