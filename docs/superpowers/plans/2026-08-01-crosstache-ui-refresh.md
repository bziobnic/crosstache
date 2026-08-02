# Crosstache Refined Command Center Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current split rail/tab shell with the approved Refined Command Center while preserving Crosstache's existing vault workflows, security behavior, and responsive content model.

**Architecture:** Keep the embedded, dependency-free ES-module frontend and its existing store/API boundaries. Move the single ARIA tab set into the context rail, add pure context and disclosure helpers, reuse existing search/filter/tree-grid state, and express desktop/mobile differences through one DOM tree plus responsive CSS. No Rust endpoint or domain behavior changes.

**Tech Stack:** Vanilla HTML/CSS/ES modules, Node's built-in test runner, Playwright, axe-core, Rust/Axum embedded assets, Tauri desktop shell.

## Global Constraints

- The shared embedded UI must continue to work in both `xv ui` and the desktop app.
- Widths above 768px use the tree grid; widths at or below 768px use stacked rows.
- Secrets, Files, and Trash must have exactly one focusable ARIA tab set at every width.
- Keep the native workspace `<select>` and existing guarded context-switch workflow.
- Keep existing semantic light/dark tokens and explicit/system theme preferences.
- Keep dirty-draft guards, protected-value timers, clipboard ownership checks, Trash, upload, typed-record, error, and selection behavior functionally equivalent.
- Do not add a production frontend dependency, build step, API endpoint, backend operation, or secret-bearing preference.
- The bearer token remains in per-tab `sessionStorage`; secret values travel only in authenticated request bodies.
- No horizontal overflow at 1180×760, 820×560, 768×700, or 390×844.
- Light and dark states must have no serious or critical axe findings and must honor reduced motion.
- Preserve the user's unrelated `package-lock.json` modification.

---

## File Structure

- `src/web/assets/index.html` — single shell DOM, rail/bottom tab set, context card, filter disclosures, headings, and drawer groups.
- `src/web/assets/style.css` — desktop rail, responsive chrome, controls, editor, operational states, and themes.
- `src/web/assets/accessibility.js` — dynamic tab orientation and generic disclosure semantics.
- `src/web/assets/context.js` — pure context summaries and context rendering.
- `src/web/assets/app.js` — shell wiring only.
- `src/web/assets/files.js` — filter-controller interface.
- `src/web/assets/secrets.js` — counts, quick access, sort direction, disclosure wiring, and view rendering.
- `src/web/assets/*.test.js` — pure and mounted frontend contracts.
- `tests/web/*.spec.js` — browser behavior, accessibility, responsiveness, and visual snapshots.
- `docs/web-ui.md` and `docs/APP-UX-IMPLEMENTATION-EVIDENCE.md` — user guidance and final evidence.

---

### Task 1: Establish the Single Semantic Application Shell

**Files:**
- Modify: `src/web/assets/accessibility.js:25-130`
- Modify: `src/web/assets/accessibility.test.js:1-145`
- Modify: `src/web/assets/index.html:10-66`
- Modify: `src/web/assets/app.js:35-50`
- Modify: `src/web/assets/app.dom.test.js:1-190`
- Modify: `src/web/assets/style.css:1-105`

**Interfaces:**
- Consumes: existing `mountRovingFocus()` and `mountTabs()` behavior.
- Produces: `mountRovingFocus(container, selector, { orientation })` and `mountTabs(tablist, { orientation })`, where `orientation` is a string or a function returning `'horizontal' | 'vertical'`.
- Produces: one `#vault-tabs` inside `#context-rail`; existing tab IDs remain unchanged.

- [ ] **Step 1: Write the failing dynamic-orientation test**

Add to `accessibility.test.js`:

```js
test('mountTabs reads its current orientation for every arrow key', () => {
  const { document, tablist, tabs } = tabFixture();
  let orientation = 'vertical';
  const mounted = mountTabs(tablist, { orientation: () => orientation });
  tabs[0].focus();
  assert.equal(key(tablist, tabs[0], 'ArrowDown'), true);
  assert.equal(document.activeElement, tabs[1]);
  assert.equal(key(tablist, tabs[1], 'ArrowRight'), false);
  orientation = 'horizontal';
  assert.equal(key(tablist, tabs[1], 'ArrowRight'), true);
  assert.equal(document.activeElement, tabs[2]);
  mounted.destroy();
});
```

- [ ] **Step 2: Run it and verify RED**

Run: `node --test src/web/assets/accessibility.test.js`

Expected: FAIL because the current implementation ignores the options argument.

- [ ] **Step 3: Implement dynamic orientation**

In `accessibility.js`, resolve orientation inside every keydown:

```js
function resolvedOrientation(orientation) {
  const value = typeof orientation === 'function' ? orientation() : orientation;
  return value === 'vertical' ? 'vertical' : 'horizontal';
}

function directions(orientation) {
  return resolvedOrientation(orientation) === 'vertical'
    ? new Map([['ArrowDown', 1], ['ArrowUp', -1]])
    : new Map([['ArrowRight', 1], ['ArrowLeft', -1]]);
}
```

Change `mountRovingFocus` to call `directions(orientation)` inside `onKeydown`, and change `mountTabs` to accept `{ orientation = 'horizontal' }` and pass it to `mountRovingFocus`. Preserve `sync()` and capability-loss fallback behavior exactly.

- [ ] **Step 4: Write the failing static shell contract**

Add to `app.dom.test.js`:

```js
test('the command-center shell owns one tab set inside the context rail', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  const railStart = html.indexOf('<aside id="context-rail"');
  const railEnd = html.indexOf('</aside>', railStart);
  const tabsStart = html.indexOf('<nav id="vault-tabs"', railStart);
  assert.ok(tabsStart > railStart && tabsStart < railEnd);
  assert.equal((html.match(/id="vault-tabs"/g) || []).length, 1);
  assert.equal((html.match(/role="tab"/g) || []).length, 3);
  assert.match(html, /class="context-rail-top"/);
  assert.match(html, /class="context-rail-footer"/);
});
```

- [ ] **Step 5: Run it and verify RED**

Run: `node --test src/web/assets/app.dom.test.js`

Expected: FAIL because the tabs remain in `#app-header`.

- [ ] **Step 6: Move existing controls into one shell**

In `index.html`, wrap existing context content in `.context-rail-top`, move the existing `#vault-tabs` immediately after it, and wrap `.context-actions` plus `#context-version` in `.context-rail-footer`. Add count spans without renaming tab IDs:

```html
<button id="tab-secrets" class="tab active" type="button" role="tab" aria-selected="true" aria-controls="secrets-view">Secrets <span id="tab-secret-count" class="rail-count"></span></button>
<button id="tab-files" class="tab" type="button" role="tab" aria-selected="false" aria-controls="files-view" tabindex="-1">Files <span id="tab-file-count" class="rail-count"></span></button>
<button id="tab-trash" class="tab" type="button" role="tab" aria-selected="false" aria-controls="trash-view" tabindex="-1">Trash <span id="tab-trash-count" class="rail-count"></span></button>
```

Replace the old header tab strip with:

```html
<div class="app-header-inner">
  <span id="top-context-line" class="top-context-line" data-context-copy></span>
  <button id="top-command-open" class="command-trigger" type="button" aria-label="Open Commands">Search or run a command <kbd>⌘K</kbd></button>
</div>
```

- [ ] **Step 7: Wire the single tab set and command delegate**

In `app.js`:

```js
const tabs = mountTabs(document.getElementById('vault-tabs'), {
  orientation: () => (
    globalThis.matchMedia?.('(max-width: 48rem)').matches ? 'horizontal' : 'vertical'
  ),
});
document.getElementById('top-command-open').onclick = () => {
  document.getElementById('commands-open').click();
};
```

- [ ] **Step 8: Implement desktop shell styling**

Use this desktop geometry and remove the old segmented-tab declarations rather than overriding them later:

```css
body { grid-template-columns:13rem minmax(0, 1fr); grid-template-rows:auto 1fr; }
.context-rail { grid-column:1; grid-row:1 / span 2; }
.context-rail-top { display:grid; gap:1rem; }
.context-rail-footer { display:grid; gap:.75rem; margin-top:auto; }
.tab-list { display:grid; gap:.25rem; margin:1rem 0; }
.tab {
  width:100%; min-height:2.35rem; display:grid;
  grid-template-columns:1fr auto; align-items:center;
  padding:.45rem .65rem; border:0; border-radius:var(--radius-control);
  color:#bcd2c5; background:transparent; text-align:left;
}
.tab[aria-selected="true"] { color:#fff; background:#285943; box-shadow:none; }
```

- [ ] **Step 9: Run focused tests**

Run:

```bash
node --test src/web/assets/accessibility.test.js src/web/assets/app.dom.test.js src/web/assets/context.test.js
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add src/web/assets/accessibility.js src/web/assets/accessibility.test.js src/web/assets/index.html src/web/assets/app.js src/web/assets/app.dom.test.js src/web/assets/style.css
git commit -m "feat(web): establish command center shell"
```

---

### Task 2: Render Compact Context, Counts, and Quick Access

**Files:**
- Modify: `src/web/assets/context.js:20-205`
- Modify: `src/web/assets/context.test.js:1-210`
- Modify: `src/web/assets/index.html:13-65`
- Modify: `src/web/assets/app.dom.test.js:1-220`
- Modify: `src/web/assets/secrets.js:100-145, 800-865, 1985-2050, 2335-2375, 3460-3510`
- Modify: `src/web/assets/secrets.routes.test.js:680-750`
- Modify: `tests/web/ui-context.spec.js:1-100`
- Modify: `tests/web/ui-navigation.spec.js:1-70`

**Interfaces:**
- Produces: `contextSummary(context) -> { workspace, destination, backend, connection }`.
- Produces: `setSort(kind, key, direction = null)` and `setNavigationCount(kind, count, state = 'ready')`.

- [ ] **Step 1: Write the failing context-summary test**

```js
test('context summary separates workspace, destination, backend, and connection', () => {
  assert.deepEqual(contextSummary(primary), {
    workspace: 'work',
    destination: 'checkout',
    backend: 'az-prod',
    connection: 'connected',
  });
});
```

- [ ] **Step 2: Run it and verify RED**

Run: `node --test src/web/assets/context.test.js`

Expected: FAIL because `contextSummary` is not exported.

- [ ] **Step 3: Implement the pure summary and render it**

```js
export function contextSummary(context) {
  return Object.freeze({
    workspace: context?.workspace?.alias || 'Default',
    destination: named(context?.project) || named(context?.vault) || 'No vault',
    backend: named(context?.backend) || 'Unknown backend',
    connection: context?.connection?.state || 'unknown',
  });
}
```

Add `#context-workspace-name` and `#context-destination` to the rail card. Replace Task 1's temporary `#top-context-line` span with this breadcrumb and populate all five IDs in `mountContextRail().render()` while retaining the full `#context-line` for accessible description and detailed copy:

```html
<div class="top-context-line" aria-label="Effective context">
  <span id="top-context-workspace"></span>
  <span aria-hidden="true">›</span>
  <strong id="top-context-destination"></strong>
  <span id="top-context-backend" class="backend-badge"></span>
</div>
```

- [ ] **Step 4: Write failing hierarchy and mounted behavior tests**

Add to `secrets.routes.test.js`:

```js
test('navigation counts use authoritative loaded lists', async () => {
  const context = { ...routedContext('primary', 'one'), capabilities: { ...routedContext('primary', 'one').capabilities, files: true } };
  const ui = await mountRouteUi({
    withContextRail: true,
    apiImpl: async (method, requestPath) => {
      if (method === 'GET' && requestPath === '/api/context') return context;
      if (requestPath === '/api/types') return { types: [] };
      if (requestPath === '/api/vaults') return { vaults: [{ name: 'one' }] };
      if (requestPath.startsWith('/api/secrets?')) return [
        { name: 'older', updated_on: '2026-01-01T00:00:00Z' },
        { name: 'newer', updated_on: '2026-07-31T00:00:00Z' },
      ];
      if (requestPath.startsWith('/api/files?')) return [{ name: 'notes.txt', size: 4 }];
      return [];
    },
  });
  assert.equal(ui.elements.get('#tab-secret-count').textContent, '2');
  assert.equal(ui.elements.get('#tab-file-count').textContent, '1');
  ui.restore();
});
```

Add this static contract to `app.dom.test.js`:

```js
test('each content view has one heading action and one dominant search control', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  const secrets = html.slice(html.indexOf('<section id="secrets-view"'), html.indexOf('<section id="files-view"'));
  const files = html.slice(html.indexOf('<section id="files-view"'), html.indexOf('<section id="trash-view"'));
  const trash = html.slice(html.indexOf('<section id="trash-view"'), html.indexOf('</main>'));
  assert.match(secrets, /class="view-heading"[\s\S]*id="new-secret"/);
  assert.match(files, /class="view-heading"[\s\S]*id="browse-files-header"/);
  assert.doesNotMatch(trash.match(/class="view-heading"[\s\S]*?<\/div>/)?.[0] || '', /button/);
  assert.equal((secrets.match(/class="search-field"/g) || []).length, 1);
  assert.equal((files.match(/class="search-field"/g) || []).length, 1);
});
```

- [ ] **Step 5: Run it and verify RED**

Run: `node --test src/web/assets/app.dom.test.js src/web/assets/secrets.routes.test.js`

Expected: FAIL because counts/quick access are not wired and primary actions still live outside the headings.

- [ ] **Step 6: Implement counts and exact sort direction**

```js
function setSort(kind, key, direction = null) {
  const state = tableSort[kind];
  if (direction === 'asc' || direction === 'desc') {
    state.key = key;
    state.direction = direction;
  } else if (state.key === key) {
    state.direction = state.direction === 'asc' ? 'desc' : 'asc';
  } else {
    state.key = key;
    state.direction = 'asc';
  }
  syncSortHeaders(kind);
  renderSelectionKind(kind);
}

function setNavigationCount(kind, count, state = 'ready') {
  const ids = { secrets: '#tab-secret-count', files: '#tab-file-count', trash: '#tab-trash-count' };
  const target = $(ids[kind]);
  if (!target) return;
  target.textContent = state === 'ready' ? String(count) : '';
  target.dataset.state = state;
}
```

Call it from `setListSummary`, `setListLoadStatus`, and `renderTrash`; clear unavailable capability counts.

In `index.html`, place `#new-secret` in the Secrets `.view-heading`, add one-sentence `.view-description` copy beneath each heading, and add a primary `#browse-files-header` button to the Files heading. Keep Trash without a creation action. Reorder each ordinary toolbar as search, Filters placeholder, Select, then compact Refresh; Task 3 supplies the working Filters disclosure.

- [ ] **Step 7: Add and wire quick access**

Add `#quick-recent` under a “Quick access” label in the rail. Wire:

```js
$('#quick-recent').onclick = () => {
  switchTab('secrets');
  setSort('secrets', 'updated', 'desc');
};
```

Wire the Files heading action through the existing browse control:

```js
$('#browse-files-header').onclick = () => $('#browse-files').click();
```

- [ ] **Step 8: Add browser assertions**

In `ui-context.spec.js`, assert compact context parts and the full context line. In `ui-navigation.spec.js`, use ArrowDown/ArrowUp at 900px and add:

```js
await page.route('**/api/secrets?*', async (route) => route.fulfill({ json: [
  { name: 'older', updated_on: '2026-01-01T00:00:00Z' },
  { name: 'newer', updated_on: '2026-07-31T00:00:00Z' },
] }));
await page.goto(baseURL);
await page.locator('#quick-recent').click();
await expect(page.locator('#secrets-table tbody tr.tree-row-item .item-name-content strong'))
  .toHaveText(['newer', 'older']);
```

Preserve Home/End assertions in the tab test.

- [ ] **Step 9: Run focused gates**

```bash
node --test src/web/assets/app.dom.test.js src/web/assets/context.test.js src/web/assets/files.test.js src/web/assets/secrets.routes.test.js
npx playwright test tests/web/ui-context.spec.js tests/web/ui-navigation.spec.js
```

Expected: PASS with no serious/critical axe findings.

- [ ] **Step 10: Commit**

```bash
git add src/web/assets/context.js src/web/assets/context.test.js src/web/assets/index.html src/web/assets/app.dom.test.js src/web/assets/secrets.js src/web/assets/secrets.routes.test.js tests/web/ui-context.spec.js tests/web/ui-navigation.spec.js
git commit -m "feat(web): render command center context"
```

---

### Task 3: Progressively Disclose Filters and Tree Controls

**Files:**
- Modify: `src/web/assets/accessibility.js:1-160`
- Modify: `src/web/assets/accessibility.test.js:1-170`
- Modify: `src/web/assets/files.js:940-1035`
- Modify: `src/web/assets/files.test.js`
- Modify: `src/web/assets/index.html:72-155`
- Modify: `src/web/assets/secrets.js:650-715`
- Modify: `src/web/assets/secrets.routes.test.js:680-750`
- Modify: `src/web/assets/style.css:360-405`
- Modify: `tests/web/ui-folders.spec.js:85-160`

**Interfaces:**
- Produces: `mountDisclosure(trigger, panel, { initialOpen, onChange }) -> { isOpen(), setOpen(open), destroy() }`.
- Produces: `mountFilterControls(...).setValue(key, value) -> boolean`.

- [ ] **Step 1: Write the failing disclosure test**

```js
test('mountDisclosure synchronizes hidden and aria-expanded', () => {
  const trigger = new TestElement('secret-filters-toggle');
  const panel = new TestElement('secret-filter-controls', { hidden: true });
  const mounted = mountDisclosure(trigger, panel);
  assert.equal(trigger.getAttribute('aria-expanded'), 'false');
  trigger.click();
  assert.equal(panel.hidden, false);
  assert.equal(trigger.getAttribute('aria-expanded'), 'true');
  mounted.setOpen(false);
  assert.equal(panel.hidden, true);
  mounted.destroy();
});
```

- [ ] **Step 2: Run it and verify RED**

Run: `node --test src/web/assets/accessibility.test.js`

Expected: FAIL because `mountDisclosure` does not exist.

- [ ] **Step 3: Implement disclosure semantics**

```js
export function mountDisclosure(trigger, panel, {
  initialOpen = false,
  onChange = () => {},
} = {}) {
  let open = Boolean(initialOpen);
  function setOpen(value) {
    open = Boolean(value);
    panel.hidden = !open;
    trigger.setAttribute('aria-expanded', String(open));
    trigger.setAttribute('aria-controls', panel.id);
    onChange(open);
  }
  const toggle = () => setOpen(!open);
  trigger.addEventListener('click', toggle);
  setOpen(open);
  return Object.freeze({
    isOpen: () => open,
    setOpen,
    destroy() { trigger.removeEventListener('click', toggle); },
  });
}
```

- [ ] **Step 4: Write the failing filter-controller test**

Add `mountFilterControls` to the `files.test.js` import and add this self-contained fixture/test:

```js
test('filter controller sets a programmatic value through the normal change path', () => {
  const elements = new Map();
  const document = {
    querySelector(selector) { return elements.get(selector); },
    createElement() { return { value: '', textContent: '', setAttribute() {} }; },
  };
  const element = (selector, children = []) => {
    const value = {
      value: '', hidden: false, children, ownerDocument: document,
      replaceChildren(...next) { this.children = next; },
    };
    elements.set(selector, value);
    return value;
  };
  const expiry = element('#secret-filter-expiry', [{ value: '', textContent: 'Any expiry' }]);
  element('#secret-filter-chips');
  element('#secret-filters-clear');
  const filters = { expiry: '' };
  const changes = [];
  const mounted = mountFilterControls({
    document,
    surface: 'secret',
    filters,
    labels: { expiry: 'Expiry' },
    keys: ['expiry'],
    onChange: () => changes.push({ ...filters }),
  });
  assert.equal(mounted.setValue('expiry', 'expiring'), true);
  assert.equal(expiry.value, 'expiring');
  assert.deepEqual(changes, [{ expiry: 'expiring' }]);
  assert.equal(mounted.setValue('missing', 'value'), false);
});
```

- [ ] **Step 5: Implement `setValue`**

Add to the frozen controller in `files.js`:

```js
setValue(key, value) {
  const control = controls.get(key);
  if (!control) return false;
  control.value = value == null ? '' : String(value);
  filters[key] = readControl(key, control);
  onChange();
  return true;
},
```

- [ ] **Step 6: Add hidden inline filter panels**

Add `#secret-filters-toggle` and `#file-filters-toggle` to their toolbars. Mark existing filter-control groups `hidden` initially. Move existing Expand all, Collapse all, and Clear filters buttons into `.filter-panel-footer` inside the appropriate panel. Keep every existing ID.

- [ ] **Step 7: Wire disclosures and visible active counts**

Mount disclosures in `secrets.js`. After each filter render, compute `XvUiModel.activeFilterChips(listFilters[kind]).length`, update `#secret-filter-count` or `#file-filter-count`, and open the panel when the count becomes nonzero. Do not auto-close it when the final chip is removed.

Add `#quick-expiring` beside `#quick-recent` and wire it through the new controller interface:

```js
$('#quick-expiring').onclick = () => {
  switchTab('secrets');
  filterControls.secrets.setValue('expiry', 'expiring');
};
```

Extend `secrets.routes.test.js` to click `#quick-expiring` and assert `#secret-filter-expiry.value === 'expiring'`, one active filter chip, and an open filter disclosure.

- [ ] **Step 8: Update folder tests**

Open `#secret-filters-toggle` before asserting or clicking expansion controls. Assert `aria-expanded` and panel visibility before preserving all existing folder-state checks.

- [ ] **Step 9: Run focused gates**

```bash
node --test src/web/assets/accessibility.test.js src/web/assets/files.test.js src/web/assets/secrets.routes.test.js
npx playwright test tests/web/ui-folders.spec.js tests/web/ui-navigation.spec.js
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add src/web/assets/accessibility.js src/web/assets/accessibility.test.js src/web/assets/files.js src/web/assets/files.test.js src/web/assets/index.html src/web/assets/secrets.js src/web/assets/secrets.routes.test.js src/web/assets/style.css tests/web/ui-folders.spec.js
git commit -m "feat(web): disclose secondary vault controls"
```

---

### Task 4: Implement Responsive Context and Bottom Navigation

**Files:**
- Modify: `src/web/assets/index.html:10-70`
- Modify: `src/web/assets/style.css:470-525`
- Modify: `src/web/assets/accessibility.js:1-25`
- Modify: `src/web/assets/accessibility.test.js:165-195`
- Modify: `src/web/assets/commands.js:1-35, 330-370`
- Modify: `src/web/assets/commands.test.js`
- Modify: `tests/web/ui-context.spec.js:30-80`
- Modify: `tests/web/ui-responsive.spec.js:55-320`
- Modify: `tests/web/ui-navigation.spec.js:1-50`

**Interfaces:**
- Consumes: one Task 1 tab set, dynamic orientation, existing stacked-row focus mapping, and `setBackgroundInert()`.
- Produces: at or below 768px, `.context-rail-top` is compact top context and `#vault-tabs` is the only sticky bottom tab set.

- [ ] **Step 1: Write the failing responsive-shell test**

Add to `ui-responsive.spec.js`:

```js
test('the single tab set becomes bottom navigation at the content breakpoint', async ({ page, baseURL }) => {
  await page.setViewportSize({ width: 769, height: 700 });
  await page.goto(baseURL);
  const tabs = page.locator('#vault-tabs');
  await expect(page.locator('[role="tablist"]')).toHaveCount(1);
  await expect(tabs).toHaveCSS('position', 'static');
  await page.setViewportSize({ width: 768, height: 700 });
  await expect(page.locator('[role="tablist"]')).toHaveCount(1);
  await expect(tabs).toHaveCSS('position', 'sticky');
  await expect(tabs).toHaveCSS('bottom', '0px');
  await expectNoHorizontalOverflow(page);
  await expectNoSeriousOrCriticalAxeViolations(page);
});
```

- [ ] **Step 2: Run it and verify RED**

Run: `npx playwright test tests/web/ui-responsive.spec.js -g "single tab set becomes bottom navigation"`

Expected: FAIL because the rail still becomes one top block at 768px.

- [ ] **Step 3: Split the same rail DOM across narrow layout rows**

Replace the current `@media (max-width: 48rem)` shell rules with:

```css
@media (max-width:48rem) {
  body {
    display:grid;
    grid-template-columns:minmax(0, 1fr);
    grid-template-rows:auto auto minmax(0, 1fr) auto;
    min-height:100vh;
  }
  .context-rail { display:contents; }
  .context-rail-top {
    grid-row:1; display:grid;
    grid-template-columns:minmax(0, 1fr) minmax(9rem, 13rem);
    align-items:center; gap:.75rem; padding:.75rem 1rem;
    color:#f4faf6; background:#123426;
  }
  .context-rail-top .brand,
  .context-rail-top .context-status,
  .context-rail-top .context-details { display:none; }
  #app-header { grid-row:2; grid-column:1; }
  main { grid-row:3; grid-column:1; padding-bottom:1.25rem; }
  #vault-tabs {
    position:sticky; z-index:12; bottom:0; grid-row:4;
    display:grid; grid-template-columns:repeat(3, minmax(0, 1fr));
    gap:.25rem; margin:0;
    padding:.45rem max(1rem, env(safe-area-inset-right)) calc(.45rem + env(safe-area-inset-bottom)) max(1rem, env(safe-area-inset-left));
    border-top:1px solid var(--color-border);
    background:color-mix(in srgb, var(--color-surface) 96%, transparent);
    backdrop-filter:blur(12px);
  }
  .context-rail-footer { display:none; }
}
```

Add Settings and Help to `DEFAULT_COMMANDS`:

```js
Object.freeze({ id: 'open-settings', label: 'Open Settings', surface: 'application', target: 'settings-open' }),
Object.freeze({ id: 'open-help', label: 'Open Help', surface: 'application', target: 'help-open' }),
```

In command activation, after the existing `new-secret` branch:

```js
if (result.target) {
  const target = byId(result.target);
  target?.click();
  return Boolean(target);
}
```

Update the existing `command registry exposes required shortcuts` assertion in `commands.test.js` so `registry.commands()` includes `open-settings` and `open-help` with no shortcut. In `ui-context.spec.js`, open Commands, search “Open Settings,” activate that option, assert the Settings dialog is visible, close it, then repeat for “Open Help.” Do not add a fourth primary tab.

- [ ] **Step 4: Extend inert-background coverage**

Change `setBackgroundInert()` to query:

```js
'#app-header, main, .context-rail-top, #vault-tabs, .context-rail-footer'
```

Update the exact selector assertion in `accessibility.test.js`; preserve fallback `aria-hidden` behavior.

- [ ] **Step 5: Verify one tab set changes keyboard orientation**

Extend `ui-navigation.spec.js`: at 900px ArrowDown/ArrowUp move between tabs; after resizing to 390px ArrowRight/ArrowLeft move between the same elements. At every stage assert one selected tab and one `tabindex="0"`.

- [ ] **Step 6: Preserve focus and dialog behavior**

Keep the existing breakpoint focus test unchanged. Extend the phone-sheet test to assert `.context-rail-top` and `#vault-tabs` are inert while `#drawer` is open and restored after Cancel.

- [ ] **Step 7: Run responsive gates**

```bash
node --test src/web/assets/accessibility.test.js src/web/assets/commands.test.js
npx playwright test tests/web/ui-responsive.spec.js tests/web/ui-navigation.spec.js tests/web/ui-accessibility.spec.js tests/web/ui-context.spec.js
```

Expected: PASS at 769, 768, and 390px with no overflow or serious/critical axe finding.

- [ ] **Step 8: Commit**

```bash
git add src/web/assets/index.html src/web/assets/style.css src/web/assets/accessibility.js src/web/assets/accessibility.test.js src/web/assets/commands.js src/web/assets/commands.test.js tests/web/ui-responsive.spec.js tests/web/ui-navigation.spec.js tests/web/ui-context.spec.js
git commit -m "feat(web): add responsive command center chrome"
```

---

### Task 5: Apply the Editor and Operational-State Hierarchy

**Files:**
- Modify: `src/web/assets/index.html:235-340`
- Modify: `src/web/assets/style.css:245-420, 500-520`
- Modify: `src/web/assets/app.dom.test.js:1-220`
- Modify: `tests/web/ui-responsive.spec.js:290-320`
- Modify: `tests/web/ui-accessibility.spec.js`
- Modify: `tests/web/ui-errors.spec.js`
- Modify: `tests/web/ui-navigation.spec.js`

**Interfaces:**
- Consumes: existing drawer/form IDs, dialog manager, dirty guards, protected-value controls, and error panels.
- Produces: `.drawer-context-banner`, `.drawer-section[data-section]`, and `.drawer-advanced`; no renamed inputs or workflow IDs.
- Produces: drawer width `min(30rem, 100vw)` and existing full-screen behavior at 34rem and below.

- [ ] **Step 1: Write the failing static drawer contract**

Add to `app.dom.test.js`:

```js
test('the editor groups context, core fields, organization, attachments, and advanced workflows', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  const drawer = html.slice(html.indexOf('<div id="drawer"'), html.indexOf('<div id="drawer-backdrop"'));
  assert.match(drawer, /id="drawer-context"[^>]*class="drawer-context-banner"/);
  assert.match(drawer, /class="drawer-section" data-section="credentials"/);
  assert.match(drawer, /class="drawer-section" data-section="organization"/);
  assert.match(drawer, /id="attachments-section"[^>]*data-section="attachments"/);
  assert.match(drawer, /id="secret-workflows"[^>]*drawer-advanced/);
  assert.ok(drawer.indexOf('data-section="credentials"') < drawer.indexOf('data-section="organization"'));
});
```

- [ ] **Step 2: Run it and verify RED**

Run: `node --test src/web/assets/app.dom.test.js`

Expected: FAIL because the drawer body is flat.

- [ ] **Step 3: Regroup existing controls without changing IDs**

Move `#drawer-context` into the start of `.drawer-body`. Wrap Name, type picker, value section, and record section in `data-section="credentials"`; wrap Folder, Groups, Note, and Expires in `data-section="organization"`; add `data-section="attachments"` to `#attachments-section`; add `.drawer-advanced` to `#secret-workflows`. Copy all current children intact—no duplicate or abbreviated controls.

- [ ] **Step 4: Apply editor geometry**

```css
#drawer {
  position:fixed; z-index:20; inset:0 0 0 auto;
  width:min(30rem, 100vw); height:100dvh;
  display:grid; grid-template-rows:auto minmax(0, 1fr);
  overflow:hidden; color:var(--color-text); background:var(--color-surface);
  border-left:1px solid var(--color-border);
  box-shadow:-18px 0 50px rgb(18 30 22 / 18%);
}
#secret-form { min-height:0; display:grid; grid-template-rows:minmax(0, 1fr) auto; }
.drawer-body { min-height:0; overflow:auto; padding:1rem 1.5rem 1.5rem; }
.drawer-context-banner {
  margin:0 0 1rem; padding:.65rem .75rem;
  border:1px solid color-mix(in srgb, var(--color-accent) 24%, var(--color-border));
  border-radius:var(--radius-control); background:var(--color-accent-quiet);
}
.drawer-section-title {
  margin:0 0 .7rem; color:var(--color-text-muted);
  font-size:.7rem; font-weight:750; letter-spacing:.09em; text-transform:uppercase;
}
.drawer-footer { position:relative; bottom:auto; }
```

At `max-width:34rem`, keep full viewport geometry, make the footer a two-column grid, place Delete on its own row, and keep Cancel/Save visible.

- [ ] **Step 5: Add operational-state assertions**

- In `ui-navigation.spec.js`, verify selection mode shows the bulk bar and restores the normal toolbar after Cancel.
- In `ui-errors.spec.js`, verify a refresh failure preserves existing rows and `#secret-refresh-error` precedes `#secrets-workspace` in DOM order.
- In `ui-responsive.spec.js`, verify a filtered empty state offers Clear filters and not New secret.
- In `ui-accessibility.spec.js`, run axe with the regrouped editor open and retain focus restoration checks.

Use this exact error-order check:

```js
const order = await page.evaluate(() => ({
  error: [...document.querySelector('main').children].indexOf(document.querySelector('#secret-refresh-error')),
  workspace: [...document.querySelector('main').children].indexOf(document.querySelector('#secrets-workspace')),
}));
expect(order.error).toBeLessThan(order.workspace);
```

- [ ] **Step 6: Run editor and state gates**

```bash
node --test src/web/assets/app.dom.test.js src/web/assets/dialogs.test.js src/web/assets/secrets.routes.test.js
npx playwright test tests/web/ui-accessibility.spec.js tests/web/ui-errors.spec.js tests/web/ui-navigation.spec.js tests/web/ui-responsive.spec.js tests/web/ui-typed-editor.spec.js tests/web/ui-protected-values.spec.js
```

Expected: PASS with no safety, focus, or protected-value regression.

- [ ] **Step 7: Commit**

```bash
git add src/web/assets/index.html src/web/assets/style.css src/web/assets/app.dom.test.js tests/web/ui-responsive.spec.js tests/web/ui-accessibility.spec.js tests/web/ui-errors.spec.js tests/web/ui-navigation.spec.js
git commit -m "feat(web): refine editor and operational states"
```

---

### Task 6: Verify the Visual System and Document the Result

**Files:**
- Modify: `tests/web/ui-visual.spec.js:1-155`
- Modify: `tests/web/snapshots/visual-1180x760/*.png`
- Modify: `tests/web/snapshots/visual-820x560/*.png`
- Modify: `tests/web/snapshots/visual-768x700/*.png`
- Modify: `tests/web/snapshots/visual-390x844/*.png`
- Modify: `docs/web-ui.md`
- Modify: `docs/APP-UX-IMPLEMENTATION-EVIDENCE.md`

**Interfaces:**
- Consumes: all previous task behavior and the stable visual fixture.
- Produces: inspected light/dark snapshots from the four existing Playwright visual projects plus desktop/phone editor snapshots.

- [ ] **Step 1: Expand the visual matrix**

Keep the existing light/dark loop; the four projects in `playwright.config.js` already supply 1180×760, 820×560, 768×700, and 390×844. Add editor tests that run only in their intended project:

```js
test('light desktop secret editor', async ({ page, baseURL }, testInfo) => {
  test.skip(testInfo.project.name !== 'visual-1180x760');
  await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' });
  await stabilizeVisualSurface(page);
  await page.goto(baseURL);
  await seedLongNames(page);
  await page.reload();
  await page.getByRole('button', { name: `Edit secret ${visualSecrets[0].name}` }).click();
  await expectNoSeriousOrCriticalAxeViolations(page);
  await expect(page).toHaveScreenshot('light-secret-editor.png', { animations: 'disabled' });
});
```

Add the matching dark phone test gated to `visual-390x844` and name its snapshot `dark-secret-editor.png`. Existing workspace snapshot names remain `light-vault-workspace.png` and `dark-vault-workspace.png` inside each project directory.

- [ ] **Step 2: Run visual tests and verify RED**

Run: `npx playwright test tests/web/ui-visual.spec.js`

Expected: FAIL with missing or changed approved-shell snapshots.

- [ ] **Step 3: Generate candidate snapshots**

Run: `npx playwright test tests/web/ui-visual.spec.js --update-snapshots`

Expected: 10 active cases PASS across the four visual projects; project-mismatched editor cases are skipped and candidate PNGs are written.

- [ ] **Step 4: Inspect every PNG**

Reject and fix any clipped/duplicated navigation, unreadable context, truncated primary identifier, horizontal overflow, bottom-nav overlap, drawer/footer obstruction, low contrast, or inconsistent dark-mode surface. Rerun and reinspect changed images; do not approve from exit code alone.

- [ ] **Step 5: Update user documentation**

Add to `docs/web-ui.md`:

```markdown
The desktop interface uses a persistent context rail containing the native
workspace switcher, effective context, Secrets/Files/Trash navigation, quick
access, Commands, Settings, and Help. Search stays in the active view;
secondary filters and Expand all/Collapse all open from Filters.

At 768px and below, content changes to stacked rows, context moves to a compact
top bar, and the same tab set becomes sticky bottom navigation. Secret editing
becomes a full-screen sheet at phone widths.
```

- [ ] **Step 6: Run the complete matrix**

```bash
npm run test:unit
npx playwright test
cargo test --features ui web:: --lib
node tests/desktop/startup-smoke.js
```

If desktop shell files changed indirectly or smoke reports packaging-sensitive behavior, also run `cargo test -p xv-desktop`. Record exact results in `docs/APP-UX-IMPLEMENTATION-EVIDENCE.md` and distinguish packaged evidence from mounted/browser coverage.

- [ ] **Step 7: Check patch scope**

```bash
git diff --check
git status --short
git diff --stat
```

Expected: no whitespace errors; only planned UI/test/snapshot/docs changes; pre-existing `package-lock.json` remains unstaged.

- [ ] **Step 8: Commit**

```bash
git add tests/web/ui-visual.spec.js docs/web-ui.md docs/APP-UX-IMPLEMENTATION-EVIDENCE.md
git add tests/web/snapshots/visual-1180x760 tests/web/snapshots/visual-820x560 tests/web/snapshots/visual-768x700 tests/web/snapshots/visual-390x844
git commit -m "test(web): verify refined command center ui"
```

## Final Integration Gate

- [ ] `git status --short` lists only the user's pre-existing `package-lock.json` modification.
- [ ] `git log --oneline` shows one intentional commit per task boundary.
- [ ] Run `git pull --rebase` with a safe worktree strategy that does not stage, discard, or overwrite `package-lock.json`.
- [ ] Rerun any gate invalidated by the rebase.
- [ ] Push the current branch and report branch, commit range, verification commands, pass counts, and the untouched user modification.
