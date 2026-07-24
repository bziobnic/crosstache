# App Context and Core Workflows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make effective scope unmistakable, expose foldered content immediately, provide guided typed-secret workflows, and keep failures contextual and recoverable.

**Architecture:** Add a display-safe context service over existing config/project/workspace resolution, render it in a persistent vault-workspace rail, and use pure view-models for nested folders. Extract record conversion into one Rust service consumed by both CLI and Axum so the guided editor never duplicates domain rules.

**Tech Stack:** Rust 2021, Axum 0.8, native ES modules, existing backend/config/workspace/record traits, Node tests, Playwright.

**Specifications:** `docs/superpowers/specs/2026-07-22-app-context-workflows-design.md` and `docs/superpowers/specs/2026-07-22-app-ux-modernization-design.md`

## Global Constraints

- This plan starts only after `2026-07-22-app-safety-accessibility.md` passes completely.
- The UI never silently falls back to another backend, vault, project, environment, or credential source.
- Context responses contain no credential material, tokens, secret names, notes, searches, or clipboard data.
- Record conversion is one backend update; no intermediate untyped record may be visible.
- Record-to-record conversion requires `--yes` in non-interactive CLI use when fields are dropped.
- Run Rust, Node, Playwright, and axe gates inherited from the safety plan before completing this plan.

---

### Task 1: Display-safe effective context service

**Files:**
- Create: `src/web/context.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/web/testutil.rs`
- Modify: `src/config/project.rs`
- Modify: `src/workspace/resolve.rs`

**Interfaces:**
- Produces `EffectiveUiContext`, `ContextSource`, `WorkspaceEntrySummary`, and `resolve_ui_context(config, registry, cwd)`.
- Produces `GET /api/context` with `backend`, `backend_kind`, `vault`, `workspace`, `project`, `environment`, `sources`, `connection`, `capabilities`, `security`, and `version`.

- [ ] **Step 1: Write failing serialization and precedence tests**

```rust
#[tokio::test]
async fn context_names_every_effective_source_without_secrets() {
    let context = resolve_ui_context(&fixture.config, &fixture.registry, fixture.cwd()).await.unwrap();
    let json = serde_json::to_value(context).unwrap();
    assert_eq!(json["sources"]["backend"], "project-environment");
    assert_eq!(json["sources"]["vault"], "workspace-entry");
    assert_eq!(json["environment"]["name"], "prod");
    let text = json.to_string();
    assert!(!text.contains("credential"));
    assert!(!text.contains("token"));
}
```

- [ ] **Step 2: Run and observe missing service failure**

Run: `cargo test --features ui web::context::tests --lib`

Expected: compile failure because `resolve_ui_context` and its models do not exist.

- [ ] **Step 3: Implement resolution by reusing existing seams**

Define explicit serializable enums:

```rust
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContextSource { Cli, Environment, ProjectEnvironment, Project, WorkspaceEntry, GlobalConfig, BuiltIn }

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionSummary { pub state: String, pub message: Option<String> }
```

Call `find_project_config`, `resolve_env`, workspace resolution, `Config::effective_backend_name`, and `resolve_vault_for_trait`; add display-safe helper functions to their owning modules only when current APIs cannot report the winning source. Derive capabilities from the resolved backend instance, not the registry default. Keep handlers parse/delegate/serialize only.

- [ ] **Step 4: Run focused tests and commit**

Run: `cargo test --features ui web::context::tests --lib`

```bash
git add src/web/context.rs src/web/mod.rs src/web/testutil.rs src/config/project.rs src/workspace/resolve.rs
git commit -m "feat(web): report effective vault context"
```

### Task 2: Persistent context rail and guarded workspace switching

**Files:**
- Create: `src/web/assets/context.js`
- Create: `src/web/assets/context.test.js`
- Modify: `src/web/assets/app.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Modify: `src/web/mod.rs`
- Create: `tests/web/ui-context.spec.js`

**Interfaces:**
- Produces `formatContextLine(context)`, `contextDetails(context)`, and `mountContextRail({ store, api, guardNavigation })`.

- [ ] **Step 1: Write failing pure-format and browser tests**

```javascript
test('context line keeps backend and vault unambiguous', () => {
  assert.equal(formatContextLine({ backend: { name: 'az-prod' }, vault: { name: 'payments' }, project: { name: 'checkout' }, environment: { name: 'prod' } }),
    'az-prod / payments · checkout · prod');
});
```

The browser test opens New secret and asserts the sheet repeats `az-prod / payments`, then attempts a workspace switch with a dirty draft and verifies the guard preserves it.

- [ ] **Step 2: Run and observe missing module/UI failures**

Run: `node --test src/web/assets/context.test.js && npx playwright test tests/web/ui-context.spec.js`

Expected: FAIL because the context rail and switching contract do not exist.

- [ ] **Step 3: Implement context-led layout**

Add a dark forest context rail containing backend/vault, project/environment, connection state, capability limitations, workspace selector, Commands, Help, Settings, theme, and version. Details disclose the source for each effective value. Every mutation sheet, confirmation, upload, and progress state consumes the same store snapshot to repeat backend/vault.

Workspace activation calls a context route with the selected registry entry and vault. Abort obsolete reads, run `guardNavigation` first, lock switching during saves, and never mutate the current snapshot until the new context and initial secret list both succeed.

- [ ] **Step 4: Verify and commit**

Run: `node --test src/web/assets/context.test.js && npx playwright test tests/web/ui-context.spec.js`

```bash
git add src/web/assets/context.js src/web/assets/context.test.js src/web/assets/app.js src/web/assets/index.html src/web/assets/style.css src/web/mod.rs tests/web/ui-context.spec.js
git commit -m "feat(web): add effective context workspace rail"
```

### Task 3: Nested folder tree and expansion preferences

**Files:**
- Modify: `src/web/assets/ui-model.js`
- Modify: `src/web/assets/ui-model.test.js`
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Create: `tests/web/ui-folders.spec.js`

**Interfaces:**
- Produces `buildFolderTree(items)`, `initialExpansion({ total, saved })`, and `folderPreferenceKey({ backend, vault, surface })`.

- [ ] **Step 1: Write failing tree and threshold tests**

```javascript
test('slash paths become nested folder nodes with stable unfiled node', () => {
  const tree = model.buildFolderTree([{ name: 'a', folder: 'apps/prod' }, { name: 'b', folder: null }]);
  assert.deepEqual(tree.map(node => node.id), ['__unfiled__', 'apps']);
  assert.equal(tree[1].children[0].id, 'apps/prod');
});

test('small vaults expand and large vaults honor saved state', () => {
  assert.equal(model.initialExpansion({ total: 50, saved: null }), 'all');
  assert.equal(model.initialExpansion({ total: 51, saved: null }), 'collapsed');
  assert.deepEqual(model.initialExpansion({ total: 51, saved: ['apps'] }), ['apps']);
});
```

- [ ] **Step 2: Run and observe missing helper failure**

Run: `node --test src/web/assets/ui-model.test.js --test-name-pattern "folder|vaults expand"`

Expected: FAIL because the tree helpers are absent.

- [ ] **Step 3: Implement semantic desktop tree and mobile filter**

Build nodes from path segments, sort with the existing collator, include a stable Unfiled node, and calculate visible/total counts separately. Render `role="tree"`/`treeitem` with `aria-expanded`, roving tabindex, Left/Right navigation, and selected-folder filtering on desktop. Reuse the same model in a labelled mobile filter sheet. Persist expansion under backend registry name + vault + surface. Keep Expand all and Collapse all visible at every size.

- [ ] **Step 4: Run browser coverage and commit**

Run: `node --test src/web/assets/ui-model.test.js && npx playwright test tests/web/ui-folders.spec.js`

```bash
git add src/web/assets/ui-model.js src/web/assets/ui-model.test.js src/web/assets/secrets.js src/web/assets/index.html src/web/assets/style.css tests/web/ui-folders.spec.js
git commit -m "feat(web): add hierarchical folder navigation"
```

### Task 4: Shared record conversion service

**Files:**
- Create: `src/records/conversion.rs`
- Modify: `src/records/mod.rs`
- Modify: `src/cli/secret_ops.rs`
- Modify: `src/cli/commands.rs`
- Modify: `tests/e2e_record_types.rs`

**Interfaces:**
- Produces `ConversionRequest { target: ConversionTarget, supplied_fields, confirm_lossy }` and `ConversionRequest::to_type(name: impl Into<String>)` with empty supplied fields and `confirm_lossy: false`.
- Produces `ConversionPreview { retained, renamed, dropped, target_type, requires_confirmation }`.
- Produces `preview_conversion(secret, types, request)` and `apply_conversion(backend, vault, name, preview)`.

- [ ] **Step 1: Write failing pure conversion tests**

```rust
#[test]
fn record_to_record_preview_names_retained_and_dropped_fields() {
    let preview = preview_conversion(&login_record(), &types(), ConversionRequest::to_type("database")).unwrap();
    assert_eq!(preview.retained, vec!["password"]);
    assert_eq!(preview.dropped, vec!["username"]);
    assert!(preview.requires_confirmation);
}

#[test]
fn plain_to_record_maps_value_to_target_primary() {
    let preview = preview_conversion(&plain("token"), &types(), ConversionRequest::to_type("api-key")).unwrap();
    assert_eq!(preview.target_secret_fields["key"], "token");
}
```

- [ ] **Step 2: Run and observe missing conversion module**

Run: `cargo test --lib records::conversion::tests`

Expected: compile failure because the module does not exist.

- [ ] **Step 3: Implement one atomic conversion shape**

Move tag preservation, envelope parsing/encoding, tag-budget checks, denormalized groups/note/folder handling, and primary-field selection out of CLI code into `conversion.rs`. `apply_conversion` constructs exactly one `SecretUpdateRequest`; it never performs an untyped intermediate write. Match fields by exact field name; explicit supplied-field values override matches. Sort retained/renamed/dropped lists for deterministic output.

- [ ] **Step 4: Expand CLI behavior with loss confirmation**

Change `--type` help to accept plain or typed records. Call `preview_conversion`, print the field-impact summary, and call existing `confirm_record_action(yes, prompt)` when `requires_confirmation`. Preserve the existing `--yes` flag. Replace `execute_record_type_conversion` and `execute_record_untype` bodies with service calls while preserving success text and cache invalidation.

- [ ] **Step 5: Run CLI and unit tests**

Run: `cargo test --lib records::conversion && cargo test --lib cli::secret_ops && cargo test --test e2e_record_types`

Expected: PASS, including the former existing-record error test updated to assert successful safe conversion and refusal of lossy non-TTY conversion without `--yes`.

- [ ] **Step 6: Commit**

```bash
git add src/records/conversion.rs src/records/mod.rs src/cli/secret_ops.rs src/cli/commands.rs tests/e2e_record_types.rs
git commit -m "feat(records): share atomic type conversion"
```

### Task 5: Conversion, rename, and validation API

**Files:**
- Modify: `src/web/secrets.rs`
- Modify: `src/web/mod.rs`
- Modify: `src/web/errors.rs`
- Modify: `src/web/testutil.rs`

**Interfaces:**
- Produces `POST /api/secrets/{name}/conversion/preview`, `POST /api/secrets/{name}/conversion`, and `POST /api/secrets/{name}/rename`.

- [ ] **Step 1: Write failing Axum tests**

```rust
let (_, preview) = get_json(app.clone(), "POST", "/api/secrets/login/conversion/preview", Some(json!({"target_type":"database"}))).await;
assert_eq!(preview["dropped"], json!(["username"]));
let (status, body) = get_json(app.clone(), "POST", "/api/secrets/login/conversion", Some(json!({"target_type":"database","confirm_lossy":false}))).await;
assert_eq!(status, StatusCode::CONFLICT);
assert_eq!(body["error"]["code"], "xv-conversion-confirmation-required");
```

- [ ] **Step 2: Run and observe 404 failures**

Run: `cargo test --features ui web::secrets::tests::conversion --lib`

Expected: FAIL with missing routes.

- [ ] **Step 3: Implement thin service-backed routes**

Deserialize stable request models, resolve the target type from `WebState.types`, and call the shared conversion functions. Rename preflights `secret_exists`; conflicts attach `field: "name"`. Return the updated `SecretProperties` and a conversion summary. Do not accept an unrelated metadata payload on rename.

- [ ] **Step 4: Verify and commit**

Run: `cargo test --features ui web::secrets::tests --lib`

```bash
git add src/web/secrets.rs src/web/mod.rs src/web/errors.rs src/web/testutil.rs
git commit -m "feat(web): expose typed conversion and rename"
```

### Task 6: Guided typed editor, chips, folders, and dates

**Files:**
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/ui-model.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Create: `src/web/assets/typed-editor.test.js`
- Create: `tests/web/ui-typed-editor.spec.js`

**Interfaces:**
- Produces `typeCards(types)`, `buildTypedDraft(type, properties)`, `groupSuggestions(items)`, and `conversionSummary(preview)`.

- [ ] **Step 1: Write failing model tests**

```javascript
test('type cards expose required protected and primary fields', () => {
  const card = typeCards([{ name: 'login', fields: [
    { name: 'username', kind: 'metadata', required: true, primary: false },
    { name: 'password', kind: 'secret', required: true, primary: true },
  ] }])[0];
  assert.deepEqual(card.required, ['username', 'password']);
  assert.deepEqual(card.protected, ['password']);
  assert.equal(card.primary, 'password');
});
```

- [ ] **Step 2: Run and observe missing editor-model failure**

Run: `node --test src/web/assets/typed-editor.test.js`

Expected: FAIL because the functions are absent.

- [ ] **Step 3: Implement the guided editor**

Creation starts with Plain plus resolved type cards. Only selected-type fields render. Required/optional/protected/primary help is inline. Existing records show type plus a separate Convert action and impact preview; loss requires explicit confirmation. Rename is a separate nested workflow. Replace comma groups with removable chips and existing-group suggestions, folder with autocomplete, and expiry with `input type="date"`, No expiry, and Clear. Preserve custom tags, enabled/not-before states, and untouched protected fields in every save request.

- [ ] **Step 4: Map durable field errors and verify browser flows**

Use `ApiError.field` to focus and describe the relevant control without clearing the draft. Run: `node --test src/web/assets/typed-editor.test.js && npx playwright test tests/web/ui-typed-editor.spec.js`

Expected: PASS for plain creation, typed creation, conversion preview/confirm, isolated rename, chips, folder suggestion, date clear, and inline validation.

- [ ] **Step 5: Commit**

```bash
git add src/web/assets tests/web/ui-typed-editor.spec.js
git commit -m "feat(web): add guided typed secret editor"
```

### Task 7: Persistent list, form, and partial-result failures

**Files:**
- Modify: `src/web/assets/api-client.js`
- Modify: `src/web/assets/store.js`
- Modify: `src/web/assets/secrets.js`
- Modify: `src/web/assets/context.js`
- Create: `tests/web/ui-errors.spec.js`
- Modify: `docs/testing.md`

**Interfaces:**
- Produces event statuses `started`, `succeeded`, `partially-succeeded`, `cancelled`, and `failed` with `operationId`.

- [ ] **Step 1: Write failing stale-view and partial-result tests**

The browser test fulfills the first list request, rejects refresh, and asserts old rows remain with a Stale marker and Retry. It then returns a bulk result with one success and one failure and asserts Retry failed and Copy details remain until dismissed.

- [ ] **Step 2: Run and observe transient/error-clearing failure**

Run: `npx playwright test tests/web/ui-errors.spec.js`

Expected: FAIL because refresh failures replace content and bulk feedback is transient.

- [ ] **Step 3: Implement explicit operation events**

Every async operation dispatches the exact status vocabulary with an operation ID. Abort obsolete reads. Ignore late completions that do not match the current generation. Keep the last successful list snapshot on connection failure and mark it stale. Persist list Retry, form errors, and bulk results. Diagnostics include code, safe message, hint, backend, vault, and failed names only; exclude values, notes, auth material, and raw request headers.

- [ ] **Step 4: Run the plan gate and commit**

Run:

```bash
cargo fmt --check
cargo clippy --features ui --all-targets -- -D warnings
cargo test --features ui web:: --lib
cargo test --test e2e_record_types
node --test src/web/assets/*.test.js
npx playwright test tests/web/ui-context.spec.js tests/web/ui-folders.spec.js tests/web/ui-typed-editor.spec.js tests/web/ui-errors.spec.js
```

Expected: all commands pass and axe has no serious or critical violations in rail, tree, editor, conversion, and error states.

```bash
git add src/web/assets tests/web/ui-errors.spec.js docs/testing.md
git commit -m "feat(web): keep failures contextual and recoverable"
```
