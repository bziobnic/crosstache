# Web UI Folder Grouping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapsible folder grouping (folders first, collapsed by default) in both `xv ui` tables, plus human-readable file sizes.

**Architecture:** Purely presentational, client-side changes in the embedded assets (`src/web/assets/app.js`, `style.css`) — no API or Rust changes. A shared `renderGrouped` helper does the group/sort/header work for both tables; `loadFiles` is split into fetch (`loadFiles`) + render (`renderFiles`) to match the secrets pattern.

**Tech Stack:** Vanilla JS/CSS assets embedded via `include_str!` behind the `ui` cargo feature.

**Spec:** `docs/superpowers/specs/2026-07-09-web-ui-folder-grouping-design.md`

## Global Constraints

- Frontend has no JS test harness — do not add one. Per-task verification is `cargo check --features ui`; behavior is verified by the browser e2e in the final task.
- `fmtSize` must mirror `src/utils/format.rs::format_size` exactly: binary (1024) steps, units `B, KB, MB, GB, TB`, `0 B` for zero, whole bytes with no decimals, larger units with two decimals (`1.46 KB`, `3.00 MB`).
- Grouping rules: secrets group by the full `folder` tag string (flat, no nesting); files group by dirname of the `/` path; empty folder / no `/` = loose bucket, never a group named `""`. Folder groups first, sorted with plain `Array.sort` (case-sensitive lexicographic); loose items after, unheadered.
- Collapsed by default; in-memory expanded-`Set`s only (`expandedSecretFolders`, `expandedFileFolders`); cleared on vault switch, NOT cleared on save/delete re-renders.
- While the secrets filter box is non-empty, collapse state is ignored (matches always visible, empty groups omitted).
- Both tables are 5 columns wide (header rows use `colSpan = 5`).
- Branch: `web-ui-folders` (already created; spec committed). Commit per task; messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- HARD GUARD for all workers: never run `xv init`, never read/write `~/.config/xv` or any user config.

---

### Task 1: Human-readable file sizes

**Files:**
- Modify: `src/web/assets/app.js` (new helper near `fmtDate` ~line 39; the Size cell in `loadFiles` ~line 477)

**Interfaces:**
- Produces: `fmtSize(bytes)` — `''` for non-number input, `'0 B'` for 0, `'<n> B'` for <1024, `'<x.xx> <UNIT>'` (two decimals) for KB/MB/GB/TB using 1024 steps. Task 3's `fileRow` reuses it.

- [ ] **Step 1: Add `fmtSize` below `fmtDate`**

```js
// Mirrors src/utils/format.rs::format_size: binary (1024) steps, whole
// bytes without decimals, larger units with two decimals.
function fmtSize(bytes) {
  if (typeof bytes !== 'number' || !isFinite(bytes)) return '';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let size = bytes;
  let i = 0;
  while (size >= 1024 && i < units.length - 1) { size /= 1024; i++; }
  return i === 0 ? `${Math.floor(size)} B` : `${size.toFixed(2)} ${units[i]}`;
}
```

- [ ] **Step 2: Use it in the files table**

In `loadFiles`, change the cells line:

```js
    const cells = [f.name, fmtSize(f.size), f.content_type, fmtDate(f.last_modified)];
```

- [ ] **Step 3: Verify build**

Run: `cargo check --features ui`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/web/assets/app.js
git commit -m "feat(ui): human-readable file sizes"
```

---

### Task 2: Collapsible folder grouping in the secrets table

**Files:**
- Modify: `src/web/assets/app.js` (`renderSecrets` rewrite ~line 235; new `renderGrouped` + `secretRow` helpers; expanded-set state; vault-switch clear in `init()`'s `sel.onchange`)
- Modify: `src/web/assets/style.css` (folder header row style)

**Interfaces:**
- Consumes: existing `secrets` array, `secretsState` guard, `showPlaceholder(tbody, text, cols)`, `fmtDate`, `openDrawer`.
- Produces (Task 3 reuses these exact signatures):
  - `renderGrouped(tbody, items, folderOf, expanded, cols, renderRow, forceExpand, rerender)` — groups `items` by `folderOf(item)` (empty string = loose), appends sorted collapsible header rows + visible item rows to `tbody`. `expanded` is a `Set<string>`; clicking a header toggles membership and calls `rerender()`. `forceExpand === true` shows all groups open (search mode) without touching the set.
  - Module state: `const expandedSecretFolders = new Set();` and `const expandedFileFolders = new Set();`

- [ ] **Step 1: Add state and the shared grouping helper**

In `app.js`, below the `showPlaceholder` helper add:

```js
// Expanded folder groups per table. In-memory only: cleared on vault
// switch, deliberately NOT cleared on save/delete re-renders so an open
// folder stays open.
const expandedSecretFolders = new Set();
const expandedFileFolders = new Set();

// Renders `items` into `tbody` as collapsible folder groups (sorted,
// listed first) followed by loose items (folderOf(item) === '').
// forceExpand shows every group open without mutating `expanded` —
// used while a search filter is active.
function renderGrouped(tbody, items, folderOf, expanded, cols, renderRow, forceExpand, rerender) {
  const groups = new Map();
  const loose = [];
  for (const it of items) {
    const f = folderOf(it);
    if (f) {
      if (!groups.has(f)) groups.set(f, []);
      groups.get(f).push(it);
    } else {
      loose.push(it);
    }
  }
  for (const name of [...groups.keys()].sort()) {
    const rows = groups.get(name);
    const open = forceExpand || expanded.has(name);
    const tr = document.createElement('tr');
    tr.className = 'folder-row';
    const td = document.createElement('td');
    td.colSpan = cols;
    td.textContent = `${open ? '▾' : '▸'} ${name} (${rows.length})`;
    tr.appendChild(td);
    tr.onclick = () => {
      if (expanded.has(name)) expanded.delete(name);
      else expanded.add(name);
      rerender();
    };
    tbody.appendChild(tr);
    if (open) for (const it of rows) tbody.appendChild(renderRow(it));
  }
  for (const it of loose) tbody.appendChild(renderRow(it));
}
```

- [ ] **Step 2: Rewrite `renderSecrets` and extract `secretRow`**

Replace the existing `renderSecrets` function (currently: filter loop building rows inline, then the empty-state check) with:

```js
function renderSecrets() {
  if (secretsState !== 'ready') return; // keep the loading/failed placeholder
  const filter = $('#search').value.toLowerCase();
  const tbody = $('#secrets-table tbody');
  tbody.innerHTML = '';
  const visible = secrets.filter((s) => {
    if (!filter) return true;
    const name = s.original_name || s.name;
    const hay = `${name} ${s.folder || ''} ${s.groups || ''} ${s.note || ''}`.toLowerCase();
    return hay.includes(filter);
  });
  // While filtering, collapse state is ignored so matches are never
  // hidden inside a collapsed folder; empty groups drop out because
  // their rows are filtered before grouping.
  renderGrouped(tbody, visible, (s) => s.folder || '', expandedSecretFolders, 5, secretRow, !!filter, renderSecrets);
  if (!tbody.children.length) {
    showPlaceholder(tbody, secrets.length ? 'no matching secrets' : 'no secrets', 5);
  }
}

function secretRow(s) {
  const name = s.original_name || s.name;
  const tr = document.createElement('tr');
  for (const cell of [name, s.folder, s.groups, s.note, fmtDate(s.updated_on)]) {
    const td = document.createElement('td');
    td.textContent = cell || '';
    tr.appendChild(td);
  }
  tr.onclick = () => openDrawer(name);
  return tr;
}
```

- [ ] **Step 3: Clear expanded sets on vault switch**

In `init()`'s `sel.onchange` handler, after `closeDrawer();` add:

```js
    expandedSecretFolders.clear();
    expandedFileFolders.clear();
```

- [ ] **Step 4: Style the header rows**

Append to `src/web/assets/style.css`:

```css
tr.folder-row td { cursor:pointer; font-weight:600; color:var(--muted);
  background:color-mix(in srgb, var(--fg) 4%, transparent); }
tr.folder-row:hover td { color:var(--fg); }
```

- [ ] **Step 5: Verify build**

Run: `cargo check --features ui`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/web/assets/app.js src/web/assets/style.css
git commit -m "feat(ui): collapsible folder groups in the secrets table"
```

---

### Task 3: Folder grouping in the files table

**Files:**
- Modify: `src/web/assets/app.js` (`loadFiles` split into fetch + `renderFiles`; new `fileRow` helper)

**Interfaces:**
- Consumes: `renderGrouped(tbody, items, folderOf, expanded, cols, renderRow, forceExpand, rerender)` and `expandedFileFolders` from Task 2; `fmtSize` from Task 1; existing `showPlaceholder`, `fmtDate`, `downloadFile`, `api`, `fail`.
- Produces: module state `let files = [];`, `renderFiles()`, `fileRow(f)`. The upload/delete flows keep calling `loadFiles()` (which now ends by calling `renderFiles()`).

- [ ] **Step 1: Split `loadFiles` and add `fileRow`**

Replace the existing `loadFiles` function (currently: capability guard, placeholder, fetch into a local `files`, inline row-building loop with download/delete buttons, empty-state check) with:

```js
let files = [];
async function loadFiles() {
  if (!ctx.capabilities.files) return;
  showPlaceholder($('#files-table tbody'), 'Loading files…', 5);
  try {
    files = await api('GET', `/api/files${vaultQS()}`);
  } catch (e) {
    files = [];
    showPlaceholder($('#files-table tbody'), 'failed to load', 5);
    throw e;
  }
  renderFiles();
}

function renderFiles() {
  const tbody = $('#files-table tbody');
  tbody.innerHTML = '';
  const dirOf = (f) => (f.name.includes('/') ? f.name.slice(0, f.name.lastIndexOf('/')) : '');
  renderGrouped(tbody, files, dirOf, expandedFileFolders, 5, fileRow, false, renderFiles);
  if (!tbody.children.length) showPlaceholder(tbody, 'no files', 5);
}

function fileRow(f) {
  const tr = document.createElement('tr');
  const cells = [f.name, fmtSize(f.size), f.content_type, fmtDate(f.last_modified)];
  for (const c of cells) {
    const td = document.createElement('td');
    td.textContent = c || '';
    tr.appendChild(td);
  }
  const td = document.createElement('td');
  const dl = document.createElement('button');
  dl.textContent = '⬇';
  dl.onclick = () => downloadFile(f.name);
  const del = document.createElement('button');
  del.textContent = '✕';
  del.className = 'danger';
  del.onclick = async () => {
    try {
      await api('DELETE', `/api/files/${encodeURIComponent(f.name)}${vaultQS()}`);
      await loadFiles();
    } catch (e) { fail(e); }
  };
  td.append(dl, del);
  tr.appendChild(td);
  return tr;
}
```

Note: `fmtSize(f.size)` replaces the old `` `${f.size}` `` — Task 1 already made this change inside the old `loadFiles`; carry it into `fileRow`, not both.

- [ ] **Step 2: Verify build**

Run: `cargo check --features ui`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add src/web/assets/app.js
git commit -m "feat(ui): collapsible folder groups in the files table"
```

---

### Task 4: Full verification

**Files:** none (verification; plus CHANGELOG/doc touch-up if gaps found)

- [ ] **Step 1: Rust gates**

Run:
```bash
cargo fmt --check && cargo clippy --features ui --all-targets && cargo test --features ui
```
Expected: all green (no Rust changed, but the assets are `include_str!`-ed into tested code).

- [ ] **Step 2: Browser e2e**

Recreate the hermetic harness (per the `web-ui-e2e-tooling` memory: temp `XDG_CONFIG_HOME` + `XV_BACKEND=local` + `XV_NO_PARENT_CONFIG=1`, `xv ui --port N --no-open`, playwright-core + headless Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`). Seed: two secrets in folder `proj/db`, one loose secret, one file `docs/readme.txt` (~1.5 KB so the size formats with decimals), one loose file. Assert:

1. Secrets table: `proj/db` header row (`▸ proj/db (2)`) appears BEFORE the loose secret row; its member rows are hidden (collapsed by default).
2. Clicking the header shows the two rows and flips the arrow to `▾`; clicking again re-collapses.
3. Typing a filter matching a secret inside the collapsed folder shows that row under its header; a filter matching nothing shows `no matching secrets`; clearing the filter restores the collapsed view.
4. Expand `proj/db`, save an edit to one of its secrets via the drawer — after the re-render the folder is still expanded. Switch vaults (if only one vault exists, skip) — expanded set resets.
5. Files tab: `docs` header row before the loose file; collapsed by default; toggle works; Size column shows `1.46 KB`-style values and a `0 B` case if a zero-byte file is seeded.
6. End with a screenshot review of both tables (expanded state) — screenshots have caught what DOM assertions missed before.

- [ ] **Step 3: Update CHANGELOG**

Add an `## Unreleased` section to `CHANGELOG.md` (above the `## v0.24.0` heading) noting: collapsible folder grouping in both web UI tables (folders first, collapsed by default) and human-readable file sizes. Commit:

```bash
git add CHANGELOG.md
git commit -m "docs: changelog for web ui folder grouping"
```

- [ ] **Step 4: Hand off**

Use superpowers:finishing-a-development-branch (push `web-ui-folders`, open a PR, watch Bugbot).
