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

## Adding a new hermetic test

1. Pick the file that fits thematically (or create a new `tests/<topic>_tests.rs`).
2. Add `mod common;` at the top.
3. Use `common::xv_isolated()` to spawn the binary in a tempdir.
4. Assert on the contract — exit code, error code, JSON envelope shape — not on incidental output text.
5. If the test mutates `std::env`, mark it `#[serial]` (via the `serial_test` crate) or run with `--test-threads=1`.
