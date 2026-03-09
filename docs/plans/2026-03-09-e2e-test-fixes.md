# E2E Test Fixes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix 11 failing E2E integration tests caused by a `format` field type collision bug and incorrect `--vault` flag usage in test harness.

**Architecture:** Two independent fixes: (1) disambiguate 5 local `format: String` fields from the global `format: OutputFormat` using clap's `#[arg(id = "...")]`; (2) replace `--vault` args in tests with `DEFAULT_VAULT` env var since secret commands use context-based vault selection.

**Tech Stack:** Rust, clap 4.0 (derive), std::process::Command, Azure Key Vault

---

### Task 1: Fix `format` field collision in `Parse` command

The global `Cli.format: OutputFormat` (line 87 in commands.rs) shadows local `format: String` fields in subcommands, causing a runtime panic: "Mismatch between definition and access of `format`. Could not downcast to OutputFormat, need to downcast to String."

**Files:**
- Modify: `src/cli/commands.rs:411-412` (struct definition)
- Modify: `src/cli/commands.rs:1076-1079` (match arm)

**Step 1: Fix the struct definition**

Change line 411-412 from:
```rust
        #[arg(short, long, default_value = "table")]
        format: String,
```
to:
```rust
        #[arg(short, long, default_value = "table", id = "parse_format")]
        format: String,
```

**Step 2: Verify `xv parse` no longer panics**

Run: `cargo run -- parse "Server=db.example.com;Database=mydb"`
Expected: Table output with parsed key-value pairs (no panic)

Run: `cargo run -- parse "Server=db.example.com;Database=mydb" --format json`
Expected: JSON output (no panic)

**Step 3: Commit**

```bash
git add src/cli/commands.rs
git commit -m "fix: resolve format field collision in Parse command

The global Cli.format (OutputFormat enum) shadowed the local
Parse.format (String), causing a runtime panic on xv parse.
Add clap id annotation to disambiguate."
```

---

### Task 2: Fix `format` field collision in `VaultCommands::Export`

**Files:**
- Modify: `src/cli/commands.rs:594-595` (struct definition)
- Match arm at line 1403 requires no change (field name stays `format`)

**Step 1: Fix the struct definition**

Change line 594-595 from:
```rust
        #[arg(short, long, default_value = "json")]
        format: String,
```
to:
```rust
        #[arg(short, long, default_value = "json", id = "export_format")]
        format: String,
```

**Step 2: Verify `xv vault export` no longer panics**

Run: `cargo run -- vault export xvtestdeleteme --output /tmp/test-export.json`
Expected: Exports vault secrets to file (no panic)

**Step 3: Commit**

```bash
git add src/cli/commands.rs
git commit -m "fix: resolve format field collision in vault export command"
```

---

### Task 3: Fix `format` field collision in `VaultCommands::Import`

**Files:**
- Modify: `src/cli/commands.rs:614-615` (struct definition)

**Step 1: Fix the struct definition**

Change line 614-615 from:
```rust
        #[arg(short, long, default_value = "json")]
        format: String,
```
to:
```rust
        #[arg(short, long, default_value = "json", id = "import_format")]
        format: String,
```

**Step 2: Verify compilation**

Run: `cargo check`
Expected: Compiles without errors

**Step 3: Commit**

```bash
git add src/cli/commands.rs
git commit -m "fix: resolve format field collision in vault import command"
```

---

### Task 4: Fix `format` field collision in `VaultShareCommands::List`

**Files:**
- Modify: `src/cli/commands.rs:689-690` (struct definition)

**Step 1: Fix the struct definition**

Change line 689-690 from:
```rust
        #[arg(short, long, default_value = "table")]
        format: String,
```
to:
```rust
        #[arg(short, long, default_value = "table", id = "share_list_format")]
        format: String,
```

**Step 2: Verify compilation**

Run: `cargo check`
Expected: Compiles without errors

**Step 3: Commit**

```bash
git add src/cli/commands.rs
git commit -m "fix: resolve format field collision in share list command"
```

---

### Task 5: Fix `format` field collision in `EnvCommands::Pull`

**Files:**
- Modify: `src/cli/commands.rs:934-935` (struct definition)

**Step 1: Fix the struct definition**

Change line 934-935 from:
```rust
        #[arg(long, default_value = "dotenv")]
        format: String,
```
to:
```rust
        #[arg(long, default_value = "dotenv", id = "pull_format")]
        format: String,
```

**Step 2: Verify all format fixes compile cleanly**

Run: `cargo check`
Expected: Compiles without errors (only pre-existing `secret_not_found` warning)

**Step 3: Commit**

```bash
git add src/cli/commands.rs
git commit -m "fix: resolve format field collision in env pull command"
```

---

### Task 6: Update test harness to use `DEFAULT_VAULT` env var

Secret commands (`set`, `get`, `list`, `delete`, `rotate`, `update`, `history`, `restore`, `purge`, `run`, `inject`) do NOT have a `--vault` flag. They resolve the vault from context or config. The simplest approach for tests is to set `DEFAULT_VAULT` env var on every `Command`.

Commands that DO accept explicit vault args (no change needed): `copy --from/--to`, `diff <vault1> <vault2>`, `audit --vault`, `vault info/export/import <name>`, `context use <name>`.

**Files:**
- Modify: `tests/e2e_integration_tests.rs`

**Step 1: Update `xv()` helper to set DEFAULT_VAULT**

Change lines 31-37 from:
```rust
fn xv(args: &[&str]) -> std::process::Output {
    let binary = env!("CARGO_BIN_EXE_xv");
    Command::new(binary)
        .args(args)
        .output()
        .expect("Failed to execute xv binary")
}
```
to:
```rust
fn xv(args: &[&str]) -> std::process::Output {
    let binary = env!("CARGO_BIN_EXE_xv");
    Command::new(binary)
        .args(args)
        .env("DEFAULT_VAULT", VAULT)
        .output()
        .expect("Failed to execute xv binary")
}
```

**Step 2: Update `cleanup_secrets` to remove `--vault` args**

Change lines 69-80 from:
```rust
fn cleanup_secrets(names: &[String]) {
    for name in names {
        let _ = xv(&["delete", name, "--vault", VAULT, "--force"]);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    for name in names {
        let _ = xv(&["purge", name, "--vault", VAULT, "--force"]);
    }
}
```
to:
```rust
fn cleanup_secrets(names: &[String]) {
    for name in names {
        let _ = xv(&["delete", name, "--force"]);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    for name in names {
        let _ = xv(&["purge", name, "--force"]);
    }
}
```

**Step 3: Verify test compiles**

Run: `cargo test --test e2e_integration_tests --no-run`
Expected: Compiles without errors

**Step 4: Commit**

```bash
git add tests/e2e_integration_tests.rs
git commit -m "fix(tests): use DEFAULT_VAULT env var instead of --vault flag

Secret commands resolve vault from context/config, not a --vault flag.
Set DEFAULT_VAULT env var on all Command invocations via the xv() helper."
```

---

### Task 7: Remove `--vault` from all test invocations

Now that `xv()` sets `DEFAULT_VAULT`, remove all `"--vault", VAULT` pairs from test args. Also update `Command::new()` calls that bypass `xv()` to include `.env("DEFAULT_VAULT", VAULT)`.

**Files:**
- Modify: `tests/e2e_integration_tests.rs`

**Step 1: Remove `--vault` from `e2e_secret_full_lifecycle`**

Remove `"--vault", VAULT,` from all `xv_ok()` and `xv()` calls in this test. For `Command::new()` calls (stdin piping), add `.env("DEFAULT_VAULT", VAULT)` and remove `"--vault", VAULT` from args.

Specifically, in `e2e_secret_full_lifecycle` (lines 164-321):
- Line 172: `["set", &secret_name, "--vault", VAULT, "--stdin", ...]` → `["set", &secret_name, "--stdin", ...]` + add `.env("DEFAULT_VAULT", VAULT)` to the Command
- Line 193: `["get", &secret_name, "--vault", VAULT, "--raw"]` → `["get", &secret_name, "--raw"]`
- Line 202: `["list", "--vault", VAULT]` → `["list"]`
- Line 211: `["list", "--vault", VAULT, "--format", "json"]` → `["list", "--format", "json"]`
- Lines 219-228: Remove `"--vault", VAULT,` from update args
- Line 231: `["list", "--vault", VAULT, "--group", ...]` → `["list", "--group", ...]`
- Lines 240-248: Remove `"--vault", VAULT,` from update args + add `.env("DEFAULT_VAULT", VAULT)`
- Line 258: Remove `"--vault", VAULT,`
- Line 267: Remove `"--vault", VAULT`
- Line 275: Remove `"--vault", VAULT,`
- Line 278: Remove `"--vault", VAULT,`
- Line 293: Remove `"--vault", VAULT,`
- Line 296: Remove `"--vault", VAULT`
- Line 306: Remove `"--vault", VAULT`
- Line 310: Remove `"--vault", VAULT`
- Line 318: Remove `"--vault", VAULT,`
- Line 320: Remove `"--vault", VAULT,`

**Step 2: Remove `--vault` from `e2e_bulk_set`**

- Line 339: `["set", &arg1, &arg2, &arg3, "--vault", VAULT]` → `["set", &arg1, &arg2, &arg3]`
- Lines 342-349: Remove `"--vault", VAULT,` from get calls

**Step 3: Remove `--vault` from format tests**

- Line 362: `["list", "--vault", VAULT, "--format", "yaml"]` → `["list", "--format", "yaml"]`
- Line 374: `["list", "--vault", VAULT, "--format", "csv"]` → `["list", "--format", "csv"]`
- Line 385: `["list", "--vault", VAULT, "--format", "json"]` → `["list", "--format", "json"]`

**Step 4: Update `e2e_vault_export_import` Command::new calls**

- Line 430: Remove `"--vault", VAULT,` from set args, add `.env("DEFAULT_VAULT", VAULT)` to Command

**Step 5: Update `e2e_run_injects_env_vars`**

- Line 525: Remove `"--vault", VAULT,` from set Command, add `.env("DEFAULT_VAULT", VAULT)`
- Lines 542-550: Remove `"--vault", VAULT,` from run args (run also uses context, not --vault)

**Step 6: Update `e2e_inject_template`**

- Line 584: Remove `"--vault", VAULT,` from set Command, add `.env("DEFAULT_VAULT", VAULT)`
- Lines 607-615: Remove `"--vault", VAULT,` from inject args

**Step 7: Update `e2e_get_nonexistent_secret`**

- Line 643: `["get", "this-secret-definitely-does-not-exist-xyz", "--vault", VAULT, "--raw"]` → `["get", "this-secret-definitely-does-not-exist-xyz", "--raw"]`

**Step 8: Update `e2e_invalid_vault_name`**

This test deliberately uses a non-existent vault. Since `DEFAULT_VAULT` is now set by `xv()`, override it:
- Change to use a direct `Command::new()` with `.env("DEFAULT_VAULT", "this-vault-definitely-does-not-exist-xyz-99999")` instead of `--vault`.

**Step 9: Update `e2e_copy_secret`**

- Line 726: Remove `"--vault", VAULT,` from set Command, add `.env("DEFAULT_VAULT", VAULT)`
- Line 753: `["get", &dest_name, "--vault", VAULT, "--raw"]` → `["get", &dest_name, "--raw"]`

**Step 10: Update `e2e_rotate_with_charset`**

- Line 780: Remove `"--vault", VAULT,` from set Command, add `.env("DEFAULT_VAULT", VAULT)`
- Lines 795-804: Remove `"--vault", VAULT,` from rotate args
- Line 807: Remove `"--vault", VAULT,`

**Step 11: Verify test compiles**

Run: `cargo test --test e2e_integration_tests --no-run`
Expected: Compiles without errors

**Step 12: Commit**

```bash
git add tests/e2e_integration_tests.rs
git commit -m "fix(tests): remove --vault flag from all secret command invocations

Secret commands (set, get, list, delete, rotate, update, history,
restore, purge, run, inject) use context/config for vault resolution.
DEFAULT_VAULT env var is already set by the xv() helper."
```

---

### Task 8: Run full E2E test suite and verify

**Step 1: Run all tests**

Run: `cargo test --test e2e_integration_tests -- --ignored --nocapture --test-threads=1`
Expected: All 25 tests pass (was 14/25, should now be 25/25)

**Step 2: If any tests still fail, diagnose and fix**

Common issues to check:
- Azure API rate limiting (add small delays between tests)
- Secret cleanup from previous failed runs (manually purge leftover `e2e-*` secrets)
- Timing issues with Azure eventual consistency (increase sleep durations)

**Step 3: Final commit if any additional fixes needed**

```bash
git add tests/e2e_integration_tests.rs
git commit -m "fix(tests): address remaining test failures from E2E run"
```

---

## Summary

| Task | Type | Description |
|------|------|-------------|
| 1-5 | App bug fix | Disambiguate `format` field collisions in 5 subcommands |
| 6 | Test fix | Add `DEFAULT_VAULT` env var to `xv()` helper |
| 7 | Test fix | Remove `--vault` from all test invocations |
| 8 | Verification | Run full suite and fix remaining issues |
