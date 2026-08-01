# Tauri Web UI Table and Secret Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix secret dates and protected-value masking, add persistent resizable and sortable tables, and simplify file download/delete behavior in the embedded Tauri web UI.

**Architecture:** Retain semantic HTML tables and the dependency-free frontend. Add a small UMD-style pure JavaScript model for date, protected-state, sorting, and width validation, tested with Node's built-in runner. `app.js` owns DOM behavior; it sorts rows before the existing folder-group renderer and keeps authenticated fetch downloads.

**Tech Stack:** Rust/Axum embedded assets, vanilla HTML/CSS/JavaScript, Node `node:test`, Cargo, Clippy, Tauri 2.

## Global Constraints

- Existing protected values display exactly `***************` until revealed.
- The mask covers ordinary secrets and every protected typed-record field; new values start blank and editable.
- Folder groups stay intact and alphabetical while rows inside each group follow the active sort.
- Widths persist per table; sort state lasts only for the current session.
- Add no frontend framework, package manager, or data-grid dependency.
- File links reuse the authenticated API fetch path.
- File deletion exists only in the top selection toolbar beside `Cancel`.
- Preserve bulk confirmation, bounded concurrency, and partial-failure reporting.
- Follow `AGENTS.md`: non-interactive commands, verification, `git pull --rebase`, and `git push`.

## File Map

- Create `src/web/assets/ui-model.js`: pure UI data/model helpers.
- Create `src/web/assets/ui-model.test.js`: dependency-free behavior tests.
- Modify `src/web/mod.rs`: embed/serve the model and update UI contracts.
- Modify `src/web/assets/index.html`: load the model; add colgroups, sort headers, and resizers; remove file actions.
- Modify `src/web/assets/app.js`: integrate masking, sorting, resizing, persistence, links, and selection-only deletion.
- Modify `src/web/assets/style.css`: style the new controls and revise responsive table rules.

---

### Task 1: Pure UI Model and Executable Tests

**Files:**
- Create: `src/web/assets/ui-model.js`
- Create: `src/web/assets/ui-model.test.js`
- Modify: `src/web/mod.rs:20-125`
- Modify: `src/web/assets/index.html:155-159`

**Interfaces:**
- Produces browser global `XvUiModel` and Node `module.exports`.
- Produces `PROTECTED_MASK`, `formatDate`, `expirationDate`, protected-state helpers, `sortedCopy`, and `normalizeWidths`.

- [ ] **Step 1: Write failing model tests**

Create `src/web/assets/ui-model.test.js`:

```javascript
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const model = require('./ui-model.js');

test('dates are date-only and absent expiration is blank', () => {
  assert.equal(model.formatDate('2026-07-15T23:45:00Z'), '2026-07-15');
  assert.equal(model.formatDate('Unknown'), 'Unknown');
  assert.equal(model.expirationDate(null), '');
  assert.equal(model.expirationDate('2027-02-03T00:00:00Z'), '2027-02-03');
});

test('all stored protected values use the same mask', () => {
  const short = model.createProtectedState('a', true);
  const long = model.createProtectedState('a much longer secret', true);
  assert.equal(model.protectedDisplay(short), '***************');
  assert.equal(model.protectedDisplay(long), '***************');
  model.revealProtected(short);
  assert.equal(model.protectedDisplay(short), 'a');
  model.editProtected(short, 'changed');
  model.hideProtected(short);
  assert.equal(model.protectedDisplay(short), '***************');
  assert.equal(short.value, 'changed');
  assert.equal(short.dirty, true);
});

test('numeric and date sorts use name tie breaking and empty-last order', () => {
  const items = [
    { name: 'beta', size: 5, updated: '2025-01-02T00:00:00Z' },
    { name: 'Alpha', size: 10, updated: '' },
    { name: 'charlie', size: 5, updated: '2025-01-01T00:00:00Z' },
  ];
  assert.deepEqual(model.sortedCopy(items, x => x.size, x => x.name, 'number', 'asc').map(x => x.name), ['beta', 'charlie', 'Alpha']);
  assert.deepEqual(model.sortedCopy(items, x => x.updated, x => x.name, 'date', 'asc').map(x => x.name), ['charlie', 'beta', 'Alpha']);
});

test('saved widths must match shape, total, and minimums', () => {
  const defaults = [28, 15, 14, 25, 18];
  const minimums = [14, 10, 10, 14, 12];
  assert.deepEqual(model.normalizeWidths('[30,15,15,22,18]', defaults, minimums), [30, 15, 15, 22, 18]);
  assert.deepEqual(model.normalizeWidths('bad', defaults, minimums), defaults);
  assert.deepEqual(model.normalizeWidths('[5,20,20,35,20]', defaults, minimums), defaults);
  assert.deepEqual(model.normalizeWidths('[28,15,14,25]', defaults, minimums), defaults);
});
```

- [ ] **Step 2: Verify RED**

Run `node --test src/web/assets/ui-model.test.js`.
Expected: FAIL because `ui-model.js` does not exist.

- [ ] **Step 3: Implement `ui-model.js`**

```javascript
'use strict';
(function expose(root, factory) {
  const model = factory();
  if (typeof module === 'object' && module.exports) module.exports = model;
  else root.XvUiModel = model;
}(typeof globalThis === 'undefined' ? this : globalThis, () => {
  const PROTECTED_MASK = '***************';
  const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });

  function formatDate(value) {
    if (!value) return '';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? String(value) : date.toISOString().slice(0, 10);
  }
  function expirationDate(value) {
    return typeof value === 'string' && value.length >= 10 ? value.slice(0, 10) : '';
  }
  function createProtectedState(value = null, hasStoredValue = value !== null) {
    return { value, hasStoredValue, masked: hasStoredValue, dirty: false };
  }
  function protectedDisplay(state) { return state.masked ? PROTECTED_MASK : (state.value ?? ''); }
  function revealProtected(state, loaded = state.value) {
    state.value = loaded ?? ''; state.hasStoredValue = true; state.masked = false; return state;
  }
  function editProtected(state, value) {
    state.value = value; state.hasStoredValue = true; state.dirty = true; return state;
  }
  function hideProtected(state) { if (state.hasStoredValue) state.masked = true; return state; }

  function comparable(value, type) {
    if (type === 'number') return typeof value === 'number' && Number.isFinite(value) ? value : null;
    if (type === 'date') {
      if (!value) return null;
      const timestamp = new Date(value).getTime();
      return Number.isNaN(timestamp) ? null : timestamp;
    }
    return value === null || value === undefined || value === '' ? null : String(value);
  }
  function compareValues(left, right, type) {
    const a = comparable(left, type); const b = comparable(right, type);
    if (a === null && b === null) return 0;
    if (a === null) return 1;
    if (b === null) return -1;
    if (type === 'text') return collator.compare(a, b);
    return a === b ? 0 : (a < b ? -1 : 1);
  }
  function sortedCopy(items, valueOf, nameOf, type = 'text', direction = 'asc') {
    const multiplier = direction === 'desc' ? -1 : 1;
    return [...items].sort((left, right) => {
      const primary = compareValues(valueOf(left), valueOf(right), type);
      return primary ? primary * multiplier : collator.compare(String(nameOf(left)), String(nameOf(right)));
    });
  }
  function normalizeWidths(serialized, defaults, minimums) {
    let widths;
    try { widths = JSON.parse(serialized); } catch (_) { return [...defaults]; }
    const valid = Array.isArray(widths) && widths.length === defaults.length
      && widths.every((width, i) => Number.isFinite(width) && width >= minimums[i])
      && Math.abs(widths.reduce((sum, width) => sum + width, 0) - 100) < 0.1;
    return valid ? widths : [...defaults];
  }
  return { PROTECTED_MASK, formatDate, expirationDate, createProtectedState,
    protectedDisplay, revealProtected, editProtected, hideProtected, sortedCopy, normalizeWidths };
}));
```

- [ ] **Step 4: Verify GREEN**

Run `node --test src/web/assets/ui-model.test.js`.
Expected: 4 pass, 0 fail.

- [ ] **Step 5: Test and serve the asset**

First add `("/ui-model.js", "application/javascript")` to
`serves_index_and_assets`; run that exact Cargo test and observe 404. Then add:

```rust
const UI_MODEL_JS: &str = include_str!("assets/ui-model.js");
```

and a `/ui-model.js` route matching `/app.js`. Add before `/app.js` in HTML:

```html
<script src="/ui-model.js"></script>
```

Rerun the Cargo test and expect PASS.

- [ ] **Step 6: Commit**

```bash
git add src/web/assets/ui-model.js src/web/assets/ui-model.test.js src/web/assets/index.html src/web/mod.rs
git commit -m "test(ui): add deterministic UI model helpers"
```

---

### Task 2: Expiration and Protected Reveal/Hide

**Files:**
- Modify: `src/web/assets/app.js:122-128,392-490,742-934`
- Modify: `src/web/mod.rs:335-445`

**Interfaces:**
- Consumes all protected/date helpers from Task 1.
- Produces `plainSecretState`, `renderProtectedControl`, `setRevealLabel`, and `loadPlainSecretValue`.
- Preserves current stale-drawer guards and PUT/PATCH semantics.

- [ ] **Step 1: Add failing contracts**

```rust
#[test]
fn ui_masks_every_existing_protected_value_with_fixed_length() {
    assert!(UI_MODEL_JS.contains("const PROTECTED_MASK = '***************';"));
    assert!(APP_JS.contains("XvUiModel.createProtectedState(value, value !== undefined)"));
    assert!(APP_JS.contains("input.readOnly = state.masked;"));
    assert!(APP_JS.contains("input.value = XvUiModel.protectedDisplay(state);"));
}

#[test]
fn ui_plain_secret_toggles_and_never_submits_the_mask() {
    assert!(APP_JS.contains("let plainSecretState = null;"));
    assert!(APP_JS.contains("async function loadPlainSecretValue(generation, selection)"));
    assert!(APP_JS.contains("else if (plainSecretState?.dirty"));
    assert!(!APP_JS.contains("value: XvUiModel.PROTECTED_MASK"));
}

#[test]
fn ui_expiration_is_blank_when_absent_and_updated_is_date_only() {
    assert!(APP_JS.contains("f.elements.expires_on.value = '';"));
    assert!(APP_JS.contains("XvUiModel.expirationDate(meta.expires_on)"));
    assert!(APP_JS.contains("XvUiModel.formatDate(s.updated_on)"));
}
```

- [ ] **Step 2: Verify RED**

Run each exact test with `cargo test --features ui <name> -- --exact`.
Expected: FAIL for missing model integration.

- [ ] **Step 3: Add protected-control state**

Delete local `fmtDate`; replace all calls with `XvUiModel.formatDate`. Near
record state add:

```javascript
let plainSecretState = null;
function setRevealLabel(button, label) {
  if (button.id === 'reveal') button.replaceChildren(icon('eye'), label);
  else button.textContent = label;
}
function renderProtectedControl(input, button, state) {
  input.type = 'text';
  input.readOnly = state.masked;
  input.value = XvUiModel.protectedDisplay(state);
  setRevealLabel(button, state.masked ? 'Reveal' : 'Hide');
}
```

Replace the secret-kind input block with:

```javascript
if (kind === 'secret') {
  const state = XvUiModel.createProtectedState(value, value !== undefined);
  input._protectedState = state;
  input.autocomplete = 'new-password';
  const row = document.createElement('span');
  row.className = 'field-actions';
  const rev = document.createElement('button');
  rev.type = 'button';
  rev.className = 'button secondary';
  renderProtectedControl(input, rev, state);
  rev.onclick = () => {
    if (state.masked) XvUiModel.revealProtected(state);
    else XvUiModel.hideProtected(state);
    renderProtectedControl(input, rev, state);
  };
  input.oninput = () => XvUiModel.editProtected(state, input.value);
  const cp = document.createElement('button');
  cp.type = 'button';
  cp.className = 'button secondary';
  cp.textContent = 'Copy';
  cp.onclick = async () => {
    try { await navigator.clipboard.writeText(state.value ?? ''); toast('copied'); }
    catch (e) { fail(e); }
  };
  row.append(rev, cp);
  label.append(input, row);
} else {
  input.value = value || '';
  label.append(input);
}
```

When saving record fields, use:

```javascript
const value = input.dataset.fieldKind === 'secret' ? input._protectedState.value : input.value;
if (!value) continue;
if (input.dataset.fieldKind === 'secret') envelope[input.dataset.fieldName] = value;
else fieldTags[FIELD_TAG_PREFIX + input.dataset.fieldName] = value;
```

- [ ] **Step 4: Add lazy ordinary-secret value state**

```javascript
async function loadPlainSecretValue(generation, selection) {
  if (plainSecretState.value !== null) return plainSecretState.value;
  const { value } = await api('POST', `/api/secrets/${encodeURIComponent(selection)}/value${vaultQS(currentVault)}`);
  if (!isCurrentDrawer(generation, selection)) return null;
  plainSecretState.value = value ?? '';
  return plainSecretState.value;
}
```

Reset it in `clearDrawerState`. Existing ordinary secrets start with
`createProtectedState(null, true)` and the fixed mask; new secrets start with
`createProtectedState('', false)` and a blank editable control. Reveal lazily
loads then shows the value; Hide stores edits then restores the mask; Copy
loads/copies the actual value. Input marks state dirty.

- [ ] **Step 5: Fix expiration and save semantics**

Immediately after `f.reset()` set `f.elements.expires_on.value = ''`. After
metadata loads set:

```javascript
f.elements.expires_on.value = XvUiModel.expirationDate(meta.expires_on);
```

Use this branch before the existing metadata-only PATCH branch:

```javascript
} else if (plainSecretState?.dirty || (!selection && f.value.value)) {
  const value = selection ? plainSecretState.value : f.value.value;
  await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS(currentVault)}`, {
    value,
    folder: f.folder.value || null,
    note: f.note.value || null,
    groups: groups.length ? groups : null,
    expires_on: expiresPut,
    content_type: editingMeta?.content_type || null,
    tags: editingMeta && Object.keys(editingMeta.tags).length ? editingMeta.tags : null,
    enabled: editingMeta ? editingMeta.enabled : true,
    not_before: editingMeta?.not_before || null,
  });
```

Existing unchanged secrets then take metadata-only PATCH. This makes
submitting the fixed mask impossible.

- [ ] **Step 6: Verify and commit**

Run `node --test src/web/assets/ui-model.test.js` and
`cargo test --features ui web::tests::ui_ --lib`. Expect all pass.

```bash
git add src/web/assets/app.js src/web/mod.rs
git commit -m "fix(ui): conceal protected values without length leaks"
```

---

### Task 3: Folder-Preserving Sortable Headers

**Files:**
- Modify: `src/web/assets/index.html:59-98`
- Modify: `src/web/assets/app.js:620-735,1000-1085,1310-1318`
- Modify: `src/web/assets/style.css:106-130`
- Modify: `src/web/mod.rs:535-725`

**Interfaces:**
- Consumes `XvUiModel.sortedCopy`.
- Produces `tableSort`, `SORT_COLUMNS`, `sortedTableItems`, `setSort`, and `syncSortHeaders`.

- [ ] **Step 1: Add failing contracts**

```rust
#[test]
fn ui_exposes_accessible_sortable_headers() {
    for key in ["name", "folder", "groups", "note", "updated", "size", "type", "modified"] {
        assert!(INDEX_HTML.contains(&format!("data-sort-key=\"{key}\"")), "missing {key}");
    }
    assert!(APP_JS.contains("function syncSortHeaders(kind)"));
    assert!(APP_JS.contains("header.setAttribute('aria-sort'"));
}

#[test]
fn ui_sorts_before_grouping() {
    assert!(APP_JS.contains("const sorted = sortedTableItems('secrets', visible);"));
    assert!(APP_JS.contains("const sorted = sortedTableItems('files', files);"));
    assert!(APP_JS.contains("for (const name of [...groups.keys()].sort())"));
}
```

- [ ] **Step 2: Verify RED**

Run each exact test; expect FAIL.

- [ ] **Step 3: Add header markup**

Use this structure for each data header, setting correct keys/labels and
`aria-sort="ascending"` only on initial Name headers (`none` elsewhere):

```html
<th class="column-secret-name sortable" data-sort-key="name" aria-sort="ascending"><button class="sort-button" type="button">Name<span class="sort-indicator" aria-hidden="true"></span></button></th>
```

- [ ] **Step 4: Implement sorting**

Add:

```javascript
const tableSort = {
  secrets: { key: 'name', direction: 'asc' },
  files: { key: 'name', direction: 'asc' },
};
const SORT_COLUMNS = {
  secrets: {
    name: [(item) => item.original_name || item.name, 'text'],
    folder: [(item) => item.folder || '', 'text'],
    groups: [(item) => item.groups || '', 'text'],
    note: [(item) => item.note || '', 'text'],
    updated: [(item) => item.updated_on || '', 'date'],
  },
  files: {
    name: [(item) => item.name, 'text'],
    size: [(item) => item.size, 'number'],
    type: [(item) => item.content_type || '', 'text'],
    modified: [(item) => item.last_modified || '', 'date'],
  },
};
function sortedTableItems(kind, items) {
  const state = tableSort[kind];
  const [valueOf, type] = SORT_COLUMNS[kind][state.key];
  const nameOf = kind === 'secrets'
    ? (item) => item.original_name || item.name
    : (item) => item.name;
  return XvUiModel.sortedCopy(items, valueOf, nameOf, type, state.direction);
}
function syncSortHeaders(kind) {
  const state = tableSort[kind];
  for (const header of document.querySelectorAll(`#${kind}-table th[data-sort-key]`)) {
    const active = header.dataset.sortKey === state.key;
    header.setAttribute('aria-sort', active ? (state.direction === 'asc' ? 'ascending' : 'descending') : 'none');
    header.querySelector('.sort-indicator').textContent = active ? (state.direction === 'asc' ? '▲' : '▼') : '';
  }
}
function setSort(kind, key) {
  const state = tableSort[kind];
  if (state.key === key) state.direction = state.direction === 'asc' ? 'desc' : 'asc';
  else { state.key = key; state.direction = 'asc'; }
  syncSortHeaders(kind);
  renderSelectionKind(kind);
}
function initSorting() {
  for (const kind of ['secrets', 'files']) {
    for (const header of document.querySelectorAll(`#${kind}-table th[data-sort-key]`)) {
      header.querySelector('.sort-button').onclick = () => setSort(kind, header.dataset.sortKey);
    }
    syncSortHeaders(kind);
  }
}
```

In `renderSecrets`, assign `const sorted = sortedTableItems('secrets', visible)`
and pass `sorted` to `renderGrouped`. In `renderFiles`, do the same with
`sortedTableItems('files', files)`. Keep folder-key sorting unchanged. Call
`initSorting()` immediately before `init()`.

- [ ] **Step 5: Style sort controls**

```css
.sortable { position:relative; padding:0 !important; }
.sort-button { width:100%; min-height:2.1875rem; display:flex; align-items:center; gap:.35rem;
  padding:0 1rem; border:0; color:inherit; background:transparent; box-shadow:none;
  font:inherit; letter-spacing:inherit; text-transform:inherit; text-align:left; }
.sort-button:hover { color:var(--color-text); border-color:transparent; }
.sort-indicator { margin-left:auto; color:var(--color-accent); font-size:.6rem; }
```

- [ ] **Step 6: Verify and commit**

Run model and all UI tests, then:

```bash
git add src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css src/web/mod.rs
git commit -m "feat(ui): add folder-preserving table sorting"
```

---

### Task 4: Persistent Resizable Columns

**Files:**
- Modify: `src/web/assets/index.html:59-98`
- Modify: `src/web/assets/app.js:190-360,1310-1318`
- Modify: `src/web/assets/style.css:65-72,106-130,240-270`
- Modify: `src/web/mod.rs:215-225,535-725`

**Interfaces:**
- Consumes `XvUiModel.normalizeWidths`.
- Persists keys `xv.ui.columns.secrets.v1` and `xv.ui.columns.files.v1`.

- [ ] **Step 1: Add failing contract**

Change the token test to reject only `localStorage.setItem(TOKEN_STORAGE_KEY`.
Then add:

```rust
#[test]
fn ui_persists_pointer_and_keyboard_column_resizing() {
    assert!(INDEX_HTML.contains("<colgroup>"));
    assert!(INDEX_HTML.contains("class=\"column-resizer\""));
    assert!(INDEX_HTML.contains("role=\"separator\""));
    assert!(APP_JS.contains("xv.ui.columns.secrets.v1"));
    assert!(APP_JS.contains("xv.ui.columns.files.v1"));
    assert!(APP_JS.contains("handle.onpointerdown"));
    assert!(APP_JS.contains("handle.onkeydown"));
}
```

- [ ] **Step 2: Verify RED**

Run exact Cargo test; expect FAIL.

- [ ] **Step 3: Add colgroups and handles**

Secrets: hidden selection col plus 5 data cols. Files: hidden selection col plus
4 data cols. Append a focusable `role="separator"` resize span to every data
header except the final one. Give each an adjacent-column aria-label.

- [ ] **Step 4: Implement widths and persistence**

```javascript
const TABLE_WIDTHS = {
  secrets: { defaults:[28,15,14,25,18], minimums:[14,10,10,14,12], storageKey:'xv.ui.columns.secrets.v1' },
  files: { defaults:[42,12,24,22], minimums:[20,10,14,14], storageKey:'xv.ui.columns.files.v1' },
};
function dataColumns(kind) {
  return [...document.querySelectorAll(`#${kind}-table colgroup col:not(.selection-col)`)];
}
function applyColumnWidths(kind, widths) {
  TABLE_WIDTHS[kind].widths = widths;
  dataColumns(kind).forEach((column, index) => { column.style.width = `${widths[index]}%`; });
}
function saveColumnWidths(kind) {
  const config = TABLE_WIDTHS[kind];
  try { localStorage.setItem(config.storageKey, JSON.stringify(config.widths)); } catch (_) { /* use in-memory widths */ }
}
function resizeColumns(kind, index, deltaPercent) {
  const config = TABLE_WIDTHS[kind];
  const widths = [...config.widths];
  const left = Math.max(config.minimums[index], widths[index] + deltaPercent);
  const applied = left - widths[index];
  const right = widths[index + 1] - applied;
  if (right < config.minimums[index + 1]) return;
  widths[index] = left;
  widths[index + 1] = right;
  applyColumnWidths(kind, widths);
  saveColumnWidths(kind);
}
function initColumnResizing() {
  for (const kind of ['secrets', 'files']) {
    const config = TABLE_WIDTHS[kind];
    let saved = '';
    try { saved = localStorage.getItem(config.storageKey) || ''; } catch (_) { saved = ''; }
    applyColumnWidths(kind, XvUiModel.normalizeWidths(saved, config.defaults, config.minimums));
    const table = $(`#${kind}-table`);
    [...table.querySelectorAll('.column-resizer')].forEach((handle, index) => {
      handle.onpointerdown = (event) => {
        event.preventDefault();
        const startX = event.clientX;
        const startWidths = [...config.widths];
        const move = (moveEvent) => {
          config.widths = [...startWidths];
          resizeColumns(kind, index, ((moveEvent.clientX - startX) / table.clientWidth) * 100);
        };
        const stop = () => {
          window.removeEventListener('pointermove', move);
          window.removeEventListener('pointerup', stop);
        };
        window.addEventListener('pointermove', move);
        window.addEventListener('pointerup', stop, { once: true });
      };
      handle.onkeydown = (event) => {
        if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
        event.preventDefault();
        resizeColumns(kind, index, event.key === 'ArrowLeft' ? -2 : 2);
      };
    });
  }
}
```

In `setSelectionMode`, add
`elements.table.querySelector('col.selection-col').hidden = !enabled;`. Call
`initColumnResizing()` before sort initialization.

- [ ] **Step 5: Style handles and remove conflicting width rules**

```css
.column-resizer { position:absolute; z-index:2; inset-block:0; inset-inline-end:-.25rem;
  width:.5rem; cursor:col-resize; touch-action:none; }
.column-resizer::after { content:""; position:absolute; inset-block:.4rem; inset-inline-start:.22rem;
  width:1px; background:var(--color-border); }
.column-resizer:hover::after, .column-resizer:focus-visible::after { width:2px; background:var(--color-accent); }
```

Keep narrow-screen column hiding, remove phone percentage widths that override
colgroups, and retain fixed selection-column width.

- [ ] **Step 6: Verify and commit**

Run model and all UI tests, then:

```bash
git add src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css src/web/mod.rs
git commit -m "feat(ui): persist resizable table columns"
```

---

### Task 5: Authenticated File Links and Selection-Only Delete

**Files:**
- Modify: `src/web/assets/index.html:68-100`
- Modify: `src/web/assets/app.js:575-590,975-1135`
- Modify: `src/web/assets/style.css:65-72,114-125,190-200,240-270`
- Modify: `src/web/mod.rs:345-425,447-490,660-717`

**Interfaces:**
- Consumes `downloadFile`, `toggleSelected`, and bulk `bulkDelete('files')`.
- Removes row-level action state and UI.

- [ ] **Step 1: Replace obsolete contracts with failing desired contracts**

Remove tests requiring per-row download/delete or pending row deletion. Add:

```rust
#[test]
fn ui_files_are_links_without_row_actions() {
    assert!(APP_JS.contains("function fileNameCell(name)"));
    assert!(APP_JS.contains("document.createElement('a')"));
    assert!(APP_JS.contains("downloadFile(name)"));
    assert!(!INDEX_HTML.contains("class=\"file-actions\""));
    assert!(!APP_JS.contains("dl.textContent = 'Download'"));
    assert!(!APP_JS.contains("del.dataset.fileName"));
}

#[test]
fn ui_file_delete_exists_only_in_selection_toolbar() {
    assert_eq!(INDEX_HTML.matches("id=\"bulk-delete-files\"").count(), 1);
    assert!(APP_JS.contains("$('#bulk-delete-files').onclick"));
    assert!(!APP_JS.contains("pendingFileDeletes"));
}

#[test]
fn ui_file_colspans_match_four_data_columns() {
    assert!(APP_JS.contains("fileSelection.enabled ? 5 : 4"));
}
```

- [ ] **Step 2: Verify RED**

Run each exact test; expect FAIL.

- [ ] **Step 3: Remove row actions**

Remove the file action `<th>`, file action cell/buttons, `fileActionGeneration`,
`pendingFileDeletes`, map helpers, row reconciliation helpers, and their vault
switch calls. Adjust normal/selection file colspans from 5/6 to 4/5.

- [ ] **Step 4: Add authenticated link cells**

```javascript
function fileNameCell(name) {
  if (fileSelection.enabled) {
    const cell = itemNameCell('file', name, () => toggleSelected('files', name), `Select file ${name}`);
    cell.classList.add('column-file-name');
    return cell;
  }
  const td = document.createElement('td');
  td.className = 'item-name column-file-name';
  const link = document.createElement('a');
  link.className = 'item-name-content file-link';
  link.href = `/api/files/${encodeURIComponent(name)}${vaultQS(currentVault)}`;
  link.download = name;
  link.appendChild(icon('file'));
  const label = document.createElement('strong');
  label.textContent = name;
  link.appendChild(label);
  link.onclick = (event) => { event.preventDefault(); downloadFile(name); };
  td.appendChild(link);
  return td;
}
```

Use it for the first file column. Only selection-mode rows receive a row click
handler.

- [ ] **Step 5: Update CSS**

Remove `.file-actions*` and their mobile rules. Add:

```css
.file-link { color:var(--color-accent); text-decoration:none; }
.file-link strong { text-decoration:underline; text-underline-offset:.18em; }
.file-link:hover { color:var(--color-accent-hover); }
.file-link:focus-visible { outline:2px solid var(--color-accent); outline-offset:2px; border-radius:.25rem; }
```

- [ ] **Step 6: Verify and commit**

```bash
node --test src/web/assets/ui-model.test.js
cargo test --features ui web::tests::ui_ --lib
cargo test --features ui web::api::tests::file_upload_list_download_delete --lib -- --exact
git add src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css src/web/mod.rs
git commit -m "fix(ui): make file deletion selection-only"
```

Expected: all tests pass before commit.

---

### Task 6: Full Verification and Packaged Handoff

**Files:**
- Verify all implementation files.
- Build `target/release/bundle/macos/Crosstache Vault.app`.

- [ ] **Step 1: Static checks**

```bash
cargo fmt --all -- --check
git diff --check
git status --short
```

Expected: formatting/diff checks exit 0; status contains only intentional work.

- [ ] **Step 2: Full relevant tests and lint**

```bash
node --test src/web/assets/ui-model.test.js
cargo test --features ui web:: --lib
cargo clippy -p xv-desktop -- -D warnings
```

Expected: 0 failures and 0 Clippy warnings.

- [ ] **Step 3: Build unsigned Tauri bundle**

From `desktop/src-tauri` run:

```bash
cargo tauri build --bundles app --no-sign --ci
```

Expected: exit 0 and bundle at
`target/release/bundle/macos/Crosstache Vault.app`.

- [ ] **Step 4: Verify bundle**

```bash
APP='/Users/scottzionic/crosstache/target/release/bundle/macos/Crosstache Vault.app'
test -x "$APP/Contents/MacOS/xv-desktop"
plutil -lint "$APP/Contents/Info.plist"
file "$APP/Contents/MacOS/xv-desktop"
du -sh "$APP"
```

Expected: plist OK, arm64 Mach-O executable, nonzero bundle size.

- [ ] **Step 5: Isolated smoke test**

Launch the packaged executable using temporary HOME/XDG directories and
`XV_BACKEND=local`. Confirm blank missing expiration, date-only Updated, fixed
masks and Reveal/Hide, folder-preserving sorts, widths surviving reload, file
link download, and selection-toolbar-only deletion. Do not use the real vault.

- [ ] **Step 6: Sync and push**

```bash
git status --short --branch
git pull --rebase
git push
```

If pull changes source, rerun Steps 1-4 before pushing. Expected: branch is
synchronized with `origin/agent/tauri-macos-poc`.
