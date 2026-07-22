# App Safety and Accessibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent edit loss, make modal workflows keyboard and screen-reader complete, add recoverable deletion, and bound exposure of protected values.

**Architecture:** First extract the embedded frontend into native ES modules served by Axum, then put drafts, modal lifecycle, errors, preferences, and protected-value timers behind explicit interfaces. Thin Axum handlers expose existing backend soft-delete operations through stable error envelopes; the UI renders capability-aware Trash and Undo flows.

**Tech Stack:** Rust 2021, Axum 0.8, native browser ES modules, Node built-in test runner, Playwright, axe-core.

**Specifications:** `docs/superpowers/specs/2026-07-22-app-safety-accessibility-design.md` and `docs/superpowers/specs/2026-07-22-app-ux-modernization-design.md`

## Global Constraints

- Keep one dependency-free production frontend embedded with `include_str!`; Node and Playwright are test-only.
- Keep the loopback bearer token in `sessionStorage`; never persist it in `localStorage` or a preference file.
- Never store secret names, values, notes, queries, clipboard contents, or credentials in UI preferences.
- Every production change begins with an observed failing test and ends with a focused passing test.
- Run `cargo fmt --check`, `cargo clippy --features ui --all-targets -- -D warnings`, `cargo test --features ui web:: --lib`, and `node --test src/web/assets/*.test.js` before completing this plan.

---

### Task 1: Native module boundary and test harness

**Files:**
- Create: `package.json`
- Create: `src/web/assets/api-client.js`
- Create: `src/web/assets/store.js`
- Create: `src/web/assets/dialogs.js`
- Create: `src/web/assets/accessibility.js`
- Create: `src/web/assets/secrets.js`
- Create: `src/web/assets/preferences.js`
- Create: `src/web/assets/module-contracts.test.js`
- Modify: `src/web/assets/app.js`
- Modify: `src/web/assets/app.dom.test.js`
- Modify: `src/web/assets/ui-model.js`
- Modify: `src/web/assets/ui-model.test.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/mod.rs`

**Interfaces:**
- Produces: `createApiClient({ token, onInflight, fetchImpl, xhrFactory })`, `createStore(initialState, reducer)`, `createDialogManager(document)`, `createPreferenceClient(api)`, `mountSecrets(dependencies)`. The client defaults `fetchImpl` to `globalThis.fetch` and `xhrFactory` to `() => new XMLHttpRequest()`.
- Produces named ES-module exports from `ui-model.js`; all browser modules import helpers directly rather than reading `globalThis.XvUiModel`.

- [ ] **Step 1: Write the failing asset-contract tests**

Add tests that require every module to be served and the entry script to be a module:

```rust
#[test]
fn ui_serves_native_module_graph() {
    assert!(INDEX_HTML.contains("<script type=\"module\" src=\"/app.js\"></script>"));
    for path in [
        "/api-client.js", "/store.js", "/dialogs.js", "/accessibility.js",
        "/secrets.js", "/preferences.js",
    ] {
        assert!(asset(path).is_some(), "missing {path}");
    }
}
```

In `module-contracts.test.js`, read each file and assert its named exports:

```javascript
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

test('frontend modules expose the approved boundaries', () => {
  const expected = {
    'api-client.js': 'export function createApiClient',
    'store.js': 'export function createStore',
    'dialogs.js': 'export function createDialogManager',
    'preferences.js': 'export function createPreferenceClient',
    'secrets.js': 'export function mountSecrets',
  };
  for (const [name, marker] of Object.entries(expected)) {
    const source = fs.readFileSync(path.join(__dirname, name), 'utf8');
    assert.match(source, new RegExp(marker));
  }
});
```

- [ ] **Step 2: Run the tests and observe failure**

Run: `cargo test --features ui web::tests::ui_serves_native_module_graph --lib && node --test src/web/assets/module-contracts.test.js`

Expected: Rust compile failure for `asset` and Node failure because the module files do not exist.

- [ ] **Step 3: Add the module registry and minimal valid modules**

Replace individual asset constants with one lookup in `src/web/mod.rs`:

```rust
fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "/app.js" => Some(("application/javascript", include_str!("assets/app.js"))),
        "/api-client.js" => Some(("application/javascript", include_str!("assets/api-client.js"))),
        "/store.js" => Some(("application/javascript", include_str!("assets/store.js"))),
        "/dialogs.js" => Some(("application/javascript", include_str!("assets/dialogs.js"))),
        "/accessibility.js" => Some(("application/javascript", include_str!("assets/accessibility.js"))),
        "/secrets.js" => Some(("application/javascript", include_str!("assets/secrets.js"))),
        "/preferences.js" => Some(("application/javascript", include_str!("assets/preferences.js"))),
        "/ui-model.js" => Some(("application/javascript", include_str!("assets/ui-model.js"))),
        "/style.css" => Some(("text/css", include_str!("assets/style.css"))),
        _ => None,
    }
}
```

Add `get_asset(Path(path): Path<String>)` returning 404 for unknown paths, register the listed routes, and change `index.html` to load only `app.js` with `type="module"`. Create `package.json` with `{ "private": true, "type": "module", "scripts": { "test:unit": "node --test src/web/assets/*.test.js" } }`. Convert `ui-model.js`, `ui-model.test.js`, and `app.dom.test.js` to ES imports/exports. Each new module must contain its named export with a valid minimal body. Move token/bootstrap code into `app.js`; move existing functions without changing behavior until `app.js` contains only bootstrap and wiring.

- [ ] **Step 4: Run focused and regression tests**

Run: `cargo test --features ui web::tests::ui_serves_native_module_graph --lib && node --test src/web/assets/*.test.js`

Expected: PASS. Existing DOM/model tests remain green after imports are updated to read the extracted modules.

- [ ] **Step 5: Commit**

```bash
git add package.json src/web/mod.rs src/web/assets
git commit -m "refactor(web): split embedded UI into native modules"
```

### Task 2: Authoritative store, normalized drafts, and navigation guard

**Files:**
- Modify: `src/web/assets/store.js`
- Modify: `src/web/assets/dialogs.js`
- Modify: `src/web/assets/secrets.js`
- Create: `src/web/assets/store.test.js`
- Create: `src/web/assets/dialogs.test.js`
- Modify: `desktop/src-tauri/src/main.rs`

**Interfaces:**
- Produces: `normalizeSecretDraft(input)`, `draftReducer(state, event)`, `isDraftDirty(draft)`, `guardNavigation({ draft, savePending, confirmDiscard })`.
- Produces: Tauri event names `xv://window-close-requested` and `xv://window-close-approved`.

- [ ] **Step 1: Write failing reducer and guard tests**

```javascript
test('draft normalization preserves secret whitespace and absent-versus-clear', () => {
  const baseline = normalizeSecretDraft({ name: ' db ', value: '  keep  ', note: undefined, folder: '' });
  assert.deepEqual(baseline, { name: 'db', value: '  keep  ', note: null, folder: '' });
  assert.equal(isDraftDirty({ baseline, working: structuredClone(baseline) }), false);
  const working = { ...baseline, note: '' };
  assert.equal(isDraftDirty({ baseline, working }), true);
});

test('navigation guard keeps a dirty draft unless discard is confirmed', async () => {
  const draft = { baseline: { name: 'a' }, working: { name: 'b' } };
  assert.equal(await guardNavigation({ draft, savePending: false, confirmDiscard: async () => false }), false);
  assert.equal(await guardNavigation({ draft, savePending: false, confirmDiscard: async () => true }), true);
  assert.equal(await guardNavigation({ draft, savePending: true, confirmDiscard: async () => true }), false);
});
```

- [ ] **Step 2: Run and observe missing exports**

Run: `node --test src/web/assets/store.test.js src/web/assets/dialogs.test.js`

Expected: FAIL because normalization and guard functions are absent.

- [ ] **Step 3: Implement immutable store and one guard path**

Use this store contract:

```javascript
export function createStore(initialState, reducer) {
  let state = structuredClone(initialState);
  const listeners = new Set();
  return Object.freeze({
    snapshot: () => structuredClone(state),
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    dispatch(event) {
      state = reducer(state, Object.freeze({ ...event }));
      const snapshot = structuredClone(state);
      for (const listener of listeners) listener(snapshot, event);
      return snapshot;
    },
  });
}
```

Implement normalized baseline/working copies and route Cancel, close button, Escape, backdrop, tab switch, vault switch, competing edit, `beforeunload`, and Tauri close requests through `guardNavigation`. Disable all close/context controls while `savePending` is true. Return focus to the invoking element after an approved close.

- [ ] **Step 4: Add the Tauri close handshake**

In the desktop process, prevent the first close request, emit `xv://window-close-requested`, and close only after the page emits approval. The page responds immediately when no dirty draft exists and opens the discard confirmation otherwise. Add a Rust unit test around a pure `CloseDecision { Allow, AskPage, DenyWhileSaving }` function before wiring the events.

- [ ] **Step 5: Run focused tests**

Run: `node --test src/web/assets/store.test.js src/web/assets/dialogs.test.js && cargo test -p xv-desktop close_decision`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/web/assets/store.js src/web/assets/store.test.js src/web/assets/dialogs.js src/web/assets/dialogs.test.js src/web/assets/secrets.js desktop/src-tauri/src/main.rs
git commit -m "feat(web): guard dirty secret drafts"
```

### Task 3: Accessible modal-sheet lifecycle

**Files:**
- Modify: `package.json`
- Create: `playwright.config.js`
- Create: `tests/web/fixtures.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/dialogs.js`
- Modify: `src/web/assets/accessibility.js`
- Modify: `src/web/assets/style.css`
- Modify: `src/web/assets/dialogs.test.js`
- Create: `tests/web/ui-accessibility.spec.js`

**Interfaces:**
- Produces: `openModal(element, { initialFocus, invoker })`, `closeModal(element)`, `topModal()`, `setBackgroundInert(active)`.

- [ ] **Step 1: Create the hermetic browser fixture and write a failing focus-contract test**

Install test-only dependencies with `npm install --save-dev @playwright/test @axe-core/playwright` and Chromium with `npx playwright install chromium`. In `tests/web/fixtures.js`, use `mkdtemp`, set isolated `HOME`, `XDG_CONFIG_HOME`, `XV_BACKEND=local`, `XV_NO_PARENT_CONFIG=1`, and a unique local store path. Build `xv --features ui`, launch `xv ui --no-open` on port 0, parse the printed URL, and terminate the child in fixture teardown. Never inherit a real Crosstache config.

```javascript
test('secret sheet traps focus, guards Escape, and restores the invoker', async ({ page }) => {
  await page.getByRole('button', { name: 'New secret' }).click();
  const dialog = page.getByRole('dialog', { name: 'New secret' });
  await expect(dialog).toBeVisible();
  await expect(page.locator('main')).toHaveAttribute('inert', '');
  await expect(page.getByLabel('Name')).toBeFocused();
  await page.getByLabel('Name').fill('draft');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', { name: 'Discard changes?' })).toBeVisible();
  await page.getByRole('button', { name: 'Keep editing' }).click();
  await expect(page.getByLabel('Name')).toHaveValue('draft');
});
```

- [ ] **Step 2: Run and observe failure**

Run: `npx playwright test tests/web/ui-accessibility.spec.js --grep "secret sheet"`

Expected: FAIL because the drawer lacks dialog semantics, inert background, focus containment, and nested confirmation.

- [ ] **Step 3: Implement the modal manager and semantic markup**

Give the sheet `role="dialog"`, `aria-modal="true"`, and `aria-labelledby="drawer-title"`. Add a separate discard-confirmation dialog with Keep editing first and Discard changes second. Implement focus containment using a `keydown` listener that cycles among visible enabled focusables, set `inert` on header/main, and use `aria-hidden` only as a fallback when `HTMLElement.prototype.inert` is unavailable. Nested dialogs restore focus to their invoking control.

- [ ] **Step 4: Verify keyboard and accessibility behavior**

Run: `npx playwright test tests/web/ui-accessibility.spec.js --grep "secret sheet" && node --test src/web/assets/dialogs.test.js`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add package.json package-lock.json playwright.config.js tests/web/fixtures.js tests/web/ui-accessibility.spec.js src/web/assets
git commit -m "feat(web): make secret sheet keyboard complete"
```

### Task 4: Stable API errors and durable UI error surfaces

**Files:**
- Create: `src/web/errors.rs`
- Modify: `src/web/api.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/web/assets/api-client.js`
- Modify: `src/web/assets/secrets.js`
- Create: `src/web/assets/api-client.test.js`

**Interfaces:**
- Produces Rust `ApiErrorBody { code, message, hint, field, details }` and `ApiErrorEnvelope { error }`.
- Produces JS `ApiError extends Error` with `status`, `code`, `hint`, `field`, and `details`.

- [ ] **Step 1: Write failing Rust and JavaScript error-contract tests**

```rust
assert_eq!(json["error"]["code"], "xv-secret-not-found");
assert_eq!(json["error"]["message"], "Secret 'missing' was not found.");
assert!(json["error"]["hint"].as_str().unwrap().contains("Refresh"));
```

```javascript
test('api client retains structured error fields', async () => {
  const fetch = async () => new Response(JSON.stringify({ error: {
    code: 'xv-conflict', message: 'Name exists', hint: 'Choose another name', field: 'name', details: { name: 'a' },
  }}), { status: 409, headers: { 'content-type': 'application/json' } });
  const api = createApiClient({ token: 't', fetchImpl: fetch });
  await assert.rejects(api.request('GET', '/x'), error => error.code === 'xv-conflict' && error.field === 'name');
});
```

- [ ] **Step 2: Run and observe old string-envelope failures**

Run: `cargo test --features ui web::api::tests::missing_secret_has_stable_error --lib && node --test src/web/assets/api-client.test.js`

Expected: FAIL because responses are `{ "error": "..." }` and the client discards fields.

- [ ] **Step 3: Implement safe classification and persistent rendering**

Map every `CrosstacheError` and `BackendError` to a stable kebab-case code and safe hint. Do not serialize debug chains or credentials. Let handler validation attach a field. In the client, parse the envelope once and throw `ApiError`. Replace four-second error toasts with inline `role="alert"` panels in lists/forms; preserve drafts and focus the field named by `error.field`. Ignore aborted/stale requests.

- [ ] **Step 4: Run focused and API regression tests**

Run: `cargo test --features ui web::api::tests --lib && node --test src/web/assets/api-client.test.js`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web/errors.rs src/web/api.rs src/web/mod.rs src/web/assets
git commit -m "feat(web): return durable structured errors"
```

### Task 5: Trash, restore, purge, and Undo API

**Files:**
- Create: `src/web/secrets.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/web/testutil.rs`
- Modify: `src/backend/mod.rs`
- Modify: `src/backend/local/mod.rs`
- Modify: `src/backend/azure/mod.rs`
- Modify: `src/backend/aws/mod.rs`
- Modify: `src/records/mod.rs`
- Modify: `src/cli/mv_ops.rs`
- Modify: `src/cli/secret_ops.rs`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/style.css`
- Create: `src/web/assets/secrets.test.js`
- Create: `tests/web/ui-trash.spec.js`

**Interfaces:**
- Produces routes: `GET /api/secrets/deleted`, `POST /api/secrets/{name}/restore`, `DELETE /api/secrets/{name}/purge`.
- Extends context capabilities with `soft_delete`, `restore`, `purge`, and `scheduled_purge` booleans.

- [ ] **Step 1: Write failing Axum route tests**

Seed a stub secret, delete it, then assert:

```rust
let (_, deleted) = get_json(app.clone(), "GET", "/api/secrets/deleted", None).await;
assert_eq!(deleted[0]["name"], "recover-me");
assert!(deleted[0]["deleted_on"].is_string());
let (status, _) = get_json(app.clone(), "POST", "/api/secrets/recover-me/restore", None).await;
assert_eq!(status, StatusCode::OK);
let (status, _) = get_json(app, "DELETE", "/api/secrets/recover-me/purge", None).await;
assert_eq!(status, StatusCode::NOT_FOUND);
```

- [ ] **Step 2: Run and observe route failures**

Run: `cargo test --features ui web::secrets::tests --lib`

Expected: FAIL with 404 routes.

- [ ] **Step 3: Add thin handlers and a recoverable test backend**

Handlers call `list_deleted_secrets`, `restore_secret`, and `purge_secret` on the selected backend/vault. Update `StubBackend` with a deleted map and collision-safe restore. Add `has_restore`, `has_purge`, and `has_scheduled_purge` to `BackendCapabilities`; set them explicitly in local, Azure, AWS, record, and test backends. Keep `has_soft_delete` as the recoverable-delete signal and report policy-blocked purge attempts through structured errors.

- [ ] **Step 4: Add capability-aware Trash and confirmation UI**

Add a real Trash ARIA tab. Delete confirmation names backend, vault, up to five targets, overflow count, and recoverability. A successful recoverable delete creates a persistent action notice with Undo; hard-delete-only paths explicitly say recovery is unavailable. Trash lists deletion and purge dates. Restore conflicts stay visible. Purge requires exact-name input and is disabled until it matches.

- [ ] **Step 5: Verify the complete browser flow**

Run: `cargo test --features ui web::secrets::tests --lib && node --test src/web/assets/secrets.test.js && npx playwright test tests/web/ui-trash.spec.js`

Expected: PASS for delete, Undo, Trash, restore conflict, and typed-name purge.

- [ ] **Step 6: Commit**

```bash
git add src/backend/mod.rs src/backend/local/mod.rs src/backend/azure/mod.rs src/backend/aws/mod.rs src/records/mod.rs src/cli/mv_ops.rs src/cli/secret_ops.rs src/web src/web/assets tests/web/ui-trash.spec.js
git commit -m "feat(web): add recoverable secret trash"
```

### Task 6: Versioned presentation preferences

**Files:**
- Create: `src/web/preferences.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/web/testutil.rs`
- Modify: `src/web/assets/preferences.js`
- Create: `src/web/assets/preferences.test.js`

**Interfaces:**
- Produces `UiPreferencesV1 { version, theme, exposure_timeout_seconds, density, folder_expansion, column_widths }`.
- Produces routes `GET /api/preferences` and `PUT /api/preferences`.

- [ ] **Step 1: Write failing path, redaction, and migration tests**

```rust
#[test]
fn preferences_reject_vault_data_keys() {
    let json = serde_json::json!({"version": 1, "theme": "dark", "secret_name": "DB_URL"});
    assert!(UiPreferencesV1::from_json(json).is_err());
}

#[test]
fn preference_path_is_ui_json_next_to_config() {
    assert_eq!(preference_path_for(Path::new("/tmp/xv/xv.conf")), PathBuf::from("/tmp/xv/ui.json"));
}
```

- [ ] **Step 2: Run and observe missing module failure**

Run: `cargo test --features ui web::preferences::tests --lib`

Expected: FAIL because the preference types and path function are absent.

- [ ] **Step 3: Implement atomic, non-secret preference storage**

Use `Config::get_config_path()` to derive `ui.json`. Deserialize with defaults and ignore unknown future fields; reject known vault-data keys. Write a sibling temporary file with restrictive permissions through existing sensitive-file helpers, then rename it over `ui.json`. Clamp exposure timeout to `min(user_choice, clipboard_timeout)` when the security policy is nonzero.

- [ ] **Step 4: Implement debounced frontend load/save**

`createPreferenceClient` loads once, exposes immutable snapshots, and saves only whitelisted keys after 250 ms. Failed saves show a Settings error without blocking vault operations.

- [ ] **Step 5: Run focused tests and commit**

Run: `cargo test --features ui web::preferences::tests --lib && node --test src/web/assets/preferences.test.js`

```bash
git add src/web/preferences.rs src/web/mod.rs src/web/testutil.rs src/web/assets/preferences.js src/web/assets/preferences.test.js
git commit -m "feat(web): persist non-secret UI preferences"
```

### Task 7: Protected-value countdown and safe clipboard clearing

**Files:**
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/accessibility.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Modify: `src/web/assets/secrets.test.js`
- Create: `tests/web/ui-protected-values.spec.js`

**Interfaces:**
- Produces `createExposureTimer({ seconds, onTick, onExpire, clock })` and `clearClipboardIfUnchanged({ clipboard, expected })`.

- [ ] **Step 1: Write failing deterministic timer and clipboard tests**

```javascript
test('clipboard clearing never overwrites a newer value', async () => {
  let value = 'newer';
  const clipboard = { readText: async () => value, writeText: async next => { value = next; } };
  assert.equal(await clearClipboardIfUnchanged({ clipboard, expected: 'copied' }), false);
  assert.equal(value, 'newer');
  value = 'copied';
  assert.equal(await clearClipboardIfUnchanged({ clipboard, expected: 'copied' }), true);
  assert.equal(value, '');
});
```

- [ ] **Step 2: Run and observe missing helper failure**

Run: `node --test src/web/assets/secrets.test.js --test-name-pattern "clipboard clearing"`

Expected: FAIL because the helpers are absent.

- [ ] **Step 3: Implement exposure lifecycle**

Copy notices identify the field and tick once per second. At expiry, read and clear only an exact clipboard match; otherwise state that clearing could not be confirmed. Reveal timers reset on protected-field interaction and hide on timeout, `visibilitychange`, window blur, sheet close, save, and context switch. Clear protected values from store memory when the sheet closes. Live regions announce state and remaining time but never the value.

- [ ] **Step 4: Verify browser behavior**

Run: `node --test src/web/assets/secrets.test.js && npx playwright test tests/web/ui-protected-values.spec.js`

Expected: PASS, including visibility/blur expiry and newer-clipboard preservation.

- [ ] **Step 5: Commit**

```bash
git add src/web/assets tests/web/ui-protected-values.spec.js
git commit -m "feat(web): bound protected-value exposure"
```

### Task 8: Playwright and axe regression gate

**Files:**
- Modify: `package.json`
- Modify: `playwright.config.js`
- Modify: `tests/web/fixtures.js`
- Modify: `tests/web/ui-accessibility.spec.js`
- Modify: `tests/web/ui-trash.spec.js`
- Modify: `tests/web/ui-protected-values.spec.js`
- Modify: `docs/testing.md`

**Interfaces:**
- Produces hermetic fixture `test` that launches `xv ui --no-open` against isolated local config and exposes `{ page, baseURL, vault }`.

- [ ] **Step 1: Add a failing axe assertion to every representative state**

```javascript
import AxeBuilder from '@axe-core/playwright';
const results = await new AxeBuilder({ page }).analyze();
expect(results.violations.filter(v => ['serious', 'critical'].includes(v.impact))).toEqual([]);
```

- [ ] **Step 2: Run the new axe assertions and observe any violations**

Run: `npx playwright test tests/web/ui-accessibility.spec.js tests/web/ui-trash.spec.js tests/web/ui-protected-values.spec.js`

Expected: FAIL if any representative state has a serious or critical violation; the report names the rule and affected locator.

- [ ] **Step 3: Fix every reported serious or critical violation**

Correct the owning HTML, CSS, or accessibility module for every reported rule, then rerun the focused state. Do not suppress axe rules globally; a narrowly documented exclusion is allowed only when the element is inaccessible to users and the browser engine produces a confirmed false positive.

- [ ] **Step 4: Run the complete plan gate**

Run:

```bash
cargo fmt --check
cargo clippy --features ui --all-targets -- -D warnings
cargo test --features ui web:: --lib
node --test src/web/assets/*.test.js
npx playwright test tests/web/ui-accessibility.spec.js tests/web/ui-trash.spec.js tests/web/ui-protected-values.spec.js
```

Expected: all commands pass; axe reports no serious or critical violations.

- [ ] **Step 5: Document and commit the harness**

Document dependency installation, hermetic isolation, and focused commands in `docs/testing.md`.

```bash
git add package.json package-lock.json playwright.config.js tests/web docs/testing.md
git commit -m "test(web): add hermetic accessibility coverage"
```
