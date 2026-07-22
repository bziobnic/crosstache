# Desktop Onboarding and Product Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an unconfigured or broken macOS desktop launch reach a verified vault, then finish the context-led visual hierarchy, settings, help, and completion evidence.

**Architecture:** Extract non-interactive setup models and atomic configuration writing from the CLI initializer into shared Rust services. Tauri exposes narrow setup/recovery commands to its bundled local frontend, then navigates to the same tokenized Axum UI after verified setup; `xv ui` retains read-only backend configuration behavior.

**Tech Stack:** Rust 2021, Tauri 2, existing local/Azure/AWS backend registry and credential chains, native HTML/CSS/JavaScript, Playwright and isolated desktop smoke tests.

**Specifications:** `docs/superpowers/specs/2026-07-22-desktop-onboarding-polish-design.md` and `docs/superpowers/specs/2026-07-22-app-ux-modernization-design.md`

## Global Constraints

- This plan starts only after the other three UX implementation plans pass.
- Desktop setup is macOS-only; shared setup services and CLI behavior stay cross-platform.
- Provider credential-chain secrets are never accepted by desktop forms or copied into diagnostics.
- Configuration replacement occurs only after parse, validation, permission-safe write, and backend verification succeed.
- A failed setup preserves the prior configuration byte-for-byte.
- `xv ui` may write presentation preferences but never backend configuration.
- Package verification uses an isolated config/store and never a real user vault.

---

### Task 1: Shared non-interactive setup models

**Files:**
- Create: `src/config/setup.rs`
- Modify: `src/config/mod.rs`
- Modify: `src/config/init.rs`
- Modify: `src/config/settings.rs`

**Interfaces:**
- Produces `SetupRequest::{Local, Azure, Aws}`, `SetupPreview`, `SetupVerification`, `SetupOutcome`, and `build_setup_config(request, base)`.
- Produces `atomic_save_config(config, path)`.

- [ ] **Step 1: Write failing model and atomic-preservation tests**

```rust
#[test]
fn local_request_builds_the_same_config_shape_as_cli_init() {
    let request = SetupRequest::Local { store_path: "/tmp/store".into(), key_file: "/tmp/key.txt".into(), vault: "default".into() };
    let config = build_setup_config(&request, Config::default()).unwrap();
    assert_eq!(config.backend.as_deref(), Some("local"));
    assert_eq!(config.local.as_ref().unwrap().default_vault.as_deref(), Some("default"));
    assert_eq!(config.clipboard_timeout, 30);
}

#[tokio::test]
async fn failed_atomic_save_preserves_existing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("xv.conf");
    tokio::fs::write(&path, b"backend = \"local\"\n").await.unwrap();
    let invalid = Config::default();
    assert!(atomic_save_config(&invalid, &path).await.is_err());
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"backend = \"local\"\n");
}
```

- [ ] **Step 2: Run and observe missing setup module failure**

Run: `cargo test --lib config::setup::tests`

Expected: compile failure because setup models and atomic writer are absent.

- [ ] **Step 3: Implement validated config construction and atomic write**

Use tagged serde models:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum SetupRequest {
    Local { store_path: PathBuf, key_file: PathBuf, vault: String },
    Azure { subscription_id: String, tenant_id: String, vault: String, resource_group: String, location: String },
    Aws { region: String, profile: Option<String>, vault_prefix: String },
}
```

Validate non-empty vault/region IDs and paths before side effects. Serialize to TOML, parse it back, call `Config::validate`, write a restrictive sibling temporary file, flush/sync, then rename. Restore/remove the temporary file on failure. Update CLI init to call the same builders/writer while keeping its interactive prompts and output unchanged.

- [ ] **Step 4: Run config and CLI-init regression tests**

Run: `cargo test --lib config::`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config/setup.rs src/config/mod.rs src/config/init.rs src/config/settings.rs
git commit -m "refactor(config): share atomic setup services"
```

### Task 2: Backend verification and safe diagnostics

**Files:**
- Modify: `src/config/setup.rs`
- Modify: `src/backend/registry.rs`
- Modify: `src/error.rs`

**Interfaces:**
- Produces `SetupVerifier` with `async fn verify(&self, config: &Config) -> Result<SetupVerification>`, `verify_setup(config) -> Result<SetupVerification>`, `setup_with_verifier(request, base, verifier)`, and `SafeSetupError { code, operation, backend, vault, message, hint, diagnostics }`.

- [ ] **Step 1: Write failing local/Azure/AWS verification tests**

```rust
#[tokio::test]
async fn local_setup_verifies_with_a_list_operation() {
    let outcome = setup_with_verifier(local_request(temp.path()), &StubVerifier::success()).await.unwrap();
    assert_eq!(outcome.verification.operation, "list-secrets");
    assert_eq!(outcome.verification.backend, "local");
}

#[test]
fn diagnostics_redact_auth_material() {
    let safe = SafeSetupError::from_message("Authorization: Bearer abc; client_secret=hunter2");
    assert!(!safe.diagnostics.contains("abc"));
    assert!(!safe.diagnostics.contains("hunter2"));
}
```

- [ ] **Step 2: Run and observe missing verifier failure**

Run: `cargo test --lib config::setup::tests::verify && cargo test --lib config::setup::tests::diagnostics`

Expected: FAIL because verifier and safe error models are absent.

- [ ] **Step 3: Implement verify-before-replace orchestration**

For Local, construct `LocalBackend` so age identity/directories are generated, then list the requested vault. For Azure and AWS, use `BackendRegistry::from_config`, `health_check`, and `list_secrets`; never accept access keys, passwords, tokens, or client secrets in `SetupRequest`. Perform verification against the candidate config before replacing the existing config. Classify auth, config, permission, network, and backend failures with safe provider-specific login hints.

- [ ] **Step 4: Run focused tests and commit**

Run: `cargo test --lib config::setup`

```bash
git add src/config/setup.rs src/backend/registry.rs src/error.rs
git commit -m "feat(config): verify setup before replacement"
```

### Task 3: Explicit desktop startup state machine and Tauri commands

**Files:**
- Modify: `desktop/src-tauri/src/main.rs`
- Modify: `desktop/src-tauri/capabilities/default.json`
- Create: `desktop/src-tauri/src/startup.rs`
- Modify: `desktop/src-tauri/Cargo.toml`

**Interfaces:**
- Produces `StartupState::{LoadingConfiguration, Connecting, SetupRequired, RecoverableFailure, Ready}`.
- Produces Tauri commands `startup_status`, `preview_setup`, `apply_setup`, `retry_startup`, `open_config`, and `copy_diagnostics`.

- [ ] **Step 1: Write failing pure state-transition tests**

```rust
#[test]
fn missing_config_routes_to_setup_required() {
    assert_eq!(transition(StartupState::LoadingConfiguration, StartupEvent::ConfigMissing), StartupState::SetupRequired);
}

#[test]
fn verified_setup_connects_before_ready() {
    assert_eq!(transition(StartupState::SetupRequired, StartupEvent::SetupVerified), StartupState::Connecting);
}
```

- [ ] **Step 2: Run and observe missing module failure**

Run: `cargo test -p xv-desktop startup::tests`

Expected: compile failure because the state machine is absent.

- [ ] **Step 3: Implement narrow Tauri command surface**

Move project-directory parsing and server startup orchestration into `startup.rs`. Store startup state in `tauri::State<Mutex<...>>`. `apply_setup` calls only the shared setup service and returns safe models. `open_config` passes the exact config path to the existing `opener` crate through a Rust command. `copy_diagnostics` copies already-redacted text. Grant only the minimal core event/window permissions needed by these commands.

- [ ] **Step 4: Emit phases and navigate only when ready**

Emit loading and connecting states without tokens. On Ready, navigate to the tokenized loopback URL; never include it in frontend errors or diagnostics. Retry reruns configuration load/registry construction without recreating the window.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p xv-desktop && cargo clippy -p xv-desktop -- -D warnings`

```bash
git add desktop/src-tauri
git commit -m "feat(desktop): add startup and setup state machine"
```

### Task 4: First-run setup and persistent recovery UI

**Files:**
- Modify: `desktop/frontend/index.html`
- Modify: `desktop/frontend/loading.js`
- Modify: `desktop/frontend/loading.css`
- Create: `desktop/frontend/loading.test.js`
- Create: `tests/desktop/startup-smoke.js`

**Interfaces:**
- Produces frontend renderers `renderStartupState(state)`, `renderSetupForm(kind)`, and `renderRecovery(error)`.

- [ ] **Step 1: Write failing state-renderer tests**

```javascript
test('setup required offers all supported backends without credential fields', () => {
  const view = renderStartupState({ kind: 'setup-required' });
  assert.match(view.textContent, /Create local vault/);
  assert.match(view.textContent, /Connect Azure/);
  assert.match(view.textContent, /Connect AWS/);
  assert.equal(view.querySelector('[name="client_secret"]'), null);
  assert.equal(view.querySelector('[name="access_key"]'), null);
});
```

- [ ] **Step 2: Run and observe current one-message UI failure**

Run: `node --test desktop/frontend/loading.test.js`

Expected: FAIL because the renderer and setup forms do not exist.

- [ ] **Step 3: Implement the four startup states**

Loading and Connecting name their phase. Setup Required offers Local, Azure, AWS, and Advanced configuration. Local collects store path, key path, and vault. Azure collects subscription, tenant, vault, resource group, and location. AWS collects region, optional profile, and vault prefix. Advanced shows config path and exact equivalent CLI commands. Forms show validation inline and call preview before apply.

Recovery displays stable code, operation, effective backend/vault, safe message, hint, expandable diagnostics, and Retry, Choose backend, Open configuration, Copy diagnostics, and Show CLI command actions. Keep it persistent until resolved.

- [ ] **Step 4: Run renderer and isolated smoke tests**

Run: `node --test desktop/frontend/loading.test.js && node tests/desktop/startup-smoke.js`

Expected: PASS for missing config, invalid config, local setup, invalid Azure, invalid AWS, Retry, and safe diagnostics.

- [ ] **Step 5: Commit**

```bash
git add desktop/frontend tests/desktop/startup-smoke.js
git commit -m "feat(desktop): add first-run setup and recovery"
```

### Task 5: Context-led visual identity, Settings, and Help

**Files:**
- Modify: `src/web/assets/context.js`
- Modify: `src/web/assets/preferences.js`
- Modify: `src/web/assets/index.html`
- Modify: `src/web/assets/style.css`
- Create: `src/web/assets/settings.js`
- Create: `src/web/assets/settings.test.js`
- Create: `tests/web/ui-settings-help.spec.js`

**Interfaces:**
- Produces `mountSettings({ preferences, securityPolicy })`, `mountHelp({ context })`, and `effectiveTheme(preference, mediaQuery)`.
- Produces `boundTimeout(requested, policy)` and `buildHelpDiagnostics(context)` as pure helpers.

- [ ] **Step 1: Write failing preference-bound and help-content tests**

```javascript
test('protected timeout cannot exceed a nonzero security policy', () => {
  assert.equal(boundTimeout(120, 30), 30);
  assert.equal(boundTimeout(15, 30), 15);
  assert.equal(boundTimeout(120, 0), 120);
});

test('help diagnostics omit the loopback token', () => {
  const text = buildHelpDiagnostics({ version: '0.26.2', configPath: '/tmp/xv.conf', url: 'http://127.0.0.1/?token=secret' });
  assert.doesNotMatch(text, /token=|secret/);
});
```

- [ ] **Step 2: Run and observe missing settings module failure**

Run: `node --test src/web/assets/settings.test.js`

Expected: FAIL because Settings/Help helpers are absent.

- [ ] **Step 3: Implement preferences and product hierarchy**

Settings includes System/Light/Dark, security-bounded timeout, density, and reset layout. Apply System live through `matchMedia`. Help includes shortcuts, capability explanations, local-session security model, config path, app/CLI version, and redacted copyable diagnostics. Use the dark forest rail, mint connection/action accent, quiet neutral canvas, high-contrast surfaces, and red only for destructive/failure states. Remove repeated generic headings/prose after first use. Keep one sheet transition plus progress/state motion; remove them under `prefers-reduced-motion`.

- [ ] **Step 4: Verify settings, help, theme, and reduced motion**

Run: `node --test src/web/assets/settings.test.js && npx playwright test tests/web/ui-settings-help.spec.js`

Expected: PASS in System/Light/Dark, policy-bound timeout, reset-layout, Help, diagnostic redaction, and reduced-motion cases.

- [ ] **Step 5: Commit**

```bash
git add src/web/assets tests/web/ui-settings-help.spec.js
git commit -m "feat(web): finish settings help and product hierarchy"
```

### Task 6: Packaged desktop verification

**Files:**
- Modify: `desktop/src-tauri/tauri.conf.json`
- Modify: `desktop/README.md`
- Modify: `desktop/HANDOFF.md`
- Create: `tests/desktop/package-smoke.sh`

**Interfaces:**
- Produces a non-signing local `.app` smoke command operating only on a temporary HOME/config/store.

- [ ] **Step 1: Write the failing package smoke script**

The script must build the app, create a temporary directory with `mktemp -d`, set isolated `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XV_NO_PARENT_CONFIG=1`, launch the `.app` executable, wait for its startup-state marker, verify a local setup/list operation, and terminate it. It must reject any resolved path outside the temporary root before cleanup.

- [ ] **Step 2: Run and observe the first packaging failure**

Run: `bash tests/desktop/package-smoke.sh`

Expected: FAIL until the script locates the built bundle and startup marker correctly.

- [ ] **Step 3: Complete package configuration and smoke assertions**

Keep macOS minimum 10.15 and app target. Ensure the chosen minimum window width exercises the 768 breakpoint. Assert the packaged executable exists, launches against missing config into Setup Required, completes isolated local setup, and reaches Ready without reading the real user config. Document source-run, test, and package commands.

- [ ] **Step 4: Run desktop gates and commit**

Run:

```bash
cargo fmt --check
cargo clippy -p xv-desktop -- -D warnings
cargo test -p xv-desktop
cargo build -p xv-desktop --release
bash tests/desktop/package-smoke.sh
```

Expected: all commands pass.

```bash
git add desktop tests/desktop/package-smoke.sh
git commit -m "test(desktop): verify packaged first-run setup"
```

### Task 7: Requirement matrix and final modernization gate

**Files:**
- Create: `docs/APP-UX-IMPLEMENTATION-EVIDENCE.md`
- Modify: `docs/APP-UX-IMPROVEMENTS.md`
- Modify: `docs/testing.md`
- Modify: `README.md`

**Interfaces:**
- Produces a 14-row evidence matrix linking production files, observed-red tests, passing commands, runtime evidence, accessibility evidence, and responsive evidence.

- [ ] **Step 1: Create the matrix with an explicit row for every backlog item**

Use these columns exactly:

```markdown
| Item | Production files | Red test observed | Passing tests | Runtime evidence | Accessibility/responsive evidence |
| ---: | --- | --- | --- | --- | --- |
| 1 | `src/web/assets/dialogs.js` | `dialogs.test.js: dirty draft guard` | command and result | desktop and browser close | focus/keyboard run |
```

Populate all 14 rows with actual commit/file/test evidence from the completed plans; do not mark the backlog complete while a cell is empty.

- [ ] **Step 2: Run the complete repository gate**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
node --test src/web/assets/*.test.js desktop/frontend/*.test.js
npx playwright test
bash tests/desktop/package-smoke.sh
```

Expected: every command passes. If environment-specific live cloud tests remain ignored by design, record the exact ignored test names and the mocked capability evidence used instead.

- [ ] **Step 3: Perform final manual evidence pass**

Launch packaged desktop against isolated Local, representative invalid Azure, and representative invalid AWS configurations. Launch `xv ui` against the isolated Local vault. Exercise all 14 backlog flows at 1180×760, 820×560, 768×700, and 390×844 in light/dark and keyboard-only modes. Record outcomes in the evidence matrix without screenshots containing secret values.

- [ ] **Step 4: Update public documentation and commit**

Update README/testing docs for startup/setup, Trash, typed editing, command palette, upload queue, responsive rows, Settings, and Help. Mark each backlog item complete only when its evidence row is complete.

```bash
git add docs/APP-UX-IMPLEMENTATION-EVIDENCE.md docs/APP-UX-IMPROVEMENTS.md docs/testing.md README.md
git commit -m "docs: record app UX modernization evidence"
```
