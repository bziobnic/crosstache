# Errors, Suggestions & Exit Codes Implementation Plan

> **Status:** ✅ Implemented in **v0.6.0-rc.1** (2026-04-30).
> Retained as design history.
> Roadmap & open work tracked in `ROADMAP.md` at the repo root.
> Implementation history lives in `CHANGELOG.md`. This file is retained as design context — do not edit to reflect current behavior; open a new spec instead.


> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship structured error codes, documented exit codes, "did you mean…?" suggestions, and a JSON error envelope so scripts and CI tools can consume crosstache failures programmatically. Foundation for v0.6.0-rc.

**Architecture:** Extend the existing `CrosstacheError` enum with `code()` and `exit_code()` methods (exhaustive match enforces variant coverage at compile time). Add an optional `suggestion: Option<String>` field to the two variants where suggestions make sense (`SecretNotFound`, `VaultNotFound`) plus a `with_suggestion()` builder. Compute suggestions at call sites that already have a candidate list cheaply available (Levenshtein in a new `utils::suggestions` module). Update `main.rs::print_user_friendly_error` to (a) exit with the variant's `exit_code()` instead of always 1, (b) emit a JSON envelope on stdout when `--format json|yaml`, and (c) append a static hint on TTY. No behavior change for unchanged error paths — exit code 1 stays for variants that don't get a specific code.

**Tech Stack:** Rust 2021, `thiserror` for the enum, `serde_json` for the envelope, `serde_yaml` for the YAML envelope, `is-terminal` (already in deps via `atty`-style usage in `output` module) for TTY detection. No new heavyweight dependencies — `strsim` is added (small, pure-Rust, ~50 LoC of trait impls) for Levenshtein.

**Reference spec:** `docs/superpowers/specs/2026-04-29-strategic-improvements-phase-1-design.md` §3.1, §5.1, §5.4 (security invariant: suggestions never include secret values).

---

## File Structure

**Created:**

| Path | Responsibility |
|------|----------------|
| `src/utils/suggestions.rs` | Levenshtein-based `closest_match(target, candidates) -> Option<&str>`. Pure function; no I/O. |
| `src/utils/error_hints.rs` | `hint_for(code) -> Option<&'static str>` static map: stable error code → one-line user hint. |
| `tests/error_codes_tests.rs` | Integration tests that invoke the `xv` binary, trigger known errors, assert exit codes & content. |
| `docs/exit-codes.md` | User-facing exit-code reference. |

**Modified:**

| Path | Change |
|------|--------|
| `src/error.rs` | Add `code()`, `exit_code()`, `with_suggestion()`, `suggestion()` methods on `CrosstacheError`. Add `suggestion: Option<String>` field to `SecretNotFound` and `VaultNotFound` variants. Add unit tests. |
| `src/utils/mod.rs` | Add `pub mod suggestions;` and `pub mod error_hints;`. |
| `src/main.rs` | Refactor `print_user_friendly_error` to take an `&CrosstacheError` plus the resolved `OutputFormat`. Print JSON envelope on json/yaml; print plain message + TTY hint otherwise. Use `error.exit_code()` in the exit call. |
| `src/cli/secret_ops.rs` | Wire suggestion attachment in `execute_secret_get` (the `xv get` path). |
| `src/cli/vault_ops.rs` | Wire suggestion attachment in `execute_vault_info` (the `xv vault info` path). |
| `Cargo.toml` | Add `strsim = "0.11"` to `[dependencies]`. |
| `README.md` | Add a one-line link to `docs/exit-codes.md` under a "Scripting & exit codes" subheading near the existing usage section. |

---

## Task 1: Add `code()` method to `CrosstacheError`

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block in `src/error.rs` (above the existing `// --- Constructor methods ---` comment is fine; pick a free spot inside the module):

```rust
// --- Stable error codes ---

#[test]
fn test_code_for_every_variant() {
    use std::collections::HashSet;
    let cases: Vec<(CrosstacheError, &str)> = vec![
        (CrosstacheError::authentication("x"), "xv-auth-failed"),
        (CrosstacheError::azure_api("x"), "xv-azure-api"),
        (CrosstacheError::config("x"), "xv-config-invalid"),
        (CrosstacheError::secret_not_found("x"), "xv-secret-not-found"),
        (CrosstacheError::vault_not_found("x"), "xv-vault-not-found"),
        (CrosstacheError::invalid_secret_name("x"), "xv-invalid-secret-name"),
        (CrosstacheError::permission_denied("x"), "xv-permission-denied"),
        (CrosstacheError::network("x"), "xv-network"),
        (CrosstacheError::dns_resolution("x", "y"), "xv-network-dns"),
        (CrosstacheError::connection_timeout("x"), "xv-network-timeout"),
        (CrosstacheError::connection_refused("x"), "xv-network-refused"),
        (CrosstacheError::ssl_error("x"), "xv-network-ssl"),
        (CrosstacheError::invalid_url("x"), "xv-invalid-url"),
        (CrosstacheError::serialization("x"), "xv-serialization"),
        (CrosstacheError::invalid_argument("x"), "xv-invalid-argument"),
        (CrosstacheError::upgrade("x"), "xv-upgrade"),
        (CrosstacheError::unknown("x"), "xv-unknown"),
        (
            CrosstacheError::IoError(std::io::Error::new(std::io::ErrorKind::NotFound, "x")),
            "xv-io",
        ),
        (
            CrosstacheError::JsonError(
                serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
            ),
            "xv-json",
        ),
    ];
    let mut seen = HashSet::new();
    for (err, expected_code) in cases {
        assert_eq!(err.code(), expected_code, "wrong code for {err:?}");
        assert!(seen.insert(expected_code), "duplicate code {expected_code}");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib error::tests::test_code_for_every_variant`
Expected: compile error — `code` method does not exist on `CrosstacheError`.

- [ ] **Step 3: Implement `code()`**

Inside the `impl CrosstacheError { ... }` block in `src/error.rs` (just above `pub fn authentication` is fine), add:

```rust
/// Stable, kebab-case error code. Part of the public scripting contract.
/// New variants must add a code; the exhaustive match keeps this honest.
pub fn code(&self) -> &'static str {
    match self {
        Self::AuthenticationError(_) => "xv-auth-failed",
        Self::AzureApiError(_) => "xv-azure-api",
        Self::ConfigError(_) => "xv-config-invalid",
        Self::ConfigLoadError(_) => "xv-config-invalid",
        Self::SecretNotFound { .. } => "xv-secret-not-found",
        Self::VaultNotFound { .. } => "xv-vault-not-found",
        Self::InvalidSecretName { .. } => "xv-invalid-secret-name",
        Self::PermissionDenied(_) => "xv-permission-denied",
        Self::NetworkError(_) => "xv-network",
        Self::DnsResolutionError { .. } => "xv-network-dns",
        Self::ConnectionTimeout(_) => "xv-network-timeout",
        Self::ConnectionRefused(_) => "xv-network-refused",
        Self::SslError(_) => "xv-network-ssl",
        Self::InvalidUrl(_) => "xv-invalid-url",
        Self::SerializationError(_) => "xv-serialization",
        Self::IoError(_) => "xv-io",
        Self::JsonError(_) => "xv-json",
        Self::YamlError(_) => "xv-yaml",
        Self::HttpError(_) => "xv-http",
        Self::UuidError(_) => "xv-uuid",
        Self::RegexError(_) => "xv-regex",
        Self::InvalidArgument(_) => "xv-invalid-argument",
        Self::Upgrade(_) => "xv-upgrade",
        Self::Unknown(_) => "xv-unknown",
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib error::tests::test_code_for_every_variant`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/error.rs
git commit -m "feat(error): add stable code() method to CrosstacheError

Stable kebab-case codes for every variant. Compile-time exhaustive
match guarantees future variants must register a code.
"
```

---

## Task 2: Add `exit_code()` method to `CrosstacheError`

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` in `src/error.rs`:

```rust
// --- Exit codes ---

#[test]
fn test_exit_code_families() {
    // 2 — invalid argument
    assert_eq!(CrosstacheError::invalid_argument("x").exit_code(), 2);

    // 3 — config family
    assert_eq!(CrosstacheError::config("x").exit_code(), 3);

    // 10–19 — not-found family
    assert_eq!(CrosstacheError::secret_not_found("x").exit_code(), 10);
    assert_eq!(CrosstacheError::vault_not_found("x").exit_code(), 11);

    // 20–29 — auth/permission
    assert_eq!(CrosstacheError::authentication("x").exit_code(), 20);
    assert_eq!(CrosstacheError::permission_denied("x").exit_code(), 21);

    // 30–39 — network
    assert_eq!(CrosstacheError::network("x").exit_code(), 30);
    assert_eq!(CrosstacheError::dns_resolution("x", "y").exit_code(), 31);
    assert_eq!(CrosstacheError::connection_timeout("x").exit_code(), 32);
    assert_eq!(CrosstacheError::connection_refused("x").exit_code(), 33);
    assert_eq!(CrosstacheError::ssl_error("x").exit_code(), 34);

    // 40–49 — Azure/backend
    assert_eq!(CrosstacheError::azure_api("x").exit_code(), 40);

    // 1 — unknown / catch-all
    assert_eq!(CrosstacheError::unknown("x").exit_code(), 1);
}

#[test]
fn test_exit_code_is_stable_for_unknown_variants() {
    // From-converted errors that don't have a clear family fall back to 1.
    let io_err = CrosstacheError::IoError(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "x",
    ));
    assert_eq!(io_err.exit_code(), 1);

    let json_err = CrosstacheError::JsonError(
        serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
    );
    assert_eq!(json_err.exit_code(), 1);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib error::tests::test_exit_code`
Expected: compile error — `exit_code` method missing.

- [ ] **Step 3: Implement `exit_code()`**

Add inside `impl CrosstacheError { ... }`, immediately below `code()`:

```rust
/// Process exit code for this error. Codes group by family; see
/// `docs/exit-codes.md` for the public table.
pub fn exit_code(&self) -> i32 {
    match self {
        Self::InvalidArgument(_) => 2,
        Self::ConfigError(_) | Self::ConfigLoadError(_) => 3,

        Self::SecretNotFound { .. } => 10,
        Self::VaultNotFound { .. } => 11,
        Self::InvalidSecretName { .. } => 12,

        Self::AuthenticationError(_) => 20,
        Self::PermissionDenied(_) => 21,

        Self::NetworkError(_) => 30,
        Self::DnsResolutionError { .. } => 31,
        Self::ConnectionTimeout(_) => 32,
        Self::ConnectionRefused(_) => 33,
        Self::SslError(_) => 34,
        Self::InvalidUrl(_) => 35,

        Self::AzureApiError(_) => 40,

        // Reserve 50–59 for the scanner feature (lands in plan 4).

        Self::SerializationError(_)
        | Self::IoError(_)
        | Self::JsonError(_)
        | Self::YamlError(_)
        | Self::HttpError(_)
        | Self::UuidError(_)
        | Self::RegexError(_)
        | Self::Upgrade(_)
        | Self::Unknown(_) => 1,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib error::tests::test_exit_code`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/error.rs
git commit -m "feat(error): add exit_code() method with documented families

Codes group: 2=invalid-arg, 3=config, 10-19=not-found, 20-29=auth,
30-39=network, 40-49=azure, 50-59=reserved-for-scanner, 1=unknown.
"
```

---

## Task 3: Wire `main.rs` to exit with `error.exit_code()`

**Files:**
- Modify: `src/main.rs:37-41`

- [ ] **Step 1: Write the failing integration test**

Create `tests/error_codes_tests.rs`:

```rust
//! Integration tests asserting the `xv` binary exits with the documented
//! exit code per error family. These tests build and run the binary.

use std::process::Command;

fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

#[test]
fn invalid_argument_exits_2() {
    let out = xv().args(["--this-flag-does-not-exist"]).output().unwrap();
    assert!(!out.status.success());
    // clap parse failures use exit 2 on its own; we rely on that being our
    // family code as well, which the new exit_code() preserves.
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn unknown_subcommand_exits_2() {
    let out = xv().args(["this-subcommand-does-not-exist"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}
```

- [ ] **Step 2: Run the test to verify it fails or passes**

Run: `cargo test --test error_codes_tests`
Expected: PASS today (clap already exits 2 for parse errors). The test guards against regression — keep it.

- [ ] **Step 3: Update `main()` to use `error.exit_code()`**

In `src/main.rs`, replace lines 37-41:

```rust
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        print_user_friendly_error(&e);
        std::process::exit(1);
    }
```

with:

```rust
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        print_user_friendly_error(&e);
        std::process::exit(e.exit_code());
    }
```

- [ ] **Step 4: Run the tests to verify they still pass**

Run: `cargo test --test error_codes_tests` and `cargo test --lib error`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs tests/error_codes_tests.rs
git commit -m "feat(main): exit with error.exit_code() instead of always 1

Adds first integration test asserting the contract.
"
```

---

## Task 4: Add `strsim` dependency

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock` (auto-updated by cargo)

- [ ] **Step 1: Add the dependency**

Open `Cargo.toml` and find the `[dependencies]` section. Add:

```toml
strsim = "0.11"
```

Keep alphabetical ordering if the section uses it; otherwise append at the end of the section.

- [ ] **Step 2: Run a sanity build**

Run: `cargo build`
Expected: build succeeds; `Cargo.lock` updates.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add strsim 0.11 for fuzzy 'did you mean' suggestions"
```

---

## Task 5: Create `utils::suggestions` module

**Files:**
- Create: `src/utils/suggestions.rs`
- Modify: `src/utils/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `src/utils/suggestions.rs` with the test scaffolding only:

```rust
//! Levenshtein-based "did you mean...?" matcher. Pure functions; no I/O.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_close_match() {
        let candidates = vec![
            "myproj-prod".to_string(),
            "myproj-dev".to_string(),
            "completely-different".to_string(),
        ];
        let result = closest_match("myproj-prood", &candidates);
        assert_eq!(result, Some("myproj-prod"));
    }

    #[test]
    fn returns_none_when_too_far() {
        let candidates = vec!["banana".to_string(), "apple".to_string()];
        let result = closest_match("xyzzy", &candidates);
        assert_eq!(result, None);
    }

    #[test]
    fn returns_none_for_empty_candidates() {
        let candidates: Vec<String> = vec![];
        let result = closest_match("anything", &candidates);
        assert_eq!(result, None);
    }

    #[test]
    fn exact_match_wins_over_close_match() {
        let candidates = vec!["foo".to_string(), "fop".to_string()];
        let result = closest_match("foo", &candidates);
        assert_eq!(result, Some("foo"));
    }

    #[test]
    fn distance_threshold_is_two() {
        // distance 2: one edit away in two places
        let candidates = vec!["abcde".to_string()];
        assert_eq!(closest_match("axcye", &candidates), Some("abcde"));
        // distance 3: too far
        let candidates = vec!["abcde".to_string()];
        assert_eq!(closest_match("axcyf", &candidates), None);
    }
}
```

Then add the import to `src/utils/mod.rs`. Find the existing list of `pub mod ...` declarations and add (alphabetically):

```rust
pub mod suggestions;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib utils::suggestions`
Expected: compile error — `closest_match` is not defined.

- [ ] **Step 3: Implement `closest_match`**

Add to the top of `src/utils/suggestions.rs` (above the `#[cfg(test)]` block):

```rust
/// Maximum edit distance for a candidate to be considered a "did you mean"
/// match. Empirically tuned for short identifier-style names.
const MAX_DISTANCE: usize = 2;

/// Return the closest candidate to `target` if any are within
/// `MAX_DISTANCE` edits. Ties broken by first-seen order in `candidates`.
///
/// Returns `None` when:
///   - `candidates` is empty
///   - no candidate scores ≤ `MAX_DISTANCE`
///   - `target` is empty (avoids degenerate matches)
pub fn closest_match<'a>(target: &str, candidates: &'a [String]) -> Option<&'a str> {
    if target.is_empty() || candidates.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = strsim::levenshtein(target, c);
        if d > MAX_DISTANCE {
            continue;
        }
        match best {
            None => best = Some((d, c.as_str())),
            Some((bd, _)) if d < bd => best = Some((d, c.as_str())),
            _ => {}
        }
    }
    best.map(|(_, name)| name)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib utils::suggestions`
Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/utils/suggestions.rs src/utils/mod.rs
git commit -m "feat(utils): add suggestions::closest_match Levenshtein matcher

Pure function. Returns Some(name) when within 2 edits, None otherwise.
Used by the error layer to attach 'did you mean ...?' hints.
"
```

---

## Task 6: Add `suggestion` field to `SecretNotFound` and `VaultNotFound`

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` in `src/error.rs`:

```rust
// --- Suggestions ---

#[test]
fn secret_not_found_suggestion_round_trip() {
    let err = CrosstacheError::secret_not_found("DB_PASSWURD")
        .with_suggestion(Some("DB_PASSWORD".to_string()));
    assert_eq!(err.suggestion(), Some("DB_PASSWORD"));
}

#[test]
fn vault_not_found_suggestion_round_trip() {
    let err = CrosstacheError::vault_not_found("myproj-prood")
        .with_suggestion(Some("myproj-prod".to_string()));
    assert_eq!(err.suggestion(), Some("myproj-prod"));
}

#[test]
fn variants_without_suggestion_field_return_none() {
    let err = CrosstacheError::network("dropped");
    assert_eq!(err.suggestion(), None);
}

#[test]
fn with_suggestion_on_variant_without_field_is_noop() {
    // Calling .with_suggestion on a variant that has no slot must not panic.
    let err = CrosstacheError::network("dropped").with_suggestion(Some("hint".into()));
    assert_eq!(err.suggestion(), None);
    // Still the same kind of error.
    assert_eq!(err.code(), "xv-network");
}

#[test]
fn secret_not_found_default_suggestion_is_none() {
    let err = CrosstacheError::secret_not_found("X");
    assert_eq!(err.suggestion(), None);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib error::tests::secret_not_found_suggestion_round_trip`
Expected: compile error — `with_suggestion` and `suggestion` not defined; `SecretNotFound` does not have a `suggestion` field.

- [ ] **Step 3: Add the field to the variants**

In `src/error.rs`, replace lines 15-19:

```rust
    #[error("Secret not found: {name}")]
    SecretNotFound { name: String },

    #[error("Vault not found: {name}")]
    VaultNotFound { name: String },
```

with:

```rust
    #[error("Secret not found: {name}")]
    SecretNotFound {
        name: String,
        suggestion: Option<String>,
    },

    #[error("Vault not found: {name}")]
    VaultNotFound {
        name: String,
        suggestion: Option<String>,
    },
```

- [ ] **Step 4: Update the constructors and existing pattern matches**

In `src/error.rs`, update `secret_not_found` (around line 93):

```rust
    #[allow(dead_code)]
    pub fn secret_not_found<S: Into<String>>(name: S) -> Self {
        Self::SecretNotFound {
            name: name.into(),
            suggestion: None,
        }
    }
```

And `vault_not_found` (around line 97):

```rust
    pub fn vault_not_found<S: Into<String>>(name: S) -> Self {
        Self::VaultNotFound {
            name: name.into(),
            suggestion: None,
        }
    }
```

- [ ] **Step 5: Update the existing tests in `error.rs` that destructure these variants**

In `src/error.rs::test_secret_not_found_constructor` (around line 198), replace the assertion line:

```rust
    assert!(matches!(err, CrosstacheError::SecretNotFound { ref name } if name == "my-secret"));
```

with:

```rust
    assert!(matches!(err, CrosstacheError::SecretNotFound { ref name, .. } if name == "my-secret"));
```

In `test_vault_not_found_constructor` (around line 207), replace:

```rust
    assert!(matches!(err, CrosstacheError::VaultNotFound { ref name } if name == "prod-vault"));
```

with:

```rust
    assert!(matches!(err, CrosstacheError::VaultNotFound { ref name, .. } if name == "prod-vault"));
```

- [ ] **Step 6: Add `with_suggestion()` and `suggestion()` methods**

In the `impl CrosstacheError { ... }` block in `src/error.rs`, add (just below the existing constructor methods is fine — pick a free spot):

```rust
/// Attach a "did you mean...?" suggestion to a variant that supports one.
/// No-op for variants without a `suggestion` field.
pub fn with_suggestion(mut self, candidate: Option<String>) -> Self {
    match &mut self {
        Self::SecretNotFound { suggestion, .. } => *suggestion = candidate,
        Self::VaultNotFound { suggestion, .. } => *suggestion = candidate,
        _ => {}
    }
    self
}

/// Return the attached suggestion, if any.
pub fn suggestion(&self) -> Option<&str> {
    match self {
        Self::SecretNotFound { suggestion, .. } => suggestion.as_deref(),
        Self::VaultNotFound { suggestion, .. } => suggestion.as_deref(),
        _ => None,
    }
}
```

- [ ] **Step 7: Update `main.rs` pattern matches that destructure `SecretNotFound` / `VaultNotFound`**

`src/main.rs` has matches at line 119 (`VaultNotFound { name }`) and line 128 (`SecretNotFound { name }`). Replace both `{ name }` with `{ name, .. }`:

Line 119: `VaultNotFound { name } => {` → `VaultNotFound { name, .. } => {`
Line 128: `SecretNotFound { name } => {` → `SecretNotFound { name, .. } => {`

- [ ] **Step 8: Find and update any other destructurings across the codebase**

Run: `grep -rn "SecretNotFound { name\|VaultNotFound { name" src/ tests/`
For each result that uses `{ name }` (without `..`), update it to `{ name, .. }`.

Run: `cargo build` after each fix to surface the next compile error if any are missed.

- [ ] **Step 9: Run all tests to verify they pass**

Run: `cargo test --lib error`
Expected: all error-module tests PASS, including the new suggestion tests.

Run: `cargo build`
Expected: builds clean.

- [ ] **Step 10: Commit**

```bash
git add src/error.rs src/main.rs
# plus any other files touched in step 8
git commit -m "feat(error): add suggestion field to SecretNotFound and VaultNotFound

with_suggestion() builder attaches a candidate; suggestion() reads it.
No-op on variants without a slot. Existing destructurings updated to
use { name, .. } pattern.
"
```

---

## Task 7: Wire suggestion attachment in `xv get`

**Files:**
- Modify: `src/cli/secret_ops.rs`

- [ ] **Step 1: Locate the `SecretNotFound` construction in `execute_secret_get`**

Run: `grep -n "SecretNotFound\|secret_not_found" src/cli/secret_ops.rs`

There may be one or more sites where the get path returns this error. Open each and identify the one that fires when a name lookup fails.

- [ ] **Step 2: Write the integration test**

Append to `tests/error_codes_tests.rs`:

```rust
// Note: this test depends on a configured xv environment with a known
// vault. We mark it ignored by default; CI runs it via XV_TEST_VAULT.
#[test]
#[ignore = "requires XV_TEST_VAULT and credentials"]
fn secret_not_found_includes_suggestion_when_close_match_exists() {
    let vault = std::env::var("XV_TEST_VAULT").expect("XV_TEST_VAULT must be set");
    // Assumes a secret named "DB_PASSWORD" exists in XV_TEST_VAULT.
    let out = xv()
        .args(["get", "DB_PASSWURD", "--vault", &vault, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(10));
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert_eq!(body["error"]["code"], "xv-secret-not-found");
    assert_eq!(body["error"]["suggestion"], "DB_PASSWORD");
}
```

(This test is `#[ignore]` so it doesn't gate the merge, but exists as documentation of the contract for anyone running the live suite.)

- [ ] **Step 3: Add the suggestion-attachment call**

At the `SecretNotFound` construction site found in step 1, change the construction from (something like):

```rust
return Err(CrosstacheError::secret_not_found(name));
```

to:

```rust
let suggestion = match secret_manager.list_secret_names().await {
    Ok(names) => crate::utils::suggestions::closest_match(name, &names).map(|s| s.to_string()),
    Err(_) => None, // suggestion is best-effort; never fail the original error path
};
return Err(CrosstacheError::secret_not_found(name).with_suggestion(suggestion));
```

If `secret_manager` does not expose a method named `list_secret_names`, use whatever existing list method returns secret names (e.g., `list_secrets().await.map(|v| v.into_iter().map(|s| s.name).collect())`). Inspect the actual `SecretManager` API and match it. Do NOT add a new method to `SecretManager` for this — use what's there.

If the method requires arguments, pass the same scope arguments the original `get` used.

- [ ] **Step 4: Run the unit tests to verify nothing broke**

Run: `cargo test --lib`
Expected: all PASS.

- [ ] **Step 5: Verify the build still compiles**

Run: `cargo build`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/cli/secret_ops.rs tests/error_codes_tests.rs
git commit -m "feat(cli): attach 'did you mean' suggestion to xv get not-found

Best-effort suggestion attachment via Levenshtein on cached candidate
list. List failures don't disrupt the original error path.
"
```

---

## Task 8: Wire suggestion attachment in `xv vault info`

**Files:**
- Modify: `src/cli/vault_ops.rs`

- [ ] **Step 1: Locate the `VaultNotFound` construction**

Run: `grep -n "VaultNotFound\|vault_not_found" src/cli/vault_ops.rs`

Find the site fired by `execute_vault_info` (or whatever the `xv vault info` handler is named). If there's no explicit construction there and the not-found error bubbles up from `VaultManager`, the wiring goes at the highest layer that has access to a vault list — typically the CLI handler.

- [ ] **Step 2: Add the suggestion-attachment call**

At the construction site, modify analogously to Task 7:

```rust
let suggestion = match vault_manager.list_vault_names().await {
    Ok(names) => crate::utils::suggestions::closest_match(name, &names).map(|s| s.to_string()),
    Err(_) => None,
};
return Err(CrosstacheError::vault_not_found(name).with_suggestion(suggestion));
```

Substitute the actual list method. If only `list_vaults()` exists returning richer structs, map them to names: `.into_iter().map(|v| v.name).collect()`.

If `VaultNotFound` is bubbled from the manager rather than constructed in `vault_ops.rs`, intercept at the CLI handler:

```rust
match vault_manager.get_vault(name).await {
    Ok(v) => v,
    Err(CrosstacheError::VaultNotFound { name: n, .. }) => {
        let suggestion = match vault_manager.list_vault_names().await {
            Ok(names) => crate::utils::suggestions::closest_match(&n, &names).map(|s| s.to_string()),
            Err(_) => None,
        };
        return Err(CrosstacheError::vault_not_found(n).with_suggestion(suggestion));
    }
    Err(e) => return Err(e),
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib && cargo build`
Expected: all PASS, clean build.

- [ ] **Step 4: Commit**

```bash
git add src/cli/vault_ops.rs
git commit -m "feat(cli): attach 'did you mean' suggestion to xv vault info not-found"
```

---

## Task 9: Create `utils::error_hints` module

**Files:**
- Create: `src/utils/error_hints.rs`
- Modify: `src/utils/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `src/utils/error_hints.rs`:

```rust
//! Static "code -> hint" map for TTY error display.
//! Hints are short (one line) and actionable.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_have_hints() {
        assert!(hint_for("xv-vault-not-found").is_some());
        assert!(hint_for("xv-secret-not-found").is_some());
        assert!(hint_for("xv-permission-denied").is_some());
        assert!(hint_for("xv-network-dns").is_some());
        assert!(hint_for("xv-config-invalid").is_some());
    }

    #[test]
    fn unknown_codes_return_none() {
        assert_eq!(hint_for("xv-this-code-does-not-exist"), None);
    }

    #[test]
    fn hints_are_one_line() {
        for code in [
            "xv-vault-not-found",
            "xv-secret-not-found",
            "xv-permission-denied",
            "xv-network-dns",
            "xv-network-timeout",
            "xv-config-invalid",
        ] {
            let hint = hint_for(code).unwrap();
            assert!(!hint.contains('\n'), "hint for {code} contains newline: {hint:?}");
            assert!(!hint.is_empty(), "hint for {code} is empty");
        }
    }
}
```

Add to `src/utils/mod.rs` (alphabetical):

```rust
pub mod error_hints;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib utils::error_hints`
Expected: compile error — `hint_for` not defined.

- [ ] **Step 3: Implement `hint_for`**

Add to the top of `src/utils/error_hints.rs`:

```rust
/// Return a one-line user hint for the given error code, or `None` if
/// no hint is registered. Hints are TTY-only — print them after the
/// main error message.
pub fn hint_for(code: &str) -> Option<&'static str> {
    Some(match code {
        "xv-vault-not-found" => "Run 'xv vault list' to see available vaults.",
        "xv-secret-not-found" => "Run 'xv list' to see secrets in the active vault.",
        "xv-invalid-secret-name" => "Names must be alphanumeric + hyphens; see 'xv help set'.",
        "xv-permission-denied" => "Check your role with 'xv whoami'; see 'xv vault share list'.",
        "xv-auth-failed" => "Try 'az login' or set AZURE_CLIENT_ID / AZURE_CLIENT_SECRET / AZURE_TENANT_ID.",
        "xv-network-dns" => "Check the vault name and your DNS settings.",
        "xv-network-timeout" => "Check your network connection or proxy settings.",
        "xv-network-refused" => "Verify the vault exists and is reachable from this network.",
        "xv-network-ssl" => "Check TLS configuration and any corporate proxy with TLS interception.",
        "xv-config-invalid" => "Run 'xv config show' to inspect, or 'xv init' to reinitialize.",
        "xv-azure-api" => "Check Azure service status and your subscription quotas.",
        _ => return None,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib utils::error_hints`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/utils/error_hints.rs src/utils/mod.rs
git commit -m "feat(utils): add error_hints::hint_for static map

Maps stable error codes to one-line user hints. TTY-only consumer in
main.rs's error printer.
"
```

---

## Task 10: Refactor `main.rs::print_user_friendly_error` to use code/hint/format

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing integration test**

Append to `tests/error_codes_tests.rs`:

```rust
#[test]
#[ignore = "requires a working config that triggers VaultNotFound predictably"]
fn json_format_emits_error_envelope() {
    // Triggers a vault-not-found by passing a vault name that cannot exist.
    let out = xv()
        .args([
            "vault", "info",
            "definitely-does-not-exist-zzzzzzzz",
            "--format", "json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(11));
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be JSON envelope");
    assert_eq!(body["error"]["code"], "xv-vault-not-found");
    assert_eq!(body["error"]["exit_code"], 11);
    assert!(body["error"]["message"].is_string());
}

#[test]
fn plain_format_writes_error_to_stderr() {
    let out = xv().args(["this-subcommand-does-not-exist"]).output().unwrap();
    assert!(!out.stderr.is_empty(), "stderr should contain clap parse error");
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --test error_codes_tests`
Expected: ignored test is skipped; the plain-format test PASSES (clap output to stderr is the existing behavior — guard it).

- [ ] **Step 3: Add format threading to `main()`**

In `src/main.rs`, the current `main()` is:

```rust
#[tokio::main]
async fn main() {
    reset_sigpipe();
    init_logging();
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        print_user_friendly_error(&e);
        std::process::exit(e.exit_code());
    }
}
```

Replace with:

```rust
#[tokio::main]
async fn main() {
    reset_sigpipe();
    init_logging();
    let cli = Cli::parse();
    let format = cli.format; // OutputFormat is Copy
    if let Err(e) = run(cli).await {
        error!("Error: {}", e);
        print_user_friendly_error(&e, format);
        std::process::exit(e.exit_code());
    }
}
```

If `OutputFormat` is not `Copy` (the build will tell you), use `cli.format.clone()` and update the parameter type in step 4 accordingly.

- [ ] **Step 4: Replace `print_user_friendly_error` body**

In `src/main.rs`, replace the entire `print_user_friendly_error` function (lines 98-204) with:

```rust
fn print_user_friendly_error(error: &CrosstacheError, format: crate::utils::format::OutputFormat) {
    use crate::utils::error_hints::hint_for;
    use crate::utils::format::OutputFormat;
    use std::io::IsTerminal;

    // Resolve auto-format the same way the data-display path does.
    let resolved = format.resolve_for_stdout();

    // Machine-readable envelope on stdout for json/yaml.
    if matches!(resolved, OutputFormat::Json | OutputFormat::Yaml) {
        let mut envelope = serde_json::json!({
            "error": {
                "code": error.code(),
                "message": error.to_string(),
                "exit_code": error.exit_code(),
            }
        });
        if let Some(s) = error.suggestion() {
            envelope["error"]["suggestion"] = serde_json::Value::String(s.to_string());
        }
        let rendered = match resolved {
            OutputFormat::Json => serde_json::to_string(&envelope).unwrap_or_default(),
            OutputFormat::Yaml => {
                serde_yaml::to_string(&envelope).unwrap_or_default()
            }
            _ => unreachable!(),
        };
        println!("{rendered}");
        return;
    }

    // Plain-text path: keep the existing rich, multi-line messages on
    // stderr but trim them — the new error layer carries the structure.
    eprintln!("error[{}]: {}", error.code(), error);

    if let Some(s) = error.suggestion() {
        eprintln!("  did you mean: {s}?");
    }

    if std::io::stderr().is_terminal() {
        if let Some(hint) = hint_for(error.code()) {
            eprintln!("  hint: {hint}");
        }
    }
}
```

- [ ] **Step 5: Drop the unused `output` import & `use CrosstacheError::*` if they become dead**

After replacing the function body, the imports at line 99-100 may now be unused. The build will warn — drop them as the warnings indicate:

- Remove `use crate::utils::output;` (if present in scope after the rewrite — it isn't, since the new body doesn't use `output`).
- Remove `use CrosstacheError::*;`.

- [ ] **Step 6: Run all tests**

Run: `cargo test`
Expected: all PASS.

Run: `cargo build`
Expected: clean — possibly some warnings to clean up in step 5 above.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs tests/error_codes_tests.rs
git commit -m "feat(main): structured error output with JSON envelope and TTY hints

- json/yaml formats emit a {error: {code, message, exit_code, suggestion}}
  envelope on stdout for script consumption
- plain format writes 'error[code]: message' to stderr with optional
  did-you-mean and TTY-only hint lines
- exits with error.exit_code() per the documented family table
"
```

---

## Task 11: Add value-leak invariant snapshot test

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `src/error.rs`:

```rust
// --- Security: no error variant carries a secret value ---

#[test]
fn no_variant_has_a_secret_value_field() {
    // This is a hand-maintained list of variant fields. If you add a
    // variant whose payload could carry a secret value, this test will
    // fail in code review — keep the list updated.
    //
    // The check is structural: we simply confirm that the only
    // string fields on every variant are message/name/details fields,
    // never anything called "value", "secret", "password", or "token".
    let variant_field_names = [
        ("AuthenticationError", vec!["msg"]),
        ("AzureApiError", vec!["msg"]),
        ("ConfigError", vec!["msg"]),
        ("ConfigLoadError", vec!["source"]),
        ("SecretNotFound", vec!["name", "suggestion"]),
        ("VaultNotFound", vec!["name", "suggestion"]),
        ("InvalidSecretName", vec!["name"]),
        ("PermissionDenied", vec!["msg"]),
        ("NetworkError", vec!["msg"]),
        ("DnsResolutionError", vec!["vault_name", "details"]),
        ("ConnectionTimeout", vec!["msg"]),
        ("ConnectionRefused", vec!["msg"]),
        ("SslError", vec!["msg"]),
        ("InvalidUrl", vec!["msg"]),
        ("SerializationError", vec!["msg"]),
        ("IoError", vec!["source"]),
        ("JsonError", vec!["source"]),
        ("YamlError", vec!["source"]),
        ("HttpError", vec!["source"]),
        ("UuidError", vec!["source"]),
        ("RegexError", vec!["source"]),
        ("InvalidArgument", vec!["msg"]),
        ("Upgrade", vec!["msg"]),
        ("Unknown", vec!["msg"]),
    ];
    let banned = ["value", "secret", "password", "token", "key"];
    for (variant, fields) in variant_field_names {
        for f in fields {
            for b in banned {
                assert!(
                    !f.contains(b),
                    "variant {variant} field {f:?} contains banned token {b:?}"
                );
            }
        }
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --lib error::tests::no_variant_has_a_secret_value_field`
Expected: PASS today (no offending fields exist).

This is a **regression guard** — it fails the build the moment someone adds e.g. `value: String` to a variant. The hand-maintained list mirrors the enum; updating it during a future refactor is what triggers the review of "is this field safe?"

- [ ] **Step 3: Commit**

```bash
git add src/error.rs
git commit -m "test(error): regression guard against secret values in error variants

Hand-maintained list of variant field names checks against banned
substrings (value, secret, password, token, key). Failing this test
is a security-review trigger.
"
```

---

## Task 12: Write `docs/exit-codes.md` and link from README

**Files:**
- Create: `docs/exit-codes.md`
- Modify: `README.md`

- [ ] **Step 1: Create the docs file**

Create `docs/exit-codes.md` with this content:

````markdown
# Exit Codes

`xv` exits with a documented code per error family. Codes are stable across
releases — they are part of the scripting contract.

## Table

| Code  | Family                | Examples                                        |
|-------|-----------------------|-------------------------------------------------|
| `0`   | Success               | command completed                               |
| `1`   | Unknown / catch-all   | unrecoverable I/O, JSON parse, regex, etc.      |
| `2`   | Invalid argument      | bad CLI flag; clap parse failure                |
| `3`   | Configuration error   | missing required config; invalid config file    |
| `10`  | Secret not found      | `xv get` on a missing secret                    |
| `11`  | Vault not found       | `xv vault info` on a missing vault              |
| `12`  | Invalid secret name   | name fails sanitization rules                   |
| `20`  | Authentication failed | bad token, expired credential, no Azure login   |
| `21`  | Permission denied     | RBAC check failed                               |
| `30`  | Network error         | generic transport failure                       |
| `31`  | DNS resolution failed | vault hostname did not resolve                  |
| `32`  | Connection timeout    | TCP connect or request timeout                  |
| `33`  | Connection refused    | TCP refused                                     |
| `34`  | SSL/TLS error         | certificate or handshake failure                |
| `35`  | Invalid URL           | malformed URL passed to a network call          |
| `40`  | Azure API error       | Azure returned an error response                |
| `50`  | Scan: leak detected   | (future, plan 4) `xv scan` found a finding      |

## Error codes

Every error also has a stable kebab-case code (e.g. `xv-vault-not-found`,
`xv-network-dns`). Use these for scripting:

```bash
if ! out=$(xv get DB_PASSWORD --format json 2>/dev/null); then
  code=$(echo "$out" | jq -r '.error.code')
  case "$code" in
    xv-secret-not-found) echo "secret missing — provisioning…" ;;
    xv-permission-denied) echo "access denied — escalate" ;;
    *) echo "unexpected: $code" ; exit 1 ;;
  esac
fi
```

## JSON error envelope

When `--format json` or `--format yaml` is in effect, errors render to
**stdout** (not stderr) as a structured envelope:

```json
{
  "error": {
    "code": "xv-vault-not-found",
    "message": "Vault not found: myproj-prood",
    "exit_code": 11,
    "suggestion": "myproj-prod"
  }
}
```

`suggestion` is omitted when no near-match was found. The rendered
plain-text form for non-JSON outputs is:

```text
error[xv-vault-not-found]: Vault not found: myproj-prood
  did you mean: myproj-prod?
  hint: Run 'xv vault list' to see available vaults.
```

The `hint` line is TTY-only.
````

- [ ] **Step 2: Add a README link**

Open `README.md`. Find the "Usage" or near-top section that points users at command help. Append a small subsection (or insert one if the structure has none):

```markdown
### Scripting & exit codes

For scripts and CI, see [`docs/exit-codes.md`](docs/exit-codes.md) for
the stable exit-code table and the `--format json` error envelope.
```

Place this after the existing usage block. If the README already has a "Documentation" / "References" list, add an entry there instead — match the existing style.

- [ ] **Step 3: Commit**

```bash
git add docs/exit-codes.md README.md
git commit -m "docs: document exit codes and JSON error envelope

User-facing reference for the scripting contract introduced in v0.6.0.
"
```

---

## Task 13: Final clippy / fmt sweep

**Files:**
- Anything the lint pass touches

- [ ] **Step 1: Run rustfmt**

Run: `cargo fmt --all`
Expected: any formatting drift across this plan's edits is normalized.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -W clippy::all`
Expected: no warnings introduced by this plan's changes. Address any new ones.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: all PASS.

Run: `cargo test -- --test-threads=1`
Expected: no test order flake.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "chore: clippy + fmt sweep after errors-and-exit-codes work" \
  --allow-empty
```

(`--allow-empty` covers the case where steps 1-2 produced no diff. If there were diffs, drop the flag.)

---

## Task 14: Cut v0.6.0-rc

**Files:**
- Modify: `Cargo.toml` (version bump)
- Modify: `CHANGELOG.md` if present
- Tag: git tag

- [ ] **Step 1: Bump the version**

Open `Cargo.toml`. Find the `[package]` block. Change `version = "0.5.4"` (or whatever the current value is — check first) to `version = "0.6.0-rc.1"`.

Run: `cargo build` to refresh `Cargo.lock`.

- [ ] **Step 2: Update CHANGELOG (if it exists)**

Run: `ls CHANGELOG.md` to check.

If present, prepend a new section:

```markdown
## [0.6.0-rc.1] - 2026-04-29

### Added
- Stable, documented exit codes per error family (`docs/exit-codes.md`).
- Stable kebab-case error codes (`xv-vault-not-found`, etc.) on every error.
- JSON error envelope on stdout when `--format json|yaml`.
- "Did you mean…?" suggestions on `xv get` and `xv vault info` for close-match names.
- TTY-only one-line hints below errors.

### Changed
- `xv` now exits with a code from the documented family table instead of always 1.
- Error format on plain output is now `error[code]: message` followed by optional
  did-you-mean and hint lines.
```

If no CHANGELOG exists, skip this step.

- [ ] **Step 3: Commit & tag**

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore: bump version to 0.6.0-rc.1"
git tag -a v0.6.0-rc.1 -m "v0.6.0-rc.1: structured errors, exit codes, suggestions"
```

- [ ] **Step 4: Push to verify**

Do **not** push without explicit user confirmation. Stop here and ask the user before `git push --tags origin main`.

---

## Verification checklist (final, before declaring plan complete)

Run all of the following and confirm each passes:

- [ ] `cargo test`
- [ ] `cargo test -- --test-threads=1`
- [ ] `cargo clippy --all-targets -- -W clippy::all` — no new warnings
- [ ] `cargo fmt --all -- --check` — clean
- [ ] `cargo audit` — no new advisories
- [ ] Manual smoke: `cargo run -- vault info this-vault-does-not-exist-zzzz` — should print `error[xv-vault-not-found]: Vault not found: …` to stderr and exit 11
- [ ] Manual smoke: `cargo run -- vault info this-vault-does-not-exist-zzzz --format json` — should print a JSON envelope to stdout, exit 11
- [ ] Manual smoke: existing happy-path commands (e.g., `xv version`, `xv list`) still work and produce the same output as before
- [ ] Soft-commitment-checklist update: append entries for the read methods used by this work to `docs/superpowers/specs/backend-trait-checklist.md` (create the file if it doesn't exist; entries should be `SecretManager::list_secret_names` and `VaultManager::list_vault_names`, used by Tasks 7 and 8 respectively)

---

## Notes for the executing engineer

- **TDD discipline.** Every task above starts with a failing test. Resist writing implementation before the test fails for the right reason. If a test passes immediately, the test is wrong — make it stricter.
- **Commit per task.** Each task ends with one commit. Don't roll up multiple tasks into one commit; the trail matters for review and bisect.
- **No surprise refactors.** If you spot pre-existing code that's wrong but unrelated to this plan, write it down for a follow-up — don't fix it in these commits.
- **Read methods used (for the phase-2 trait checklist):**
  - `SecretManager::list_secret_names` (Task 7) — verify the actual method name when wiring; substitute as needed.
  - `VaultManager::list_vault_names` (Task 8) — same.
  - These are the only new read-surface usages introduced by this plan.
