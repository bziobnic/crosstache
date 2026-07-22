# App Search, Upload, Responsive, and Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add fast search/filter/command workflows, a managed upload queue, legible responsive rows, and correct navigation and selection semantics.

**Architecture:** Keep search, filters, ranking, queue transitions, and responsive view-models pure and independently tested. Add Axum upload preflight and conflict contracts over the existing `FileBackend`, while browser uploads use XMLHttpRequest for real byte progress and the shared store for cancellation-safe reconciliation.

**Tech Stack:** Rust 2021, Axum multipart, native ES modules, XMLHttpRequest, Node tests, Playwright visual snapshots and axe.

**Specifications:** `docs/superpowers/specs/2026-07-22-app-search-upload-responsive-design.md` and `docs/superpowers/specs/2026-07-22-app-ux-modernization-design.md`

## Global Constraints

- This plan starts only after both safety/accessibility and context/core-workflow plans pass.
- Search and commands index loaded metadata only; never values, notes, clipboard contents, or prior queries.
- Maximum request body stays exactly 100 MB and is stated before file selection.
- Replace is never implicit; each conflict is Skip, Replace, or Rename, optionally applied to all.
- Desktop tables remain above 768 px; stacked rows render at 768 px and below; the sheet becomes full-screen below 544 px.
- Provider finalization remains indeterminate until the backend confirms it.

---

### Task 1: Search index and composable filters

**Files:**
- Create: `src/web/assets/commands.js`
- Create: `src/web/assets/commands.test.js`
- Modify: `src/web/assets/ui-model.js`
- Modify: `src/web/assets/ui-model.test.js`
- Modify: `src/web/assets/secrets.js`
- Create: `src/web/assets/files.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`

**Interfaces:**
- Produces `buildMetadataIndex({ secrets, files, folders })`, `searchIndex(index, query)`, `filterSecrets(items, filters)`, and `filterFiles(items, filters)`.

- [ ] **Step 1: Write failing privacy and combination tests**

```javascript
test('metadata index excludes values notes and prior queries', () => {
  const index = buildMetadataIndex({
    secrets: [{ name: 'db-url', folder: 'prod', groups: 'ops', note: 'private note', value: 'secret' }],
    files: [], folders: ['prod'],
  });
  const serialized = JSON.stringify(index);
  assert.match(serialized, /db-url/);
  assert.doesNotMatch(serialized, /private note|secret/);
});

test('secret filters compose with AND semantics', () => {
  const result = filterSecrets(fixtures, { folder: 'prod', group: 'ops', type: 'login', enabled: true, expiry: 'expiring' });
  assert.deepEqual(result.map(item => item.name), ['prod-login']);
});
```

- [ ] **Step 2: Run and observe missing helper failures**

Run: `node --test src/web/assets/commands.test.js src/web/assets/ui-model.test.js`

Expected: FAIL because index and filter functions are absent.

- [ ] **Step 3: Implement local search and filter chips**

Normalize case and Unicode for names, folders, groups, record type, and file MIME type only. Rank exact name, prefix, word boundary, substring, then folder matches with stable name tie-breaking. Add visible clear actions, `visible / total` counts, and removable chips. Secret filters: folder, group, record type, expiry, enabled. File filters: folder, type, upload status. Announce filters as controls, not list content.

- [ ] **Step 4: Run tests and commit**

Run: `node --test src/web/assets/commands.test.js src/web/assets/ui-model.test.js`

```bash
git add src/web/assets
git commit -m "feat(web): add metadata search and filters"
```

### Task 2: Command registry, palette, and shortcuts

**Files:**
- Modify: `src/web/assets/commands.js`
- Modify: `src/web/assets/commands.test.js`
- Modify: `src/web/assets/app.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Create: `tests/web/ui-commands.spec.js`

**Interfaces:**
- Produces `createCommandRegistry()`, `shouldHandleShortcut(event)`, and `mountCommandPalette({ registry, store, guardNavigation })`.

- [ ] **Step 1: Write failing registry and suppression tests**

```javascript
test('shortcuts do not fire in compatible form controls', () => {
  for (const tagName of ['INPUT', 'TEXTAREA', 'SELECT']) {
    assert.equal(shouldHandleShortcut({ target: { tagName, isContentEditable: false }, key: '/', metaKey: false, ctrlKey: false }), false);
  }
  assert.equal(shouldHandleShortcut({ target: { tagName: 'MAIN', isContentEditable: false }, key: '/', metaKey: false, ctrlKey: false }), true);
});
```

- [ ] **Step 2: Run and observe failure**

Run: `node --test src/web/assets/commands.test.js --test-name-pattern "shortcuts"`

Expected: FAIL because the registry/suppression function is absent.

- [ ] **Step 3: Implement commands and palette**

Register Cmd/Ctrl+K palette, `/` local search, Cmd/Ctrl+N new secret, Escape topmost transient/selection exit, and arrow/Home/End tab navigation. Palette results include commands, loaded secret/file metadata, folders, and workspace/vault targets with explicit surface and scope. Activating a context-changing result calls `guardNavigation` before dispatch. Never persist the query.

- [ ] **Step 4: Verify keyboard behavior and commit**

Run: `node --test src/web/assets/commands.test.js && npx playwright test tests/web/ui-commands.spec.js`

```bash
git add src/web/assets tests/web/ui-commands.spec.js
git commit -m "feat(web): add command palette and shortcuts"
```

### Task 3: Upload preflight and stable conflict API

**Files:**
- Create: `src/web/files.rs`
- Modify: `src/web/api.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/web/errors.rs`
- Modify: `src/web/testutil.rs`

**Interfaces:**
- Produces `UploadCandidate { client_id, name, size, content_type, destination }`.
- Produces `UploadPreflightResult { client_id, status, existing_name, suggested_name, max_bytes }`.
- Produces `POST /api/files/preflight` and conflict query `?policy=skip|replace|rename&target=<name>` on `POST /api/files`.

- [ ] **Step 1: Write failing preflight, size, and conflict tests**

```rust
let (status, body) = get_json(app.clone(), "POST", "/api/files/preflight", Some(json!({"files":[
    {"client_id":"1","name":"report.pdf","size":104857601,"content_type":"application/pdf","destination":"docs"}
]}))).await;
assert_eq!(status, StatusCode::OK);
assert_eq!(body["results"][0]["status"], "too-large");
assert_eq!(body["results"][0]["max_bytes"], 104857600);
```

Seed `docs/report.pdf`, then assert an upload without a policy returns 409 `xv-file-conflict`, Skip leaves bytes unchanged, Replace updates bytes, and Rename writes the requested collision-free target.

- [ ] **Step 2: Run and observe missing route failure**

Run: `cargo test --features ui web::files::tests --lib`

Expected: FAIL with 404 or missing module.

- [ ] **Step 3: Implement preflight and explicit conflict policies**

Validate path/name with the backend's existing rules, enforce `100 * 1024 * 1024`, check file-storage capability, and call `get_file_info` to detect conflicts. Generate a deterministic `name (2).ext` suggestion without reserving it. Recheck conflicts at upload time. Skip returns a structured non-error outcome; Replace and Rename are accepted only when explicit. Preserve the existing body limit.

- [ ] **Step 4: Run tests and commit**

Run: `cargo test --features ui web::files::tests --lib`

```bash
git add src/web/files.rs src/web/api.rs src/web/mod.rs src/web/errors.rs src/web/testutil.rs
git commit -m "feat(web): add upload preflight and conflicts"
```

### Task 4: Managed upload queue with progress, cancel, and retry

**Files:**
- Modify: `src/web/assets/api-client.js`
- Modify: `src/web/assets/store.js`
- Modify: `src/web/assets/files.js`
- Create: `src/web/assets/files.test.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Create: `tests/web/ui-uploads.spec.js`

**Interfaces:**
- Produces `upload({ path, formData, signal, onProgress })` in API client.
- Produces queue states `queued`, `preflighting`, `awaiting-conflict`, `uploading`, `finishing`, `completed`, `failed`, `cancelled`, and `ambiguous`.
- Produces `createUploadQueue(entries)` and `nextUploadState(current, event)` for deterministic scheduling tests.

- [ ] **Step 1: Write failing queue transition tests**

```javascript
test('retry selects only failed cancelled and ambiguous entries', () => {
  const queue = createUploadQueue([{ id: 'a' }, { id: 'b' }, { id: 'c' }]);
  queue.transition('a', 'completed');
  queue.transition('b', 'failed', { error: 'network' });
  queue.transition('c', 'cancelled');
  assert.deepEqual(queue.retryable().map(item => item.id), ['b', 'c']);
});

test('client bytes complete enters finishing before completed', () => {
  assert.equal(nextUploadState('uploading', { type: 'bytes-sent' }), 'finishing');
  assert.equal(nextUploadState('finishing', { type: 'server-confirmed' }), 'completed');
});
```

- [ ] **Step 2: Run and observe missing queue failure**

Run: `node --test src/web/assets/files.test.js`

Expected: FAIL because the queue model is absent.

- [ ] **Step 3: Implement XHR transport and queue scheduler**

Use `XMLHttpRequest.upload.onprogress` to report `{ loaded, total }`, resolve only after a 2xx response parses, and wire `AbortSignal` to `xhr.abort()`. Schedule no more than the configured `max_concurrent_uploads` from context. Show destination before transfer. Resolve conflicts per item or Apply to all. On local byte completion show Finishing; on abort/network ambiguity refresh file metadata and label the evidence instead of guessing success/failure.

- [ ] **Step 4: Implement durable summary and verify**

Queue rows expose status, byte progress, Cancel, and Retry where applicable. Final result remains until dismissed and names every outcome. Run: `node --test src/web/assets/files.test.js && npx playwright test tests/web/ui-uploads.spec.js`

Expected: PASS for conflict policies, bounded concurrency, progress, cancellation, ambiguous refresh, retry, and summary.

- [ ] **Step 5: Commit**

```bash
git add src/web/assets tests/web/ui-uploads.spec.js
git commit -m "feat(web): add managed upload queue"
```

### Task 5: Responsive tables and stacked rows

**Files:**
- Modify: `src/web/assets/ui-model.js`
- Modify: `src/web/assets/ui-model.test.js`
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/files.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Modify: `desktop/src-tauri/tauri.conf.json`
- Create: `tests/web/ui-responsive.spec.js`

**Interfaces:**
- Produces `contentMode(width)` returning `table` above 768 and `stacked` at or below 768.

- [ ] **Step 1: Write failing breakpoint and long-identifier tests**

```javascript
test('content mode changes at the approved breakpoint', () => {
  assert.equal(model.contentMode(769), 'table');
  assert.equal(model.contentMode(768), 'stacked');
  assert.equal(model.contentMode(390), 'stacked');
});
```

The browser test uses a 100-character name and nested folder at 768×700 and 390×844, asserting the complete identifier is visible and the page has no horizontal overflow.

- [ ] **Step 2: Run and observe missing responsive model failure**

Run: `node --test src/web/assets/ui-model.test.js --test-name-pattern "content mode" && npx playwright test tests/web/ui-responsive.spec.js`

Expected: FAIL.

- [ ] **Step 3: Implement two semantic renderers from one view-model**

Keep sortable/resizable tables above 768. At/below 768, render one semantic activation control per stacked row: full identifier first, priority metadata second, selection checkbox only in selection mode. Remove hidden table columns and separators from accessibility tree/focus order. Folder headers span the list. Make toolbars wrap without horizontal scrolling and the modal sheet full-screen below 544.

Lower the Tauri minimum width so the approved tablet/phone breakpoints can be exercised while preserving usable macOS controls; pin the chosen value with a config test.

- [ ] **Step 4: Verify and commit**

Run: `node --test src/web/assets/ui-model.test.js && npx playwright test tests/web/ui-responsive.spec.js && cargo test -p xv-desktop`

```bash
git add src/web/assets desktop/src-tauri/tauri.conf.json tests/web/ui-responsive.spec.js
git commit -m "feat(web): add responsive stacked content rows"
```

### Task 6: ARIA tabs, tree navigation, and selection semantics

**Files:**
- Modify: `src/web/assets/accessibility.js`
- Modify: `src/web/assets/app.js`
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/files.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Modify: `tests/web/ui-accessibility.spec.js`
- Create: `tests/web/ui-navigation.spec.js`

**Interfaces:**
- Produces `mountTabs(tablist)`, `mountRovingFocus(container, selector)`, and `syncVisibleSelection({ visibleIds, selectedIds })`.

- [ ] **Step 1: Write failing tab and selection focus-order tests**

```javascript
test('tabs use roving focus and activate with arrows', async ({ page }) => {
  const secrets = page.getByRole('tab', { name: 'Secrets' });
  await secrets.focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.getByRole('tab', { name: 'Files' })).toBeFocused();
  await expect(page.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true');
});
```

Selection coverage asserts one checkbox and one activation control per item, a visible-scope header label, and `aria-checked="mixed"` for partial visible selection.

- [ ] **Step 2: Run and observe semantic failures**

Run: `npx playwright test tests/web/ui-navigation.spec.js`

Expected: FAIL because tabs are plain buttons and item names duplicate activation semantics.

- [ ] **Step 3: Implement approved interaction patterns**

Use `tablist`, `tab`, labelled `tabpanel`, `aria-selected`, and roving tabindex. Left/Right moves and activates; Home/End jump boundaries. Keep desktop folder tree keyboard semantics from the prior plan. Use one row activation control. In selection mode add one checkbox per row and remove activation ambiguity. Header checkbox names its visible-item scope and reflects mixed state. Escape exits selection only when no higher modal/transient surface is open.

- [ ] **Step 4: Verify and commit**

Run: `npx playwright test tests/web/ui-accessibility.spec.js tests/web/ui-navigation.spec.js`

```bash
git add src/web/assets tests/web/ui-accessibility.spec.js tests/web/ui-navigation.spec.js
git commit -m "fix(web): correct navigation and selection semantics"
```

### Task 7: Responsive visual and accessibility gate

**Files:**
- Create: `tests/web/ui-visual.spec.js`
- Create: `tests/web/snapshots/.gitkeep`
- Modify: `playwright.config.js`
- Modify: `docs/testing.md`

**Interfaces:**
- Produces deterministic visual projects for 1180×760, 820×560, 768×700, and 390×844 in light and dark themes.

- [ ] **Step 1: Add failing deterministic snapshots**

```javascript
for (const theme of ['light', 'dark']) {
  test(`${theme} vault workspace`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: theme });
    await seedLongNames(page);
    await expect(page).toHaveScreenshot(`${theme}-vault-workspace.png`, { fullPage: true, animations: 'disabled' });
  });
}
```

- [ ] **Step 2: Run once and review newly generated baselines**

Run: `npx playwright test tests/web/ui-visual.spec.js --update-snapshots`

Expected: snapshot files are generated for all eight viewport/theme combinations; inspect them before accepting.

- [ ] **Step 3: Run the complete plan gate**

Run:

```bash
cargo fmt --check
cargo clippy --features ui --all-targets -- -D warnings
cargo test --features ui web:: --lib
node --test src/web/assets/*.test.js
npx playwright test tests/web/ui-commands.spec.js tests/web/ui-uploads.spec.js tests/web/ui-responsive.spec.js tests/web/ui-navigation.spec.js tests/web/ui-visual.spec.js
```

Expected: all tests and snapshots pass; axe has no serious or critical violations; no viewport has horizontal page overflow.

- [ ] **Step 4: Document and commit**

```bash
git add tests/web playwright.config.js docs/testing.md
git commit -m "test(web): cover responsive UX and navigation"
```
