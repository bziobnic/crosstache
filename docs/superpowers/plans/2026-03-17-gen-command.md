# `xv gen` Password Generator Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `xv gen` command that generates a random password, copies it to the clipboard, and optionally saves it as a vault secret.

**Architecture:** Wire together three existing pieces — `generate_random_value()`, `copy_to_clipboard()`, and the secret-set path — behind a new `Commands::Gen` variant and `execute_gen_command()` function. Add `CharsetType` trait impls (`Serialize`, `Deserialize`, `Default`, `Display`, `FromStr`) to enable config-file storage of the default charset. Store `gen_default_charset` as `Option<String>` on `Config` to avoid a circular module dependency (config → cli is forbidden).

**Tech Stack:** Rust, clap derive macros, serde, tokio, `rand` 0.8 (already in Cargo.toml)

---

## File Map

| File | Change |
|------|--------|
| `src/cli/commands.rs` | Add trait impls to `CharsetType`; add `Commands::Gen` variant; add `execute_gen_command()`; wire dispatch; add `execute_config_set` arm + known-keys string; make `generate_random_value` `pub(crate)` for testing |
| `src/config/settings.rs` | Add `gen_default_charset: Option<String>` field to `Config` struct |
| `tests/gen_tests.rs` | Integration test for `--save` path (requires Azure credentials) |

---

## Task 1: Extend `CharsetType` with trait impls

**Files:**
- Modify: `src/cli/commands.rs:140-171`

These impls are needed for: `FromStr` (parsing `gen_default_charset` from config), `Display` (serde serialization round-trip), `Serialize`/`Deserialize` (future-proofing), `Default` (fallback value).

- [ ] **Step 1: Write the failing tests**

Add to the bottom of `src/cli/commands.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── CharsetType trait tests ──────────────────────────────────────────────

    #[test]
    fn test_charset_default_is_alphanumeric() {
        assert_eq!(CharsetType::default(), CharsetType::Alphanumeric);
    }

    #[test]
    fn test_charset_display() {
        assert_eq!(CharsetType::Alphanumeric.to_string(), "alphanumeric");
        assert_eq!(CharsetType::AlphanumericSymbols.to_string(), "alphanumeric-symbols");
        assert_eq!(CharsetType::Hex.to_string(), "hex");
        assert_eq!(CharsetType::Base64.to_string(), "base64");
        assert_eq!(CharsetType::Numeric.to_string(), "numeric");
        assert_eq!(CharsetType::Uppercase.to_string(), "uppercase");
        assert_eq!(CharsetType::Lowercase.to_string(), "lowercase");
    }

    #[test]
    fn test_charset_from_str_valid() {
        assert_eq!("alphanumeric".parse::<CharsetType>().unwrap(), CharsetType::Alphanumeric);
        assert_eq!("alphanumeric-symbols".parse::<CharsetType>().unwrap(), CharsetType::AlphanumericSymbols);
        assert_eq!("alphanumeric_symbols".parse::<CharsetType>().unwrap(), CharsetType::AlphanumericSymbols);
        assert_eq!("hex".parse::<CharsetType>().unwrap(), CharsetType::Hex);
        assert_eq!("base64".parse::<CharsetType>().unwrap(), CharsetType::Base64);
        assert_eq!("numeric".parse::<CharsetType>().unwrap(), CharsetType::Numeric);
        assert_eq!("uppercase".parse::<CharsetType>().unwrap(), CharsetType::Uppercase);
        assert_eq!("lowercase".parse::<CharsetType>().unwrap(), CharsetType::Lowercase);
        // case insensitive
        assert_eq!("ALPHANUMERIC".parse::<CharsetType>().unwrap(), CharsetType::Alphanumeric);
    }

    #[test]
    fn test_charset_from_str_invalid() {
        assert!("alpha".parse::<CharsetType>().is_err());
        assert!("unknown".parse::<CharsetType>().is_err());
        assert!("".parse::<CharsetType>().is_err());
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cd /Users/scottzionic/Code/crosstache
cargo test test_charset -- --nocapture 2>&1 | tail -20
```

Expected: compile error — `CharsetType` has no `Default`, `Display`, or `FromStr` impl.

- [ ] **Step 3: Add trait impls to `CharsetType`**

Update the `CharsetType` derive line at `src/cli/commands.rs:140`:

```rust
// Change line 140 from:
#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
// To (add only Default — no Serialize/Deserialize needed since gen_default_charset is Option<String>):
#[derive(Debug, Clone, Copy, PartialEq, Default, ValueEnum)]
```

Add `#[default]` attribute to the `Alphanumeric` variant (line 143):
```rust
    /// Alphanumeric characters (A-Z, a-z, 0-9)
    #[default]
    Alphanumeric,
```

Add `Display` and `FromStr` impls after the existing `chars()` impl block (after line 171):

```rust
impl fmt::Display for CharsetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Alphanumeric => write!(f, "alphanumeric"),
            Self::AlphanumericSymbols => write!(f, "alphanumeric-symbols"),
            Self::Hex => write!(f, "hex"),
            Self::Base64 => write!(f, "base64"),
            Self::Numeric => write!(f, "numeric"),
            Self::Uppercase => write!(f, "uppercase"),
            Self::Lowercase => write!(f, "lowercase"),
        }
    }
}

impl std::str::FromStr for CharsetType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "alphanumeric" => Ok(Self::Alphanumeric),
            "alphanumeric-symbols" | "alphanumeric_symbols" => Ok(Self::AlphanumericSymbols),
            "hex" => Ok(Self::Hex),
            "base64" => Ok(Self::Base64),
            "numeric" => Ok(Self::Numeric),
            "uppercase" => Ok(Self::Uppercase),
            "lowercase" => Ok(Self::Lowercase),
            _ => Err(format!(
                "Invalid charset: '{s}'. Valid options: alphanumeric, alphanumeric-symbols, hex, base64, numeric, uppercase, lowercase"
            )),
        }
    }
}
```

Also add `use std::fmt;` at the top of `commands.rs` if not already present (check with `grep "^use std::fmt" src/cli/commands.rs`).

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test test_charset -- --nocapture 2>&1 | tail -20
```

Expected: all 4 charset tests pass.

- [ ] **Step 5: Verify the build still compiles**

```bash
cargo build 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs
git commit -m "feat: add Default, Display, FromStr impls to CharsetType"
```

---

## Task 2: Add `gen_default_charset` to `Config`

> **Note — spec deviation (intentional):** The spec describes `gen_default_charset: CharsetType` on `Config`. This plan uses `gen_default_charset: Option<String>` instead. Reason: `CharsetType` lives in `src/cli/commands.rs` and `Config` lives in `src/config/settings.rs`. Adding a `CharsetType` field would require `settings.rs` to import `crate::cli::commands`, creating a forbidden circular dependency (`config → cli` while `cli → config` already exists). Using `Option<String>` and parsing at call time avoids the cycle.

**Files:**
- Modify: `src/config/settings.rs:76-109` (Config struct)
- Modify: `src/cli/commands.rs:4009-4020` (execute_config_set)

- [ ] **Step 1: Write the failing test**

Add inside the existing `#[cfg(test)] mod tests` block in `src/config/settings.rs` at line 463 — do NOT create a new `mod tests` block:

```rust
    #[test]
    fn test_gen_default_charset_defaults_to_none() {
        let config = Config::default();
        assert!(config.gen_default_charset.is_none());
    }

    #[test]
    fn test_gen_default_charset_serde_round_trip() {
        let toml = r#"
            debug = false
            subscription_id = ""
            default_vault = ""
            default_resource_group = "Vaults"
            default_location = "eastus"
            tenant_id = ""
            function_app_url = ""
            output_json = false
            no_color = false
            gen_default_charset = "alphanumeric-symbols"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.gen_default_charset.as_deref(), Some("alphanumeric-symbols"));
    }

    #[test]
    fn test_gen_default_charset_absent_in_toml_is_none() {
        let toml = r#"
            debug = false
            subscription_id = ""
            default_vault = ""
            default_resource_group = "Vaults"
            default_location = "eastus"
            tenant_id = ""
            function_app_url = ""
            output_json = false
            no_color = false
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.gen_default_charset.is_none());
    }
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test test_gen_default_charset -- --nocapture 2>&1 | tail -10
```

Expected: compile error — field `gen_default_charset` does not exist on `Config`.

- [ ] **Step 3: Add the field to `Config`**

In `src/config/settings.rs`, add after the `clipboard_timeout` field (around line 108):

```rust
    /// Default character set for the `gen` command.
    /// Valid values: alphanumeric, alphanumeric-symbols, hex, base64, numeric, uppercase, lowercase
    /// If absent, `gen` defaults to alphanumeric.
    #[tabled(skip)]
    #[serde(default)]
    pub gen_default_charset: Option<String>,
```

Also add `gen_default_charset: None` to `Config::default()` (around line 130).

- [ ] **Step 4: Run the serde tests**

```bash
cargo test test_gen_default_charset -- --nocapture 2>&1 | tail -10
```

Expected: all 3 tests pass.

- [ ] **Step 5: Wire into `execute_config_set`**

In `src/cli/commands.rs`, add a new match arm in `execute_config_set` just before the `_` catch-all (around line 4015):

```rust
        "gen_default_charset" => {
            // Validate the value by parsing it — reject unknown charsets early
            value.parse::<CharsetType>().map_err(CrosstacheError::config)?;
            config.gen_default_charset = Some(value.to_string());
        }
```

Update the known-keys error string on line 4018 — append `, gen_default_charset` to the end of the list:

```
"Unknown configuration key: {key}. Available keys: debug, subscription_id, default_vault, default_resource_group, default_location, tenant_id, function_app_url, cache_ttl, output_json, no_color, azure_credential_priority, storage_account, storage_container, storage_endpoint, blob_chunk_size_mb, blob_max_concurrent_uploads, clipboard_timeout, gen_default_charset"
```

- [ ] **Step 6: Verify the build compiles**

```bash
cargo build 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add src/config/settings.rs src/cli/commands.rs
git commit -m "feat: add gen_default_charset config key"
```

---

## Task 3: Add `Commands::Gen` variant and dispatch

**Files:**
- Modify: `src/cli/commands.rs:174` (Commands enum)
- Modify: `src/cli/commands.rs:964` (execute dispatch match)

- [ ] **Step 1: Add `Gen` to the `Commands` enum**

In `src/cli/commands.rs`, add the `Gen` variant after the `Rotate` variant (around line 281). Insert before the `Run` variant:

```rust
    /// Generate a random password and copy it to the clipboard
    Gen {
        /// Password length — must be between 6 and 100 (default: 15)
        #[arg(short, long, default_value = "15")]
        length: usize,
        /// Character set to use (default: alphanumeric, or gen_default_charset config)
        #[arg(short, long, value_enum)]
        charset: Option<CharsetType>,
        /// Save the generated password as a secret in the vault
        #[arg(long)]
        save: Option<String>,
        /// Target vault for --save (overrides context/config default)
        #[arg(long)]
        vault: Option<String>,
        /// Print to stdout instead of copying to clipboard
        #[arg(long)]
        raw: bool,
    },
```

- [ ] **Step 2: Add dispatch arm in `execute()`**

In the `match self.command` block (around line 964), add after the `Rotate` arm:

```rust
            Commands::Gen {
                length,
                charset,
                save,
                vault,
                raw,
            } => execute_gen_command(length, charset, save, vault, raw, config).await,
```

- [ ] **Step 3: Verify the build compiles** (with a stub)

Add a temporary stub function after the last function in the file to make it compile:

```rust
async fn execute_gen_command(
    _length: usize,
    _charset: Option<CharsetType>,
    _save: Option<String>,
    _vault: Option<String>,
    _raw: bool,
    _config: Config,
) -> Result<()> {
    todo!("gen command not yet implemented")
}
```

```bash
cargo build 2>&1 | tail -10
```

Expected: builds successfully. Also verify the help text works:

```bash
cargo run -- gen --help 2>&1
```

Expected: shows the gen command help with all flags.

- [ ] **Step 4: Commit the stub**

```bash
git add src/cli/commands.rs
git commit -m "feat: add Commands::Gen variant with stub implementation"
```

---

## Task 4: Implement `execute_gen_command()`

**Files:**
- Modify: `src/cli/commands.rs` (replace stub, add unit tests to `#[cfg(test)]` block)

Also make `generate_random_value` `pub(crate)` so unit tests can call it directly:

Change line 4648 from `fn generate_random_value(` to `pub(crate) fn generate_random_value(`.

- [ ] **Step 1: Write the failing unit tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/cli/commands.rs`:

```rust
    // ── gen command unit tests ───────────────────────────────────────────────

    #[test]
    fn test_gen_length_validation_lower_bound() {
        // 6 is the minimum valid length
        let result = generate_random_value(6, CharsetType::Alphanumeric, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 6);
    }

    #[test]
    fn test_gen_length_validation_upper_bound() {
        let result = generate_random_value(100, CharsetType::Alphanumeric, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 100);
    }

    #[test]
    fn test_gen_default_length_is_15() {
        // The default_value = "15" on the CLI arg covers this, but verify
        // generate_random_value itself produces 15 chars when called with 15
        let result = generate_random_value(15, CharsetType::Alphanumeric, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 15);
    }

    #[test]
    fn test_gen_alphanumeric_chars_only() {
        let value = generate_random_value(200, CharsetType::Alphanumeric, None).unwrap();
        let valid = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        for ch in value.chars() {
            assert!(valid.contains(ch), "Unexpected char '{ch}' in alphanumeric output");
        }
    }

    #[test]
    fn test_gen_numeric_chars_only() {
        let value = generate_random_value(200, CharsetType::Numeric, None).unwrap();
        for ch in value.chars() {
            assert!(ch.is_ascii_digit(), "Unexpected char '{ch}' in numeric output");
        }
    }

    #[test]
    fn test_gen_uppercase_chars_only() {
        let value = generate_random_value(200, CharsetType::Uppercase, None).unwrap();
        for ch in value.chars() {
            assert!(ch.is_ascii_uppercase(), "Unexpected char '{ch}' in uppercase output");
        }
    }

    #[test]
    fn test_gen_lowercase_chars_only() {
        let value = generate_random_value(200, CharsetType::Lowercase, None).unwrap();
        for ch in value.chars() {
            assert!(ch.is_ascii_lowercase(), "Unexpected char '{ch}' in lowercase output");
        }
    }

    #[test]
    fn test_gen_hex_chars_only() {
        let value = generate_random_value(200, CharsetType::Hex, None).unwrap();
        let valid = "0123456789ABCDEF";
        for ch in value.chars() {
            assert!(valid.contains(ch), "Unexpected char '{ch}' in hex output");
        }
    }

    #[test]
    fn test_gen_alphanumeric_symbols_chars_only() {
        let value = generate_random_value(500, CharsetType::AlphanumericSymbols, None).unwrap();
        let valid = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()_+-=[]{}|;:,.<>?";
        for ch in value.chars() {
            assert!(valid.contains(ch), "Unexpected char '{ch}' in alphanumeric-symbols output");
        }
    }

    #[test]
    fn test_gen_charset_resolution_flag_overrides_config() {
        // When --charset is provided, it takes precedence over config default
        // Verified by checking that Numeric only produces digits (charset flag = Numeric)
        let value = generate_random_value(100, CharsetType::Numeric, None).unwrap();
        for ch in value.chars() {
            assert!(ch.is_ascii_digit());
        }
    }
```

- [ ] **Step 2: Run the tests to confirm they pass (they use existing logic)**

```bash
cargo test test_gen_ -- --nocapture 2>&1 | tail -20
```

Expected: all tests pass (since `generate_random_value` already works — we're just adding test coverage).

- [ ] **Step 3: Replace the stub with the real `execute_gen_command` implementation**

Replace the `todo!` stub with the full implementation:

```rust
async fn execute_gen_command(
    length: usize,
    charset: Option<CharsetType>,
    save: Option<String>,
    vault: Option<String>,
    raw: bool,
    config: Config,
) -> Result<()> {
    // Step 1: Validate length
    if length < 6 || length > 100 {
        return Err(CrosstacheError::invalid_argument(
            "Length must be between 6 and 100",
        ));
    }

    // Step 2: Resolve charset: CLI flag → config default → Alphanumeric
    let resolved_charset = charset.unwrap_or_else(|| {
        config
            .gen_default_charset
            .as_deref()
            .and_then(|s| s.parse::<CharsetType>().ok())
            .unwrap_or(CharsetType::Alphanumeric)
    });

    // Step 3: Generate the password
    let password = generate_random_value(length, resolved_charset, None)?;

    // Step 4: Handle --save
    if let Some(ref name) = save {
        use crate::auth::provider::DefaultAzureCredentialProvider;
        use crate::secret::manager::SecretManager;
        use std::sync::Arc;

        let auth_provider = Arc::new(
            DefaultAzureCredentialProvider::with_credential_priority(
                config.azure_credential_priority.clone(),
            )
            .map_err(|e| {
                CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
            })?,
        );
        let secret_manager = SecretManager::new(auth_provider, config.no_color);
        let vault_name = config.resolve_vault_name(vault).await?;

        match secret_manager
            .set_secret_safe(&vault_name, name, password.as_str(), None)
            .await
        {
            Ok(_) => {
                if raw {
                    println!("{}", password.as_str());
                    output::success(&format!("Secret '{name}' saved."));
                } else {
                    match copy_to_clipboard(password.as_str()) {
                        Ok(()) => {
                            let timeout = config.clipboard_timeout;
                            if timeout > 0 {
                                output::success(&format!(
                                    "Secret '{name}' saved and copied to clipboard (auto-clears in {timeout}s)"
                                ));
                                schedule_clipboard_clear(timeout);
                            } else {
                                output::success(&format!(
                                    "Secret '{name}' saved and copied to clipboard"
                                ));
                            }
                        }
                        Err(e) => {
                            output::warn(&format!("Failed to copy to clipboard: {e}"));
                            println!("{}", password.as_str());
                        }
                    }
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to save secret '{name}': {e}"));
                output::warn("Generated password (save this now):");
                println!("{}", password.as_str());
            }
        }
        return Ok(());
    }

    // Step 5: No --save — just output
    if raw {
        println!("{}", password.as_str());
    } else {
        match copy_to_clipboard(password.as_str()) {
            Ok(()) => {
                let timeout = config.clipboard_timeout;
                if timeout > 0 {
                    output::success(&format!(
                        "Password copied to clipboard (auto-clears in {timeout}s)"
                    ));
                    schedule_clipboard_clear(timeout);
                } else {
                    output::success("Password copied to clipboard");
                }
            }
            Err(e) => {
                output::warn(&format!("Failed to copy to clipboard: {e}"));
                println!("{}", password.as_str());
            }
        }
    }

    Ok(())
}
```

- [ ] **Step 4: Run the full test suite**

```bash
cargo test --lib -- --nocapture 2>&1 | tail -20
```

Expected: all tests pass, no regressions.

- [ ] **Step 5: Test the command manually**

```bash
# Basic run — generates a password and copies to clipboard
cargo run -- gen

# With --raw (should print to stdout)
cargo run -- gen --raw

# Custom length and charset
cargo run -- gen --length 20 --charset alphanumeric-symbols --raw

# Length out of range — should fail with a clear error
cargo run -- gen --length 5 2>&1
cargo run -- gen --length 101 2>&1
```

Expected for `--length 5`: `Error: Length must be between 6 and 100`

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs
git commit -m "feat: implement execute_gen_command password generator"
```

---

## Task 5: Integration test for `--save`

**Files:**
- Create: `tests/gen_tests.rs`
- Modify: `Cargo.toml` (add `[[test]]` entry if needed — check if the file is auto-discovered)

> **Note:** These tests require live Azure credentials and a configured vault. Run with:
> `cargo test --test gen_tests -- --test-threads=1`

- [ ] **Step 1: Write the integration test**

Create `tests/gen_tests.rs`:

```rust
//! Integration tests for the `xv gen` command.
//!
//! Tests marked `#[ignore]` require live Azure credentials and a configured default vault.
//! Run Azure tests with:
//!   cargo test --test gen_tests -- --ignored --nocapture --test-threads=1

#[cfg(test)]
mod gen_integration_tests {
    use std::process::Command;

    /// Helper: run the compiled `xv` binary with the given args.
    /// Returns (exit_success, stdout, stderr).
    fn run_xv(args: &[&str]) -> (bool, String, String) {
        let binary = env!("CARGO_BIN_EXE_xv");
        let output = Command::new(binary)
            .args(args)
            .output()
            .expect("failed to execute xv binary");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (output.status.success(), stdout, stderr)
    }

    /// Does not require Azure — just tests CLI argument validation.
    #[test]
    fn test_gen_length_out_of_range_fails() {
        let (ok, _, stderr) = run_xv(&["gen", "--length", "5", "--raw"]);
        assert!(!ok, "gen with length 5 should fail");
        assert!(
            stderr.contains("6") || stderr.contains("between"),
            "Error message should mention valid range: {stderr}"
        );
    }

    /// Requires live Azure credentials and a configured default vault.
    #[test]
    #[ignore]
    fn test_gen_save_creates_secret_and_can_be_retrieved() {
        let test_secret_name = format!("xv-gen-test-{}", std::process::id());

        // Generate and save; --raw prints the value to stdout before the success message
        let (ok, stdout, _stderr) = run_xv(&[
            "gen",
            "--length", "16",
            "--charset", "alphanumeric",
            "--save", &test_secret_name,
            "--raw",
        ]);
        assert!(ok, "gen --save should succeed");
        // First non-empty line is the password (println! adds a newline)
        let password_line = stdout.lines().next().unwrap_or("").trim().to_string();
        assert_eq!(password_line.len(), 16, "Generated password should be 16 chars");

        // Retrieve and verify the saved secret matches
        let (ok2, stdout2, _) = run_xv(&["get", &test_secret_name, "--raw"]);
        assert!(ok2, "get should succeed after gen --save");
        assert_eq!(stdout2.trim(), password_line, "Retrieved value should match generated password");

        // Cleanup
        let (ok3, _, _) = run_xv(&["delete", &test_secret_name, "--force"]);
        assert!(ok3, "cleanup delete should succeed");
    }
}
```

- [ ] **Step 2: Run the integration test (requires Azure credentials)**

```bash
cargo test --test gen_tests -- --test-threads=1 --nocapture 2>&1
```

Expected: `test_gen_length_out_of_range_fails` passes. The `#[ignore]`-annotated Azure test is skipped by default. To run the Azure test explicitly: `cargo test --test gen_tests -- --ignored --nocapture --test-threads=1`

- [ ] **Step 3: Commit**

```bash
git add tests/gen_tests.rs
git commit -m "test: add gen command integration tests"
```

---

## Final Verification

- [ ] Run the full unit test suite:
  ```bash
  cargo test --lib 2>&1 | tail -10
  ```
  Expected: all pass.

- [ ] Run clippy:
  ```bash
  cargo clippy --all-targets 2>&1 | grep "^error" | head -20
  ```
  Expected: no errors.

- [ ] Verify the help text is correct:
  ```bash
  cargo run -- gen --help
  cargo run -- help gen
  ```

- [ ] Verify `xv config set gen_default_charset alphanumeric-symbols` works end-to-end:
  ```bash
  cargo run -- config set gen_default_charset alphanumeric-symbols
  cargo run -- config set gen_default_charset invalid-value  # should fail with clear error
  ```
