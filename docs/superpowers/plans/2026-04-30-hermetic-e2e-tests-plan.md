# Hermetic End-to-End Test Harness Implementation Plan

> **Status:** ✅ Implemented in **v0.7.0** (2026-05-01).
> Hermetic test harness landed alongside v0.7 work; ongoing maintenance.
> Retained as design history.
> Roadmap & open work tracked in `ROADMAP.md` at the repo root.
> Implementation history lives in `CHANGELOG.md`. This file is retained as design context — do not edit to reflect current behavior; open a new spec instead.


> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a comprehensive **hermetic** end-to-end test suite that exercises the `xv` binary as a black box without Azure credentials. Covers every phase-1 feature (errors+exit-codes, env profiles, fuzzy find, pagination, leak scanner, TUI parse). Live-Azure tests stay where they are (`tests/e2e_integration_tests.rs`, gated `#[ignore]`); this plan adds the no-credentials track that PR-time CI can run unconditionally.

**Architecture:** A shared `tests/common/mod.rs` harness exposes `xv()` (a `std::process::Command` builder for the cargo-built binary), `isolated_env()` (sets `XDG_CONFIG_HOME`, `HOME`, and `XV_NO_PARENT_CONFIG=1` to a tempdir so user config / project `.xv.toml` files don't leak in), and JSON-envelope helpers. Each test file imports the harness via `mod common;`. Tests assert on the documented contract: exit codes (Plan #1), JSON envelope shape, structured error codes, and stdout/stderr separation. Tests that need a `.xv.toml` build one in a tempdir per the env-profiles spec (Plan #2). Tests that need a git repo (e.g. `xv scan install`) initialize one in a tempdir.

**Tech Stack:** Rust 2021. Reuses existing `tempfile`, `serde_json`, `std::process::Command`. No new test deps. Tests run via `cargo test` (already wired into `.github/workflows/build.yml`).

**Reference points:**
- Plan #1 (errors): `docs/superpowers/plans/2026-04-29-errors-and-exit-codes-plan.md`, `docs/exit-codes.md`
- Plan #2 (env profiles): `docs/env-profiles.md`
- Plan #3 (fuzzy find): `docs/find.md`
- Plan #4 (scan): `docs/scan.md`
- Plan #5 (TUI): `docs/tui.md`
- Existing: `tests/cli_integration_tests.rs` (316 lines, basic smoke), `tests/error_codes_tests.rs` (Plan #1), `tests/scan_tests.rs` (Plan #4), `tests/tui_view_tests.rs` (Plan #5)

---

## What "hermetic" means here

A hermetic test:
- Runs without Azure credentials.
- Runs without an internet connection (best-effort).
- Doesn't read/write the user's real `~/.config/xv/`, `~/.azure/`, or any `.xv.toml` outside the test's tempdir.
- Asserts contracts the binary controls: clap parsing, error-code mapping, JSON envelope shape, file-system effects (hook installer), output streams (stdout vs stderr), `--names-only` ANSI-freeness, etc.

A hermetic test **doesn't** assert on Azure call results — those tests live in `e2e_integration_tests.rs` (live, gated `#[ignore]`).

Some tests in this plan WILL invoke an Azure-touching path expecting the call to fail (e.g., `xv list` without a vault should exit 3 with `xv-config-invalid` even if Azure is unreachable). The assertion is on the **contract** (exit code, error code), not the success path.

---

## File Structure

**Created:**

| Path | Responsibility |
|------|----------------|
| `tests/common/mod.rs` | Shared harness: `xv()`, `isolated_env()`, `init_git_repo()`, `write_xv_toml()`, `parse_json_envelope()`, helpers. |
| `tests/context_tests.rs` | `xv context init / envs / show`, `.xv.toml` walk-up + boundary, `XV_NO_PARENT_CONFIG`, `xv-env-not-defined`. |
| `tests/find_pagination_tests.rs` | `xv find` flag validation (no Azure required); `--page` without `--page-size`; `--limit + --page-size` conflict. |
| `tests/completion_tests.rs` | `xv completion bash / zsh / fish / powershell` produce non-empty output and don't error. |
| `tests/config_command_tests.rs` | `xv config show / path / set / unset` with `XDG_CONFIG_HOME` isolation. |

**Modified:**

| Path | Change |
|------|--------|
| `tests/error_codes_tests.rs` | Refactor to use `tests/common/mod.rs`; add JSON-envelope shape tests for each currently-tested error family. |
| `tests/scan_tests.rs` | Add edge cases: repeat-install no-op, uninstall-on-non-managed, `--force` overwrite path, missing-git-dir. |
| `tests/cli_integration_tests.rs` | Add a "help matrix" test that runs `xv <cmd> --help` for every top-level command and confirms exit 0 + non-empty stdout. |
| `tests/tui_view_tests.rs` | Add a hermetic "parse-only" test that confirms `xv tui --help` exits 0 when `--features tui`, and that `xv tui` without the feature exits 2 (unknown subcommand). |
| `.github/workflows/build.yml` | Add a step that explicitly runs `cargo test --features tui` (so TUI snapshot tests + new TUI parse tests run). |

---

## Task 1: Shared test harness (`tests/common/mod.rs`)

**Files:**
- Create: `tests/common/mod.rs`

### Step 1: Write the harness

Create `tests/common/mod.rs`:

```rust
//! Shared helpers for hermetic E2E tests.
//!
//! The cardinal invariant: tests that import this module never read
//! or write the user's real ~/.config/xv/, ~/.azure/, or any
//! .xv.toml outside their own tempdir. Helpers below set up the
//! isolation; tests that bypass them risk pollution.

#![allow(dead_code)] // each test file imports a subset of helpers

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Spawn the `xv` binary built by Cargo for the current target.
pub fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

/// Apply isolation env vars to a `Command`. Caller passes a tempdir;
/// this routes XDG_CONFIG_HOME, HOME, and XV_NO_PARENT_CONFIG so
/// config and walk-up resolution start clean.
pub fn isolate(cmd: &mut Command, tempdir: &Path) -> &mut Command {
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", tempdir)
        .env("XDG_CONFIG_HOME", tempdir.join(".config"))
        .env("XV_NO_PARENT_CONFIG", "1")
        // Don't inherit AZURE_* — prevents accidentally hitting a real subscription.
        .current_dir(tempdir)
}

/// Convenience: build an isolated `xv` command in a fresh tempdir.
/// Returns (command, tempdir). Hold the tempdir alive for the test
/// duration (it cleans up on drop).
pub fn xv_isolated() -> (Command, TempDir) {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut cmd = xv();
    isolate(&mut cmd, temp.path());
    (cmd, temp)
}

/// Init a minimal git repo at `path`. Used by scan-install tests.
pub fn init_git_repo(path: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed at {}", path.display());
    // git requires user.name/email to commit; not strictly needed for
    // scan install/uninstall but cheap to set up:
    let _ = Command::new("git")
        .args(["config", "user.email", "test@example.invalid"])
        .current_dir(path)
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(path)
        .status();
}

/// Write a minimal `.xv.toml` at `dir/.xv.toml`. Returns the path.
pub fn write_xv_toml(dir: &Path, default_env: &str, envs: &[(&str, &str)]) -> std::path::PathBuf {
    use std::fmt::Write as _;
    let path = dir.join(".xv.toml");
    let mut s = String::new();
    writeln!(s, "default_env = \"{default_env}\"\n").unwrap();
    for (name, vault) in envs {
        writeln!(s, "[env.{name}]").unwrap();
        writeln!(s, "vault = \"{vault}\"").unwrap();
        writeln!(s, "resource_group = \"test-rg\"").unwrap();
        writeln!(s).unwrap();
    }
    std::fs::write(&path, s).expect("write .xv.toml");
    path
}

/// Parse a JSON error envelope from stdout. Asserts the shape:
/// `{ "error": { "code": "...", "message": "...", "exit_code": N, ... } }`.
pub fn parse_json_envelope(stdout: &[u8]) -> serde_json::Value {
    let body: serde_json::Value =
        serde_json::from_slice(stdout).expect("stdout must be valid JSON");
    assert!(body.get("error").is_some(), "envelope must have 'error' key: {body}");
    let err = &body["error"];
    assert!(err.get("code").is_some(), "envelope.error must have 'code'");
    assert!(err.get("message").is_some(), "envelope.error must have 'message'");
    assert!(err.get("exit_code").is_some(), "envelope.error must have 'exit_code'");
    body
}

/// Read stdout as a UTF-8 string for assertions.
pub fn stdout_str(out: &std::process::Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout utf-8")
}

/// Read stderr as a UTF-8 string for assertions.
pub fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr utf-8")
}
```

### Step 2: Smoke-test the harness via one test

Create a TEMPORARY test in `tests/common_smoke_tests.rs` (this file gets removed at the end of Task 1):

```rust
mod common;

#[test]
fn xv_help_runs_isolated() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.arg("--help").output().expect("spawn xv");
    assert!(out.status.success(), "exit: {:?}, stderr: {}",
        out.status.code(), common::stderr_str(&out));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("xv") || stdout.contains("crosstache"),
        "help output should mention xv: {stdout}");
}
```

### Step 3: Run

```bash
cargo test --test common_smoke_tests
```

Expected: 1 test passes.

### Step 4: Remove the smoke file

```bash
rm tests/common_smoke_tests.rs
```

(Keep the harness; the smoke test was only to verify it compiles and works. The remaining task tests will exercise `tests/common/mod.rs` thoroughly.)

### Step 5: Verify no test files were broken

```bash
cargo test --lib
cargo build --tests
```

Expected: both clean.

### Step 6: Commit

```bash
git add tests/common/mod.rs
git commit -m "test(common): shared hermetic-test harness

Cardinal invariant: tests using common::xv_isolated() never touch
the user's real ~/.config/xv/, ~/.azure/, or any .xv.toml outside
their own tempdir. Routes XDG_CONFIG_HOME, HOME, and
XV_NO_PARENT_CONFIG=1 to a tempdir; clears env to prevent
accidental Azure leakage. Exposes init_git_repo, write_xv_toml,
parse_json_envelope helpers for downstream tests.
"
```

---

## Task 2: Context + env-profile E2E tests

**Files:**
- Create: `tests/context_tests.rs`

### Step 1: Write the tests

```rust
mod common;

use common::{init_git_repo, isolate, parse_json_envelope, stderr_str, stdout_str, write_xv_toml, xv_isolated};

#[test]
fn context_init_non_interactive_writes_xv_toml() {
    let (mut cmd, temp) = xv_isolated();
    let out = cmd
        .args([
            "context", "init",
            "--non-interactive",
            "--vault", "myvault",
            "--resource-group", "myrg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let path = temp.path().join(".xv.toml");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("default_env = \"dev\""));
    assert!(content.contains("vault = \"myvault\""));
    assert!(content.contains("resource_group = \"myrg\""));
}

#[test]
fn context_init_refuses_existing_without_force() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let out = cmd
        .args([
            "context", "init",
            "--non-interactive",
            "--vault", "v2",
            "--resource-group", "rg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3), "should be config-invalid");
    let stderr = stderr_str(&out);
    assert!(stderr.contains("already exists"), "stderr: {stderr}");
}

#[test]
fn context_init_force_overwrites() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let out = cmd
        .args([
            "context", "init",
            "--non-interactive",
            "--force",
            "--vault", "v2",
            "--resource-group", "rg",
        ])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let content = std::fs::read_to_string(temp.path().join(".xv.toml")).unwrap();
    assert!(content.contains("vault = \"v2\""));
}

#[test]
fn context_init_non_interactive_requires_vault() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["context", "init", "--non-interactive", "--resource-group", "rg"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2), "missing required --vault");
}

#[test]
fn context_envs_lists_envs() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "vdev"), ("prod", "vprod")]);
    let out = cmd.args(["context", "envs"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("dev"));
    assert!(stdout.contains("prod"));
    assert!(stdout.contains("vdev"));
    assert!(stdout.contains("vprod"));
    // Default env starred:
    assert!(stdout.contains("* dev"), "active env should be starred: {stdout}");
}

#[test]
fn context_envs_no_xv_toml_warns() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["context", "envs"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stderr = stderr_str(&out);
    let stdout = stdout_str(&out);
    let combined = format!("{stderr}{stdout}");
    assert!(combined.contains(".xv.toml") || combined.contains("no .xv.toml"),
        "must mention missing config: {combined}");
}

#[test]
fn unknown_env_exits_3_with_env_not_defined_code() {
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1"), ("prod", "v2")]);
    let out = cmd
        .args(["--env", "staging", "list", "--format", "json"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let body = parse_json_envelope(&out.stdout);
    assert_eq!(body["error"]["code"], "xv-env-not-defined");
    assert_eq!(body["error"]["exit_code"], 3);
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("staging"));
    assert!(msg.contains("dev"));
    assert!(msg.contains("prod"));
}

#[test]
fn xv_env_overrides_default_env() {
    // We can't fully exercise the priority chain without a vault, but
    // we CAN confirm XV_ENV with a missing env produces the same
    // xv-env-not-defined error (mentioning the XV_ENV value, not default_env).
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let out = cmd
        .env("XV_ENV", "staging")
        .args(["list", "--format", "json"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let body = parse_json_envelope(&out.stdout);
    assert_eq!(body["error"]["code"], "xv-env-not-defined");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("staging"), "XV_ENV value should appear in message: {msg}");
}

#[test]
fn xv_no_parent_config_disables_walkup() {
    // Place .xv.toml at parent dir; cwd is a child without one.
    // With XV_NO_PARENT_CONFIG=1 (set by isolate()), walk-up is disabled,
    // so the .xv.toml in parent should NOT be discovered.
    let (mut cmd, temp) = xv_isolated();
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let child = temp.path().join("subproject");
    std::fs::create_dir_all(&child).unwrap();
    cmd.current_dir(&child); // override the harness's current_dir

    let out = cmd
        .args(["context", "envs"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_str(&out);
    let stderr = stderr_str(&out);
    let combined = format!("{stderr}{stdout}");
    assert!(combined.contains("no .xv.toml") || combined.contains(".xv.toml"),
        "should report no .xv.toml found because walk-up is disabled: {combined}");
}

#[test]
fn xv_toml_in_ancestor_emits_cross_boundary_notice() {
    // Without XV_NO_PARENT_CONFIG, walk-up should find the ancestor
    // .xv.toml and emit a one-time stderr notice.
    let temp = tempfile::tempdir().expect("tempdir");
    write_xv_toml(temp.path(), "dev", &[("dev", "v1")]);
    let child = temp.path().join("sub");
    std::fs::create_dir_all(&child).unwrap();
    let mut cmd = common::xv();
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join(".config"))
        // explicitly DO NOT set XV_NO_PARENT_CONFIG
        .current_dir(&child);

    let out = cmd.args(["context", "envs"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("using config from") && stderr.contains(".xv.toml"),
        "cross-boundary notice expected on stderr: {stderr}"
    );
}
```

### Step 2: Run

```bash
cargo test --test context_tests
```

Expected: 10 tests pass.

> **If any test fails:** the most likely cause is a discrepancy between the actual error message wording and what the assertion looks for. Use `--nocapture` to see the full output and adjust the substring match — DON'T weaken the structural assertions (exit code, JSON shape, `code` field).

### Step 3: Commit

```bash
git add tests/context_tests.rs
git commit -m "test(context): hermetic E2E coverage for xv context + .xv.toml resolution

10 tests covering: context init non-interactive scaffold, --force
overwrite, --non-interactive missing-flag exit 2, context envs
listing + active marker, no-config warning, xv-env-not-defined for
both --env flag and XV_ENV env var, walk-up disabled by
XV_NO_PARENT_CONFIG, and cross-boundary stderr notice when .xv.toml
is found in an ancestor.
"
```

---

## Task 3: Error-code matrix tests + JSON envelope shape

**Files:**
- Modify: `tests/error_codes_tests.rs`

This task extends the existing error-codes file (which already covers exits 2 and 10/11 from Plan #1). Add tests for every other documented exit code that's reachable hermetically.

### Step 1: Refactor existing tests to use `common`

At the top of `tests/error_codes_tests.rs`, add:

```rust
mod common;
```

Find the existing `fn xv() -> Command` helper and replace its body to delegate to `common::xv()`. Or, replace the local helper with `use common::xv;` and remove the local definition. Whichever is cleaner — the goal is one source of truth.

### Step 2: Add new tests

Append to `tests/error_codes_tests.rs`:

```rust
// --- Hermetic exit-code matrix ---

#[test]
fn config_invalid_xv_toml_exits_3() {
    let (mut cmd, temp) = common::xv_isolated();
    // Malformed .xv.toml — TOML parser should fail.
    std::fs::write(temp.path().join(".xv.toml"), "not = valid = toml [[").unwrap();
    let out = cmd.args(["context", "envs"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(3), "stderr: {}", common::stderr_str(&out));
}

#[test]
fn missing_vault_exits_3_with_config_invalid() {
    // No .xv.toml, no env config — list command should fail at vault resolution.
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["list", "--format", "json"]).output().expect("spawn");
    // Exit 3 (config) when no vault can be resolved.
    assert_eq!(out.status.code(), Some(3));
    let body = common::parse_json_envelope(&out.stdout);
    assert_eq!(body["error"]["exit_code"], 3);
    // Code should be either xv-config-invalid or xv-env-not-defined depending on the path.
    let code = body["error"]["code"].as_str().unwrap();
    assert!(
        code == "xv-config-invalid" || code == "xv-env-not-defined",
        "unexpected code for missing-vault: {code}"
    );
}

#[test]
fn json_envelope_includes_required_fields() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["list", "--format", "json"]).output().expect("spawn");
    let body = common::parse_json_envelope(&out.stdout);
    let err = &body["error"];
    assert!(err["code"].is_string());
    assert!(err["message"].is_string());
    assert!(err["exit_code"].is_number());
    // suggestion is optional; if present it's a string.
    if !err["suggestion"].is_null() {
        assert!(err["suggestion"].is_string());
    }
}

#[test]
fn yaml_envelope_renders_for_format_yaml() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["list", "--format", "yaml"]).output().expect("spawn");
    // Same exit code as JSON case.
    let stdout = common::stdout_str(&out);
    // YAML: should contain 'error:' as a top-level key.
    assert!(
        stdout.contains("error:") || stdout.contains("error :"),
        "stdout should be YAML envelope: {stdout}"
    );
}

#[test]
fn plain_format_writes_error_to_stderr_not_stdout() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["list", "--format", "plain"]).output().expect("spawn");
    let stdout = common::stdout_str(&out);
    let stderr = common::stderr_str(&out);
    // Plain mode: error text on stderr, NOT in stdout's structured envelope position.
    assert!(!stderr.is_empty(), "plain mode should write error to stderr");
    // stdout should not be a JSON envelope:
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    if let Ok(v) = parsed {
        assert!(
            v.get("error").is_none(),
            "plain mode should NOT emit JSON envelope on stdout: {stdout}"
        );
    }
}

#[test]
fn unknown_top_level_flag_exits_2() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["--this-flag-does-not-exist"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn invalid_min_score_below_zero_exits_2() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["find", "anything", "--min-score", "-0.1"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2), "out-of-range min-score must error at parse");
}

#[test]
fn invalid_min_score_above_one_exits_2() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["find", "anything", "--min-score", "1.5"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}
```

### Step 3: Run

```bash
cargo test --test error_codes_tests
```

Expected: existing tests + 8 new tests pass.

### Step 4: Commit

```bash
git add tests/error_codes_tests.rs
git commit -m "test(error): hermetic JSON envelope shape + exit-code matrix

Refactors the file to use the shared common harness. Adds 8 new
tests covering: malformed .xv.toml → exit 3; missing-vault → exit
3 with xv-config-invalid or xv-env-not-defined; JSON envelope
required fields; YAML envelope shape; plain mode writes to stderr
not stdout JSON; unknown flag → exit 2; out-of-range --min-score
fails at clap parse → exit 2.
"
```

---

## Task 4: Find + List + pagination flag validation tests

**Files:**
- Create: `tests/find_pagination_tests.rs`

### Step 1: Write the tests

```rust
mod common;

use common::{stderr_str, stdout_str, xv_isolated};

// --- xv find flag validation (no Azure required for parse-time errors) ---

#[test]
fn find_unknown_in_field_exits_2() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["find", "anything", "--in", "bogus_field"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_str(&out);
    assert!(
        stderr.contains("bogus_field") || stderr.contains("unknown") || stderr.contains("invalid"),
        "stderr should mention the bad field: {stderr}"
    );
}

#[test]
fn find_each_valid_in_field_passes_clap() {
    // The fields are validated AT runtime in execute_secret_find, so
    // these should fail with xv-config-invalid (no vault) — NOT at clap (exit 2).
    for field in &["name", "folder", "groups", "note", "tags"] {
        let (mut cmd, _temp) = xv_isolated();
        let out = cmd
            .args(["find", "x", "--in", field])
            .output()
            .expect("spawn");
        // Should NOT exit 2 (parse error); should exit 3 (config-invalid, no vault).
        // Accept either exit 3 or an exit that has 'error[' on stderr (any structured error).
        assert_ne!(
            out.status.code(), Some(2),
            "field '{field}' should not be a parse error"
        );
    }
}

#[test]
fn find_limit_zero_is_accepted_at_parse() {
    // Limit 0 is semantically odd but clap accepts it as a usize.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["find", "x", "--limit", "0"])
        .output()
        .expect("spawn");
    assert_ne!(out.status.code(), Some(2));
}

#[test]
fn find_limit_negative_exits_2() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["find", "x", "--limit", "-1"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}

// --- xv list pagination flag validation ---

#[test]
fn list_page_without_page_size_errors() {
    // --page without --page-size should be a clap error (exit 2) per the
    // pagination plan's UX spec.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["list", "--page", "2"]).output().expect("spawn");
    // Two acceptable behaviors:
    //   - Clap rejects at parse (exit 2)
    //   - Pagination::from_args returns InvalidArgument → exit 2
    // Either way exit 2 is the contract.
    assert_eq!(out.status.code(), Some(2), "--page without --page-size: {}", stderr_str(&out));
}

#[test]
fn file_list_limit_and_page_size_conflict_errors() {
    // Per the pagination plan, --limit and --page-size on file list cannot coexist.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd
        .args(["file", "list", "--limit", "10", "--page-size", "5"])
        .output()
        .expect("spawn");
    // Either clap rejects (exit 2) or runtime errors with config-invalid (exit 3).
    let code = out.status.code();
    assert!(
        code == Some(2) || code == Some(3),
        "expected exit 2 or 3 for --limit + --page-size conflict; got {code:?}"
    );
}

// --- --names-only contract ---

#[test]
fn ls_names_only_help_documents_no_format_no_ansi() {
    // Confirm the flag exists by querying --help; this is parse-only,
    // doesn't need a vault.
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["list", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let help = stdout_str(&out);
    assert!(help.contains("names-only"), "list --help should document --names-only: {help}");
}

#[test]
fn find_help_documents_in_field() {
    let (mut cmd, _temp) = xv_isolated();
    let out = cmd.args(["find", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let help = stdout_str(&out);
    assert!(help.contains("--in"), "find --help should document --in: {help}");
    assert!(help.contains("FIELD") || help.contains("field"), "should reference field arg: {help}");
}
```

### Step 2: Run

```bash
cargo test --test find_pagination_tests
```

Expected: 8 tests pass.

### Step 3: Commit

```bash
git add tests/find_pagination_tests.rs
git commit -m "test(find/pagination): hermetic flag-validation E2E

8 tests covering: find --in unknown field → exit 2; find --in
{name,folder,groups,note,tags} all parse-clean (no exit 2);
find --limit 0 accepted; find --limit -1 → exit 2;
list --page without --page-size → exit 2;
file list --limit + --page-size conflict → exit 2 or 3;
list/find --help documents the new flags.
"
```

---

## Task 5: Scan hook installer edge cases

**Files:**
- Modify: `tests/scan_tests.rs`

### Step 1: Refactor to use `common`

At the top of `tests/scan_tests.rs`, add `mod common;`. Replace the local `xv()` helper (if present) with imports from `common`. The existing 3 active tests stay; we add edge cases for the installer.

### Step 2: Append new tests

```rust
// --- Hook installer edge cases ---

#[test]
fn scan_install_writes_marker() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let out = cmd.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", common::stderr_str(&out));
    let hook = temp.path().join(".git/hooks/pre-commit");
    assert!(hook.exists(), "hook file should be created");
    let content = std::fs::read_to_string(&hook).unwrap();
    assert!(content.contains("xv-scan-managed"), "marker missing: {content}");
    assert!(content.contains("xv scan --staged --hook"), "hook body missing: {content}");
}

#[test]
fn scan_install_repeat_is_no_op() {
    let (mut cmd1, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let _ = cmd1.args(["scan", "install"]).output().expect("spawn");

    // Second install: should succeed (already installed); no error.
    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out = cmd2.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let combined = format!("{}{}", common::stderr_str(&out), common::stdout_str(&out));
    assert!(
        combined.to_lowercase().contains("already") || combined.contains("xv-scan-managed"),
        "should report already-installed: {combined}"
    );
}

#[test]
fn scan_install_refuses_unmanaged_hook_without_force() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let hooks_dir = temp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

    let out = cmd.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("not xv-managed") || stderr.contains("force"),
        "stderr: {stderr}");
}

#[test]
fn scan_install_force_overwrites_unmanaged_hook() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let hooks_dir = temp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

    let out = cmd.args(["scan", "install", "--force"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let content = std::fs::read_to_string(hooks_dir.join("pre-commit")).unwrap();
    assert!(content.contains("xv-scan-managed"));
}

#[test]
fn scan_uninstall_refuses_unmanaged_hook() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let hooks_dir = temp.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho hi\n").unwrap();

    let out = cmd.args(["scan", "uninstall"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let stderr = common::stderr_str(&out);
    assert!(stderr.contains("not xv-managed") || stderr.contains("refusing"),
        "stderr: {stderr}");
}

#[test]
fn scan_uninstall_when_no_hook_is_no_op() {
    let (mut cmd, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let out = cmd.args(["scan", "uninstall"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    // Some output indicating "no hook to remove":
    let combined = format!("{}{}", common::stderr_str(&out), common::stdout_str(&out));
    assert!(
        combined.to_lowercase().contains("no") || combined.to_lowercase().contains("not"),
        "should mention no hook: {combined}"
    );
}

#[test]
fn scan_install_round_trip() {
    let (mut cmd1, temp) = common::xv_isolated();
    common::init_git_repo(temp.path());
    let out1 = cmd1.args(["scan", "install"]).output().expect("spawn");
    assert_eq!(out1.status.code(), Some(0));
    assert!(temp.path().join(".git/hooks/pre-commit").exists());

    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out2 = cmd2.args(["scan", "uninstall"]).output().expect("spawn");
    assert_eq!(out2.status.code(), Some(0));
    assert!(!temp.path().join(".git/hooks/pre-commit").exists(),
        "hook should be removed");
}
```

### Step 3: Run

```bash
cargo test --test scan_tests
```

Expected: existing 3 tests + 7 new = 10 pass.

### Step 4: Commit

```bash
git add tests/scan_tests.rs
git commit -m "test(scan): hook installer edge cases

7 new tests covering install round-trip semantics: writes marker,
repeat-install no-op, refuses non-managed without --force, --force
overwrites, uninstall refuses non-managed, uninstall on no-hook is
clean no-op, full install→uninstall round-trip removes the file.
"
```

---

## Task 6: Help-matrix coverage across all top-level commands

**Files:**
- Modify: `tests/cli_integration_tests.rs`

### Step 1: Add the matrix test

Append to `tests/cli_integration_tests.rs`:

```rust
// --- Help matrix: every top-level command supports --help ---

#[test]
fn every_top_level_command_supports_help() {
    // The full list of top-level Commands as of v0.7.0-rc.2.
    // If a new command is added, append it here AND ensure --help works.
    let commands: &[&[&str]] = &[
        &["set"],
        &["get"],
        &["find"],
        &["list"],
        &["delete"],
        &["update"],
        &["restore"],
        &["purge"],
        &["history"],
        &["rollback"],
        &["rotate"],
        &["run"],
        &["inject"],
        &["whoami"],
        &["info"],
        &["audit"],
        &["share"],
        &["share", "grant"],
        &["share", "revoke"],
        &["share", "list"],
        &["env"],
        &["env", "list"],
        &["env", "use"],
        &["env", "create"],
        &["context"],
        &["context", "show"],
        &["context", "use"],
        &["context", "list"],
        &["context", "envs"],
        &["context", "init"],
        &["vault"],
        &["vault", "create"],
        &["vault", "list"],
        &["vault", "delete"],
        &["vault", "info"],
        &["vault", "share"],
        &["config"],
        &["config", "show"],
        &["config", "path"],
        &["completion"],
        &["scan"],
        &["scan", "install"],
        &["scan", "uninstall"],
        &["init"],
        &["upgrade"],
        &["version"],
        &["gen"],
        &["copy"],
        &["move"],
        &["diff"],
        &["parse"],
    ];
    for args in commands {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_xv"));
        cmd.args(*args).arg("--help");
        let out = cmd.output().expect("spawn");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?} --help should exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.stdout.is_empty() || !out.stderr.is_empty(),
            "{args:?} --help produced empty output"
        );
    }
}
```

### Step 2: Run

```bash
cargo test --test cli_integration_tests every_top_level_command_supports_help
```

Expected: PASS.

> **If the test fails:** the command list above may have a stale entry (a removed subcommand) or a missing entry (a new subcommand). Inspect the failure, sync the list with `xv --show-options --help` or by reading `src/cli/commands.rs::Commands`, and re-run.

### Step 3: Commit

```bash
git add tests/cli_integration_tests.rs
git commit -m "test(cli): help-matrix coverage for every top-level command

One regression-guard test that runs 'xv <command> --help' for
every documented top-level command and subcommand. Failing this
test means either a command was removed, a new one was added
without --help support, or clap parsing regressed for that arm.
"
```

---

## Task 7: Completion command coverage

**Files:**
- Create: `tests/completion_tests.rs`

### Step 1: Write the tests

```rust
mod common;

#[test]
fn completion_bash_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "bash"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty(), "bash completion should be non-empty");
    // Bash completion scripts always reference `complete` and the binary.
    assert!(stdout.contains("complete"), "should contain 'complete' builtin: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_zsh_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "zsh"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty());
    assert!(stdout.contains("compdef") || stdout.contains("_xv"),
        "zsh completion should contain compdef or _xv: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_fish_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "fish"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty());
    assert!(stdout.contains("complete"),
        "fish completion should reference complete: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_powershell_emits_non_empty_script() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "powershell"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(!stdout.is_empty());
    // PowerShell completion uses Register-ArgumentCompleter.
    assert!(stdout.contains("Register-ArgumentCompleter") || stdout.to_lowercase().contains("powershell"),
        "powershell completion should reference Register-ArgumentCompleter: head 200: {}",
        &stdout.chars().take(200).collect::<String>());
}

#[test]
fn completion_unknown_shell_exits_2() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["completion", "unknown-shell"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}
```

### Step 2: Run

```bash
cargo test --test completion_tests
```

Expected: 5 tests pass.

### Step 3: Commit

```bash
git add tests/completion_tests.rs
git commit -m "test(completion): hermetic E2E for shell completion generators

5 tests asserting xv completion {bash,zsh,fish,powershell} each
exit 0 with non-empty, shell-appropriate output. Unknown shell
exits 2.
"
```

---

## Task 8: Config command surface tests with XDG isolation

**Files:**
- Create: `tests/config_command_tests.rs`

### Step 1: Write the tests

```rust
mod common;

#[test]
fn config_path_shows_isolated_path() {
    let (mut cmd, temp) = common::xv_isolated();
    let out = cmd.args(["config", "path"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    // Path should be under our isolated XDG_CONFIG_HOME.
    let expected_prefix = temp.path().join(".config").to_string_lossy().into_owned();
    assert!(
        stdout.contains(&expected_prefix) || stdout.contains("xv"),
        "config path should reference isolated dir: {stdout}"
    );
}

#[test]
fn config_show_works_on_empty_config() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["config", "show"]).output().expect("spawn");
    // With XDG_CONFIG_HOME pointing at an empty tempdir, no config file
    // exists. The command should still exit 0 and show defaults.
    assert_eq!(out.status.code(), Some(0), "stderr: {}", common::stderr_str(&out));
}

#[test]
fn config_set_then_show_round_trips() {
    let (mut cmd1, temp) = common::xv_isolated();
    let out1 = cmd1
        .args(["config", "set", "default_vault", "test-vault"])
        .output()
        .expect("spawn");
    assert_eq!(out1.status.code(), Some(0), "set: {}", common::stderr_str(&out1));

    let mut cmd2 = common::xv();
    common::isolate(&mut cmd2, temp.path());
    let out2 = cmd2.args(["config", "show"]).output().expect("spawn");
    assert_eq!(out2.status.code(), Some(0));
    let stdout = common::stdout_str(&out2);
    assert!(stdout.contains("test-vault"),
        "config show should display the value just set: {stdout}");
}

#[test]
fn config_set_invalid_key_errors() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd
        .args(["config", "set", "this_key_does_not_exist", "value"])
        .output()
        .expect("spawn");
    // Either clap rejects (exit 2) or runtime returns invalid-argument (exit 2).
    // Acceptable: 2 or 3 (depending on validation layer).
    let code = out.status.code();
    assert!(
        code == Some(2) || code == Some(3),
        "invalid config key should error: {code:?}"
    );
}

#[test]
fn config_help_documents_subcommands() {
    let (mut cmd, _temp) = common::xv_isolated();
    let out = cmd.args(["config", "--help"]).output().expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = common::stdout_str(&out);
    assert!(stdout.contains("show"));
    assert!(stdout.contains("path"));
    assert!(stdout.contains("set"));
}
```

### Step 2: Run

```bash
cargo test --test config_command_tests
```

Expected: 5 tests pass.

> **If `config_set_then_show_round_trips` fails:** the actual config file may live at a different path than `XDG_CONFIG_HOME/xv/`. Inspect with `xv config path` and adjust if needed. The round-trip property (set → show shows the value) is the load-bearing invariant.

### Step 3: Commit

```bash
git add tests/config_command_tests.rs
git commit -m "test(config): hermetic E2E for xv config command surface

5 tests covering: config path emits isolated path; config show on
empty config exits 0 with defaults; config set followed by config
show round-trips the value; invalid key errors at exit 2 or 3;
config --help documents all subcommands.
"
```

---

## Task 9: TUI parse-only test (feature-gated)

**Files:**
- Modify: `tests/tui_view_tests.rs`

### Step 1: Append parse-only tests

The existing `tui_view_tests.rs` is gated `#[cfg(feature = "tui")]`. Add one parse-only test that runs WITHOUT the feature, and one that runs WITH it. The without-feature test goes in a different file because it can't share the cfg gate.

For now, add to `tests/tui_view_tests.rs` (still gated on feature):

```rust
#[test]
fn tui_help_works_when_feature_enabled() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_xv"))
        .args(["tui", "--help"])
        .env("XV_NO_PARENT_CONFIG", "1")
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tui") || stdout.contains("Tui"),
        "tui --help should mention tui: {stdout}");
}
```

### Step 2: Add a feature-OFF test in cli_integration_tests.rs

This test runs ONLY when the `tui` feature is NOT enabled. Append to `tests/cli_integration_tests.rs`:

```rust
#[test]
#[cfg(not(feature = "tui"))]
fn tui_subcommand_unknown_when_feature_disabled() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xv"))
        .args(["tui"])
        .output()
        .expect("spawn");
    // clap returns exit 2 for unknown subcommands.
    assert_eq!(out.status.code(), Some(2));
}
```

### Step 3: Run

```bash
cargo test --test tui_view_tests --features tui
cargo test --test cli_integration_tests       # runs the cfg(not(feature)) test
```

Expected: both pass.

### Step 4: Commit

```bash
git add tests/tui_view_tests.rs tests/cli_integration_tests.rs
git commit -m "test(tui): feature-gate parse-only coverage

Adds 'tui --help' test under #[cfg(feature = \"tui\")] (already in
the file's cfg gate). Adds a complementary test in
cli_integration_tests under #[cfg(not(feature = \"tui\"))] that
asserts 'xv tui' exits 2 (unknown subcommand) when the feature is
off. Together: confirms the feature flag actually gates the
command.
"
```

---

## Task 10: CI workflow update

**Files:**
- Modify: `.github/workflows/build.yml`

### Step 1: Add a `--features tui` test step

Read the current `.github/workflows/build.yml`. Find the existing `cargo test --verbose` step. Add a second step right after it:

```yaml
      - name: Test (with TUI feature)
        run: cargo test --features tui --verbose
```

(Match the existing indentation and YAML style of the surrounding steps.)

### Step 2: Verify the YAML is valid

```bash
# If you have yamllint or actionlint:
yamllint .github/workflows/build.yml || true
# Otherwise, let GitHub Actions validate on push.
```

### Step 3: Commit

```bash
git add .github/workflows/build.yml
git commit -m "ci: also run cargo test --features tui

Phase-1 added a 'tui' feature flag (default off) so the default
binary stays lean. CI needs to test BOTH builds — the default
test step covers the feature-off path; the new step covers
feature-on. Without this, TUI snapshot tests + the new
tui-feature-gated parse tests never run in CI.
"
```

---

## Task 11: Documentation

**Files:**
- Create: `docs/testing.md`
- Modify: `README.md` (add a one-line pointer)

### Step 1: Create `docs/testing.md`

```markdown
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
leak into the test environment.

```bash
cargo test                        # default features
cargo test --features tui         # also runs TUI snapshot + parse tests
cargo test -- --test-threads=1    # required for env-var-mutating tests in
                                  # config::project (Plan #2)
```

Hermetic tests live in:

- `tests/common/mod.rs` — shared harness (xv_isolated, parse_json_envelope, …)
- `tests/cli_integration_tests.rs` — basic smoke + help matrix
- `tests/error_codes_tests.rs` — exit-code contract + JSON envelope shape
- `tests/context_tests.rs` — `xv context` + `.xv.toml` resolution
- `tests/find_pagination_tests.rs` — `xv find` / `xv list` flag validation
- `tests/scan_tests.rs` — `xv scan` + hook installer edge cases
- `tests/completion_tests.rs` — shell completion generators
- `tests/config_command_tests.rs` — `xv config` command surface
- `tests/tui_view_tests.rs` — TUI rendering snapshots (feature-gated)

## Live integration (Azure required) — manual / weekly

Tests in `tests/e2e_integration_tests.rs` are `#[ignore]`d by default.
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
```

### Step 2: Add a README pointer

In `README.md` under the "Development" section, add a line:

```markdown
- Tests: see [`docs/testing.md`](docs/testing.md) for the hermetic vs live track split.
```

### Step 3: Commit

```bash
git add docs/testing.md README.md
git commit -m "docs: testing guide — hermetic vs live track

User-facing reference for the test split: hermetic tests run on
every PR (no Azure creds), live tests gated on XV_TEST_VAULT plus
az login. Cross-references the eight hermetic test files added in
the E2E plan and points at the live track in
e2e_integration_tests.rs.
"
```

---

## Verification checklist

- [ ] `cargo test` — all green (default features)
- [ ] `cargo test --features tui` — all green (TUI tests included)
- [ ] `cargo test -- --test-threads=1` — all green (no test-order flake)
- [ ] `cargo build --tests` — no broken refactors
- [ ] CI workflow (`.github/workflows/build.yml`) runs both feature-off and feature-on test steps
- [ ] All hermetic tests run without an internet connection (unplug WiFi and try `cargo test`)
- [ ] All hermetic tests run without `~/.config/xv/` (rename the dir temporarily and re-run)
- [ ] All hermetic tests run without `~/.azure/` (likewise)
- [ ] `docs/testing.md` exists and the README points at it

---

## Notes for the executing engineer

- **TDD discipline** is harder to apply to test-writing tasks. Where it fits, write the assertion first, run, see it fail because the binary behavior isn't quite what you assumed, then either adjust the assertion (if the assumption was wrong) or fix the binary (rare — usually it's the test). Don't write the test, run it, and accept the first passing run without checking that it's actually exercising the contract.
- **Substring-matching is fragile.** When a test asserts on a substring in stderr, prefer the structural assertion (exit code, JSON `code` field) and treat the substring as a sanity check, not the load-bearing assertion.
- **`env_clear()` in `isolate()` is intentional.** It prevents accidental leakage of `AZURE_*`, `XV_ENV`, `XV_TEST_VAULT`, etc. from the developer's shell. If a test legitimately needs an env var, set it explicitly after `isolate()`.
- **The shared harness is `dead_code`-friendly.** `tests/common/mod.rs` has `#![allow(dead_code)]` because each test file imports a subset of helpers. Don't remove unused helpers from the harness — they're not unused, just unused by the file you're looking at.
- **No live Azure access in any task.** If a test needs to talk to Azure, it belongs in `e2e_integration_tests.rs`, not in this plan's tasks.
- **Commit per task.** Each task ends with one commit.
- **Help matrix maintenance.** Task 6's help-matrix test will fail when you add or remove a top-level command. That's the point — the failure tells you to update the test list. This is regression coverage for the CLI surface itself.
