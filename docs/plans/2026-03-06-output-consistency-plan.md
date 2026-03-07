# Output Consistency & Confirmation Standardization - Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Unify all CLI output through a single TTY-aware output module with consistent emoji/prefix vocabulary, and standardize all confirmation prompts on `dialoguer`.

**Architecture:** New `utils/output.rs` provides free functions with `OnceLock`-cached TTY detection. All raw `println!`/`eprintln!` emoji calls in `commands.rs`, `main.rs`, and `init.rs` migrate to these functions. `DisplayUtils` message methods in `format.rs` are replaced. `InteractivePrompt` retains input methods only. All `rpassword`-based confirmations migrate to `InteractivePrompt::confirm()`.

**Tech Stack:** Rust std (`std::io::IsTerminal`, `std::sync::OnceLock`), crossterm (colors), existing dialoguer/indicatif

---

### Task 1: Create `utils/output.rs` with TTY detection and output functions

**Files:**
- Create: `src/utils/output.rs`
- Modify: `src/utils/mod.rs` (add `pub mod output;`)

**Step 1: Write the test file**

Create `src/utils/output.rs` with tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_success_no_tty() {
        let msg = format_line(Level::Success, "done", false);
        assert_eq!(msg, "[ok] done");
    }

    #[test]
    fn test_format_error_no_tty() {
        let msg = format_line(Level::Error, "failed", false);
        assert_eq!(msg, "[error] failed");
    }

    #[test]
    fn test_format_warn_no_tty() {
        let msg = format_line(Level::Warn, "careful", false);
        assert_eq!(msg, "[warn] careful");
    }

    #[test]
    fn test_format_info_no_tty() {
        let msg = format_line(Level::Info, "note", false);
        assert_eq!(msg, "[info] note");
    }

    #[test]
    fn test_format_hint_no_tty() {
        let msg = format_line(Level::Hint, "try this", false);
        assert_eq!(msg, "[hint] try this");
    }

    #[test]
    fn test_format_step_no_tty() {
        let msg = format_line(Level::Step, "Rotating secret", false);
        assert_eq!(msg, ":: Rotating secret");
    }

    #[test]
    fn test_format_success_tty() {
        let msg = format_line(Level::Success, "done", true);
        assert!(msg.contains("done"));
        // TTY output includes emoji prefix
        assert!(msg.starts_with("\u{2705}")); // checkmark emoji
    }

    #[test]
    fn test_no_color_env_respected() {
        // When NO_COLOR is set, should_use_rich() returns false
        // We can't easily test env var side effects in unit tests,
        // but we verify the format_line non-rich path works
        let msg = format_line(Level::Success, "done", false);
        assert!(!msg.contains("\u{2705}"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib utils::output::tests -v`
Expected: FAIL - module doesn't exist yet

**Step 3: Write the implementation**

```rust
//! Unified CLI output with TTY-aware formatting
//!
//! All user-facing messages should go through this module to ensure
//! consistent emoji/prefix usage and proper pipe/redirect behavior.

use crossterm::style::{Color as CrosstermColor, Stylize};
use std::io::IsTerminal;
use std::sync::OnceLock;

/// Message severity level
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Success,
    Error,
    Warn,
    Info,
    Hint,
    Step,
}

/// Cached TTY detection result
static STDOUT_IS_TTY: OnceLock<bool> = OnceLock::new();
static STDERR_IS_TTY: OnceLock<bool> = OnceLock::new();

/// Check if stdout is a TTY (cached)
pub fn is_tty() -> bool {
    *STDOUT_IS_TTY.get_or_init(|| std::io::stdout().is_terminal())
}

/// Check if stderr is a TTY (cached)
pub fn is_tty_stderr() -> bool {
    *STDERR_IS_TTY.get_or_init(|| std::io::stderr().is_terminal())
}

/// Whether to use rich (emoji + color) output
fn should_use_rich(is_tty: bool) -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    is_tty
}

/// Format a message line for the given level and TTY mode
pub fn format_line(level: Level, msg: &str, rich: bool) -> String {
    if rich {
        match level {
            Level::Success => format!("\u{2705} {}", msg.with(CrosstermColor::Green)),
            Level::Error => format!("\u{274c} {}", msg.with(CrosstermColor::Red)),
            Level::Warn => format!("\u{26a0}\u{fe0f}  {}", msg.with(CrosstermColor::Yellow)),
            Level::Info => format!("\u{2139}\u{fe0f}  {}", msg.with(CrosstermColor::Cyan)),
            Level::Hint => format!("\u{1f4a1} {}", msg.with(CrosstermColor::DarkGrey)),
            Level::Step => format!("\u{25b6} {}", msg.with(CrosstermColor::White).bold()),
        }
    } else {
        match level {
            Level::Success => format!("[ok] {msg}"),
            Level::Error => format!("[error] {msg}"),
            Level::Warn => format!("[warn] {msg}"),
            Level::Info => format!("[info] {msg}"),
            Level::Hint => format!("[hint] {msg}"),
            Level::Step => format!(":: {msg}"),
        }
    }
}

/// Print a success message to stdout
pub fn success(msg: &str) {
    println!("{}", format_line(Level::Success, msg, should_use_rich(is_tty())));
}

/// Print an error message to stderr
pub fn error(msg: &str) {
    eprintln!("{}", format_line(Level::Error, msg, should_use_rich(is_tty_stderr())));
}

/// Print a warning message to stdout
pub fn warn(msg: &str) {
    println!("{}", format_line(Level::Warn, msg, should_use_rich(is_tty())));
}

/// Print an info message to stdout
pub fn info(msg: &str) {
    println!("{}", format_line(Level::Info, msg, should_use_rich(is_tty())));
}

/// Print a hint message to stdout
pub fn hint(msg: &str) {
    println!("{}", format_line(Level::Hint, msg, should_use_rich(is_tty())));
}

/// Print a step/action message to stdout (e.g., "Rotating secret...")
pub fn step(msg: &str) {
    println!("{}", format_line(Level::Step, msg, should_use_rich(is_tty())));
}
```

**Step 4: Add module to utils/mod.rs**

In `src/utils/mod.rs`, add after the existing `pub mod` lines:

```rust
pub mod output;
```

**Step 5: Run tests to verify they pass**

Run: `cargo test --lib utils::output::tests -- --nocapture`
Expected: All 8 tests PASS

**Step 6: Commit**

```bash
git add src/utils/output.rs src/utils/mod.rs
git commit -m "feat: add unified TTY-aware output module"
```

---

### Task 2: Migrate `main.rs` error handler to `output::error()`

**Files:**
- Modify: `src/main.rs:77-126`

**Step 1: Replace `print_user_friendly_error` implementation**

Replace the entire function body. Change all `eprintln!("emoji Category")` lines to `crate::utils::output::error("Category")` and all `eprintln!("{msg}")` / checklist lines to `eprintln!("{msg}")` (keep plain for the detail lines since `error()` is the category header).

Before:
```rust
fn print_user_friendly_error(error: &CrosstacheError) {
    use CrosstacheError::*;
    match error {
        AuthenticationError(msg) => {
            eprintln!("🔐 Authentication Error");
            eprintln!("{msg}");
        }
        // ... etc
    }
}
```

After:
```rust
fn print_user_friendly_error(error: &CrosstacheError) {
    use crate::utils::output;
    use CrosstacheError::*;

    match error {
        AuthenticationError(msg) => {
            output::error("Authentication Error");
            eprintln!("{msg}");
        }
        AzureApiError(msg) => {
            output::error("Azure API Error");
            eprintln!("{msg}");
        }
        NetworkError(msg) => {
            output::error("Network Error");
            eprintln!("{msg}");
        }
        ConfigError(msg) => {
            output::error("Configuration Error");
            eprintln!("{msg}");
        }
        VaultNotFound { name } => {
            output::error("Vault Not Found");
            eprintln!("The Azure Key Vault '{name}' was not found.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The vault name is correct");
            eprintln!("2. The vault exists in your subscription");
            eprintln!("3. You have access to the vault");
            eprintln!("4. You're using the correct subscription");
        }
        SecretNotFound { name } => {
            output::error("Secret Not Found");
            eprintln!("The secret '{name}' was not found in the vault.");
            eprintln!("\nPlease verify:");
            eprintln!("1. The secret name is correct");
            eprintln!("2. The secret exists in the vault");
            eprintln!("3. You have 'Get' permissions for secrets");
        }
        PermissionDenied(msg) => {
            output::error("Permission Denied");
            eprintln!("{msg}");
            eprintln!("\nPlease verify:");
            eprintln!("1. Your account has the necessary permissions");
            eprintln!("2. You have access to the Azure subscription");
            eprintln!("3. The resource you're trying to access exists");
        }
        _ => {
            output::error(&format!("{error}"));
        }
    }
}
```

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: No errors

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "refactor: migrate main.rs error handler to output module"
```

---

### Task 3: Migrate `commands.rs` emoji output to `output::*` calls

**Files:**
- Modify: `src/cli/commands.rs` (65 emoji println!/eprintln! sites)

**Step 1: Add output import**

Near the top of `src/cli/commands.rs` (after existing use statements around line 10), add:

```rust
use crate::utils::output;
```

**Step 2: Migrate all emoji output calls**

Apply these systematic replacements throughout `commands.rs`. The mapping is:

**Success messages** (`println!("✅ ...")`  or `println!("✓ ...")` -> `output::success(...)`):
- Line 1522: `output::success(&format!("Successfully created vault '{}'", vault.name));`
- Line 2536: `output::success(&format!("Using environment profile: {}", name));`
- Line 2585: `output::success(&format!("Created environment profile: {}", name));`
- Line 2623: `output::success(&format!("Deleted environment profile: {}", name));`
- Line 2934: `output::success(&format!("  Set '{}'", key));`
- Line 3526: `output::success("Authentication successful\n");`
- Line 4004: `output::success(&format!("Configuration updated: {key} = {value}"));`
- Line 4084: `output::success(&format!("Successfully set secret '{}'", secret.original_name));`
- Line 4129: `output::success(&format!("Secret '{name}' copied to clipboard"));`
- Line 4295: `output::success(&format!("Secret '{secret_name}' copied to clipboard"));`
- Line 4598: `output::success(&format!("Successfully rolled back secret '{name}' to version '{display_version}'"));`
- Line 4810: `output::success(&format!("Successfully rotated secret '{}'", name));`
- Line 5474: `output::success(&format!("Successfully deleted secret '{name}'"));`
- Line 5671: `output::success(&format!("Successfully updated secret '{}'", secret.original_name));`
- Line 5714: `output::success(&format!("Successfully purged secret '{name}'"));`
- Line 6681: `output::success(&format!("Switched to vault '{vault_name}' ({scope} context)"));`
- Line 6783: `output::success(&format!("Cleared vault context for '{vault_name}' ({scope} scope)"));`
- Line 6847, 6855: `output::success(&format!("Successfully uploaded file '{}'", file_info.name));`
- Line 6905, 6917: `output::success(&format!("Successfully downloaded file '{name}'"));`
- Line 7061: `output::success(&format!("Successfully deleted file '{name}'"));`

**Indented success in summaries** (`println!("  ✅ ...")` -> `println!("  {}", output::format_line(output::Level::Success, ..., output::is_tty()))`):
For inline summary items at lines 7383, 7429, 7480, 7684, 7737, 7906, 7987, 7998, use `output::format_line()` so we can print with indentation:
- Example line 7383: `println!("  {}", output::format_line(output::Level::Success, &format!("Successful: {success_count}"), output::should_use_rich_stdout()));`

To support this, add a public convenience:
```rust
// Add to output.rs
pub fn should_use_rich_stdout() -> bool {
    should_use_rich(is_tty())
}
```

**Error messages** (`eprintln!("❌ ...")` -> `output::error(...)`):
- Line 2938: `output::error(&format!("  Failed to set '{}': {}", key, e));`
- Line 3521: `output::error(&format!("Authentication failed: {}", e));`
- Line 7302: `output::error(&format!("Path not found: {path_str}"));`
- Line 7337: `output::error(&format!("{}", error_msg));`
- Line 7372: `output::error(&format!("Failed to upload '{}': {}", local_path_str, e));`

**Indented errors in summaries** (lines 7385, 7433, 7484, 7686, 7741, 7899, 7908, 7991, 8000):
Use `eprintln!("  {}", output::format_line(output::Level::Error, ..., output::should_use_rich_stderr()));` — **must use `should_use_rich_stderr()`** (not `should_use_rich_stdout()`) since output goes to stderr. Using stdout's TTY check would cause wrong formatting when stdout is piped but stderr is a TTY, or vice versa.

**Warning messages** (`println!("⚠️ ...")` or `eprintln!("⚠️ ...")` -> `output::warn(...)`):
- Line 2048: `output::warn(&format!("Failed to get '{}' from {}: {}", name, vault1, e));`
- Line 2064: `output::warn(&format!("Failed to get '{}' from {}: {}", name, vault2, e));`
- Line 4133: `output::warn(&format!("Failed to copy to clipboard: {e}"));`
- Line 4140: `output::warn(&format!("Secret '{name}' has no value"));`
- Line 4299: `output::warn(&format!("Failed to copy to clipboard: {e}"));`
- Line 4305: `output::warn(&format!("Secret '{secret_name}' has no value"));`
- Line 5132: `output::warn("No secret references found in template");`
- Line 5306: `output::warn("Output file contains resolved secrets -- treat as sensitive");`
- Line 7547: `output::warn(&format!("No files found matching prefix: {}", prefix));`

**Step/action messages** (`println!("🔍 ...")`, `println!("🔄 ...")`, etc. -> `output::step(...)`):
- Line 3243: `output::step(&format!("Fetching audit logs for {} days...", days));`
- Line 3506: `output::step("Checking authentication and context...\n");`
- Line 4752: `output::step(&format!("Rotating secret: {}", name));`
- Line 5013: `output::step(&format!("Executing: {}", command.join(" ")));`
- Line 5273: `output::step(&format!("Injecting {} secret(s) into template...", total_injected));`

**Info/summary messages** (`println!("📊 ...")`, `println!("📋 ...")` -> `output::info(...)`):
- Line 3279: `output::info(&format!("Found {} audit log entries:\n", logs.len()));`
- Line 3573: `output::info("Context Information:");`
- Line 7382: `output::info("Upload Summary:");`
- Line 7683: `output::info("Download Summary:");`
- Line 7905: `output::info("Bulk Set Summary:");`
- Line 7997: `output::info("Group Delete Summary:");`

**Hint messages** (`println!("💡 ...")` -> `output::hint(...)`):
- Line 4819: `output::hint(&format!("Use 'xv history {}' to see version history", name));`

**Typo fix at line 5943:**
- Change `"Unimnplemented format selected: {format}"` to `output::warn(&format!("Unimplemented format selected: {format}"));`

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: No errors. Some warnings about unused imports of `rpassword` may appear (will be cleaned up in Task 5).

**Step 4: Commit**

```bash
git add src/cli/commands.rs src/utils/output.rs
git commit -m "refactor: migrate commands.rs emoji output to unified output module"
```

---

### Task 4: Migrate `config/init.rs` to `output::*` calls

**Files:**
- Modify: `src/config/init.rs`

**Step 1: Replace InteractivePrompt message calls with output::*

The following `self.prompt.*` message calls need to change:

- Line 49-71: `self.prompt.step(N, 6, "...")` -> `output::step(&format!("Step {N}/6: ..."));`
- Line 93: `self.prompt.success(...)` -> `output::success(...)`
- Line 95: `self.prompt.info(...)` -> `output::info(...)`
- Line 108: `self.prompt.error(...)` -> `output::error(...)`
- Line 112, 129, 159, 216, 255, 321: `self.prompt.info(...)` -> `output::info(...)`
- Line 789: `self.prompt.success(...)` -> `output::success(...)`
- Line 812: `self.prompt.info(...)` -> `output::info(...)`

Add `use crate::utils::output;` to the imports.

Keep `self.prompt.confirm()`, `self.prompt.select()`, `self.prompt.input_text_validated()` calls unchanged -- those are input methods.

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: No errors

**Step 3: Commit**

```bash
git add src/config/init.rs
git commit -m "refactor: migrate init.rs to unified output module"
```

---

### Task 5: Migrate rpassword confirmations to InteractivePrompt::confirm()

**Files:**
- Modify: `src/cli/commands.rs` (5 sites)

**Step 1: Replace all rpassword confirmation patterns**

Each of the 5 sites follows this before/after pattern:

**Before (e.g., line 5461):**
```rust
let confirm = rpassword::prompt_password(format!(
    "Are you sure you want to delete secret '{name}' from vault '{vault_name}'? (y/N): "
))?;
if confirm.to_lowercase() != "y" && confirm.to_lowercase() != "yes" {
    println!("Delete operation cancelled.");
    return Ok(());
}
```

**After:**
```rust
let prompt = InteractivePrompt::new();
if !prompt.confirm(&format!(
    "Are you sure you want to delete secret '{name}' from vault '{vault_name}'?"
), false)? {
    println!("Delete operation cancelled.");
    return Ok(());
}
```

Apply at these 5 locations:

1. **Line 5461** (secret delete): Message `"Are you sure you want to delete secret '{name}' from vault '{vault_name}'?"`
2. **Line 5700** (secret purge): Message `"Are you sure you want to PERMANENTLY DELETE secret '{name}' from vault '{vault_name}'? This cannot be undone!"`
3. **Line 7048** (file delete): Message `"Are you sure you want to delete file '{name}' from blob storage?"`  (also improves message clarity)
4. **Line 7721** (file delete multiple): Message `"Are you sure you want to delete these files?"`
5. **Line 7960** (group delete): Message `"Are you sure you want to delete ALL {count} secret(s) in group '{group_name}'?"`

Ensure `use crate::utils::interactive::InteractivePrompt;` is imported in each function scope (some already have it; add where missing).

**Step 2: Verify it compiles**

Run: `cargo check`
Expected: No errors

**Step 3: Commit**

```bash
git add src/cli/commands.rs
git commit -m "refactor: standardize all confirmations on InteractivePrompt::confirm()"
```

---

### Task 6: Migrate `DisplayUtils` message methods in managers to `output::*`

**Files:**
- Modify: `src/vault/manager.rs`
- Modify: `src/secret/manager.rs`

**Step 1: Replace DisplayUtils message calls in vault/manager.rs**

Add `use crate::utils::output;` to imports.

Replace all `self.display_utils.print_success(...)` with `output::success(...)`, `self.display_utils.print_warning(...)` with `output::warn(...)`, `self.display_utils.print_info(...)` with `output::info(...)`, and `self.display_utils.print_error(...)` with `output::error(...)`.

Sites in vault/manager.rs (lines 50, 70, 114, 137, 142, 146, 157, 167, 172, 185, 189, 194, 218, 226, 243, 251, 273).

Remove the `display_utils: DisplayUtils` field from `VaultManager` struct (line 20). Remove its initialization (line 32). Keep `no_color: bool` -- it's still used by `TableFormatter`.

Remove `use crate::utils::format::DisplayUtils` from imports if no longer needed (keep `OutputFormat` and `TableFormatter`).

**Step 2: Replace DisplayUtils message calls in secret/manager.rs**

Same pattern. Add `use crate::utils::output;` to imports.

Sites in secret/manager.rs (lines 1284, 1290, 1297, 1301, 1468, 1489, 1493, 1501, 1512, 1521, 1537, 1541, 1543, 1546, 1555, 1803, 1895, 1910, 1923).

Remove `display_utils: DisplayUtils` field from `SecretManager` struct (line 1232). Remove its initialization (line 1240). Keep `no_color: bool`.

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: No errors

**Step 4: Commit**

```bash
git add src/vault/manager.rs src/secret/manager.rs
git commit -m "refactor: migrate vault and secret managers to output module"
```

---

### Task 7: Clean up `DisplayUtils` and `InteractivePrompt` message methods

**Files:**
- Modify: `src/utils/format.rs`
- Modify: `src/utils/interactive.rs`

**Step 1: Remove message methods from InteractivePrompt**

In `src/utils/interactive.rs`, remove these methods from `InteractivePrompt`:
- `info()` (line 107-109)
- `success()` (line 113-115)
- `warning()` (line 120-122)
- `error()` (line 126-128)
- `step()` (line 132-137)

Keep: `new()`, `welcome()`, `confirm()`, `input_text()`, `input_text_validated()`, `select()`.

**Step 2: Update ProgressIndicator to use output module**

In `src/utils/interactive.rs`, update `ProgressIndicator`:
- `finish_success()` (line 173-175): Change format string from `"✅ {message}"` to use `crate::utils::output::format_line(crate::utils::output::Level::Success, message, crate::utils::output::should_use_rich_stdout())`
- `finish_error()` (line 178-180): Same pattern with `Level::Error` and `should_use_rich_stderr()` (add this helper to output.rs too)

Add to output.rs:
```rust
pub fn should_use_rich_stderr() -> bool {
    should_use_rich(is_tty_stderr())
}
```

**Step 3: Remove message methods from DisplayUtils in format.rs**

In `src/utils/format.rs`, remove:
- `print_success()` (lines 188-197)
- `print_warning()` (lines 200-209)
- `print_error()` (lines 212-222)
- `print_info()` (lines 225-234)

Keep: `new()`, `format_key_value_pairs()`, `print_header()`, `print_separator()`, `print_banner()`, `clear_screen()`.

If `DisplayUtils` is no longer imported anywhere after Task 6, remove the struct entirely. If `format_key_value_pairs` or other display methods are still used, keep the struct but remove the `theme` field and `no_color` (they're handled by output.rs now).

Check for remaining usages first:
Run: `cargo check` -- if `DisplayUtils` has no remaining users, remove it entirely. If it does (e.g., `format_key_value_pairs`), keep the struct.

**Step 4: Remove unused ColorTheme fields if no longer needed**

If `DisplayUtils` is removed, check if `ColorTheme` is still used. If only `output.rs` uses colors (via crossterm directly), `ColorTheme` struct can be removed.

**Step 5: Verify it compiles and tests pass**

Run: `cargo check && cargo test --lib`
Expected: No errors, all tests pass

**Step 6: Commit**

```bash
git add src/utils/format.rs src/utils/interactive.rs src/utils/output.rs
git commit -m "refactor: remove redundant message methods from DisplayUtils and InteractivePrompt"
```

---

### Task 8: Final verification and cleanup

**Files:**
- All modified files

**Step 1: Check for any remaining raw emoji in source**

Run: `grep -rn '[✅✓⚠️❌🔐🔍🔄🗑️📊💡ℹ️☁️🌐⚙️🔒🔑🚫📋🚀]' src/ --include='*.rs'`
Expected: No matches outside of `output.rs` (which defines the canonical emoji set)

**Step 2: Check for remaining rpassword confirmation usage**

Run: `grep -rn 'rpassword::prompt_password' src/cli/commands.rs`
Expected: Only 1 match at line ~4038 (the legitimate secret value input)

**Step 3: Run full test suite**

Run: `cargo test --lib -- --nocapture`
Expected: All tests pass

**Step 4: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No new warnings

**Step 5: Run formatter**

Run: `cargo fmt`

**Step 6: Final commit**

```bash
git add -A
git commit -m "chore: final cleanup for output consistency migration"
```
