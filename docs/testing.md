# Testing crosstache

The test suite has two tracks:

## Hermetic (no Azure required) — runs on every PR

Black-box CLI tests that spawn the `xv` binary and assert on its
contract: exit codes, JSON envelope shape, structured error codes,
stdout/stderr separation, file-system effects (hook installer),
ANSI-freeness of `--names-only`, etc.

These tests use a shared harness in `tests/common/mod.rs` that
isolates `XDG_CONFIG_HOME`, `HOME`, and sets `XV_NO_PARENT_CONFIG=1`
so the user's real config and any project `.xv.toml` files don't
leak into the test environment. The harness also injects fake
`AZURE_SUBSCRIPTION_ID` and `AZURE_TENANT_ID` UUIDs so config
validation passes without real credentials — `env_clear()` then
prevents accidentally inheriting a real subscription from the
developer's shell.

```bash
cargo test                        # default features
cargo test --features tui         # also runs TUI snapshot + parse tests
cargo test -- --test-threads=1    # required for env-var-mutating tests in
                                  # config::project (Plan #2)
```

Hermetic tests live in:

- `tests/common/mod.rs` — shared harness (xv_isolated, parse_json_envelope, …)
- `tests/cli_integration_tests.rs` — basic smoke + help-matrix regression guard
- `tests/error_codes_tests.rs` — exit-code contract + JSON envelope shape
- `tests/context_tests.rs` — `xv context` + `.xv.toml` resolution
- `tests/find_pagination_tests.rs` — `xv find` / `xv list` flag validation
- `tests/scan_tests.rs` — `xv scan` + hook installer edge cases
- `tests/completion_tests.rs` — shell completion generators
- `tests/config_command_tests.rs` — `xv config` command surface
- `tests/tui_view_tests.rs` — TUI rendering snapshots (feature-gated)

## Hermetic browser and accessibility tests

The embedded UI has Playwright coverage for keyboard/dialog behavior, Trash
and purge safety, and protected-value reveal/copy lifecycles. Axe scans the
initial list, sheet, nested confirmation, Secrets and Files selection,
standalone Undo notice, post-Undo restored list, populated/error Trash, purge,
file-delete confirmation, and plain/record protected-value states. Serious and
critical violations fail the test and report the owning rule and locator.

`tests/web/ui-errors.spec.js` covers recoverable failure states. It verifies
that a failed refresh keeps the last successful rows visible and marks them
Stale with Retry, and that partial bulk results remain available with Retry
failed and Copy details until resolved or dismissed. Copied diagnostics are
restricted to the structured error code, safe message and hint, backend,
vault, and failed item names; secret values, notes, authorization material,
and request headers are never included. The browser test also asserts the
operation lifecycle vocabulary (`started`, `succeeded`,
`partially-succeeded`, `cancelled`, and `failed`) and runs axe against stale
and partial-result surfaces.

Refresh and action failures use independent owned surfaces, so delayed refresh
failures and bulk partial results cannot overwrite each other's Retry or Copy
details handlers. The suite exercises both completion orders, dismissal during
an in-flight retry, handler cleanup, and suppression of late completions.
Store tests cap routine terminal operation history while retaining active
operations and currently actionable durable failures; dismissal releases the
durable diagnostic and retry state.

Install the test-only JavaScript dependencies and a worktree-local Chromium:

```bash
npm ci
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright install chromium
```

Run the focused accessibility gate or the complete browser suite:

```bash
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npm run test:a11y
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npm run test:browser
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-errors.spec.js
```

Responsive visual coverage lives in `tests/web/ui-visual.spec.js`. Four
visual-only Playwright projects exercise 1180×760, 820×560, 768×700, and
390×844; each project captures the same realistic long-name vault in light and
dark themes. The fixture fixes displayed dates and fonts, requests reduced
motion, disables screenshot animations, checks for horizontal page overflow,
and runs axe before each capture. Committed baselines are stored under
`tests/web/snapshots/<project>/`.

Generate or intentionally refresh all eight baselines, then inspect every
image before committing it:

```bash
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-visual.spec.js --update-snapshots
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-visual.spec.js
```

The `functional-chromium` project excludes the visual spec, and the four
visual projects include only that spec. This keeps ordinary browser tests at
one execution per test while allowing a mixed functional-and-visual command
to run the complete responsive gate.

The fixture builds `xv --features ui`, launches `xv ui --no-open`, and exposes
the generated `baseURL` and `vault` to each test. Each app process receives an
explicit environment with a temporary `HOME`, `XDG_CONFIG_HOME`,
`XDG_DATA_HOME`, local backend store and key paths, `XV_BACKEND=local`, and
`XV_NO_PARENT_CONFIG=1`. It cannot inherit a real Crosstache configuration or
vault, and its temporary tree is removed during teardown. Protected-value
tests install an in-page fake clipboard before navigation; they never read or
write the host clipboard.

The complete safety/accessibility phase gate is:

```bash
cargo fmt --check
cargo clippy --features ui --all-targets -- -D warnings
cargo test --features ui web:: --lib
node --test src/web/assets/*.test.js
PLAYWRIGHT_BROWSERS_PATH=.playwright-browsers npx playwright test tests/web/ui-accessibility.spec.js tests/web/ui-trash.spec.js tests/web/ui-protected-values.spec.js
```

## Desktop onboarding and product-polish coverage

The native desktop shell has a first-run Setup Required flow and a recoverable
startup screen. Run it from source with `cargo run --manifest-path
desktop/src-tauri/Cargo.toml`; with no configuration, choose Local to create a
vault or choose Azure/AWS and validate the nonsecret configuration before it is
saved. The web workspace includes the effective context rail, Trash and Undo,
the typed secret editor, command palette (`Cmd/Ctrl+K`), upload queue,
responsive stacked rows below 768px, and context-led Settings and Help sheets.

The desktop static and isolated startup checks are:

```bash
cargo test -p xv-desktop
node --test desktop/frontend/loading.test.js
node tests/desktop/startup-smoke.js
bash -n tests/desktop/package-smoke.sh
```

`tests/desktop/package-smoke.sh` builds an unsigned local macOS bundle and
launches it only with an isolated Local HOME/XDG root. It must be run on a host
that permits GUI launch; do not treat a headless `Abort trap: 6` as a package
pass. The task ledger records a separate host pass, but release/final sign-off
requires a fresh result.

## Final app UX modernization gate

Run these commands for final sign-off, then record their exact output/counts in
`docs/APP-UX-IMPLEMENTATION-EVIDENCE.md`:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
node --test src/web/assets/*.test.js desktop/frontend/*.test.js
npx playwright test
bash tests/desktop/package-smoke.sh
```

The final manual matrix is controller-owned: packaged desktop against isolated
Local plus representative invalid Azure/AWS configurations, and `xv ui`
against isolated Local, at 1180×760, 820×560, 768×700, and 390×844 in light,
dark, and keyboard-only modes. Exercise startup/recovery, scope, Trash,
typed editing, command palette, upload queue, responsive rows, Settings, and
Help without recording secret values in screenshots.

## Live integration (Azure required) — manual / weekly

Tests in `tests/e2e_integration_tests.rs` are `#[ignore]`'d by default.
They require:

- Azure CLI authentication (`az login`)
- A test Azure Key Vault (default name: `xvtestdeleteme`; configurable)
- Internet connection

Run with:

```bash
cargo test --test e2e_integration_tests -- --ignored --nocapture --test-threads=1
```

These tests:
- Use a unique prefix per run to avoid collisions
- Clean up created secrets at the end of the suite
- Are intentionally NOT in the default CI run (no Azure creds in CI)

## Authenticated backend e2e tests — manual

Two suites exercise the `AwsBackend` / `AzureBackend` types directly
(not via the `xv` binary) against the **real** cloud APIs, using the
credentials already configured for the `aws` / `az` CLIs. All tests are
`#[ignore]`'d so they never run in normal `cargo test`.

### AWS — `tests/e2e_aws_backend.rs`

Requires a working AWS identity (`aws sts get-caller-identity`) with
Secrets Manager create/read/update/delete permission.

```bash
cargo test --features aws --test e2e_aws_backend -- --ignored --nocapture --test-threads=1
```

Notes:
- Each test uses a unique timestamped vault prefix (`xv-e2e-aws-<ts>`)
  and force-purges everything it creates.
- AWS CLI v2.22+ caches credentials under `~/.aws/login/cache/`, a format
  the Rust SDK credential chain does not read. The test harness bridges
  this with `aws configure export-credentials --format env` before
  building the client.
- `ListSecrets` / `list_vaults` are eventually consistent — the harness
  polls (12 × 2s) instead of asserting immediately after a write.

### Azure — `tests/e2e_azure_backend.rs`

Requires Azure CLI auth (`az login`) and a reachable test Key Vault.
Defaults to vault `heythere`; override with `XV_E2E_AZURE_VAULT` or
`DEFAULT_VAULT`.

```bash
cargo test --test e2e_azure_backend -- --ignored --nocapture --test-threads=1
```

Notes:
- Each secret name is uniquely timestamped (`xv-e2e-az-<ts>`).
- `heythere` has purge protection enabled, so cleanup is best-effort
  soft-delete only; unique names guarantee no reuse within the recovery
  window.

## Adding a new hermetic test

1. Pick the file that fits thematically (or create a new `tests/<topic>_tests.rs`).
2. Add `mod common;` at the top.
3. Use `common::xv_isolated()` to spawn the binary in a tempdir.
4. Assert on the contract — exit code, error code, JSON envelope shape — not on incidental output text.
5. If the test mutates `std::env`, mark it `#[serial]` (via the `serial_test` crate) or run with `--test-threads=1`.
