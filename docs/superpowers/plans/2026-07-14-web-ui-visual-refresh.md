# `xv ui` Visual Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transform the embedded `xv ui` frontend into the approved calm-premium vault design while preserving every existing secret, file, selection, authentication, and storage workflow.

**Architecture:** Keep the existing embedded `index.html` + `style.css` + `app.js` architecture and the existing Rust asset-contract tests. Add semantic HTML wrappers, a tokenized CSS component system, a small inline SVG symbol set, and narrowly scoped JavaScript presentation helpers; do not change API calls, request generation guards, or client-side persistence.

**Tech Stack:** Rust embedded-asset tests, vanilla HTML, vanilla JavaScript, CSS custom properties/media queries, existing axum asset server.

## Global Constraints

- No frontend framework, bundler, package manager, or external font.
- No remote images, icon service, analytics, or other network asset.
- No manual theme switch; follow `prefers-color-scheme` with complete light and dark token sets.
- Do not change the Rust API, bearer-token flow, `sessionStorage`, backend behavior, or existing request/state guards.
- Keep the workspace at a 76rem maximum width and standard data rows at 40–45px high.
- Preserve semantic controls and table structure; decorative SVG icons must be hidden from assistive technology.
- Avoid horizontal page scrolling from desktop through phone widths.
- Do not add a JavaScript package manager solely for visual tests.

## File Structure

- Modify `src/web/assets/index.html`: semantic application shell, view headings, data-surface wrappers, drawer sections, accessible feedback markup, and inline SVG symbols.
- Modify `src/web/assets/style.css`: semantic light/dark tokens, layout primitives, components, states, responsive rules, and reduced-motion behavior.
- Modify `src/web/assets/app.js`: decorative icon helper, list counts, richer list placeholders, presentation classes, and drawer copy. Existing API and workflow functions remain authoritative.
- Modify `src/web/mod.rs`: embedded-asset contract tests for structural hooks, token names, state helpers, and accessibility/responsive requirements.

No runtime file is created and no dependency manifest changes.

---

### Task 1: Establish the visual tokens and semantic application shell

**Files:**
- Modify: `src/web/mod.rs:172-400`
- Modify: `src/web/assets/index.html:9-62`
- Modify: `src/web/assets/style.css:1-19`

**Interfaces:**
- Consumes: existing IDs `backend-badge`, `vault-select`, `tab-secrets`, `tab-files`, `secrets-view`, `files-view`, and `search` used by `app.js`.
- Produces: `#app-header`, `.app-header-inner`, `.brand-mark`, `.brand-name`, `.vault-context`, `.tab-list`, `.view-heading`, `#secret-item-count`, and `#file-item-count`; semantic CSS tokens used by every later task.

- [ ] **Step 1: Write the failing shell/token contract test**

Add this test inside `src/web/mod.rs`'s existing `tests` module:

```rust
#[test]
fn ui_has_semantic_visual_shell_and_tokens() {
    for marker in [
        "id=\"app-header\"",
        "class=\"app-header-inner\"",
        "class=\"brand-mark\"",
        "class=\"brand-name\"",
        "class=\"vault-context\"",
        "class=\"tab-list\"",
        "id=\"secret-item-count\"",
        "id=\"file-item-count\"",
    ] {
        assert!(INDEX_HTML.contains(marker), "missing {marker}");
    }
    for token in [
        "--color-canvas:",
        "--color-surface:",
        "--color-surface-subtle:",
        "--color-text:",
        "--color-text-muted:",
        "--color-border:",
        "--color-accent:",
        "--color-accent-quiet:",
        "--color-danger:",
        "--shadow-raised:",
    ] {
        assert!(STYLE_CSS.contains(token), "missing {token}");
    }
}
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
cargo test --features ui web::tests::ui_has_semantic_visual_shell_and_tokens -- --exact
```

Expected: FAIL with `missing id="app-header"`.

- [ ] **Step 3: Replace the header with the semantic application bar**

Replace the existing top-level `<header>` in `index.html` with this markup. Keep every existing JavaScript-owned ID unchanged:

```html
<header id="app-header" class="app-header">
  <div class="app-header-inner">
    <div class="brand" aria-label="Crosstache Vault">
      <span class="brand-mark" aria-hidden="true">xv</span>
      <span class="brand-name">Crosstache Vault</span>
    </div>
    <div class="vault-context">
      <span id="backend-badge" class="backend-badge"></span>
      <label class="vault-picker">
        <span class="status-dot" aria-hidden="true"></span>
        <span class="sr-only">Current vault</span>
        <select id="vault-select" title="Current vault"></select>
      </label>
    </div>
    <nav class="tab-list" aria-label="Vault content">
      <button id="tab-secrets" class="tab active" type="button">Secrets</button>
      <button id="tab-files" class="tab" type="button">Files</button>
    </nav>
  </div>
</header>
```

- [ ] **Step 4: Add the approved view headings without changing toolbar IDs**

Add this block as the first child of `#secrets-view`:

```html
<div class="view-heading">
  <div>
    <span class="eyebrow">Vault contents</span>
    <h1>Your secrets</h1>
    <p>Browse, organize, and safely manage credentials in this vault.</p>
  </div>
  <span id="secret-item-count" class="item-count" aria-live="polite">0 secrets</span>
</div>
```

Add this block as the first child of `#files-view`:

```html
<div class="view-heading">
  <div>
    <span class="eyebrow">Vault files</span>
    <h1>Your files</h1>
    <p>Upload, download, and organize encrypted files in this vault.</p>
  </div>
  <span id="file-item-count" class="item-count" aria-live="polite">0 files</span>
</div>
```

- [ ] **Step 5: Replace the legacy root variables and shell rules**

Replace the entire current `style.css` with the following foundation and baseline component rules. This removes the legacy `--fg`, `--bg`, `--muted`, and `--line` references in one atomic change; later tasks add more specific component layers after this baseline:

```css
:root {
  color-scheme: light dark;
  --color-canvas: #f3f1eb;
  --color-surface: #ffffff;
  --color-surface-subtle: #f8f8f5;
  --color-text: #18221c;
  --color-text-muted: #68726b;
  --color-border: #d7dad5;
  --color-accent: #216446;
  --color-accent-hover: #174c36;
  --color-accent-quiet: #e7f0e9;
  --color-danger: #9f332e;
  --color-danger-quiet: #fff8f7;
  --radius-control: .5625rem;
  --radius-surface: .75rem;
  --shadow-raised: 0 14px 34px rgb(37 54 43 / 8%);
  --focus-ring: 0 0 0 3px rgb(61 131 92 / 18%);
  font-family: Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
* { box-sizing: border-box; }
[hidden] { display: none !important; }
.sr-only { position:absolute; width:1px; height:1px; padding:0; margin:-1px; overflow:hidden;
  clip:rect(0,0,0,0); white-space:nowrap; border:0; }
body { margin:0; min-width:20rem; color:var(--color-text); background:var(--color-canvas);
  font-size:.875rem; line-height:1.5; }
button, input, select, textarea { font:inherit; }
.app-header { border-bottom:1px solid var(--color-border); background:color-mix(in srgb, var(--color-surface) 92%, transparent); }
.app-header-inner { width:min(100%, 76rem); min-height:4.125rem; margin:0 auto; padding:.75rem 1.5rem;
  display:flex; align-items:center; gap:.875rem; }
.brand { display:flex; align-items:center; gap:.625rem; font-weight:720; letter-spacing:-.025em; }
.brand-mark { width:2.125rem; height:2.125rem; display:grid; place-items:center; border-radius:.625rem;
  color:#fff; background:var(--color-accent); font-weight:850; letter-spacing:-.08em;
  box-shadow:0 5px 12px rgb(33 100 70 / 24%); }
.vault-context { display:flex; align-items:center; gap:.5rem; margin-left:.5rem; padding-left:1rem;
  border-left:1px solid var(--color-border); }
.backend-badge { padding:.2rem .5rem; border:1px solid var(--color-border); border-radius:999px;
  color:var(--color-text-muted); background:var(--color-surface-subtle); font-size:.7rem; font-weight:650; }
.vault-picker { display:flex; align-items:center; gap:.4rem; }
.status-dot { width:.45rem; height:.45rem; border-radius:50%; background:var(--color-accent);
  box-shadow:0 0 0 3px var(--color-accent-quiet); }
.vault-picker select { max-width:14rem; border:0; color:var(--color-text); background:transparent; font-weight:650; }
.tab-list { margin-left:auto; display:flex; gap:.1875rem; padding:.25rem; border:1px solid var(--color-border);
  border-radius:.625rem; background:color-mix(in srgb, var(--color-text) 4%, transparent); }
.tab { min-height:2.125rem; padding:.4rem .75rem; border:0; border-radius:.4375rem; color:var(--color-text-muted);
  background:transparent; cursor:pointer; font-weight:650; }
.tab.active { color:var(--color-accent-hover); background:var(--color-surface); box-shadow:0 1px 4px rgb(28 44 34 / 8%); }
main { width:min(100%, 76rem); margin:0 auto; padding:2rem 1.5rem 3rem; }
.view-heading { display:flex; align-items:flex-end; justify-content:space-between; gap:1.5rem; margin-bottom:1.375rem; }
.view-heading h1 { margin:.2rem 0 0; font-size:1.625rem; line-height:1.15; letter-spacing:-.04em; }
.view-heading p { margin:.4rem 0 0; color:var(--color-text-muted); }
.eyebrow { color:var(--color-accent); font-size:.6875rem; font-weight:750; letter-spacing:.1em; text-transform:uppercase; }
.item-count { flex:none; color:var(--color-text-muted); font-size:.75rem; }
.recovery { max-width:36rem; margin:4rem auto; padding:2rem; border:1px solid var(--color-border);
  border-radius:var(--radius-surface); background:var(--color-surface); box-shadow:var(--shadow-raised); text-align:center; }
.recovery h1 { margin:0 0 .5rem; font-size:1.5rem; letter-spacing:-.035em; }
.recovery p { margin:0; color:var(--color-text-muted); }
.toolbar { display:flex; gap:.5rem; align-items:center; margin-bottom:.75rem; }
.toolbar-spacer { flex:1; }
#search { flex:1; padding:.4rem; }
.bulk-toolbar { display:flex; flex-wrap:wrap; gap:.5rem; align-items:center; margin-bottom:.75rem;
  padding:.55rem .65rem; border:1px solid var(--color-border); border-radius:.5rem;
  background:var(--color-accent-quiet); }
.bulk-toolbar > span { min-width:6.5rem; font-weight:650; }
.bulk-toolbar input { min-width:10rem; flex:1; }
table { width:100%; table-layout:fixed; border-collapse:collapse; }
th, td { text-align:left; padding:.4rem .6rem; border-bottom:1px solid var(--color-border);
  overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.selection-column { width:2.5rem; text-align:center; padding-inline:.35rem; }
.selection-column input { width:1rem; height:1rem; margin:0; vertical-align:middle; accent-color:var(--color-accent); }
.file-actions { display:flex; flex-wrap:wrap; gap:.25rem; }
#secrets-table tbody tr { cursor:pointer; }
#secrets-table tbody tr:hover { background:color-mix(in srgb, var(--color-accent) 8%, transparent); }
#drawer { position:fixed; z-index:20; top:0; right:0; width:min(26rem, 100vw); height:100vh;
  padding:1rem; overflow-y:auto; color:var(--color-text); background:var(--color-surface);
  border-left:1px solid var(--color-border); box-shadow:-4px 0 12px rgb(0 0 0 / 15%); }
#drawer label { display:block; margin-bottom:.7rem; color:var(--color-text-muted); }
#drawer input, #drawer textarea { width:100%; padding:.4rem; color:var(--color-text);
  background:var(--color-surface); border:1px solid var(--color-border); border-radius:.25rem; }
.row { display:flex; gap:.5rem; margin-top:.3rem; }
button { padding:.35rem .8rem; border:1px solid var(--color-border); border-radius:.25rem;
  color:var(--color-text); background:var(--color-surface); cursor:pointer; }
button:hover { border-color:var(--color-accent); }
button:disabled { cursor:not-allowed; opacity:.65; }
button.pending:disabled { cursor:wait; }
button.danger { color:var(--color-danger); }
#dropzone { margin-bottom:1rem; padding:2rem; border:2px dashed var(--color-border);
  border-radius:.5rem; color:var(--color-text-muted); text-align:center; }
#dropzone.over { border-color:var(--color-accent); }
.linkish { color:var(--color-accent); cursor:pointer; text-decoration:underline; }
#toast { position:fixed; z-index:30; bottom:1rem; left:50%; transform:translateX(-50%);
  padding:.5rem 1rem; border-radius:.375rem; color:var(--color-surface); background:var(--color-text); }
#toast.error { color:#fff; background:var(--color-danger); }
#progress { position:fixed; z-index:40; top:0; left:0; right:0; height:2px; overflow:hidden; }
#progress::after { content:""; display:block; width:30%; height:100%; background:var(--color-accent);
  animation:progress-slide 1.2s linear infinite; }
@keyframes progress-slide { from { transform:translateX(-100vw); } to { transform:translateX(100vw); } }
td.placeholder { color:var(--color-text-muted); text-align:center; padding:1rem; }
#record-type { color:var(--color-text-muted); margin-bottom:.5rem; }
tr.folder-row td { cursor:pointer; font-weight:650; color:var(--color-text-muted);
  background:color-mix(in srgb, var(--color-text) 4%, transparent); }
tr.folder-row:not(.static):hover td { color:var(--color-text); }
tr.folder-row.static td { cursor:default; }
.folder-child .item-name { position:relative; padding-inline-start:2rem; }
.folder-child .item-name::before { content:""; position:absolute; inset-block:0; inset-inline-start:1rem;
  width:1px; background:var(--color-border); }
tr.selected-row { background:color-mix(in srgb, var(--color-accent) 12%, transparent); }
```

- [ ] **Step 6: Run the focused test and existing web tests**

Run:

```bash
cargo test --features ui web::tests::ui_has_semantic_visual_shell_and_tokens -- --exact
cargo test --features ui web::tests
```

Expected: PASS. Existing tests prove the preserved IDs still support token persistence, recovery, tab switching, and selection.

- [ ] **Step 7: Commit the foundation**

```bash
git add src/web/mod.rs src/web/assets/index.html src/web/assets/style.css
git commit -m "style(ui): establish visual foundation"
```

---

### Task 2: Add inline icons, structured data surfaces, and live summaries

**Files:**
- Modify: `src/web/mod.rs:172-400`
- Modify: `src/web/assets/index.html:27-61,94`
- Modify: `src/web/assets/app.js:39,247-300,512-560,859-923`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Consumes: Task 1 view headings and semantic tokens; existing `renderGrouped`, `renderSecrets`, `secretRow`, `renderFiles`, and `fileRow` functions.
- Produces: `icon(name) -> SVGElement`, `.icon`, `.data-surface`, `.list-summary`, `setListSummary(kind, visibleCount, totalCount, folderCount)`, structured folder cells, and decorated item-name cells.

- [ ] **Step 1: Write the failing icon/data-surface contract test**

Add:

```rust
#[test]
fn ui_has_embedded_icons_and_data_surface_summaries() {
    for marker in [
        "id=\"xv-icon-sprite\"",
        "id=\"icon-secret\"",
        "id=\"icon-folder\"",
        "id=\"icon-check\"",
        "id=\"icon-alert\"",
        "class=\"data-surface\"",
        "id=\"secret-list-summary\"",
        "id=\"file-list-summary\"",
    ] {
        assert!(INDEX_HTML.contains(marker), "missing {marker}");
    }
    assert!(APP_JS.contains("function icon(name)"));
    assert!(APP_JS.contains("function setListSummary(kind, visibleCount, totalCount, folderCount)"));
    assert!(APP_JS.contains("icon('secret')"));
    assert!(APP_JS.contains("icon('file')"));
    assert!(APP_JS.contains("icon(open ? 'chevron-down' : 'chevron-right')"));
}
```

- [ ] **Step 2: Run the test and verify RED**

```bash
cargo test --features ui web::tests::ui_has_embedded_icons_and_data_surface_summaries -- --exact
```

Expected: FAIL with `missing id="xv-icon-sprite"`.

- [ ] **Step 3: Add the embedded symbol sprite**

Add this immediately before `<script src="/app.js"></script>`:

```html
<svg id="xv-icon-sprite" class="icon-sprite" aria-hidden="true">
  <symbol id="icon-search" viewBox="0 0 24 24"><circle cx="11" cy="11" r="6"></circle><path d="m16 16 4 4"></path></symbol>
  <symbol id="icon-plus" viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"></path></symbol>
  <symbol id="icon-secret" viewBox="0 0 24 24"><circle cx="8" cy="12" r="4"></circle><path d="M12 12h8m-3 0v3m-3-3v2"></path></symbol>
  <symbol id="icon-folder" viewBox="0 0 24 24"><path d="M3 7h7l2 2h9v10H3z"></path></symbol>
  <symbol id="icon-file" viewBox="0 0 24 24"><path d="M6 3h8l4 4v14H6zM14 3v5h4"></path></symbol>
  <symbol id="icon-upload" viewBox="0 0 24 24"><path d="M12 16V4m-5 5 5-5 5 5M5 20h14"></path></symbol>
  <symbol id="icon-chevron-right" viewBox="0 0 24 24"><path d="m9 5 7 7-7 7"></path></symbol>
  <symbol id="icon-chevron-down" viewBox="0 0 24 24"><path d="m5 9 7 7 7-7"></path></symbol>
  <symbol id="icon-copy" viewBox="0 0 24 24"><rect x="8" y="8" width="11" height="11" rx="2"></rect><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3"></path></symbol>
  <symbol id="icon-download" viewBox="0 0 24 24"><path d="M12 4v12m-5-5 5 5 5-5M5 20h14"></path></symbol>
  <symbol id="icon-eye" viewBox="0 0 24 24"><path d="M2 12s4-6 10-6 10 6 10 6-4 6-10 6S2 12 2 12z"></path><circle cx="12" cy="12" r="2.5"></circle></symbol>
  <symbol id="icon-close" viewBox="0 0 24 24"><path d="m6 6 12 12M18 6 6 18"></path></symbol>
  <symbol id="icon-check" viewBox="0 0 24 24"><path d="m5 12 4 4L19 6"></path></symbol>
  <symbol id="icon-alert" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"></circle><path d="M12 7v6M12 17h.01"></path></symbol>
</svg>
```

- [ ] **Step 4: Wrap each table and add summary text**

Wrap each existing table in `<div class="data-surface">`. Place these elements immediately after their corresponding wrapper:

```html
<div id="secret-list-summary" class="list-summary" aria-live="polite">Values remain hidden until revealed.</div>
```

```html
<div id="file-list-summary" class="list-summary" aria-live="polite">Files remain encrypted in the current vault.</div>
```

Do not move a bulk toolbar into the data surface; it remains directly above the table wrapper.

- [ ] **Step 5: Add the JavaScript icon and summary helpers**

Add immediately after `const $ = (sel) => document.querySelector(sel);`:

```javascript
const SVG_NS = 'http://www.w3.org/2000/svg';
function icon(name) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.classList.add('icon');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#icon-${name}`);
  svg.appendChild(use);
  return svg;
}

function setListSummary(kind, visibleCount, totalCount, folderCount) {
  const singular = kind === 'secrets' ? 'secret' : 'file';
  const noun = visibleCount === 1 ? singular : kind;
  $(`#${singular}-item-count`).textContent = `${visibleCount} ${noun}`;
  const visibility = visibleCount === totalCount
    ? `${totalCount} ${totalCount === 1 ? singular : kind}`
    : `${visibleCount} of ${totalCount} ${kind}`;
  const folders = `${folderCount} ${folderCount === 1 ? 'folder' : 'folders'}`;
  const safety = kind === 'secrets' ? 'Values remain hidden until revealed.' : 'Files remain encrypted in this vault.';
  $(`#${singular}-list-summary`).textContent = `${visibility} across ${folders}. ${safety}`;
}
```

- [ ] **Step 6: Structure folder rows and item-name cells**

In `renderGrouped`, replace `td.textContent = `${open ? '▾' : '▸'} ${name} (${rows.length})`;` with:

```javascript
td.className = 'folder-cell';
td.appendChild(icon(open ? 'chevron-down' : 'chevron-right'));
td.appendChild(icon('folder'));
const label = document.createElement('span');
label.className = 'folder-name';
label.textContent = name;
const count = document.createElement('span');
count.className = 'folder-count';
count.textContent = `${rows.length} ${rows.length === 1 ? 'item' : 'items'}`;
td.append(label, count);
```

In `secretRow`, replace the existing cell loop with:

```javascript
for (const [index, cell] of [name, s.folder, s.groups, s.note, fmtDate(s.updated_on)].entries()) {
  const td = document.createElement('td');
  if (index === 0) {
    td.classList.add('item-name');
    td.appendChild(icon('secret'));
    const label = document.createElement('strong');
    label.textContent = cell || '';
    td.appendChild(label);
  } else if (index === 2 && cell) {
    const tag = document.createElement('span');
    tag.className = 'tag';
    tag.textContent = cell;
    td.appendChild(tag);
  } else {
    td.textContent = cell || '';
  }
  tr.appendChild(td);
}
```

In `fileRow`, replace the existing cell loop with:

```javascript
for (const [index, cell] of [f.name, fmtSize(f.size), f.content_type, fmtDate(f.last_modified)].entries()) {
  const td = document.createElement('td');
  if (index === 0) {
    td.classList.add('item-name');
    td.appendChild(icon('file'));
    const label = document.createElement('strong');
    label.textContent = cell || '';
    td.appendChild(label);
  } else {
    td.textContent = cell || '';
  }
  tr.appendChild(td);
}
```

- [ ] **Step 7: Update summaries from the existing render functions**

After computing `visible` in `renderSecrets`, add:

```javascript
const secretFolders = new Set(visible.map((secret) => secret.folder).filter(Boolean));
setListSummary('secrets', visible.length, secrets.length, secretFolders.size);
```

After declaring `dirOf` in `renderFiles`, add:

```javascript
const fileFolders = new Set(files.map(dirOf).filter(Boolean));
setListSummary('files', files.length, files.length, fileFolders.size);
```

- [ ] **Step 8: Add data-surface and icon styling**

Append:

```css
.icon-sprite { position:absolute; width:0; height:0; overflow:hidden; }
.icon { width:1rem; height:1rem; flex:none; fill:none; stroke:currentColor; stroke-width:1.8;
  stroke-linecap:round; stroke-linejoin:round; }
.data-surface { overflow:hidden; border:1px solid var(--color-border); border-radius:var(--radius-surface);
  background:var(--color-surface); box-shadow:var(--shadow-raised); }
.data-surface table { width:100%; border-collapse:collapse; }
.data-surface th { height:2.1875rem; padding:0 1rem; text-align:left; color:var(--color-text-muted);
  background:var(--color-surface-subtle); font-size:.6875rem; font-weight:730; letter-spacing:.07em;
  text-transform:uppercase; }
.data-surface td { height:2.75rem; padding:0 1rem; border-top:1px solid color-mix(in srgb, var(--color-border) 72%, transparent);
  color:var(--color-text-muted); }
.data-surface tbody tr:not(.folder-row):hover { background:color-mix(in srgb, var(--color-accent) 6%, transparent); }
.item-name { display:flex; align-items:center; gap:.625rem; color:var(--color-text); }
.item-name > .icon { width:1.5rem; height:1.5rem; padding:.32rem; border-radius:.4375rem;
  color:var(--color-accent); background:var(--color-accent-quiet); }
.item-name strong { overflow:hidden; text-overflow:ellipsis; }
.folder-cell { display:flex; align-items:center; gap:.5rem; color:var(--color-text); }
.folder-cell .icon { width:.875rem; height:.875rem; color:var(--color-accent); }
.folder-name { font-weight:700; }
.folder-count, .tag { display:inline-flex; padding:.15rem .45rem; border-radius:999px;
  color:var(--color-text-muted); background:color-mix(in srgb, var(--color-text) 6%, transparent); font-size:.7rem; }
.folder-count { margin-left:.25rem; }
.list-summary { display:flex; justify-content:flex-end; margin-top:.75rem; color:var(--color-text-muted); font-size:.72rem; }
```

- [ ] **Step 9: Run tests and commit**

```bash
cargo test --features ui web::tests::ui_has_embedded_icons_and_data_surface_summaries -- --exact
cargo test --features ui web::tests
git add src/web/mod.rs src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css
git commit -m "style(ui): refine vault data surfaces"
```

Expected: both test commands PASS; the commit contains only embedded-frontend and contract-test changes.

---

### Task 3: Replace bare placeholders with loading, empty, filtered, and failed states

**Files:**
- Modify: `src/web/mod.rs:172-400`
- Modify: `src/web/assets/app.js:99-108,492-540,838-867`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Consumes: existing `secretsState`, `filesState`, `openDrawer(null)`, `#file-input`, and the Task 2 data surfaces.
- Produces: `showListState(tbody, kind, state, cols)` supporting `loading`, `failed`, `empty`, and `filtered` states; `.skeleton-row`; `.empty-state`; existing create/browse handlers reused as empty-state actions.

- [ ] **Step 1: Write the failing list-state contract test**

Add:

```rust
#[test]
fn ui_renders_purposeful_list_states() {
    assert!(APP_JS.contains("function showListState(tbody, kind, state, cols)"));
    assert!(APP_JS.contains("for (let index = 0; index < 3; index++)"));
    assert!(APP_JS.contains("button.onclick = () => openDrawer(null)"));
    assert!(APP_JS.contains("button.onclick = () => $('#file-input').click()"));
    assert!(APP_JS.contains("showListState($('#secrets-table tbody'), 'secrets', 'loading'"));
    assert!(APP_JS.contains("showListState($('#files-table tbody'), 'files', 'failed'"));
    assert!(STYLE_CSS.contains(".skeleton-row"));
    assert!(STYLE_CSS.contains(".empty-state"));
}
```

- [ ] **Step 2: Run the test and verify RED**

```bash
cargo test --features ui web::tests::ui_renders_purposeful_list_states -- --exact
```

Expected: FAIL because `showListState` does not exist.

- [ ] **Step 3: Replace `showPlaceholder` with the complete state renderer**

Replace `showPlaceholder` with:

```javascript
function showListState(tbody, kind, state, cols) {
  tbody.innerHTML = '';
  if (state === 'loading') {
    for (let index = 0; index < 3; index++) {
      const tr = document.createElement('tr');
      tr.className = 'skeleton-row';
      const td = document.createElement('td');
      td.colSpan = cols;
      td.innerHTML = '<span></span><span></span><span></span>';
      tr.appendChild(td);
      tbody.appendChild(tr);
    }
    return;
  }

  const copy = {
    secrets: {
      failed: ['Couldn’t load secrets', 'The current vault could not be read.'],
      empty: ['No secrets yet', 'Create the first secret in this vault.'],
      filtered: ['No matching secrets', 'Try a different name, folder, group, or note.'],
    },
    files: {
      failed: ['Couldn’t load files', 'The current vault could not be read.'],
      empty: ['No files yet', 'Upload the first encrypted file to this vault.'],
    },
  };
  const [title, description] = copy[kind][state];
  const tr = document.createElement('tr');
  const td = document.createElement('td');
  td.colSpan = cols;
  const container = document.createElement('div');
  container.className = `empty-state ${state}`;
  const heading = document.createElement('strong');
  heading.textContent = title;
  const message = document.createElement('span');
  message.textContent = description;
  container.append(heading, message);
  if (state === 'empty') {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'button secondary';
    button.textContent = kind === 'secrets' ? 'New secret' : 'Browse files';
    if (kind === 'secrets') button.onclick = () => openDrawer(null);
    else button.onclick = () => $('#file-input').click();
    container.appendChild(button);
  }
  td.appendChild(container);
  tr.appendChild(td);
  tbody.appendChild(tr);
}
```

- [ ] **Step 4: Route every existing list state through the new helper**

Use these exact calls:

```javascript
showListState($('#secrets-table tbody'), 'secrets', 'loading', secretSelection.enabled ? 6 : 5);
showListState($('#secrets-table tbody'), 'secrets', 'failed', secretSelection.enabled ? 6 : 5);
showListState(tbody, 'secrets', secrets.length ? 'filtered' : 'empty', cols);
showListState($('#files-table tbody'), 'files', 'loading', fileSelection.enabled ? 6 : 5);
showListState($('#files-table tbody'), 'files', 'failed', fileSelection.enabled ? 6 : 5);
showListState(tbody, 'files', 'empty', cols);
```

Do not change the surrounding generation checks or the `throw e` behavior in either loader.

- [ ] **Step 5: Add exact state styling**

Append:

```css
.skeleton-row td { height:2.75rem; padding:0 1rem; }
.skeleton-row td { display:grid; grid-template-columns:1.4fr .8fr .6fr; gap:1.5rem; align-items:center; }
.skeleton-row span { height:.55rem; border-radius:999px; background:color-mix(in srgb, var(--color-text) 9%, transparent);
  animation:skeleton-pulse 1.4s ease-in-out infinite alternate; }
.skeleton-row span:nth-child(2) { width:75%; }
.skeleton-row span:nth-child(3) { width:55%; }
@keyframes skeleton-pulse { to { opacity:.42; } }
.empty-state { min-height:12rem; display:flex; flex-direction:column; align-items:center; justify-content:center;
  gap:.45rem; padding:2rem; text-align:center; white-space:normal; }
.empty-state strong { color:var(--color-text); font-size:1rem; }
.empty-state span { max-width:28rem; color:var(--color-text-muted); }
.empty-state .button { margin-top:.55rem; }
.empty-state.failed strong { color:var(--color-danger); }
```

- [ ] **Step 6: Run tests and commit**

```bash
cargo test --features ui web::tests::ui_renders_purposeful_list_states -- --exact
cargo test --features ui web::tests::ui_guards_list_loads_against_stale_responses -- --exact
cargo test --features ui web::tests::ui_preserves_failed_file_load_state_during_bulk_recovery -- --exact
git add src/web/mod.rs src/web/assets/app.js src/web/assets/style.css
git commit -m "style(ui): add purposeful list states"
```

Expected: all three tests PASS. The second and third commands prove the visual state renderer did not weaken stale-response handling.

---

### Task 4: Rebuild the secret drawer and form-control hierarchy

**Files:**
- Modify: `src/web/mod.rs:172-400`
- Modify: `src/web/assets/index.html:64-92`
- Modify: `src/web/assets/app.js:339-375,582-600`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Consumes: existing drawer/form IDs and all existing reveal, copy, save, delete, close, typed-record, and generation-guard handlers.
- Produces: `.drawer-header`, `#drawer-kicker`, `.drawer-body`, `.drawer-footer`, `.form-field`, `.field-label`, `.field-hint`, and button variants `.button.primary`, `.button.secondary`, `.button.ghost`, and `.button.danger`.

- [ ] **Step 1: Write the failing drawer contract test**

Add:

```rust
#[test]
fn ui_has_structured_drawer_and_button_hierarchy() {
    for marker in [
        "class=\"drawer-header\"",
        "id=\"drawer-kicker\"",
        "class=\"drawer-body\"",
        "class=\"drawer-footer\"",
        "class=\"button primary\"",
        "class=\"button ghost\"",
        "class=\"button danger\"",
    ] {
        assert!(INDEX_HTML.contains(marker), "missing {marker}");
    }
    assert!(APP_JS.contains("label.className = 'form-field'"));
    assert!(APP_JS.contains("$('#drawer-kicker').textContent = name ? 'Edit secret' : 'Create secret'"));
    assert!(STYLE_CSS.contains(".drawer-footer {"));
    assert!(STYLE_CSS.contains("position:sticky"));
}
```

- [ ] **Step 2: Run the test and verify RED**

```bash
cargo test --features ui web::tests::ui_has_structured_drawer_and_button_hierarchy -- --exact
```

Expected: FAIL with `missing class="drawer-header"`.

- [ ] **Step 3: Replace the drawer markup while preserving every owned ID**

Replace the existing `<aside id="drawer">` with:

```html
<aside id="drawer" aria-labelledby="drawer-title" hidden>
  <div class="drawer-header">
    <span id="drawer-kicker" class="eyebrow">Create secret</span>
    <h2 id="drawer-title">New secret</h2>
    <p>Values remain concealed until explicitly revealed.</p>
  </div>
  <form id="secret-form" autocomplete="off">
    <div class="drawer-body">
      <label class="form-field"><span class="field-label">Name <span class="field-hint">Required</span></span><input name="name" required></label>
      <label id="type-picker-label" class="form-field" hidden><span class="field-label">Type</span><select id="type-picker"></select></label>
      <div id="value-section">
        <label class="form-field"><span class="field-label">Value <span class="field-hint">Protected</span></span>
          <textarea name="value" rows="4" placeholder="Leave blank to keep the current value"></textarea>
          <span class="field-actions">
            <button type="button" id="reveal" class="button secondary"><svg class="icon" aria-hidden="true"><use href="#icon-eye"></use></svg>Reveal</button>
            <button type="button" id="copy" class="button secondary"><svg class="icon" aria-hidden="true"><use href="#icon-copy"></use></svg>Copy</button>
          </span>
        </label>
      </div>
      <div id="record-section" hidden>
        <div id="record-type"></div>
        <div id="record-fields"></div>
      </div>
      <label class="form-field"><span class="field-label">Folder</span><input name="folder"></label>
      <label class="form-field"><span class="field-label">Groups</span><input name="groups" placeholder="Comma-separated"></label>
      <label class="form-field"><span class="field-label">Note</span><input name="note"></label>
      <label class="form-field"><span class="field-label">Expires</span><input name="expires_on" type="date"></label>
    </div>
    <div class="drawer-footer">
      <button type="button" id="delete" class="button danger" data-default-label="Delete">Delete</button>
      <span class="drawer-footer-spacer"></span>
      <button type="button" id="close-drawer" class="button ghost"><svg class="icon" aria-hidden="true"><use href="#icon-close"></use></svg>Cancel</button>
      <button type="submit" id="save" class="button primary">Save changes</button>
    </div>
  </form>
</aside>
```

- [ ] **Step 4: Make dynamic record fields use the same form hierarchy**

At the beginning of `fieldRow`, replace the label text setup with:

```javascript
const label = document.createElement('label');
label.className = 'form-field';
const heading = document.createElement('span');
heading.className = 'field-label';
heading.append(name);
if (required || kind === 'secret') {
  const hint = document.createElement('span');
  hint.className = 'field-hint';
  hint.textContent = required ? 'Required' : 'Protected';
  heading.appendChild(hint);
}
label.appendChild(heading);
```

When creating `rev` and `cp`, set:

```javascript
row.className = 'field-actions';
rev.className = 'button secondary';
cp.className = 'button secondary';
```

Keep the input dataset, password masking, clipboard call, and event handlers unchanged.

- [ ] **Step 5: Update drawer mode copy only**

Replace the existing drawer-title assignment in `openDrawer` with:

```javascript
$('#drawer-kicker').textContent = name ? 'Edit secret' : 'Create secret';
$('#drawer-title').textContent = name || 'New secret';
$('#save').textContent = name ? 'Save changes' : 'Create secret';
```

Do not change when the drawer is hidden/shown or any generation check.

- [ ] **Step 6: Add control, button, and drawer styling**

Append:

```css
.button { min-height:2.375rem; display:inline-flex; align-items:center; justify-content:center; gap:.4rem;
  padding:.45rem .8rem; border:1px solid var(--color-border); border-radius:var(--radius-control);
  color:var(--color-text); background:var(--color-surface); cursor:pointer; font-weight:680;
  box-shadow:0 1px 2px rgb(24 34 28 / 5%); }
.button:hover:not(:disabled) { border-color:color-mix(in srgb, var(--color-accent) 55%, var(--color-border)); }
.button.primary { color:#fff; border-color:var(--color-accent); background:var(--color-accent);
  box-shadow:0 5px 12px rgb(33 100 70 / 20%); }
.button.primary:hover:not(:disabled) { background:var(--color-accent-hover); }
.button.ghost { border-color:transparent; background:transparent; box-shadow:none; }
.button.danger { color:var(--color-danger); border-color:color-mix(in srgb, var(--color-danger) 26%, var(--color-border));
  background:var(--color-danger-quiet); }
button:disabled { cursor:not-allowed; opacity:.58; }
button.pending:disabled { cursor:wait; opacity:.72; }
input, select, textarea { width:100%; color:var(--color-text); background:var(--color-surface);
  border:1px solid var(--color-border); border-radius:var(--radius-control); }
input, select { min-height:2.375rem; padding:.45rem .65rem; }
textarea { padding:.65rem; resize:vertical; }
input:focus-visible, select:focus-visible, textarea:focus-visible { outline:0; border-color:var(--color-accent); box-shadow:var(--focus-ring); }
#drawer { position:fixed; z-index:20; inset:0 0 0 auto; width:min(29rem, 100vw); height:100dvh;
  padding:0; overflow:auto; color:var(--color-text); background:var(--color-surface); border-left:1px solid var(--color-border);
  box-shadow:-12px 0 36px rgb(18 30 22 / 16%); }
.drawer-header { padding:1.4rem 1.5rem 1.1rem; border-bottom:1px solid var(--color-border); }
.drawer-header h2 { margin:.25rem 0 .15rem; font-size:1.35rem; letter-spacing:-.035em; }
.drawer-header p { margin:0; color:var(--color-text-muted); font-size:.78rem; }
.drawer-body { padding:1.25rem 1.5rem 1.5rem; }
.form-field { display:block; margin-bottom:1rem; }
.field-label { display:flex; justify-content:space-between; gap:1rem; margin-bottom:.35rem;
  color:var(--color-text); font-size:.78rem; font-weight:680; }
.field-hint { color:var(--color-text-muted); font-weight:520; }
.field-actions { display:flex; gap:.5rem; margin-top:.5rem; }
.drawer-footer { position:sticky; bottom:0; display:flex; align-items:center; gap:.5rem; padding:.8rem 1.5rem;
  border-top:1px solid var(--color-border); background:color-mix(in srgb, var(--color-surface-subtle) 94%, transparent); }
.drawer-footer-spacer { flex:1; }
#record-type { margin-bottom:.75rem; color:var(--color-text-muted); font-size:.75rem; }
```

- [ ] **Step 7: Run drawer regressions and commit**

```bash
cargo test --features ui web::tests::ui_has_structured_drawer_and_button_hierarchy -- --exact
cargo test --features ui web::tests::ui_guards_drawer -- --nocapture
cargo test --features ui web::tests::ui_resets_secret_delete_confirmation_on_drawer_transitions -- --exact
git add src/web/mod.rs src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css
git commit -m "style(ui): polish secret editor drawer"
```

Expected: all focused tests PASS. The `ui_guards_drawer` filter runs both stale-continuation tests.

---

### Task 5: Unify toolbars, selection, file upload, and feedback components

**Files:**
- Modify: `src/web/mod.rs:172-400`
- Modify: `src/web/assets/index.html:27-61,94`
- Modify: `src/web/assets/app.js:40-47,339-369,913-923`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Consumes: Task 4 button variants, existing toolbar IDs, selection controls, file upload input/handlers, toast timer, and pending-action helpers.
- Produces: `.toolbar`, `.search-field`, `.bulk-toolbar`, `.dropzone-content`, `.toast.success`, `.toast.error`, and consistent classes on dynamic field/file action buttons.

- [ ] **Step 1: Write the failing component-state contract test**

Add:

```rust
#[test]
fn ui_unifies_actions_upload_and_feedback_components() {
    for marker in [
        "class=\"search-field\"",
        "class=\"dropzone-content\"",
        "role=\"status\"",
        "aria-live=\"polite\"",
    ] {
        assert!(INDEX_HTML.contains(marker), "missing {marker}");
    }
    assert!(APP_JS.contains("t.className = `toast ${isError ? 'error' : 'success'}`"));
    assert!(APP_JS.contains("t.replaceChildren(icon(isError ? 'alert' : 'check')"));
    assert!(APP_JS.contains("dl.className = 'button secondary compact'"));
    assert!(APP_JS.contains("dl.prepend(icon('download'))"));
    assert!(APP_JS.contains("del.className = 'button danger compact'"));
    assert!(STYLE_CSS.contains(".bulk-toolbar {"));
    assert!(STYLE_CSS.contains(".dropzone-content {"));
    assert!(STYLE_CSS.contains(".toast.success {"));
}
```

- [ ] **Step 2: Run the test and verify RED**

```bash
cargo test --features ui web::tests::ui_unifies_actions_upload_and_feedback_components -- --exact
```

Expected: FAIL with `missing class="search-field"`.

- [ ] **Step 3: Apply semantic classes to static controls**

Use this secrets toolbar:

```html
<div class="toolbar">
  <label class="search-field"><span class="sr-only">Search secrets</span><svg class="icon" aria-hidden="true"><use href="#icon-search"></use></svg><input id="search" type="search" placeholder="Search by name, folder, group, or note"></label>
  <button id="select-secrets" class="button secondary" type="button">Select</button>
  <button id="new-secret" class="button primary" type="button"><svg class="icon" aria-hidden="true"><use href="#icon-plus"></use></svg>New secret</button>
</div>
```

Add `class="button primary"` to bulk Move, `class="button danger"` to bulk Delete, and `class="button ghost"` to bulk Cancel buttons. Apply `class="button secondary"` to `#select-files`.

Replace the dropzone contents with:

```html
<div id="dropzone">
  <div class="dropzone-content">
    <span class="dropzone-icon"><svg class="icon" aria-hidden="true"><use href="#icon-upload"></use></svg></span>
    <span><strong>Drop files here to upload</strong><small>or <label class="linkish">browse from your computer<input id="file-input" type="file" multiple hidden></label></small></span>
  </div>
</div>
```

Replace the toast with:

```html
<div id="toast" class="toast" role="status" aria-live="polite" hidden></div>
```

- [ ] **Step 4: Apply variants to dynamic controls and toast state**

In `toast`, replace the existing `t.textContent` and class assignments with:

```javascript
t.replaceChildren(icon(isError ? 'alert' : 'check'), document.createTextNode(msg));
t.className = `toast ${isError ? 'error' : 'success'}`;
t.setAttribute('role', isError ? 'alert' : 'status');
```

In `fieldRow`, keep the Task 4 assignments. In `fileRow`, add:

```javascript
dl.className = 'button secondary compact';
dl.prepend(icon('download'));
```

and replace the delete class assignment with:

```javascript
del.className = 'button danger compact';
```

Do not replace text labels with icon-only controls; the accessible action names remain visible.

- [ ] **Step 5: Add the unified component CSS**

Append:

```css
.toolbar { display:flex; align-items:center; gap:.55rem; margin-bottom:.875rem; }
.toolbar-spacer { flex:1; }
.search-field { min-height:2.375rem; flex:1; display:flex; align-items:center; gap:.55rem; padding:0 .7rem;
  border:1px solid var(--color-border); border-radius:var(--radius-control); background:var(--color-surface);
  box-shadow:0 1px 2px rgb(24 34 28 / 5%); }
.search-field:focus-within { border-color:var(--color-accent); box-shadow:var(--focus-ring); }
.search-field > .icon { color:var(--color-text-muted); }
#search { min-height:0; padding:0; border:0; border-radius:0; background:transparent; box-shadow:none; }
#search:focus-visible { box-shadow:none; }
.button.compact { min-height:2rem; padding:.3rem .6rem; font-size:.75rem; }
.bulk-toolbar { display:flex; flex-wrap:wrap; align-items:center; gap:.5rem; margin-bottom:.75rem; padding:.6rem .7rem;
  border:1px solid color-mix(in srgb, var(--color-accent) 32%, var(--color-border));
  border-radius:.625rem; background:var(--color-accent-quiet); box-shadow:0 4px 12px rgb(33 76 50 / 6%); }
.bulk-toolbar > span { min-width:6.5rem; color:var(--color-accent-hover); font-weight:700; }
.bulk-toolbar input { min-width:10rem; flex:1; }
.selection-column { width:2.75rem; text-align:center; padding-inline:.5rem !important; }
.selection-column input { width:1rem; height:1rem; margin:0; accent-color:var(--color-accent); }
tr.selected-row { background:color-mix(in srgb, var(--color-accent) 11%, transparent); }
.file-actions { display:flex; justify-content:flex-end; flex-wrap:wrap; gap:.35rem; }
#dropzone { margin-bottom:1rem; padding:1.25rem; border:1.5px dashed color-mix(in srgb, var(--color-text-muted) 52%, var(--color-border));
  border-radius:var(--radius-surface); background:var(--color-surface-subtle); transition:border-color .15s ease, background .15s ease; }
#dropzone.over { border-color:var(--color-accent); background:var(--color-accent-quiet); }
.dropzone-content { display:flex; align-items:center; justify-content:center; gap:.75rem; color:var(--color-text-muted); }
.dropzone-content strong, .dropzone-content small { display:block; }
.dropzone-content strong { color:var(--color-text); }
.dropzone-icon { width:2.4rem; height:2.4rem; display:grid; place-items:center; border-radius:.625rem;
  color:var(--color-accent); background:var(--color-accent-quiet); }
.linkish { color:var(--color-accent); cursor:pointer; text-decoration:underline; text-underline-offset:.15em; }
.toast { position:fixed; z-index:30; right:1rem; bottom:1rem; left:auto; transform:none;
  display:flex; align-items:center; gap:.5rem;
  max-width:min(24rem, calc(100vw - 2rem));
  padding:.65rem .8rem; border:1px solid var(--color-border); border-radius:var(--radius-control);
  background:var(--color-surface); box-shadow:0 10px 28px rgb(24 34 28 / 18%); }
.toast.success { color:var(--color-accent-hover); border-color:color-mix(in srgb, var(--color-accent) 32%, var(--color-border)); }
.toast.error { color:var(--color-danger); border-color:color-mix(in srgb, var(--color-danger) 32%, var(--color-border));
  background:var(--color-danger-quiet); }
```

- [ ] **Step 6: Run state regressions and commit**

```bash
cargo test --features ui web::tests::ui_unifies_actions_upload_and_feedback_components -- --exact
cargo test --features ui web::tests::ui_file_actions_are_named_and_delete_is_confirmed -- --exact
cargo test --features ui web::tests::ui_delete_buttons_enter_non_repeatable_pending_state -- --exact
cargo test --features ui web::tests::ui_bulk -- --nocapture
git add src/web/mod.rs src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css
git commit -m "style(ui): unify actions and feedback"
```

Expected: all tests PASS. Visible action labels, two-click deletion, pending locks, and bounded bulk operations remain unchanged.

---

### Task 6: Complete dark mode, responsive behavior, accessibility, and verification

**Files:**
- Modify: `src/web/mod.rs:172-400`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Consumes: all components from Tasks 1–5.
- Produces: complete dark tokens; `:focus-visible`; tablet layout at `48rem`; phone layout at `34rem`; column-priority classes `.column-groups`, `.column-note`, and `.column-file-type`; full-width phone drawer; reduced-motion overrides.

- [ ] **Step 1: Write the failing responsive/accessibility contract test**

Add:

```rust
#[test]
fn ui_has_dark_responsive_and_accessible_visual_rules() {
    for marker in [
        "class=\"column-groups\"",
        "class=\"column-note\"",
        "class=\"column-file-type\"",
        "aria-label=\"Vault content\"",
        "aria-labelledby=\"drawer-title\"",
    ] {
        assert!(INDEX_HTML.contains(marker), "missing {marker}");
    }
    for rule in [
        "@media (prefers-color-scheme: dark)",
        "@media (max-width: 48rem)",
        "@media (max-width: 34rem)",
        "@media (prefers-reduced-motion: reduce)",
        ":focus-visible",
        ".column-groups",
        ".column-note",
        ".column-file-type",
    ] {
        assert!(STYLE_CSS.contains(rule), "missing {rule}");
    }
}
```

- [ ] **Step 2: Run the test and verify RED**

```bash
cargo test --features ui web::tests::ui_has_dark_responsive_and_accessible_visual_rules -- --exact
```

Expected: FAIL with `missing class="column-groups"`.

- [ ] **Step 3: Mark lower-priority columns explicitly**

In the secrets header, set `class="column-groups"` on the Groups `<th>` and `class="column-note"` on the Note `<th>`.

In the `secretRow` cell loop, add:

```javascript
if (index === 2) td.classList.add('column-groups');
if (index === 3) td.classList.add('column-note');
```

File Type is lower priority at phone widths. Add `class="column-file-type"` to the Type `<th>`, and in `fileRow` add:

```javascript
if (index === 2) td.classList.add('column-file-type');
```

Add `"class=\"column-file-type\""` and `".column-file-type"` to the contract test's corresponding marker/rule lists.

- [ ] **Step 4: Add dark-theme tokens**

Append:

```css
@media (prefers-color-scheme: dark) {
  :root {
    --color-canvas:#121814;
    --color-surface:#19211c;
    --color-surface-subtle:#202922;
    --color-text:#dce5df;
    --color-text-muted:#9aa79e;
    --color-border:#344138;
    --color-accent:#65c68e;
    --color-accent-hover:#8ad9aa;
    --color-accent-quiet:#1f3428;
    --color-danger:#f18e85;
    --color-danger-quiet:#321f1e;
    --shadow-raised:0 14px 34px rgb(0 0 0 / 20%);
    --focus-ring:0 0 0 3px rgb(101 198 142 / 22%);
  }
  .brand-mark, .button.primary { color:#102218; }
}
```

The muted and border dark values are deliberately slightly lighter than the design's starting values so secondary text and boundaries pass browser contrast inspection.

- [ ] **Step 5: Add keyboard focus, tablet, phone, and reduced-motion rules**

Append:

```css
button:focus-visible, .tab:focus-visible, [role="button"]:focus-visible, .linkish:focus-visible {
  outline:2px solid var(--color-accent); outline-offset:2px;
}
@media (max-width: 48rem) {
  .app-header-inner { padding-inline:1rem; }
  .brand-name, .backend-badge { display:none; }
  .vault-context { margin-left:0; padding-left:0; border-left:0; }
  main { padding:1.5rem 1rem 2rem; }
  .view-heading { align-items:flex-start; }
  .column-note { display:none; }
}
@media (max-width: 34rem) {
  .app-header-inner { min-height:3.75rem; flex-wrap:wrap; gap:.55rem; }
  .vault-context { order:3; width:100%; }
  .vault-picker { width:100%; }
  .vault-picker select { max-width:none; }
  .view-heading { display:block; }
  .item-count { display:block; margin-top:.5rem; }
  .toolbar { flex-wrap:wrap; }
  .search-field { flex-basis:100%; }
  .bulk-toolbar input { flex-basis:100%; }
  .column-groups, .column-file-type { display:none; }
  .data-surface th, .data-surface td { padding-inline:.65rem; }
  .list-summary { justify-content:flex-start; }
  #drawer { width:100vw; border-left:0; }
  .drawer-footer { padding-inline:1rem; }
  .dropzone-content { align-items:flex-start; justify-content:flex-start; }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { scroll-behavior:auto !important; transition-duration:.01ms !important;
    animation-duration:.01ms !important; animation-iteration-count:1 !important; }
  .skeleton-row span { animation:none; }
}
```

- [ ] **Step 6: Run automated verification**

Run:

```bash
cargo fmt --check
cargo test --features ui web::tests
cargo clippy --features ui --all-targets -- -D warnings
cargo test --features ui
git diff --check
```

Expected: every command exits 0 with no warnings, failures, or whitespace errors.

- [ ] **Step 7: Perform the exact browser verification matrix**

Start the UI without opening an extra browser window:

```bash
cargo run --features ui -- ui --no-open
```

Expected: the terminal prints `xv ui listening at http://127.0.0.1:<port>/?token=<token>` and remains running.

Open the printed URL and verify each row of this matrix:

| Viewport/theme | Required checks |
| --- | --- |
| 1440×900 light | Complete header context, five secret metadata columns, 40–45px rows, aligned toolbar/table, fixed drawer |
| 1440×900 dark | No flat-black surfaces, readable muted text, visible borders, green focus/primary states, readable danger states |
| 768×900 light | Product label and backend badge hidden, vault/nav usable, Note column hidden, no page-level horizontal scroll |
| 390×844 light | Search occupies first toolbar line, bulk input stacks, Groups/Note/File Type hidden, drawer fills width |
| 390×844 dark | Same responsive behavior with readable surfaces, focus, tags, selection, errors, and drawer controls |

At both desktop and phone widths, exercise these workflows in order:

1. Switch vaults and tabs; confirm the drawer closes and selection clears as before.
2. Filter secrets; confirm matching rows and “No matching secrets” state.
3. Create a secret, edit it, reveal/copy its value, save it, and close the drawer.
4. Enter selection mode, use visible-only select-all, bulk-move secrets, and exercise two-click bulk delete without completing a destructive delete unless test data is disposable.
5. Upload a disposable file, download it, and exercise the two-click file delete.
6. Tab through all controls; confirm visible focus and logical order.
7. Enable reduced motion in browser devtools; confirm the progress and skeleton indicators become effectively static.
8. Open the page without a token; confirm the recovery page remains persistent and correctly styled.

Stop the server with Ctrl-C after verification.

- [ ] **Step 8: Review the final diff and commit**

Run:

```bash
git status --short
git diff --stat
git diff -- src/web/assets/index.html src/web/assets/style.css src/web/assets/app.js src/web/mod.rs
```

Expected: only the four planned files are modified; there are no dependency, generated-browser, token, or unrelated changes.

Commit:

```bash
git add src/web/mod.rs src/web/assets/index.html src/web/assets/app.js src/web/assets/style.css
git commit -m "style(ui): finish responsive visual refresh"
```

---

## Completion Criteria

- All six task commits exist and the working tree is clean.
- `cargo fmt --check`, `cargo clippy --features ui --all-targets -- -D warnings`, and `cargo test --features ui` pass.
- The complete browser matrix passes in automatic light and dark themes.
- Existing `sessionStorage` assertions still pass and `localStorage` remains absent.
- No API route, Rust state model, dependency manifest, or frontend build tooling changes.
- The implementation matches `docs/superpowers/specs/2026-07-14-web-ui-visual-refresh-design.md`.
