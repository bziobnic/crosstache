# Web UI Tweaks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Dates-only display, full typed-record editing, and background-loading indicators in the embedded web UI (`xv ui`).

**Architecture:** One new server endpoint (`GET /api/types`, resolved record types stored on `WebState` at startup); everything else is frontend work in the three embedded assets (`app.js`, `index.html`, `style.css`), which are `include_str!`-ed into the binary. Record saves reuse the existing PUT endpoint (it already accepts arbitrary `content_type` + `tags`).

**Tech Stack:** Rust (axum, feature `ui`), vanilla JS/HTML/CSS assets.

**Spec:** `docs/superpowers/specs/2026-07-09-web-ui-tweaks-design.md`

## Global Constraints

- Everything web lives behind the `ui` cargo feature: build with `cargo build --features ui`, test with `cargo test --features ui web::`.
- Record constants (must match `src/records/envelope.rs` exactly): content type `application/vnd.xv.record`, type tag `xv-type`, metadata-field tag prefix `f.`.
- The raw record envelope JSON must never be rendered in the UI, and a record save must never write through the plain Value textarea.
- The frontend has no JS test harness — do not add one. Frontend tasks verify via `cargo check --features ui` plus the final manual-verification task.
- Branch: `web-ui-tweaks` (already created; spec committed).
- Commit after each task. Commit messages end with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: `GET /api/types` endpoint

**Files:**
- Modify: `src/web/mod.rs` (WebState struct ~line 25, `build_router` route list ~line 33, `run_web` ~line 108)
- Modify: `src/web/testutil.rs:7-13`
- Modify: `src/web/api.rs` (new handler + test)

**Interfaces:**
- Produces: `GET /api/types` → `200 {"types": [{"name": "login", "source": "builtin", "fields": [{"name": "username", "kind": "metadata", "required": true, "primary": false}, ...]}, ...]}`. `kind` serializes lowercase (`"metadata"`/`"secret"`), `source` lowercase (`"builtin"`/`"global"`/`"project"`). Task 5/6's frontend consumes this shape.
- Produces: `WebState.types: Vec<crate::records::RecordType>` for any future handler.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `src/web/api.rs` (alongside `vaults_falls_back_to_current_when_unsupported`):

```rust
#[tokio::test]
async fn types_returns_builtin_types() {
    let app = crate::web::build_router(testutil::test_state());
    let (status, json_body) = get_json(app, "GET", "/api/types", None).await;
    assert_eq!(status, StatusCode::OK);
    let types = json_body["types"].as_array().unwrap();
    let login = types.iter().find(|t| t["name"] == "login").unwrap();
    // login's declared field order and shape, exactly as builtin_types() defines
    assert_eq!(login["source"], "builtin");
    assert_eq!(login["fields"][0]["name"], "username");
    assert_eq!(login["fields"][0]["kind"], "metadata");
    assert_eq!(login["fields"][0]["required"], true);
    assert_eq!(login["fields"][2]["name"], "password");
    assert_eq!(login["fields"][2]["kind"], "secret");
    assert_eq!(login["fields"][2]["primary"], true);
    // all three builtins present
    for name in ["login", "api-key", "database"] {
        assert!(types.iter().any(|t| t["name"] == name), "{name} missing");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features ui web::api::tests::types_returns_builtin_types`
Expected: compile FAILS (`WebState` has no field `types` isn't hit yet — the route doesn't exist, so the test compiles but the request 404s). Either a 404 assertion failure or a compile error once Step 3 is partially applied is acceptable evidence; the point is red-before-green.

- [ ] **Step 3: Implement**

In `src/web/mod.rs`, add the field to `WebState`:

```rust
/// Shared state for all handlers.
pub(crate) struct WebState {
    pub backend: Arc<dyn Backend>,
    pub token: String,
    /// Default vault, resolved once at startup. Requests may override per-call.
    pub vault: String,
    /// Record types (builtin + [types.*] config), resolved once at startup.
    pub types: Vec<crate::records::RecordType>,
}
```

In `build_router`, add the route after the `/vaults` line:

```rust
        .route("/types", get(api::list_types))
```

In `run_web`, resolve types right after `resolve_vault_for_trait` and add to the state literal:

```rust
    let vault = crate::cli::helpers::resolve_vault_for_trait(&config, Some(registry)).await?;
    let backend = registry.active_arc();
    // Fail loud at startup on a broken [types.*] block, matching the CLI's
    // eager type-resolution paths.
    let types = config.resolve_record_types().await?;
```

```rust
    let state = Arc::new(WebState {
        backend,
        token: token.clone(),
        vault,
        types,
    });
```

In `src/web/testutil.rs`:

```rust
pub(crate) fn test_state_with_token(token: &str) -> Arc<WebState> {
    Arc::new(WebState {
        backend: Arc::new(stub::StubBackend::new()),
        token: token.to_string(),
        vault: "default".to_string(),
        types: crate::records::builtin_types(),
    })
}
```

In `src/web/api.rs`, add the handler after `list_vaults`:

```rust
pub(crate) async fn list_types(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    Json(json!({ "types": state.types }))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features ui web::`
Expected: all web tests PASS, including `types_returns_builtin_types`.

- [ ] **Step 5: Commit**

```bash
git add src/web/mod.rs src/web/api.rs src/web/testutil.rs
git commit -m "feat(ui): expose resolved record types at GET /api/types"
```

---

### Task 2: Record round-trip regression test (server, test-only)

**Files:**
- Modify: `src/web/api.rs` (tests module only)

**Interfaces:**
- Consumes: existing PUT/GET/reveal handlers; nothing new.
- Produces: a pinned guarantee Task 5 relies on — record `content_type`, `xv-type`/`f.*` tags, and envelope value survive a PUT/GET cycle, and the list/metadata endpoints never leak the envelope.

- [ ] **Step 1: Write the test**

Add to the `tests` module in `src/web/api.rs`:

```rust
#[tokio::test]
async fn record_roundtrip_preserves_envelope_and_field_tags() {
    let app = crate::web::build_router(testutil::test_state());

    // PUT a typed record the way the record drawer saves one
    let (status, _) = get_json(
        app.clone(),
        "PUT",
        "/api/secrets/gh-login",
        Some(json!({
            "value": r#"{"password":"hunter2"}"#,
            "content_type": "application/vnd.xv.record",
            "tags": {"xv-type": "login", "f.username": "bob"},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // metadata GET: record markers survive, value stays null
    let (status, meta) = get_json(app.clone(), "GET", "/api/secrets/gh-login", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["content_type"], "application/vnd.xv.record");
    assert_eq!(meta["tags"]["xv-type"], "login");
    assert_eq!(meta["tags"]["f.username"], "bob");
    assert!(meta["value"].is_null());

    // the list never leaks envelope contents
    let (_, list) = get_json(app.clone(), "GET", "/api/secrets", None).await;
    assert!(!list.to_string().contains("hunter2"));

    // reveal returns the raw envelope for the drawer to parse
    let (_, revealed) = get_json(app, "POST", "/api/secrets/gh-login/value", None).await;
    assert_eq!(revealed["value"], r#"{"password":"hunter2"}"#);
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --features ui web::api::tests::record_roundtrip_preserves_envelope_and_field_tags`
Expected: PASS (this pins existing behavior; if it fails, stop — the drawer design assumption is wrong and the plan needs revisiting).

- [ ] **Step 3: Commit**

```bash
git add src/web/api.rs
git commit -m "test(ui): pin record envelope/tag round-trip through the web API"
```

---

### Task 3: Dates-only display + Expires date picker (frontend)

**Files:**
- Modify: `src/web/assets/app.js` (helpers near top; `renderSecrets`; `openDrawer` expires line; submit handler expires values; `loadFiles` modified column)
- Modify: `src/web/assets/index.html:55` (Expires input)

**Interfaces:**
- Produces: `fmtDate(s)` — returns `''` for falsy input, `YYYY-MM-DD` (UTC) for parseable datetimes, the raw string otherwise. Tasks 4–6 leave it untouched.
- Produces: Expires drawer semantics later tasks must preserve — input is `type="date"`; PUT sends `<date>T00:00:00Z` or `null`; PATCH sends `<date>T00:00:00Z` or `''` (clear).

- [ ] **Step 1: Add `fmtDate` and apply it to both tables**

In `app.js`, add below the `fail` helper (line 36):

```js
// Dates only, never timestamps. Unparseable strings pass through raw.
function fmtDate(s) {
  if (!s) return '';
  const d = new Date(s);
  return isNaN(d) ? s : d.toISOString().slice(0, 10);
}
```

In `renderSecrets`, change the cells line:

```js
    for (const cell of [name, s.folder, s.groups, s.note, fmtDate(s.updated_on)]) {
```

In `loadFiles`, change the cells line:

```js
    const cells = [f.name, `${f.size}`, f.content_type, fmtDate(f.last_modified)];
```

- [ ] **Step 2: Switch Expires to a date input**

In `index.html`, replace line 55:

```html
    <label>Expires <input name="expires_on" type="date"></label>
```

In `app.js` `openDrawer`, replace the expires population line:

```js
      f.elements.expires_on.value = meta.expires_on ? meta.expires_on.slice(0, 10) : '';
```

In the submit handler, compute the wire values once at the top (after the `groups` line):

```js
  const expiresPut = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : null;
  const expiresPatch = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : '';
```

and use them: in the PUT body `expires_on: expiresPut,` and in the PATCH body `expires_on: expiresPatch,`.

- [ ] **Step 3: Verify the build**

Run: `cargo check --features ui`
Expected: clean check (assets are `include_str!`-ed; this confirms nothing Rust-side broke).

- [ ] **Step 4: Commit**

```bash
git add src/web/assets/app.js src/web/assets/index.html
git commit -m "feat(ui): dates-only display and a native date picker for expiry"
```

---

### Task 4: Loading indicators (frontend)

**Files:**
- Modify: `src/web/assets/app.js` (`api()` wrapper, `loadSecrets`, `renderSecrets`, `loadFiles`, vault-switch handler)
- Modify: `src/web/assets/index.html` (progress bar element)
- Modify: `src/web/assets/style.css` (progress bar + placeholder styles)

**Interfaces:**
- Produces: `showPlaceholder(tbody, text, cols)` — replaces a table body with one muted full-width row. Task 5/6 must not break the "Loading secrets…" flow in `loadSecrets`.
- Produces: global in-flight progress bar driven from inside `api()`; later tasks get it for free on every call.

- [ ] **Step 1: Add the progress bar element and styles**

In `index.html`, insert directly after `<body>`:

```html
<div id="progress" hidden></div>
```

In `style.css`, append:

```css
#progress { position:fixed; top:0; left:0; right:0; height:2px; overflow:hidden; z-index:10; }
#progress::after { content:""; display:block; width:30%; height:100%; background:var(--accent);
  animation:progress-slide 1.2s linear infinite; }
@keyframes progress-slide { from { transform:translateX(-100vw); } to { transform:translateX(100vw); } }
td.placeholder { color:var(--muted); text-align:center; padding:1rem; }
```

- [ ] **Step 2: Track in-flight calls in `api()`**

In `app.js`, wrap the body of `api()` in try/finally with a module-level counter (the `#progress` lookup uses `document.getElementById` directly because `$` is declared after `api`):

```js
let inflight = 0;
async function api(method, path, body, raw = false) {
  inflight++;
  document.getElementById('progress').hidden = false;
  try {
    const opts = { method, headers: { Authorization: `Bearer ${TOKEN}` } };
    if (body instanceof FormData) {
      opts.body = body;
    } else if (body !== undefined) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    const res = await fetch(path, opts);
    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).error || msg; } catch { /* not json */ }
      throw new Error(msg);
    }
    if (raw) return res;
    const text = await res.text();
    return text ? JSON.parse(text) : null;
  } finally {
    if (--inflight <= 0) { inflight = 0; document.getElementById('progress').hidden = true; }
  }
}
```

- [ ] **Step 3: Placeholder rows in both tables**

In `app.js`, add near `fmtDate`:

```js
function showPlaceholder(tbody, text, cols) {
  tbody.innerHTML = '';
  const tr = document.createElement('tr');
  const td = document.createElement('td');
  td.colSpan = cols;
  td.className = 'placeholder';
  td.textContent = text;
  tr.appendChild(td);
  tbody.appendChild(tr);
}
```

Rewrite `loadSecrets`:

```js
async function loadSecrets() {
  showPlaceholder($('#secrets-table tbody'), 'Loading secrets…', 5);
  secrets = await api('GET', `/api/secrets${vaultQS()}`);
  renderSecrets();
}
```

At the end of `renderSecrets` (after the `for` loop), add an empty state:

```js
  if (!tbody.children.length) {
    showPlaceholder(tbody, secrets.length ? 'no matching secrets' : 'no secrets', 5);
  }
```

In `loadFiles`, after the capabilities guard and before the fetch:

```js
  showPlaceholder($('#files-table tbody'), 'Loading files…', 5);
  const files = await api('GET', `/api/files${vaultQS()}`);
```

and at the end of `loadFiles`' row loop add:

```js
  if (!tbody.children.length) showPlaceholder(tbody, 'no files', 5);
```

In the vault `sel.onchange` handler, surface failures instead of dropping the promises:

```js
  sel.onchange = () => {
    currentVault = sel.value;
    // Close the drawer: anything open in it belongs to the previous vault,
    // and saving/deleting it against the new vault would hit the wrong secret.
    closeDrawer();
    loadSecrets().catch(fail);
    loadFiles().catch(fail);
  };
```

- [ ] **Step 4: Verify the build**

Run: `cargo check --features ui`
Expected: clean check.

- [ ] **Step 5: Commit**

```bash
git add src/web/assets/app.js src/web/assets/index.html src/web/assets/style.css
git commit -m "feat(ui): loading placeholders and in-flight progress bar"
```

---

### Task 5: Record editing for existing records (frontend)

**Files:**
- Modify: `src/web/assets/index.html` (drawer form: value section wrapper + record section)
- Modify: `src/web/assets/app.js` (record constants/state, envelope parsing, field rendering, `openDrawer`, `closeDrawer`, submit handler)
- Modify: `src/web/assets/style.css` (record field styles)

**Interfaces:**
- Consumes: `GET /api/types` from Task 1 (`{types: [{name, source, fields: [{name, kind, required, primary}]}]}`); `fmtDate`/expires semantics from Task 3; `showPlaceholder` untouched from Task 4.
- Produces: module state `types` (array) and `recordState` (`{typeName, secretFields, metaFields}` or `null`); `renderRecordFields(typeName, secretFields, metaFields, forNew)`; `fieldRow(name, kind, value, required)`. Task 6 reuses all of these for creation.

- [ ] **Step 1: Restructure the drawer form in `index.html`**

Replace the Value label block (lines 45–51) with a wrapped value section followed by a record section:

```html
    <div id="value-section">
      <label>Value
        <textarea name="value" rows="4" placeholder="(leave blank to keep current value)"></textarea>
        <span class="row">
          <button type="button" id="reveal">Reveal</button>
          <button type="button" id="copy">Copy</button>
        </span>
      </label>
    </div>
    <div id="record-section" hidden>
      <div id="record-type"></div>
      <div id="record-fields"></div>
    </div>
```

Append to `style.css`:

```css
#record-type { color:var(--muted); margin-bottom:.5rem; }
```

- [ ] **Step 2: Record constants, state, and helpers in `app.js`**

Below the `CANONICAL_TAGS` line, add:

```js
// Must match src/records/envelope.rs exactly.
const RECORD_CONTENT_TYPE = 'application/vnd.xv.record';
const TYPE_TAG = 'xv-type';
const FIELD_TAG_PREFIX = 'f.';

let types = []; // resolved record types from /api/types
// Non-null while the drawer holds a typed record:
// { typeName, secretFields: {name: value}, metaFields: {name: value} }
let recordState = null;

// Same rule as the TUI: the xv-type tag OR the exact record content type.
function isRecordMeta(meta) {
  return meta.content_type === RECORD_CONTENT_TYPE || !!(meta.tags || {})[TYPE_TAG];
}

// Strict, mirroring records::parse_envelope: a JSON object of strings.
function parseEnvelope(raw) {
  const obj = JSON.parse(raw);
  if (!obj || typeof obj !== 'object' || Array.isArray(obj)) throw new Error('not a JSON object');
  for (const [k, v] of Object.entries(obj)) {
    if (typeof v !== 'string') throw new Error(`field '${k}' is not a string`);
  }
  return obj;
}

function fieldRow(name, kind, value, required) {
  const label = document.createElement('label');
  label.append(`${name}${required ? ' *' : ''}`);
  const input = document.createElement('input');
  input.dataset.fieldName = name;
  input.dataset.fieldKind = kind;
  input.value = value || '';
  if (required) input.required = true;
  if (kind === 'secret') {
    input.type = 'password';
    const row = document.createElement('span');
    row.className = 'row';
    const rev = document.createElement('button');
    rev.type = 'button';
    rev.textContent = 'Reveal';
    rev.onclick = () => {
      const showing = input.type === 'text';
      input.type = showing ? 'password' : 'text';
      rev.textContent = showing ? 'Reveal' : 'Hide';
    };
    const cp = document.createElement('button');
    cp.type = 'button';
    cp.textContent = 'Copy';
    cp.onclick = async () => {
      try {
        await navigator.clipboard.writeText(input.value);
        toast('copied');
      } catch (e) { fail(e); }
    };
    row.append(rev, cp);
    label.append(input, row);
  } else {
    label.append(input);
  }
  return label;
}

// One input per field: declared fields in CLI prompt order (non-primary
// first, primary last), then undeclared extras sorted by name.
function renderRecordFields(typeName, secretFields, metaFields, forNew) {
  const type = types.find((t) => t.name === typeName);
  $('#record-type').textContent = `type: ${typeName || '(unknown)'}`;
  const container = $('#record-fields');
  container.innerHTML = '';
  const seen = new Set();
  const declared = type
    ? [...type.fields.filter((f) => !f.primary), ...type.fields.filter((f) => f.primary)]
    : [];
  for (const def of declared) {
    seen.add(def.name);
    const value = def.kind === 'secret' ? secretFields[def.name] : metaFields[def.name];
    container.appendChild(fieldRow(def.name, def.kind, value, forNew && def.required));
  }
  const extras = [
    ...Object.keys(secretFields).filter((n) => !seen.has(n)).map((n) => [n, 'secret']),
    ...Object.keys(metaFields).filter((n) => !seen.has(n)).map((n) => [n, 'metadata']),
  ].sort((a, b) => a[0].localeCompare(b[0]));
  for (const [n, kind] of extras) {
    container.appendChild(fieldRow(n, kind, kind === 'secret' ? secretFields[n] : metaFields[n], false));
  }
  $('#record-section').hidden = false;
  $('#value-section').hidden = true;
}
```

- [ ] **Step 3: Fetch types at startup**

In `init()`, after the context fetch (`ctx = await api(...)`) add:

```js
  ({ types } = await api('GET', '/api/types'));
```

- [ ] **Step 4: Open records in the drawer**

Rewrite `closeDrawer` and `openDrawer`, and add `openRecord`:

```js
function closeDrawer() {
  $('#drawer').hidden = true;
  editing = null;
  editingMeta = null;
  recordState = null;
}

async function openDrawer(name) {
  editing = name;
  editingMeta = null;
  recordState = null;
  const f = $('#secret-form');
  f.reset();
  $('#drawer-title').textContent = name ? `Edit: ${name}` : 'New secret';
  f.elements.name.value = name || '';
  f.elements.name.readOnly = false;
  $('#reveal').hidden = $('#copy').hidden = $('#delete').hidden = !name;
  $('#record-section').hidden = true;
  $('#value-section').hidden = false;
  $('#record-fields').innerHTML = '';
  $('#save').disabled = false;
  if (name) {
    try {
      const meta = await api('GET', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`);
      const tags = meta.tags || {};
      f.elements.folder.value = tags.folder || '';
      f.elements.groups.value = tags.groups || '';
      f.elements.note.value = tags.note || '';
      f.elements.expires_on.value = meta.expires_on ? meta.expires_on.slice(0, 10) : '';
      const customTags = {};
      for (const [k, v] of Object.entries(tags)) {
        // xv-type and f.* are managed by the record editor, not echoed blindly.
        if (!CANONICAL_TAGS.has(k) && k !== TYPE_TAG && !k.startsWith(FIELD_TAG_PREFIX)) {
          customTags[k] = v;
        }
      }
      editingMeta = {
        content_type: meta.content_type || '',
        tags: customTags,
        enabled: meta.enabled,
        not_before: meta.not_before || null,
      };
      if (isRecordMeta(meta)) await openRecord(name, tags);
    } catch (e) {
      // Without the fetched metadata a save would send enabled:true and no
      // custom tags — silently mutating the secret. Don't open the drawer.
      fail(e);
      editing = null;
      return;
    }
  }
  $('#drawer').hidden = false;
}

// Fetches the envelope so secret fields are editable. Values live in JS
// memory but display masked — the same exposure as the Reveal button.
async function openRecord(name, tags) {
  const { value } = await api('POST', `/api/secrets/${encodeURIComponent(name)}/value${vaultQS()}`);
  let secretFields;
  try {
    secretFields = parseEnvelope(value ?? '');
  } catch (e) {
    // Content type says record but the value isn't a valid envelope: open
    // read-only in the plain view rather than pretending fields are empty.
    toast(`record envelope is invalid: ${e.message}`, true);
    $('#save').disabled = true;
    return;
  }
  const metaFields = {};
  for (const [k, v] of Object.entries(tags)) {
    if (k.startsWith(FIELD_TAG_PREFIX)) metaFields[k.slice(FIELD_TAG_PREFIX.length)] = v;
  }
  recordState = { typeName: tags[TYPE_TAG] || '', secretFields, metaFields };
  // Whole-value reveal/copy would expose the raw envelope JSON.
  $('#reveal').hidden = $('#copy').hidden = true;
  renderRecordFields(recordState.typeName, secretFields, metaFields, false);
}
```

- [ ] **Step 5: Record-aware save**

Rewrite the submit handler (this is the complete final version, including Task 3's expires handling):

```js
$('#secret-form').onsubmit = async (ev) => {
  ev.preventDefault();
  const f = ev.target.elements;
  const name = f.name.value.trim();
  if (!name) return;
  const groups = f.groups.value.split(',').map(s => s.trim()).filter(Boolean);
  const expiresPut = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : null;
  const expiresPatch = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : '';
  try {
    if (editing && name !== editing) {
      await api('POST', `/api/secrets/${encodeURIComponent(editing)}/move${vaultQS()}`, { new_name: name });
      editing = name;
    }
    if (recordState) {
      // Records always take the full-PUT path: field edits change the value.
      const envelope = {};
      const fieldTags = {};
      for (const input of $('#record-fields').querySelectorAll('input[data-field-name]')) {
        if (!input.value) continue; // empty = omit field / drop tag
        if (input.dataset.fieldKind === 'secret') envelope[input.dataset.fieldName] = input.value;
        else fieldTags[FIELD_TAG_PREFIX + input.dataset.fieldName] = input.value;
      }
      const sorted = {};
      for (const k of Object.keys(envelope).sort()) sorted[k] = envelope[k];
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
        value: JSON.stringify(sorted),
        content_type: RECORD_CONTENT_TYPE,
        folder: f.folder.value || null,
        note: f.note.value || null,
        groups: groups.length ? groups : null,
        expires_on: expiresPut,
        tags: { ...(editingMeta?.tags || {}), [TYPE_TAG]: recordState.typeName, ...fieldTags },
        enabled: editingMeta ? editingMeta.enabled : true,
        not_before: editingMeta?.not_before || null,
      });
    } else if (f.value.value) {
      // full write: value + all metadata
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
        value: f.value.value,
        folder: f.folder.value || null,
        note: f.note.value || null,
        groups: groups.length ? groups : null,
        expires_on: expiresPut,
        content_type: editingMeta?.content_type || null,
        tags: editingMeta && Object.keys(editingMeta.tags).length ? editingMeta.tags : null,
        enabled: editingMeta ? editingMeta.enabled : true,
        not_before: editingMeta?.not_before || null,
      });
    } else if (editing) {
      // metadata-only patch ("" clears)
      await api('PATCH', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
        folder: f.folder.value,
        note: f.note.value,
        groups,
        expires_on: expiresPatch,
      });
    } else {
      throw new Error('a new secret needs a value');
    }
    closeDrawer();
    toast('saved');
    await loadSecrets();
  } catch (e) { fail(e); }
};
```

- [ ] **Step 6: Verify the build**

Run: `cargo check --features ui && cargo test --features ui web::`
Expected: clean check, all web tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/web/assets/app.js src/web/assets/index.html src/web/assets/style.css
git commit -m "feat(ui): edit typed records field-by-field in the drawer"
```

---

### Task 6: Record creation with a type picker (frontend)

**Files:**
- Modify: `src/web/assets/index.html` (type picker in the drawer form)
- Modify: `src/web/assets/app.js` (`init()` picker population, `openDrawer` picker visibility)

**Interfaces:**
- Consumes: `types`, `recordState`, `renderRecordFields(typeName, {}, {}, true)` from Task 5. The submit handler already handles `recordState` for new secrets (`editingMeta` is null → `enabled: true`).

- [ ] **Step 1: Add the picker to `index.html`**

Insert between the Name label and `<div id="value-section">`:

```html
    <label id="type-picker-label" hidden>Type <select id="type-picker"></select></label>
```

- [ ] **Step 2: Populate and wire the picker in `app.js`**

In `init()`, after the types fetch, add:

```js
  const picker = $('#type-picker');
  picker.innerHTML = '';
  const plain = document.createElement('option');
  plain.value = '';
  plain.textContent = 'plain secret';
  picker.appendChild(plain);
  for (const rt of types) {
    const opt = document.createElement('option');
    opt.value = opt.textContent = rt.name;
    picker.appendChild(opt);
  }
  picker.onchange = () => {
    if (!picker.value) {
      recordState = null;
      $('#record-section').hidden = true;
      $('#value-section').hidden = false;
      $('#record-fields').innerHTML = '';
    } else {
      recordState = { typeName: picker.value, secretFields: {}, metaFields: {} };
      renderRecordFields(picker.value, {}, {}, true);
    }
  };
```

In `openDrawer`, alongside the other section resets (after `$('#save').disabled = false;`), add:

```js
  $('#type-picker-label').hidden = !!name; // type is chosen at creation only
  $('#type-picker').value = '';
```

- [ ] **Step 3: Verify the build**

Run: `cargo check --features ui`
Expected: clean check.

- [ ] **Step 4: Commit**

```bash
git add src/web/assets/app.js src/web/assets/index.html
git commit -m "feat(ui): create typed records via a type picker"
```

---

### Task 7: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Rust gates**

Run:
```bash
cargo fmt --check && cargo clippy --features ui --all-targets && cargo test --features ui
```
Expected: fmt clean, clippy no new warnings, all tests PASS. Fix anything that fails before proceeding.

- [ ] **Step 2: Manual end-to-end against the local backend**

Drive `cargo run --features ui -- ui` (local backend). Verify each spec item:

1. Secrets **Updated** and files **Modified** columns show `YYYY-MM-DD` only.
2. On launch and on vault switch, the secrets table shows "Loading secrets…" then results; the top progress bar appears during any API call (e.g. a save).
3. Create a record: New secret → Type `login` → the Value textarea is replaced by `username`/`url`/`password` inputs (password last, masked); save; the secret lists normally.
4. Open that record: no Value textarea, `type: login` header, `username`/`url` plain, `password` masked with working per-field Reveal and Copy; edit a field, save, reopen — the edit stuck and `xv get <name> --field username` (CLI) agrees.
5. Set an expiry via the date picker; reopen — the same date shows.
6. Confirm the raw envelope JSON is never visible anywhere in the UI.

If browser automation is available (browser-harness), use it for this; otherwise ask the user to click through.

- [ ] **Step 3: Commit any fixes, then hand off**

Use the superpowers:finishing-a-development-branch skill (expect: push `web-ui-tweaks`, open a PR per the repo's dev workflow, watch Bugbot).
