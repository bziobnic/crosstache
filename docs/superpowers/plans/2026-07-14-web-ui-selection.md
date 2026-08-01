# Web UI Selection and Folder Indentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Indent expanded folder children and add accessible selection mode with visible-only select-all, bulk delete for secrets/files, and bulk secret move.

**Architecture:** Keep the feature inside the embedded vanilla HTML/JS/CSS frontend and reuse existing per-item API routes. Shared selection helpers own checkbox state and bounded bulk execution; table-specific renderers supply identifiers and operations.

**Tech Stack:** Rust embedded-asset tests, vanilla JavaScript, HTML, CSS, existing axum API routes.

## Global Constraints

- No new frontend framework, build step, or dependency.
- `Select all` affects only currently visible rows.
- Bulk file move is out of scope because `FileBackend` has no move operation.
- At most four bulk requests may be in flight.
- Changing vaults or tabs clears selection.

---

### Task 1: Lock the UI contract with failing tests

**Files:**
- Modify: `src/web/mod.rs`

**Interfaces:**
- Consumes: embedded constants `INDEX_HTML`, `APP_JS`, and `STYLE_CSS`.
- Produces: regression tests for selection controls, grouped indentation hooks,
  bounded execution, and bulk endpoint usage.

- [ ] **Step 1: Add failing embedded-asset tests**

Add tests asserting:

```rust
#[test]
fn ui_exposes_selection_controls_for_both_tables() {
    for id in [
        "select-secrets",
        "select-files",
        "select-all-secrets",
        "select-all-files",
        "secret-bulk-bar",
        "file-bulk-bar",
    ] {
        assert!(INDEX_HTML.contains(&format!("id=\"{id}\"")), "{id}");
    }
}

#[test]
fn ui_marks_group_children_for_indentation() {
    assert!(APP_JS.contains("renderRow(it, true)"));
    assert!(APP_JS.contains("tr.classList.add('folder-child')"));
    assert!(APP_JS.contains("td.classList.add('item-name')"));
    assert!(STYLE_CSS.contains(".folder-child .item-name"));
}

#[test]
fn ui_bulk_actions_are_bounded_and_reuse_item_routes() {
    assert!(APP_JS.contains("async function runBounded(items, limit, operation)"));
    assert!(APP_JS.contains("runBounded(items, 4"));
    assert!(APP_JS.contains("api('DELETE', `/api/secrets/"));
    assert!(APP_JS.contains("api('DELETE', `/api/files/"));
    assert!(APP_JS.contains("/move${vaultQS(vault)}`, { folder }"));
}
```

Add companion assertions for visible identifiers driving select-all, the
indeterminate header state, selection reset on vault/tab changes, and the
two-click bulk-delete confirmation labels.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test --features ui web::tests::ui_exposes_selection_controls_for_both_tables
cargo test --features ui web::tests::ui_marks_group_children_for_indentation
cargo test --features ui web::tests::ui_bulk_actions_are_bounded_and_reuse_item_routes
```

Expected: each test fails because the controls and JavaScript/CSS hooks do not
exist.

### Task 2: Add semantic controls and folder indentation

**Files:**
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/app.js`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Produces: `renderGrouped(... renderRow(item, grouped))`; `.folder-child`
  rows; `.item-name` cells; selection toggles, bulk bars, and select-all inputs.

- [ ] **Step 1: Add HTML controls**

Add `Select` buttons to both normal toolbars, hidden checkbox header cells, and
hidden bulk bars. The secret bar contains selected count, destination-folder
input, `Move`, `Delete`, and `Cancel`; the file bar contains selected count,
`Delete`, and `Cancel`.

- [ ] **Step 2: Mark grouped child rows**

Change grouped rendering to call `renderRow(it, true)` for folder members and
`renderRow(it, false)` for loose items. In `secretRow` and `fileRow`, add
`folder-child` to grouped rows and `item-name` to the name cell.

- [ ] **Step 3: Add hierarchy and selection styling**

Style `.folder-child .item-name` with extra inline-start padding and a subtle
guide line. Add compact checkbox-column, bulk-toolbar, pending, and responsive
styles without changing the existing light/dark color variables.

- [ ] **Step 4: Run the first two contract tests and verify GREEN**

Run:

```bash
cargo test --features ui web::tests::ui_exposes_selection_controls_for_both_tables
cargo test --features ui web::tests::ui_marks_group_children_for_indentation
```

Expected: PASS.

### Task 3: Implement selection state and visible-only select-all

**Files:**
- Modify: `src/web/assets/app.js`

**Interfaces:**
- Produces: `secretSelection`, `fileSelection`, mode flags,
  `setSelectionMode(kind, enabled)`, `selectionCell(...)`,
  `syncSelectionUi(...)`, and visible identifier lists.

- [ ] **Step 1: Add per-table selection state**

Use `Set<string>` instances for selected identifiers and booleans for mode.
Central helpers toggle the checkbox columns and bulk bars, update count labels,
and reset confirmation buttons.

- [ ] **Step 2: Render row checkboxes**

In selection mode prepend a checkbox cell with an item-specific `aria-label`.
Stop checkbox click propagation. Secret row clicks toggle selection rather than
opening the drawer; file per-item action buttons are omitted.

- [ ] **Step 3: Synchronize select-all**

Have `renderSecrets` and `renderFiles` compute visible identifiers. Set the
header checkbox's `checked` and `indeterminate` properties from only those
identifiers. Its change handler adds/removes only visible identifiers.

- [ ] **Step 4: Reset and prune state**

Clear both modes and sets on vault and tab changes. After list reloads, remove
selected identifiers absent from the new dataset.

- [ ] **Step 5: Run selection tests**

Run:

```bash
cargo test --features ui web::tests::ui_selection
```

Expected: all selection contract tests PASS.

### Task 4: Implement bounded bulk actions

**Files:**
- Modify: `src/web/assets/app.js`

**Interfaces:**
- Produces: `runBounded(items, limit, operation) -> Promise<Result[]>`,
  bulk delete handlers for both tables, and bulk secret move handler.

- [ ] **Step 1: Implement bounded execution**

Create up to `Math.min(limit, items.length)` workers sharing an index. Each
worker records `{ item, ok, error }` so one rejection never stops remaining
items.

- [ ] **Step 2: Implement bulk secret move**

Validate the destination input, capture vault and generation, POST each selected
name to its existing move route with `{ folder }`, retain failed names, reload
the current vault, and summarize successes/failures.

- [ ] **Step 3: Implement confirmed bulk deletion**

Use `armConfirmation` for `Delete N secrets?` / `Delete N files?`. Disable
selection controls while pending, run at concurrency four, retain failed names,
reload, restore controls, and emit a summary toast.

- [ ] **Step 4: Guard stale UI continuations**

Increment a selection action generation on vault switch, tab switch, or cancel.
Requests may finish against their captured vault, but stale completions must not
modify the current selection UI.

- [ ] **Step 5: Run bulk-action tests**

Run:

```bash
cargo test --features ui web::tests::ui_bulk
```

Expected: all bulk contract tests PASS.

### Task 5: Document and verify the feature

**Files:**
- Modify: `docs/web-ui.md`
- Modify: `TODO.md`

**Interfaces:**
- Produces: user-facing behavior documentation and completed execution record.

- [ ] **Step 1: Update web UI documentation**

Document indented expanded rows, explicit selection mode, visible-only
select-all, bulk delete for both tables, and secret-only bulk move.

- [ ] **Step 2: Run formatting and focused tests**

Run:

```bash
cargo fmt --check
cargo test --features ui web::tests
```

Expected: PASS with no warnings or failures.

- [ ] **Step 3: Run full feature verification**

Run:

```bash
cargo clippy --features ui --all-targets
cargo test --features ui
```

Expected: PASS with no clippy warnings or test failures.

- [ ] **Step 4: Review the final diff**

Confirm no unrelated files, secrets, generated browser data, or dependency
changes are present.

