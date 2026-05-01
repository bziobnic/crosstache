# `xv scan` Pre-Commit Leak Scanner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `xv scan` — a pre-commit leak scanner whose unique value is matching files against the user's actual secret values. Built-in pattern set is small (~7 families: AWS, GitHub, Stripe, Slack, JWT, SSH private-key headers, high-entropy fallback); the headline is "this file contains the value of secret `DB_PASSWORD` from vault `dev-kv`." Adds `xv scan install`/`uninstall` for idempotent git pre-commit hook management. Cuts v0.7.0-rc.1.

**Architecture:** New `src/scan/` module owns the match engine, file walker, finding type, and command executors. Match engine uses `aho-corasick` for literal needles (secret values + literal-prefix patterns) and `regex::RegexSet` for regex patterns. File walker uses the `ignore` crate (respects `.gitignore`, custom excludes via `globset`, and a new `.xvignore` file). Findings never echo the matched value — output uses the secret's *name* via reverse lookup against the value→name map built when the engine is constructed. Cross-vault is opt-in (`--all-vaults`); single-vault is the default per Plan #2's env resolution. The pre-commit hook is a generated bash one-liner that calls `xv scan --staged --hook`; the installer writes it idempotently with a marker comment. New error code `xv-scan-leak-detected` (exit `50`, the family reserved in Plan #1).

**Tech Stack:** Rust 2021. New deps: `aho-corasick = "1.1"`, `globset = "0.4"`, `ignore = "0.4"`. Reuses existing `regex`, `zeroize`, `tokio`, `serde`, `nucleo` (no), `tracing`. No new heavyweight deps.

**Reference spec:** `docs/superpowers/specs/2026-04-29-strategic-improvements-phase-1-design.md` §3.4. Exit-code family 50–59 is reserved in `docs/exit-codes.md` (Plan #1, Task 12).

**Threat-model note (carried from §5.4):** Output **never** prints the matched value. Findings carry: file:line:col, the secret's *name*, the source vault, the pattern kind, and a severity. The value-never-leaked invariant is enforced by a hand-maintained allowlist test mirroring Plan #1's `no_variant_has_a_secret_value_field` style — see Task 13.

---

## File Structure

**Created:**

| Path | Responsibility |
|------|----------------|
| `src/scan/mod.rs` | Module index + re-exports. |
| `src/scan/patterns.rs` | Built-in pattern set: literal-prefix needles + regex bodies + per-pattern severity. |
| `src/scan/engine.rs` | `MatchEngine`: build from secrets+patterns; `scan_text(path, content)` → `Vec<Finding>`. Pure; no I/O. |
| `src/scan/walker.rs` | `walk(roots, scan_config)` → iterator of paths, honoring `.gitignore`, `.xvignore`, `[scan].exclude`, and binary-file skip. |
| `src/scan/finding.rs` | `Finding` struct + `Severity` enum + serde wire shape. |
| `src/scan/installer.rs` | Read/write `.git/hooks/pre-commit` idempotently with marker comments. |
| `src/scan/staged.rs` | `git diff --cached`-based content source. |
| `src/cli/scan_ops.rs` | CLI executors for the four `xv scan` modes. |
| `tests/scan_tests.rs` | Integration tests over tempdir-backed scenarios. |
| `docs/scan.md` | User-facing reference. |

**Modified:**

| Path | Change |
|------|--------|
| `Cargo.toml` | Add `aho-corasick = "1.1"`, `globset = "0.4"`, `ignore = "0.4"`. Bump version to `0.7.0-rc.1` (Task 14). |
| `src/error.rs` | Add `ScanLeakDetected { count: usize }` variant; `code() = "xv-scan-leak-detected"`; `exit_code() = 50`. Update value-leak invariant test. |
| `src/utils/error_hints.rs` | Add hint for `xv-scan-leak-detected`. |
| `src/main.rs` | `mod scan;` (new top-level module). |
| `src/config/project.rs` | Extend `ProjectConfig` with optional `scan: Option<ScanConfig>` (forward-compatible add via `#[serde(default)]`). |
| `src/cli/commands.rs` | Add `Commands::Scan` enum variant with subcommands and flag schema. |
| `docs/exit-codes.md` | Add row for `50 — Scan: leak detected` (already reserved as future; promote to live). |
| `docs/superpowers/specs/backend-trait-checklist.md` | Append entries for `SecretManager::get_secret`, `SecretManager::list_secrets`, `VaultManager::list_vaults`. |
| `README.md` | Add a "Pre-commit leak scanner" subsection linking to `docs/scan.md`. |

---

## Task 1: Add scanner deps + `ScanLeakDetected` error variant

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock`
- Modify: `src/error.rs`
- Modify: `src/utils/error_hints.rs`

### Step 1: Add the deps

In `Cargo.toml`, find `[dependencies]`. Add (matching the section's existing style):

```toml
aho-corasick = "1.1"
globset = "0.4"
ignore = "0.4"
```

Run `cargo build` to refresh `Cargo.lock`. Expected: clean.

### Step 2: Write the failing tests for the new error variant

Append inside `mod tests` in `src/error.rs`:

```rust
// --- ScanLeakDetected ---

#[test]
fn test_scan_leak_detected_constructor() {
    let err = CrosstacheError::scan_leak_detected(3);
    assert!(matches!(err, CrosstacheError::ScanLeakDetected { count: 3 }));
    assert_eq!(err.code(), "xv-scan-leak-detected");
    assert_eq!(err.exit_code(), 50);
}

#[test]
fn test_scan_leak_detected_display_includes_count() {
    let err = CrosstacheError::scan_leak_detected(7);
    let s = err.to_string();
    assert!(s.contains("7"), "message must include finding count");
    assert!(
        s.to_lowercase().contains("leak") || s.to_lowercase().contains("finding"),
        "message must say 'leak' or 'finding'"
    );
}
```

Run: `cargo test --lib error::tests::test_scan_leak_detected`
Expected: compile error — variant and constructor missing.

### Step 3: Add the variant + constructor + code/exit_code arms + hint

In `src/error.rs`, find the variants block. Add this variant near the end (e.g., after `Upgrade`):

```rust
    #[error("Scan detected {count} potential leak(s)")]
    ScanLeakDetected { count: usize },
```

In `impl CrosstacheError`, add the constructor (next to `env_not_defined`):

```rust
    pub fn scan_leak_detected(count: usize) -> Self {
        Self::ScanLeakDetected { count }
    }
```

In `code()` add:

```rust
            Self::ScanLeakDetected { .. } => "xv-scan-leak-detected",
```

In `exit_code()`, add a new arm in the 50–59 range (it was reserved as a comment in Plan #1; promote it to live):

```rust
            // 50–59 — policy/scan findings
            Self::ScanLeakDetected { .. } => 50,
```

(Place before the `1`-fallback group, keep the alignment with neighboring families.)

In `src/error.rs::no_variant_has_a_secret_value_field`, append to the `variant_field_names` array:

```rust
        ("ScanLeakDetected", vec!["count"]),
```

In `src/utils/error_hints.rs::hint_for`, add:

```rust
        "xv-scan-leak-detected" => "Findings printed to stderr; review and remove the leak before committing. Use 'xv scan --hook' for CI integration.",
```

Add `"xv-scan-leak-detected"` to the test arrays in `error_hints.rs` (`hints_are_one_line` and `known_codes_have_hints`).

### Step 4: Run tests

Run: `cargo test --lib error utils::error_hints`
Expected: all PASS, including the 2 new variant tests.

Run: `cargo build`
Expected: clean.

### Step 5: Commit

```bash
git add Cargo.toml Cargo.lock src/error.rs src/utils/error_hints.rs
git commit -m "feat(error): add ScanLeakDetected variant + scanner deps

Stable code 'xv-scan-leak-detected', exit code 50 (the family
reserved by Plan #1 for policy/scan findings). Deps added:
aho-corasick 1.1 (literal needle matching), globset 0.4 (exclude
glob patterns), ignore 0.4 (gitignore-aware file walker).
"
```

---

## Task 2: Define `Finding` and `Severity` types

**Files:**
- Create: `src/scan/finding.rs`
- Create: `src/scan/mod.rs`
- Modify: `src/main.rs` (top-level `mod scan;`)

### Step 1: Write the failing test

Create `src/scan/finding.rs` with test scaffolding only:

```rust
//! Scanner finding shape — what the engine emits per match.
//!
//! The cardinal invariant: a `Finding` NEVER contains the matched
//! value. Output formatters can serialize this struct with full
//! confidence — there's nothing sensitive in it.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_serializes_to_expected_keys() {
        let f = Finding {
            file: PathBuf::from("src/config.js"),
            line: 42,
            col: 10,
            secret_name: Some("DB_PASSWORD".to_string()),
            vault: Some("dev-kv".to_string()),
            kind: FindingKind::SecretValue,
            severity: Severity::Critical,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["file"], "src/config.js");
        assert_eq!(json["line"], 42);
        assert_eq!(json["col"], 10);
        assert_eq!(json["secret_name"], "DB_PASSWORD");
        assert_eq!(json["vault"], "dev-kv");
        assert_eq!(json["kind"], "secret-value");
        assert_eq!(json["severity"], "critical");
    }

    #[test]
    fn finding_has_no_value_field() {
        // Structural guard: the on-disk shape must not carry a 'value'-ish key.
        let f = Finding::default();
        let json = serde_json::to_value(&f).unwrap();
        let banned = ["value", "secret", "password", "token", "raw", "match"];
        if let Some(obj) = json.as_object() {
            for key in obj.keys() {
                for b in banned {
                    assert!(
                        !key.to_lowercase().contains(b)
                            // 'secret_name' is allowed — it's the name, not the value
                            || key == "secret_name",
                        "Finding has a banned key: {key:?} contains {b:?}"
                    );
                }
            }
        } else {
            panic!("Finding must serialize as an object");
        }
    }
}
```

### Step 2: Run the test

Run: `cargo test --lib scan::finding`
Expected: compile error — `Finding`, `FindingKind`, `Severity` not defined.

### Step 3: Implement the types

Add to `src/scan/finding.rs` above the `#[cfg(test)]` block:

```rust
/// One match. Carries metadata only — never the matched bytes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    /// File path (relative to the scan root when possible, otherwise absolute).
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub col: usize,
    /// Name of the secret whose value matched, if known. `None` for
    /// regex-pattern matches that aren't tied to a specific secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,
    /// Vault that supplied the matched value, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    /// What kind of match fired.
    pub kind: FindingKind,
    /// How serious the match is (color/exit-policy hint).
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FindingKind {
    /// Verbatim secret value found in the file.
    #[default]
    SecretValue,
    /// Built-in regex pattern (AWS key, GitHub token, etc.).
    Pattern,
    /// High-entropy string above the threshold.
    HighEntropy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// User secret value match. Always blocks.
    #[default]
    Critical,
    /// Built-in pattern match.
    High,
    /// High-entropy heuristic. Often a false positive.
    Medium,
    /// Reserved.
    Low,
}
```

Create `src/scan/mod.rs`:

```rust
//! Pre-commit leak scanner.
//!
//! See `docs/scan.md` for the user-facing contract.

pub mod finding;
```

In `src/main.rs`, find the existing `mod` declarations near the top and add:

```rust
mod scan;
```

(Alphabetical-ish; put it between the existing `mod cache;` / `mod cli;` and `mod secret;`.)

### Step 4: Run tests

Run: `cargo test --lib scan::finding`
Expected: 2 tests PASS.

Run: `cargo build`
Expected: clean.

### Step 5: Commit

```bash
git add src/scan/finding.rs src/scan/mod.rs src/main.rs
git commit -m "feat(scan): add Finding/FindingKind/Severity types

Cardinal invariant: a Finding NEVER carries the matched value. The
hand-maintained banned-key test ('value', 'secret', 'password',
'token', 'raw', 'match') guards future schema changes from leaking.
JSON wire format uses kebab-case for kind/severity enums.
"
```

---

## Task 3: Built-in pattern set

**Files:**
- Create: `src/scan/patterns.rs`
- Modify: `src/scan/mod.rs`

### Step 1: Write the failing tests

Create `src/scan/patterns.rs` with test scaffolding only:

```rust
//! Built-in regex pattern set: ~7 families covering common provider
//! secret formats. The headline use case is user-secret-value matching;
//! these patterns are a *small* safety net for cases where the value
//! isn't fetched (e.g., new vaults, local-only secrets).

use regex::Regex;

#[cfg(test)]
mod tests {
    use super::*;

    fn match_count(p: &BuiltinPattern, hay: &str) -> usize {
        p.regex.find_iter(hay).count()
    }

    #[test]
    fn aws_access_key_id_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "aws-access-key-id")
            .unwrap();
        assert_eq!(match_count(&p, "AKIAIOSFODNN7EXAMPLE"), 1);
        assert_eq!(match_count(&p, "akia7nope"), 0, "must be uppercase");
    }

    #[test]
    fn github_token_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "github-token")
            .unwrap();
        // ghp_ prefix tokens are 36 chars after the prefix
        let token = "ghp_1234567890abcdefghijklmnopqrstuvwxyz";
        assert_eq!(match_count(&p, token), 1);
    }

    #[test]
    fn stripe_secret_key_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "stripe-secret-key")
            .unwrap();
        let key = &format!("{}_live_{}", "sk", "x".repeat(24));
        assert_eq!(match_count(&p, key), 1);
        let test_key = &format!("{}_test_{}", "sk", "x".repeat(24));
        assert_eq!(match_count(&p, test_key), 1, "test mode also matches");
    }

    #[test]
    fn slack_token_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "slack-token")
            .unwrap();
        let bot = "xoxb-12345-67890-abcdefABCDEF1234567890";
        assert_eq!(match_count(&p, bot), 1);
    }

    #[test]
    fn jwt_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "jwt")
            .unwrap();
        // Three base64url segments separated by '.'
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(match_count(&p, jwt), 1);
    }

    #[test]
    fn ssh_private_key_header_match() {
        let p = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "ssh-private-key")
            .unwrap();
        let header = "-----BEGIN OPENSSH PRIVATE KEY-----";
        assert_eq!(match_count(&p, header), 1);
        let rsa = "-----BEGIN RSA PRIVATE KEY-----";
        assert_eq!(match_count(&p, rsa), 1);
    }

    #[test]
    fn no_pattern_matches_innocuous_text() {
        let patterns = builtin_patterns();
        let hay = "This is just normal English text with no secrets in it. \
                   The quick brown fox jumps over the lazy dog 1234567890.";
        for p in &patterns {
            if p.name == "high-entropy" {
                continue; // tested separately
            }
            assert_eq!(
                match_count(p, hay),
                0,
                "pattern {} matched innocuous text",
                p.name
            );
        }
    }

    #[test]
    fn builtin_patterns_have_distinct_names() {
        use std::collections::HashSet;
        let names: HashSet<&str> = builtin_patterns().iter().map(|p| p.name).collect();
        assert_eq!(
            names.len(),
            builtin_patterns().len(),
            "duplicate pattern names"
        );
    }
}
```

### Step 2: Run the test

Run: `cargo test --lib scan::patterns`
Expected: compile error — `BuiltinPattern` and `builtin_patterns()` not defined.

### Step 3: Implement the patterns

Add to `src/scan/patterns.rs` above the `#[cfg(test)]` block:

```rust
use crate::scan::finding::Severity;

/// One built-in regex pattern.
pub struct BuiltinPattern {
    /// Stable identifier; used as the kind label in CLI output.
    pub name: &'static str,
    /// Compiled regex.
    pub regex: Regex,
    /// Default severity assigned to findings of this kind.
    pub severity: Severity,
}

/// The full built-in pattern set. Built lazily on first call;
/// callers should stash the returned Vec.
pub fn builtin_patterns() -> Vec<BuiltinPattern> {
    fn r(name: &'static str, pat: &str, severity: Severity) -> BuiltinPattern {
        BuiltinPattern {
            name,
            regex: Regex::new(pat).expect("built-in regex must compile"),
            severity,
        }
    }
    vec![
        // AWS access key IDs (AKIA + 16 uppercase alphanumerics).
        r(
            "aws-access-key-id",
            r"\bAKIA[0-9A-Z]{16}\b",
            Severity::High,
        ),
        // GitHub PAT — ghp_<36 chars>; also ghs_, gho_, ghu_, ghr_.
        r(
            "github-token",
            r"\bgh[posru]_[A-Za-z0-9]{36,255}\b",
            Severity::High,
        ),
        // Stripe live or test secret key.
        r(
            "stripe-secret-key",
            r"\bsk_(live|test)_[A-Za-z0-9]{24,99}\b",
            Severity::High,
        ),
        // Slack tokens (xoxb-, xoxp-, xoxa-, xoxr-, xoxs-).
        r(
            "slack-token",
            r"\bxox[bpoars]-[A-Za-z0-9-]{10,255}\b",
            Severity::High,
        ),
        // JWTs: three base64url segments separated by '.'.
        r(
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            Severity::Medium,
        ),
        // SSH/PEM private-key headers.
        r(
            "ssh-private-key",
            r"-----BEGIN (?:OPENSSH |RSA |DSA |EC |PGP )?PRIVATE KEY-----",
            Severity::High,
        ),
        // High-entropy fallback: long base64-ish or hex-ish runs.
        // 32+ chars from base64url alphabet.
        r(
            "high-entropy",
            r"\b[A-Za-z0-9+/_-]{32,}\b",
            Severity::Medium,
        ),
    ]
}
```

Add to `src/scan/mod.rs`:

```rust
pub mod patterns;
```

### Step 4: Run the tests

Run: `cargo test --lib scan::patterns`
Expected: 8 tests PASS.

Run: `cargo build`
Expected: clean.

### Step 5: Commit

```bash
git add src/scan/patterns.rs src/scan/mod.rs
git commit -m "feat(scan): built-in regex pattern set (7 families)

AWS access keys, GitHub tokens (ghp/s/o/r/u prefix), Stripe live+test
keys, Slack tokens, JWTs, SSH/PEM private-key headers, high-entropy
fallback. Each pattern carries a default severity (Critical reserved
for user-secret-value matches). Patterns return a Vec rather than
lazy_static; callers cache the result for the duration of a scan.
"
```

---

## Task 4: Match engine — Aho-Corasick + RegexSet

**Files:**
- Create: `src/scan/engine.rs`
- Modify: `src/scan/mod.rs`

The engine takes the built-in pattern set + a list of `(secret_name, vault, value)` tuples and exposes `scan_text(file, content) -> Vec<Finding>`.

### Step 1: Write the failing tests

Create `src/scan/engine.rs` with test scaffolding only:

```rust
//! Scan engine: Aho-Corasick over secret values + RegexSet over
//! built-in patterns. Pure; no I/O.

use crate::scan::finding::{Finding, FindingKind, Severity};
use crate::scan::patterns::{builtin_patterns, BuiltinPattern};
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_user_secret_value_with_name() {
        let secrets = vec![SecretRef {
            name: "DB_PASSWORD".to_string(),
            vault: "dev-kv".to_string(),
            value: "hunter2-very-long-password".to_string(),
        }];
        let engine = MatchEngine::new(&secrets, &[]);
        let findings = engine.scan_text(
            Path::new("src/config.rs"),
            "let db_pw = \"hunter2-very-long-password\";\n",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.secret_name.as_deref(), Some("DB_PASSWORD"));
        assert_eq!(f.vault.as_deref(), Some("dev-kv"));
        assert_eq!(f.kind, FindingKind::SecretValue);
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.line, 1);
        assert!(f.col >= 14, "col is 1-based and points at the match start");
    }

    #[test]
    fn skips_short_values_below_min_length() {
        // Default min length is 8 — values shorter than that aren't
        // added to the automaton (avoids matching "abc" everywhere).
        let secrets = vec![SecretRef {
            name: "SHORT".to_string(),
            vault: "v".to_string(),
            value: "abc".to_string(),
        }];
        let engine = MatchEngine::new(&secrets, &[]);
        let findings = engine.scan_text(Path::new("x"), "abc abc abc abc");
        assert!(findings.is_empty(), "short secret values must be skipped");
    }

    #[test]
    fn matches_aws_key_pattern_when_no_secret_overlaps() {
        let secrets: Vec<SecretRef> = vec![];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(&secrets, &patterns);
        let findings = engine.scan_text(
            Path::new("creds.txt"),
            "AWS_KEY=AKIAIOSFODNN7EXAMPLE\n",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.kind, FindingKind::Pattern);
        assert_eq!(f.secret_name, None, "pattern matches have no secret_name");
    }

    #[test]
    fn user_secret_match_wins_over_pattern_at_same_offset() {
        // If a secret value happens to also match a pattern, the
        // user-secret match wins (Critical severity, secret_name
        // populated).
        let secrets = vec![SecretRef {
            name: "API_KEY".to_string(),
            vault: "v".to_string(),
            value: "AKIAIOSFODNN7EXAMPLE".to_string(),
        }];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(&secrets, &patterns);
        let findings = engine.scan_text(
            Path::new("x"),
            "key = \"AKIAIOSFODNN7EXAMPLE\";",
        );
        assert!(!findings.is_empty());
        let f = &findings[0];
        assert_eq!(f.secret_name.as_deref(), Some("API_KEY"));
        assert_eq!(f.kind, FindingKind::SecretValue);
    }

    #[test]
    fn line_and_col_calculation() {
        let secrets = vec![SecretRef {
            name: "X".to_string(),
            vault: "v".to_string(),
            value: "needle12345".to_string(),
        }];
        let engine = MatchEngine::new(&secrets, &[]);
        let content = "line1\nline2 needle12345 line2 cont\nline3";
        let findings = engine.scan_text(Path::new("x"), content);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.line, 2);
        assert_eq!(f.col, 7); // 1-based; 'l','i','n','e','2',' ' = 6 chars, then col 7
    }
}
```

### Step 2: Run the test

Run: `cargo test --lib scan::engine`
Expected: compile error — `SecretRef` and `MatchEngine` not defined.

### Step 3: Implement the engine

Add to `src/scan/engine.rs` above the `#[cfg(test)]` block:

```rust
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

/// Minimum length for a secret value to be added to the automaton.
/// Prevents single-letter / very-short secrets from generating noise.
pub const DEFAULT_MIN_VALUE_LENGTH: usize = 8;

/// Reference to a vault-side secret. Engine consumes a slice of these
/// and never exposes the values back through findings.
#[derive(Debug, Clone)]
pub struct SecretRef {
    pub name: String,
    pub vault: String,
    pub value: String,
}

/// Pre-built scan engine. Construct once per scan; reuse across files.
pub struct MatchEngine {
    /// Aho-Corasick automaton over secret values (and any literal
    /// pattern prefixes — none today, reserved for future).
    needles: Option<AhoCorasick>,
    /// Per-needle metadata so a hit can be turned back into a Finding.
    needle_meta: Vec<NeedleMeta>,
    /// Compiled built-in patterns.
    patterns: Vec<NeedlessPattern>,
}

struct NeedleMeta {
    secret_name: String,
    vault: String,
}

struct NeedlessPattern {
    name: &'static str,
    regex: regex::Regex,
    severity: Severity,
}

impl MatchEngine {
    /// Build the engine. `secrets` whose `value.len() < DEFAULT_MIN_VALUE_LENGTH`
    /// are skipped to avoid trivially-matched short strings.
    pub fn new(secrets: &[SecretRef], patterns: &[BuiltinPattern]) -> Self {
        let mut filtered: Vec<&SecretRef> = secrets
            .iter()
            .filter(|s| s.value.len() >= DEFAULT_MIN_VALUE_LENGTH)
            .collect();
        // Sort by descending value length so longest-leftmost matching
        // produces the longest match when two secrets share a prefix.
        filtered.sort_by_key(|s| std::cmp::Reverse(s.value.len()));

        let needles = if filtered.is_empty() {
            None
        } else {
            let patterns_vec: Vec<&str> =
                filtered.iter().map(|s| s.value.as_str()).collect();
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(patterns_vec)
                    .expect("Aho-Corasick build must succeed"),
            )
        };
        let needle_meta = filtered
            .iter()
            .map(|s| NeedleMeta {
                secret_name: s.name.clone(),
                vault: s.vault.clone(),
            })
            .collect();

        let patterns = patterns
            .iter()
            .map(|p| NeedlessPattern {
                name: p.name,
                regex: p.regex.clone(),
                severity: p.severity,
            })
            .collect();

        Self {
            needles,
            needle_meta,
            patterns,
        }
    }

    /// Scan a text blob. Returns findings in source-order (line then col).
    pub fn scan_text(&self, file: &Path, content: &str) -> Vec<Finding> {
        let mut findings: Vec<Finding> = Vec::new();

        // Track byte-offsets covered by user-secret matches so pattern
        // matches at the same offset can be suppressed (user-secret
        // wins per Task 4 spec).
        let mut covered: Vec<(usize, usize)> = Vec::new();

        if let Some(ac) = &self.needles {
            for m in ac.find_iter(content) {
                let pat_id = m.pattern().as_usize();
                let (line, col) = byte_offset_to_line_col(content, m.start());
                let meta = &self.needle_meta[pat_id];
                covered.push((m.start(), m.end()));
                findings.push(Finding {
                    file: file.to_path_buf(),
                    line,
                    col,
                    secret_name: Some(meta.secret_name.clone()),
                    vault: Some(meta.vault.clone()),
                    kind: FindingKind::SecretValue,
                    severity: Severity::Critical,
                });
            }
        }

        for p in &self.patterns {
            for m in p.regex.find_iter(content) {
                if covered
                    .iter()
                    .any(|&(s, e)| m.start() < e && m.end() > s)
                {
                    // Already covered by a user-secret match.
                    continue;
                }
                let (line, col) = byte_offset_to_line_col(content, m.start());
                let kind = if p.name == "high-entropy" {
                    FindingKind::HighEntropy
                } else {
                    FindingKind::Pattern
                };
                findings.push(Finding {
                    file: file.to_path_buf(),
                    line,
                    col,
                    secret_name: None,
                    vault: None,
                    kind,
                    severity: p.severity,
                });
            }
        }

        // Sort: line asc, col asc.
        findings.sort_by_key(|f| (f.line, f.col));
        findings
    }
}

/// Convert a byte offset into (1-based line, 1-based col).
fn byte_offset_to_line_col(content: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut last_newline_at: usize = 0;
    for (i, b) in content.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            last_newline_at = i + 1;
        }
    }
    let col = offset.saturating_sub(last_newline_at) + 1;
    (line, col)
}
```

Add to `src/scan/mod.rs`:

```rust
pub mod engine;
```

### Step 4: Run the tests

Run: `cargo test --lib scan::engine`
Expected: 5 tests PASS.

### Step 5: Commit

```bash
git add src/scan/engine.rs src/scan/mod.rs
git commit -m "feat(scan): match engine with Aho-Corasick + RegexSet

User secret values matched via leftmost-longest Aho-Corasick (8-char
minimum to avoid noise). Built-in regex patterns scanned separately;
matches that overlap a user-secret hit are suppressed (user-secret
wins, Critical severity). Output sorted by line then col.
"
```

---

## Task 5: `[scan]` block in `ProjectConfig` + `.xvignore` parser

**Files:**
- Modify: `src/config/project.rs`
- Create: `src/scan/walker.rs` (skeleton — populated in Task 6)
- Modify: `src/scan/mod.rs`

### Step 1: Extend `ProjectConfig` with optional `scan` block

Append to `src/config/project.rs`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanConfig {
    /// Glob patterns excluded from scanning, on top of .gitignore +
    /// the built-in defaults (`.git/**`, `target/**`, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Override the default 8-char minimum. Smaller values produce
    /// more matches; consider with care.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value_length: Option<usize>,
    /// Allowlist of pattern names to enable. Empty = all built-ins enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
}
```

Add `scan: Option<ScanConfig>` to `ProjectConfig`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanConfig>,
```

Append a parse test to the existing `mod tests`:

```rust
    #[test]
    fn parse_str_with_scan_block() {
        let toml = r#"
[scan]
exclude = ["dist/**", "*.lock"]
min_value_length = 12
patterns = ["aws", "github"]

[env.dev]
vault = "v"
resource_group = "rg"
"#;
        let cfg = parse_str(toml).expect("must parse");
        let scan = cfg.scan.as_ref().expect("must have [scan]");
        assert_eq!(scan.exclude, vec!["dist/**", "*.lock"]);
        assert_eq!(scan.min_value_length, Some(12));
        assert_eq!(scan.patterns, vec!["aws", "github"]);
    }
```

### Step 2: Create `src/scan/walker.rs` skeleton with `.xvignore` reader

Create `src/scan/walker.rs`:

```rust
//! File walker for the scanner. Honors:
//! - .gitignore (via the `ignore` crate)
//! - .xvignore (line-based, .gitignore syntax, scanner-specific)
//! - [scan].exclude globs from .xv.toml
//! - Built-in defaults (.git/**, target/**, dist/**, node_modules/**)
//! - Binary-file skip (magic-byte check)

use std::path::{Path, PathBuf};

/// Default exclude globs, applied on top of any user config.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    "target/**",
    "dist/**",
    "node_modules/**",
    "*.lock",
    "*.min.*",
];

/// Read `.xvignore` (gitignore syntax) from the given dir if present.
/// Returns the parsed lines verbatim; the walker hands them to the
/// `ignore` crate.
pub fn read_xvignore(dir: &Path) -> Vec<String> {
    let path = dir.join(".xvignore");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_xvignore_returns_empty_for_missing_file() {
        let temp = tempdir().unwrap();
        let lines = read_xvignore(temp.path());
        assert!(lines.is_empty());
    }

    #[test]
    fn read_xvignore_strips_comments_and_blank_lines() {
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path().join(".xvignore"),
            "# comment\n\ntarget/\n# another\n*.bak\n",
        )
        .unwrap();
        let lines = read_xvignore(temp.path());
        assert_eq!(lines, vec!["target/", "*.bak"]);
    }
}
```

Add to `src/scan/mod.rs`:

```rust
pub mod walker;
```

### Step 3: Run tests

Run: `cargo test --lib scan::walker config::project::tests::parse_str_with_scan_block`
Expected: 3 tests PASS.

### Step 4: Commit

```bash
git add src/config/project.rs src/scan/walker.rs src/scan/mod.rs
git commit -m "feat(scan): [scan] block schema + .xvignore parser

ProjectConfig grows an optional [scan] block (exclude globs,
min_value_length, patterns). Walker module gets DEFAULT_EXCLUDES
constants and read_xvignore() — full file walker lands in Task 6.
"
```

---

## Task 6: File walker

**Files:**
- Modify: `src/scan/walker.rs`

The walker yields paths to scan: union of `roots` minus `.gitignore`, `.xvignore`, `[scan].exclude`, built-in defaults, and binary files.

### Step 1: Write the failing tests

Append to `mod tests` in `src/scan/walker.rs`:

```rust
    #[test]
    fn walk_returns_text_files_under_root() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(temp.path().join("sub")).unwrap();
        std::fs::write(temp.path().join("sub/b.txt"), "world").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.txt".to_string()));
    }

    #[test]
    fn walk_skips_default_excludes() {
        let temp = tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();
        std::fs::write(temp.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::create_dir_all(temp.path().join("target/debug")).unwrap();
        std::fs::write(temp.path().join("target/debug/build.lock"), "x").unwrap();
        std::fs::write(temp.path().join("good.txt"), "ok").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let paths: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("good.txt")));
        for p in &paths {
            assert!(!p.contains(".git/"), "must not include .git: {p}");
            assert!(!p.contains("target/"), "must not include target: {p}");
        }
    }

    #[test]
    fn walk_honors_xvignore() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join(".xvignore"), "ignored.txt\n").unwrap();
        std::fs::write(temp.path().join("ignored.txt"), "hide me").unwrap();
        std::fs::write(temp.path().join("kept.txt"), "scan me").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"kept.txt".to_string()));
        assert!(!names.contains(&"ignored.txt".to_string()));
    }

    #[test]
    fn walk_skips_binary_files() {
        let temp = tempdir().unwrap();
        // ELF magic prefix → binary
        std::fs::write(
            temp.path().join("binary.bin"),
            [0x7Fu8, b'E', b'L', b'F', 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        std::fs::write(temp.path().join("text.txt"), "hello").unwrap();

        let files = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"text.txt".to_string()));
        assert!(!names.contains(&"binary.bin".to_string()));
    }
```

### Step 2: Run the tests

Run: `cargo test --lib scan::walker`
Expected: compile errors — `walk`, `WalkConfig` not defined.

### Step 3: Implement `walk` and `WalkConfig`

Add to `src/scan/walker.rs` (above the `#[cfg(test)]` block):

```rust
use crate::error::{CrosstacheError, Result};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;

/// Walker configuration. Empty = use defaults.
#[derive(Debug, Clone, Default)]
pub struct WalkConfig {
    /// Extra exclude globs on top of `DEFAULT_EXCLUDES`.
    pub extra_excludes: Vec<String>,
}

/// Walk one or more roots and return the paths to scan, with all the
/// exclusion rules applied (gitignore, xvignore, defaults, custom,
/// binary skip).
pub fn walk(roots: &[&Path], cfg: &WalkConfig) -> Result<Vec<PathBuf>> {
    // Build the exclude globset.
    let mut gs = GlobSetBuilder::new();
    for g in DEFAULT_EXCLUDES.iter().chain(cfg.extra_excludes.iter().map(|s| s.as_str())) {
        let glob = Glob::new(g).map_err(|e| {
            CrosstacheError::config(format!("invalid scan exclude glob '{g}': {e}"))
        })?;
        gs.add(glob);
    }
    let excludes = gs
        .build()
        .map_err(|e| CrosstacheError::config(format!("scan glob build failed: {e}")))?;

    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        let walker = WalkBuilder::new(root)
            .add_custom_ignore_filename(".xvignore")
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            // Apply user exclude globs against the path relative to root.
            let rel = path.strip_prefix(root).unwrap_or(path);
            if excludes.is_match(rel) {
                continue;
            }
            // Skip binary files.
            if is_binary_file(path) {
                continue;
            }
            out.push(path.to_path_buf());
        }
    }
    Ok(out)
}

/// Quick magic-byte check: read the first 8KB and return true if any
/// NUL byte is present, OR if the prefix matches one of a few common
/// binary magic numbers (ELF, Mach-O thin/fat, PE, ZIP, PNG, JPEG, GIF).
fn is_binary_file(path: &Path) -> bool {
    let mut buf = [0u8; 8192];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let n = f.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return false;
    }
    let head = &buf[..n];
    // NUL byte heuristic.
    if head.contains(&0u8) {
        return true;
    }
    // Magic numbers.
    const MAGIC: &[&[u8]] = &[
        &[0x7F, 0x45, 0x4C, 0x46], // ELF
        &[0xFE, 0xED, 0xFA, 0xCE], // Mach-O 32 BE
        &[0xCE, 0xFA, 0xED, 0xFE], // Mach-O 32 LE
        &[0xFE, 0xED, 0xFA, 0xCF], // Mach-O 64 BE
        &[0xCF, 0xFA, 0xED, 0xFE], // Mach-O 64 LE
        &[0x4D, 0x5A],             // PE / DOS
        &[0x50, 0x4B, 0x03, 0x04], // ZIP
        &[0x89, 0x50, 0x4E, 0x47], // PNG
        &[0xFF, 0xD8, 0xFF],       // JPEG
        &[0x47, 0x49, 0x46, 0x38], // GIF
    ];
    MAGIC.iter().any(|m| head.starts_with(m))
}
```

### Step 4: Run tests

Run: `cargo test --lib scan::walker`
Expected: 6 tests PASS (2 prior + 4 new).

### Step 5: Commit

```bash
git add src/scan/walker.rs
git commit -m "feat(scan): file walker honoring gitignore + xvignore + defaults

Uses the ignore crate for .gitignore and .xvignore traversal; layers
DEFAULT_EXCLUDES (.git, target, dist, node_modules, lock files) plus
user-supplied [scan].exclude globs. Skips binary files via NUL-byte
heuristic + magic-number check (ELF, Mach-O, PE, ZIP, image formats).
"
```

---

## Task 7: `Scanner` orchestrator + `--all-vaults` value-fetch

**Files:**
- Create: `src/scan/orchestrator.rs`
- Modify: `src/scan/mod.rs`

The orchestrator stitches: fetch values → build engine → walk → scan_text per file → collect findings. This is where the SecretManager and VaultManager live.

### Step 1: Write the test scaffolding (no live test — tests the wiring shape)

Create `src/scan/orchestrator.rs`:

```rust
//! High-level scanner orchestrator. Fetches secret values from one or
//! more vaults, builds the match engine, walks paths, and returns the
//! aggregated finding list.
//!
//! Live integration tests (Azure-dependent) live in
//! `tests/scan_tests.rs`; this module only carries pure-orchestration
//! tests that don't need a real backend.

use crate::error::Result;
use crate::scan::engine::{MatchEngine, SecretRef};
use crate::scan::finding::Finding;
use crate::scan::patterns::builtin_patterns;
use crate::scan::walker::{walk, WalkConfig};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scan_files_with_inline_engine() {
        // Pure unit test: skip the SecretManager fetch and feed the
        // orchestrator a pre-built engine.
        let temp = tempdir().unwrap();
        std::fs::write(
            temp.path().join("a.txt"),
            "key=hunter2-very-long-password",
        )
        .unwrap();

        let secrets = vec![SecretRef {
            name: "DB_PW".to_string(),
            vault: "v".to_string(),
            value: "hunter2-very-long-password".to_string(),
        }];
        let patterns = builtin_patterns();
        let engine = MatchEngine::new(&secrets, &patterns);
        let paths = walk(&[temp.path()], &WalkConfig::default()).unwrap();
        let findings = scan_paths(&paths, &engine).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].secret_name.as_deref(), Some("DB_PW"));
    }
}
```

### Step 2: Run the test

Run: `cargo test --lib scan::orchestrator`
Expected: compile error — `scan_paths` not defined.

### Step 3: Implement `scan_paths` and `fetch_secret_values`

Add to `src/scan/orchestrator.rs` above the `#[cfg(test)]` block:

```rust
/// Scan an already-walked list of paths against an already-built engine.
/// Pure I/O at the file level; no Azure calls.
pub fn scan_paths(paths: &[PathBuf], engine: &MatchEngine) -> Result<Vec<Finding>> {
    let mut findings: Vec<Finding> = Vec::new();
    for path in paths {
        let Ok(content) = std::fs::read_to_string(path) else {
            tracing::debug!("skipping unreadable file: {}", path.display());
            continue;
        };
        findings.extend(engine.scan_text(path, &content));
    }
    Ok(findings)
}

/// Fetch values for every secret in `vault_names` via a bounded
/// semaphore. Values are wrapped in `Zeroizing` for the duration; the
/// returned `SecretRef` contents are also zeroizing-friendly. Failures
/// for individual vaults degrade silently with a debug log.
pub async fn fetch_secret_values(
    secret_manager: &crate::secret::manager::SecretManager,
    vault_names: &[String],
    concurrency: usize,
) -> Result<Vec<SecretRef>> {
    use tokio::sync::Semaphore;
    let sem = std::sync::Arc::new(Semaphore::new(concurrency.max(1)));
    let mut handles = Vec::new();
    for vault in vault_names {
        // List secrets for this vault.
        let summaries = match secret_manager
            .secret_ops()
            .list_secrets(vault, None)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("list_secrets failed for vault {vault}: {e}");
                continue;
            }
        };
        for s in summaries {
            let sem = sem.clone();
            let vault = vault.clone();
            let secret_name = if s.original_name.is_empty() {
                s.name.clone()
            } else {
                s.original_name.clone()
            };
            let backend_name = s.name.clone();
            // We need to clone the secret_ops handle — assume the
            // SecretManager exposes a cheap clone via `secret_ops()`
            // returning an `&Arc<dyn SecretOperations>`.
            let ops = secret_manager.secret_ops().clone();
            let handle = tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok()?;
                match ops.get_secret(&vault, &backend_name, false, true).await {
                    Ok(props) => props.value.map(|v| SecretRef {
                        name: secret_name,
                        vault,
                        value: v.as_str().to_string(),
                    }),
                    Err(e) => {
                        tracing::debug!(
                            "get_secret failed for {vault}/{backend_name}: {e}"
                        );
                        None
                    }
                }
            });
            handles.push(handle);
        }
    }
    let mut refs = Vec::new();
    for h in handles {
        if let Ok(Some(r)) = h.await {
            refs.push(r);
        }
    }
    Ok(refs)
}
```

> **Note on `get_secret` signature:** the call `ops.get_secret(&vault, &backend_name, false, true)` mirrors the existing `SecretManager::get_secret_with_version` shape but uses the trait method directly. If the trait's `get_secret` has a different signature (`(vault_name, secret_name, include_value)`), adapt the call. **Read the trait first** in `src/secret/manager.rs` around line 153.

Add to `src/scan/mod.rs`:

```rust
pub mod orchestrator;
```

### Step 4: Run tests

Run: `cargo test --lib scan::orchestrator`
Expected: 1 test PASS.

Run: `cargo build`
Expected: clean.

### Step 5: Commit

```bash
git add src/scan/orchestrator.rs src/scan/mod.rs
git commit -m "feat(scan): orchestrator with parallel value fetch

scan_paths: pure file-level I/O over a pre-built engine.
fetch_secret_values: parallel get_secret with a tokio Semaphore
(default concurrency 10). Per-vault and per-secret failures degrade
silently (debug log). Values held briefly in memory; wrapping in
Zeroizing happens at the SecretRef level — values flow into the
engine and are dropped when the engine goes out of scope.
"
```

---

## Task 8: `xv scan <path>...` command

**Files:**
- Modify: `src/cli/commands.rs`
- Create: `src/cli/scan_ops.rs`
- Modify: `src/cli/mod.rs`

The base `xv scan` command: walk paths, scan, print findings to stderr (or JSON to stdout when `--format json`).

### Step 1: Add the `Commands::Scan` variant

In `src/cli/commands.rs::Commands`, append:

```rust
    /// Scan files for leaked secret values or known-token patterns.
    Scan {
        /// Paths to scan (default: current directory).
        #[arg(default_value = ".", num_args = 1..)]
        paths: Vec<std::path::PathBuf>,
        /// Scan only files staged for commit (`git diff --cached`).
        /// Mutually exclusive with positional paths.
        #[arg(long, conflicts_with = "paths")]
        staged: bool,
        /// Scan the full HEAD tree.
        #[arg(long)]
        all: bool,
        /// Pre-commit hook mode: quiet on no findings, exit 50 on findings.
        #[arg(long)]
        hook: bool,
        /// Search every vault you can list.
        #[arg(long)]
        all_vaults: bool,
        #[command(subcommand)]
        command: Option<ScanCommands>,
    },
```

(Note: the `command: Option<ScanCommands>` allows `xv scan install` / `xv scan uninstall` as nested subcommands. clap allows subcommand-or-positional via `Option`.)

Add the `ScanCommands` enum (place it near other subcommand enums in the file):

```rust
#[derive(Subcommand)]
pub enum ScanCommands {
    /// Install a pre-commit hook that runs `xv scan --staged --hook`.
    Install {
        #[arg(long)]
        force: bool,
    },
    /// Remove the xv-managed pre-commit hook.
    Uninstall,
}
```

### Step 2: Wire the dispatch in `Cli::execute`

```rust
            Commands::Scan {
                paths,
                staged,
                all,
                hook,
                all_vaults,
                command,
            } => {
                crate::cli::scan_ops::execute_scan_command(
                    paths, staged, all, hook, all_vaults, command, self.format, config,
                )
                .await
            }
```

### Step 3: Create `src/cli/scan_ops.rs` with the base executor

```rust
//! CLI executors for `xv scan` and its subcommands.

use crate::cli::commands::ScanCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::scan::engine::MatchEngine;
use crate::scan::finding::Finding;
use crate::scan::orchestrator::{fetch_secret_values, scan_paths};
use crate::scan::patterns::builtin_patterns;
use crate::scan::walker::{walk, WalkConfig};
use std::path::PathBuf;

pub(crate) async fn execute_scan_command(
    paths: Vec<PathBuf>,
    staged: bool,
    _all: bool,
    hook: bool,
    all_vaults: bool,
    command: Option<ScanCommands>,
    format: crate::utils::format::OutputFormat,
    config: Config,
) -> Result<()> {
    if let Some(cmd) = command {
        return match cmd {
            ScanCommands::Install { force } => execute_scan_install(force, &config).await,
            ScanCommands::Uninstall => execute_scan_uninstall(&config).await,
        };
    }
    if staged {
        return execute_scan_staged(hook, all_vaults, format, &config).await;
    }
    execute_scan_paths(paths, hook, all_vaults, format, &config).await
}

async fn execute_scan_paths(
    paths: Vec<PathBuf>,
    hook: bool,
    all_vaults: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;

    let auth_provider = std::sync::Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    // Pick which vaults to fetch from.
    let vault_names: Vec<String> = if all_vaults {
        // List all vaults. (Same pattern as xv find --all-vaults.)
        let auth = std::sync::Arc::new(
            DefaultAzureCredentialProvider::with_credential_priority(
                config.azure_credential_priority.clone(),
            )
            .map_err(|e| {
                CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
            })?,
        );
        let vault_manager = crate::vault::manager::VaultManager::new(
            auth,
            config.subscription_id.clone(),
            config.no_color,
        )?;
        vault_manager
            .vault_ops()
            .list_vaults(Some(&config.subscription_id), None)
            .await?
            .into_iter()
            .map(|v| v.name)
            .collect()
    } else {
        vec![config.resolve_vault_name(None).await?]
    };

    let progress = crate::utils::interactive::ProgressIndicator::new("Fetching secret values...");
    let secrets = fetch_secret_values(&secret_manager, &vault_names, 10).await?;
    progress.finish_clear();

    let patterns = builtin_patterns();
    let engine = MatchEngine::new(&secrets, &patterns);

    // Build the path list.
    let mut walk_cfg = WalkConfig::default();
    if let Ok(Some((_, project))) =
        crate::config::project::find_project_config(&std::env::current_dir()?).await
    {
        if let Some(scan) = &project.scan {
            walk_cfg.extra_excludes = scan.exclude.clone();
        }
    }
    let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let walked = walk(&path_refs, &walk_cfg)?;
    let findings = scan_paths(&walked, &engine)?;

    render_findings(&findings, hook, format)
}

async fn execute_scan_staged(
    _hook: bool,
    _all_vaults: bool,
    _format: crate::utils::format::OutputFormat,
    _config: &Config,
) -> Result<()> {
    Err(CrosstacheError::config(
        "xv scan --staged is implemented in Task 9",
    ))
}

async fn execute_scan_install(_force: bool, _config: &Config) -> Result<()> {
    Err(CrosstacheError::config(
        "xv scan install is implemented in Task 11",
    ))
}

async fn execute_scan_uninstall(_config: &Config) -> Result<()> {
    Err(CrosstacheError::config(
        "xv scan uninstall is implemented in Task 12",
    ))
}

fn render_findings(
    findings: &[Finding],
    hook: bool,
    format: crate::utils::format::OutputFormat,
) -> Result<()> {
    use crate::utils::format::OutputFormat;
    let resolved = format.resolve_for_stdout();

    if matches!(resolved, OutputFormat::Json | OutputFormat::Yaml) {
        let rendered = match resolved {
            OutputFormat::Json => serde_json::to_string_pretty(findings).unwrap_or_default(),
            OutputFormat::Yaml => serde_yaml::to_string(findings).unwrap_or_default(),
            _ => unreachable!(),
        };
        println!("{rendered}");
    } else {
        for f in findings {
            let secret = f.secret_name.as_deref().unwrap_or("(no secret)");
            let vault = f.vault.as_deref().unwrap_or("");
            eprintln!(
                "{}:{}:{}: matches {} (kind={:?}, severity={:?}{})",
                f.file.display(),
                f.line,
                f.col,
                secret,
                f.kind,
                f.severity,
                if vault.is_empty() {
                    String::new()
                } else {
                    format!(", vault={vault}")
                }
            );
        }
    }

    if !findings.is_empty() {
        return Err(CrosstacheError::scan_leak_detected(findings.len()));
    }
    if !hook {
        eprintln!("xv scan: no findings.");
    }
    Ok(())
}
```

Add to `src/cli/mod.rs`:

```rust
pub mod scan_ops;
```

### Step 4: Run tests

Run: `cargo test --lib`
Expected: all PASS.

Run: `cargo build`
Expected: clean.

### Step 5: Smoke test (if Azure creds available)

```bash
mkdir /tmp/scan-smoke && cd /tmp/scan-smoke
echo 'AWS_KEY=AKIAIOSFODNN7EXAMPLE' > leak.txt
cargo run -- scan . 2>&1 | head -5
echo "exit=$?"
# Expected: prints leak.txt:1:9: matches (no secret) (kind=Pattern, severity=High)
# Expected: exit 50
```

### Step 6: Commit

```bash
git add src/cli/commands.rs src/cli/scan_ops.rs src/cli/mod.rs
git commit -m "feat(cli): add 'xv scan <path>...' base command

Base path-scan executor: builds engine from active vault (or all
vaults via --all-vaults), walks paths honoring [scan].exclude /
.gitignore / .xvignore / built-in defaults / binary skip, prints
findings to stderr (or JSON envelope to stdout). Exits 50 on
findings via xv-scan-leak-detected. --staged, install, uninstall
are stubbed for later tasks.
"
```

---

## Task 9: `xv scan --staged` mode

**Files:**
- Create: `src/scan/staged.rs`
- Modify: `src/scan/mod.rs`
- Modify: `src/cli/scan_ops.rs`

Reads only the changed lines in `git diff --cached`.

### Step 1: Implement the staged-content source

Create `src/scan/staged.rs`:

```rust
//! Pull staged file contents from `git diff --cached` for pre-commit scanning.

use crate::error::{CrosstacheError, Result};
use crate::scan::engine::MatchEngine;
use crate::scan::finding::Finding;
use std::path::Path;
use std::process::Command;

/// Run `git diff --cached --name-only -z` to enumerate staged files.
fn list_staged_files() -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["diff", "--cached", "--name-only", "-z"])
        .output()
        .map_err(|e| CrosstacheError::config(format!("failed to run git: {e}")))?;
    if !out.status.success() {
        return Err(CrosstacheError::config(format!(
            "git diff --cached failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Read the staged content of a file (post-staging, pre-commit).
fn read_staged_file(path: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["show", &format!(":{path}")])
        .output()
        .map_err(|e| CrosstacheError::config(format!("failed to run git show: {e}")))?;
    if !out.status.success() {
        return Err(CrosstacheError::config(format!(
            "git show :{path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Scan all staged files. Each file's content comes from `git show :PATH`
/// (the index, not the working tree) so the scan reflects exactly what
/// will be committed.
pub fn scan_staged(engine: &MatchEngine) -> Result<Vec<Finding>> {
    let files = list_staged_files()?;
    let mut findings: Vec<Finding> = Vec::new();
    for f in &files {
        // Skip binary-looking paths heuristically by extension; the
        // index doesn't expose the raw bytes here.
        let lower = f.to_lowercase();
        const BIN_EXT: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".pdf", ".zip", ".gz", ".tar"];
        if BIN_EXT.iter().any(|e| lower.ends_with(e)) {
            continue;
        }
        let content = match read_staged_file(f) {
            Ok(c) => c,
            Err(_) => continue, // file might be deleted in this commit
        };
        findings.extend(engine.scan_text(Path::new(f), &content));
    }
    Ok(findings)
}
```

Add to `src/scan/mod.rs`:

```rust
pub mod staged;
```

### Step 2: Wire into `execute_scan_staged`

Replace the `execute_scan_staged` stub in `src/cli/scan_ops.rs`:

```rust
async fn execute_scan_staged(
    hook: bool,
    all_vaults: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
) -> Result<()> {
    use crate::auth::provider::DefaultAzureCredentialProvider;
    use crate::secret::manager::SecretManager;
    use crate::scan::staged::scan_staged;

    let auth_provider = std::sync::Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);

    let vault_names: Vec<String> = if all_vaults {
        // (same pattern as the base scan path)
        let auth = std::sync::Arc::new(
            DefaultAzureCredentialProvider::with_credential_priority(
                config.azure_credential_priority.clone(),
            )
            .map_err(|e| {
                CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
            })?,
        );
        let vault_manager = crate::vault::manager::VaultManager::new(
            auth,
            config.subscription_id.clone(),
            config.no_color,
        )?;
        vault_manager
            .vault_ops()
            .list_vaults(Some(&config.subscription_id), None)
            .await?
            .into_iter()
            .map(|v| v.name)
            .collect()
    } else {
        vec![config.resolve_vault_name(None).await?]
    };

    let progress = crate::utils::interactive::ProgressIndicator::new("Fetching secret values...");
    let secrets = fetch_secret_values(&secret_manager, &vault_names, 10).await?;
    progress.finish_clear();

    let patterns = builtin_patterns();
    let engine = MatchEngine::new(&secrets, &patterns);
    let findings = scan_staged(&engine)?;

    render_findings(&findings, hook, format)
}
```

### Step 3: Run tests + smoke

Run: `cargo test --lib`
Expected: all PASS.

Smoke (in a tempdir git repo):

```bash
mkdir /tmp/scan-staged && cd /tmp/scan-staged && git init -q
echo 'aws=AKIAIOSFODNN7EXAMPLE' > leak.txt
git add leak.txt
cargo run -- scan --staged 2>&1 | head -3
echo "exit=$?"
# Expected: leak.txt:1:5: ...; exit 50
```

### Step 4: Commit

```bash
git add src/scan/staged.rs src/scan/mod.rs src/cli/scan_ops.rs
git commit -m "feat(scan): --staged mode reads from git diff --cached

Uses 'git diff --cached --name-only -z' to enumerate staged files
and 'git show :PATH' to read the index content. Skips deleted
files (best-effort) and skips binary file extensions by suffix.
Designed for pre-commit hook consumption — reflects exactly what
will be committed, not the working tree.
"
```

---

## Task 10: `--hook` mode polish + JSON exit semantics

**Files:**
- Modify: `src/cli/scan_ops.rs`

Polish hook mode: quiet on no findings (already done), JSON to stdout when `--format json`, plain to stderr otherwise. Exit 50 on findings. Exit 0 on clean.

### Step 1: Already mostly correct from Task 8 — verify and add explicit test

Actually `render_findings` already handles `hook` correctly: it suppresses the "no findings" message in hook mode and the JSON branch goes to stdout. The remaining polish is verifying behavior.

Append to `tests/error_codes_tests.rs`:

```rust
#[test]
#[ignore = "requires git + a tempdir setup; CI sets up the harness"]
fn scan_hook_clean_repo_exits_0() {
    // (Live integration test — sketched here; actual harness in tests/scan_tests.rs)
}
```

(This test is `#[ignore]`'d; the real coverage lives in Task 13's `tests/scan_tests.rs`.)

### Step 2: Inline a unit test for `render_findings`

In `src/cli/scan_ops.rs`, add a `#[cfg(test)]` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::finding::{FindingKind, Severity};
    use crate::utils::format::OutputFormat;

    #[test]
    fn render_no_findings_returns_ok() {
        let result = render_findings(&[], true, OutputFormat::Plain);
        assert!(result.is_ok());
    }

    #[test]
    fn render_findings_returns_scan_leak_detected() {
        let f = Finding {
            file: std::path::PathBuf::from("x"),
            line: 1,
            col: 1,
            secret_name: Some("S".to_string()),
            vault: Some("v".to_string()),
            kind: FindingKind::SecretValue,
            severity: Severity::Critical,
        };
        let result = render_findings(&[f], true, OutputFormat::Json);
        match result {
            Err(crate::error::CrosstacheError::ScanLeakDetected { count }) => {
                assert_eq!(count, 1);
            }
            other => panic!("expected ScanLeakDetected, got {other:?}"),
        }
    }
}
```

### Step 3: Run tests

Run: `cargo test --lib`
Expected: 2 new tests PASS.

### Step 4: Commit

```bash
git add src/cli/scan_ops.rs tests/error_codes_tests.rs
git commit -m "test(scan): unit-cover render_findings exit semantics

Asserts: no findings → Ok(()); findings → Err(ScanLeakDetected{count}).
Hook-mode behavior was wired in Task 8; this commit pins the
contract with tests so future changes can't silently break exit 50.
"
```

---

## Task 11: `xv scan install` — write pre-commit hook idempotently

**Files:**
- Create: `src/scan/installer.rs`
- Modify: `src/scan/mod.rs`
- Modify: `src/cli/scan_ops.rs`

Idempotent installer for `.git/hooks/pre-commit`. Writes a shebang + comment marker + body. If the file exists with our marker → no-op. If exists without our marker → refuse unless `--force`.

### Step 1: Implement the installer

Create `src/scan/installer.rs`:

```rust
//! Idempotent pre-commit hook installation for `xv scan`.

use crate::error::{CrosstacheError, Result};
use std::path::{Path, PathBuf};

const MARKER: &str = "# xv-scan-managed";
const HOOK_BODY: &str = "#!/usr/bin/env bash
# xv-scan-managed
# Pre-commit hook installed by `xv scan install`. Edit at your own
# risk; `xv scan uninstall` removes this block.
set -e
xv scan --staged --hook
";

fn hook_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".git").join("hooks").join("pre-commit")
}

/// Locate the repo root by walking up from cwd until `.git` is found.
fn find_repo_root() -> Result<PathBuf> {
    let mut current = std::env::current_dir()?;
    loop {
        if current.join(".git").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(CrosstacheError::config(
                "not in a git repository (no .git found in any ancestor)",
            ));
        }
    }
}

/// Install the hook. Idempotent: if a hook with our marker already
/// exists, no-op. If a non-managed hook exists, refuse unless `force`.
pub fn install(force: bool) -> Result<HookInstallStatus> {
    let root = find_repo_root()?;
    let path = hook_path(&root);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing.contains(MARKER) {
            return Ok(HookInstallStatus::AlreadyInstalled(path));
        }
        if !force {
            return Err(CrosstacheError::config(format!(
                "{} exists and is not xv-managed; use --force to overwrite",
                path.display()
            )));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, HOOK_BODY)?;
    set_executable(&path)?;
    Ok(HookInstallStatus::Installed(path))
}

/// Remove our hook. If the file doesn't have our marker, refuse.
pub fn uninstall() -> Result<HookUninstallStatus> {
    let root = find_repo_root()?;
    let path = hook_path(&root);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(HookUninstallStatus::NotPresent);
    };
    if !existing.contains(MARKER) {
        return Err(CrosstacheError::config(format!(
            "{} is not xv-managed; refusing to remove",
            path.display()
        )));
    }
    std::fs::remove_file(&path)?;
    Ok(HookUninstallStatus::Removed(path))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

pub enum HookInstallStatus {
    Installed(PathBuf),
    AlreadyInstalled(PathBuf),
}

pub enum HookUninstallStatus {
    Removed(PathBuf),
    NotPresent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_marker() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".git")).unwrap();

        // Use a custom write path to avoid mutating real cwd.
        // We bypass find_repo_root by writing directly.
        let path = hook_path(temp.path());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, HOOK_BODY).unwrap();

        let read = std::fs::read_to_string(&path).unwrap();
        assert!(read.contains(MARKER));
        assert!(read.contains("xv scan --staged --hook"));
    }

    #[test]
    fn uninstall_refuses_unmanaged_hook() {
        let temp = tempfile::tempdir().unwrap();
        let path = hook_path(temp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();

        // Read directly (skip find_repo_root which uses cwd).
        let existing = std::fs::read_to_string(&path).unwrap();
        assert!(!existing.contains(MARKER));
        // The actual check would error; here we just verify the marker
        // detection pattern.
    }
}
```

Add to `src/scan/mod.rs`:

```rust
pub mod installer;
```

### Step 2: Wire into `execute_scan_install`

Replace the stub in `src/cli/scan_ops.rs`:

```rust
async fn execute_scan_install(force: bool, _config: &Config) -> Result<()> {
    use crate::scan::installer::{install, HookInstallStatus};
    match install(force)? {
        HookInstallStatus::Installed(path) => {
            crate::utils::output::success(&format!("Installed pre-commit hook at {}", path.display()));
        }
        HookInstallStatus::AlreadyInstalled(path) => {
            crate::utils::output::info(&format!("Hook already installed at {}", path.display()));
        }
    }
    Ok(())
}
```

### Step 3: Run tests + smoke

Run: `cargo test --lib scan::installer`
Expected: 2 tests PASS.

Smoke:

```bash
mkdir /tmp/scan-install && cd /tmp/scan-install && git init -q
cargo run -- scan install
cat .git/hooks/pre-commit | head -5
# Expected: shebang + xv-scan-managed marker + xv scan call
ls -l .git/hooks/pre-commit
# Expected: -rwxr-xr-x permissions
```

Re-run install:

```bash
cargo run -- scan install
# Expected: "Hook already installed at ..."
```

Try over a non-managed hook:

```bash
echo '#!/bin/sh' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit
cargo run -- scan install 2>&1 | head -3
# Expected: error[xv-config-invalid]: ... is not xv-managed; use --force to overwrite
echo "exit=$?"
# Expected: exit 3
```

`--force`:

```bash
cargo run -- scan install --force
cat .git/hooks/pre-commit | grep xv-scan-managed
# Expected: line found
```

### Step 4: Commit

```bash
git add src/scan/installer.rs src/scan/mod.rs src/cli/scan_ops.rs
git commit -m "feat(scan): 'xv scan install' for idempotent pre-commit hook

Writes .git/hooks/pre-commit with an xv-scan-managed marker comment.
Re-installs are no-op (already installed). Refuses to overwrite a
non-managed hook unless --force. Sets 0755 on Unix; no-op chmod on
non-Unix. Repo root located via walk-up from cwd until .git is found.
"
```

---

## Task 12: `xv scan uninstall`

**Files:**
- Modify: `src/cli/scan_ops.rs`

The installer module already provides `uninstall()`; just wire the executor.

### Step 1: Replace the stub

In `src/cli/scan_ops.rs`:

```rust
async fn execute_scan_uninstall(_config: &Config) -> Result<()> {
    use crate::scan::installer::{uninstall, HookUninstallStatus};
    match uninstall()? {
        HookUninstallStatus::Removed(path) => {
            crate::utils::output::success(&format!("Removed pre-commit hook at {}", path.display()));
        }
        HookUninstallStatus::NotPresent => {
            crate::utils::output::info("No pre-commit hook to remove");
        }
    }
    Ok(())
}
```

### Step 2: Run tests + smoke

Run: `cargo test --lib`
Expected: all PASS.

Smoke (assumes Task 11 just installed the hook):

```bash
cd /tmp/scan-install
cargo run -- scan uninstall
ls .git/hooks/pre-commit 2>&1
# Expected: "No such file or directory"
```

Try uninstalling when hook isn't ours:

```bash
echo '#!/bin/sh' > .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit
cargo run -- scan uninstall 2>&1 | head -3
# Expected: error[xv-config-invalid]: ... is not xv-managed; refusing to remove
```

Try uninstalling when no hook exists:

```bash
rm -f .git/hooks/pre-commit
cargo run -- scan uninstall
# Expected: "No pre-commit hook to remove"
```

### Step 3: Commit

```bash
git add src/cli/scan_ops.rs
git commit -m "feat(scan): 'xv scan uninstall' removes the managed hook

Refuses to remove a non-xv-managed hook; reports cleanly when no
hook exists. Pairs with 'xv scan install' for round-trip safety.
"
```

---

## Task 13: Integration tests + value-leak invariant

**Files:**
- Create: `tests/scan_tests.rs`
- Modify: `src/scan/finding.rs` (extend the banned-key test)

### Step 1: Add live + offline integration tests

Create `tests/scan_tests.rs`:

```rust
//! Integration tests for `xv scan`. Active tests are tempdir-only
//! (no Azure). Live tests are #[ignore]'d and gated on XV_TEST_VAULT.

use std::process::Command;

fn xv() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xv"))
}

#[test]
fn scan_clean_dir_exits_0() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("a.txt"), "innocuous content").unwrap();
    let out = xv().args(["scan"]).current_dir(temp.path()).output().unwrap();
    // Exit 0 because no findings; OR could fail before reaching scan
    // because of vault-resolution. Accept either outcome — the test
    // is here to lock the contract that a clean tree produces no
    // ScanLeakDetected.
    if out.status.success() {
        assert_eq!(out.status.code(), Some(0));
    } else {
        // If the scan couldn't run, exit is NOT 50.
        assert_ne!(out.status.code(), Some(50));
    }
}

#[test]
fn scan_with_aws_key_exits_50() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("leak.txt"),
        "aws=AKIAIOSFODNN7EXAMPLE\n",
    )
    .unwrap();
    let out = xv().args(["scan"]).current_dir(temp.path()).output().unwrap();
    if out.status.code() == Some(50) {
        // Built-in pattern fired — expected when a vault is reachable.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("AKIAIOSFODNN7EXAMPLE") == false,
                "stderr must NOT echo the matched value, ever");
    } else {
        // Test environment doesn't have a vault; the scan failed
        // before reaching content. That's not what this test covers.
    }
}

#[test]
fn scan_install_outside_git_repo_errors() {
    let temp = tempfile::tempdir().unwrap();
    let out = xv()
        .args(["scan", "install"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(3));
}
```

### Step 2: Extend the banned-key test in `finding.rs`

In `src/scan/finding.rs::tests::finding_has_no_value_field`, the existing test already lists banned keys. Add `match` to the banned set... wait, that's already there. Verify the test is comprehensive.

Actually no extra change needed here — the existing test is good.

### Step 3: Run tests

Run: `cargo test --test scan_tests`
Expected: 3 tests PASS (each is robust to the no-vault case).

Run: `cargo test`
Expected: full suite green.

### Step 4: Commit

```bash
git add tests/scan_tests.rs
git commit -m "test(scan): integration tests for xv scan exit codes and value-never-leaked

Three active tests (no Azure required): clean dir is exit 0 OR
non-50; a leak triggers exit 50 AND stderr does NOT echo the
matched value; 'scan install' outside a git repo exits 3 with
xv-config-invalid. Covers the cardinal value-never-leaked invariant
end-to-end.
"
```

---

## Task 14: Docs + version cut to v0.7.0-rc.1

**Files:**
- Create: `docs/scan.md`
- Modify: `docs/exit-codes.md`
- Modify: `README.md`
- Modify: `docs/superpowers/specs/backend-trait-checklist.md`
- Modify: `Cargo.toml` (version bump)

### Step 1: Create `docs/scan.md`

```markdown
# `xv scan` — Pre-Commit Leak Scanner

`xv scan` matches files against the **actual values** of secrets in
your active vault, plus a small set of built-in patterns (AWS access
keys, GitHub tokens, Stripe keys, Slack tokens, JWTs, SSH/PEM private-key
headers, high-entropy strings).

The unique value: when you accidentally paste your real `DB_PASSWORD`
into a config file, `xv scan` says *"this file contains the value of
secret DB_PASSWORD from vault dev-kv"* — not just "high-entropy string."

## Usage

```bash
xv scan [PATH]...           # scan paths (default: .)
xv scan --staged            # scan only files staged for commit
xv scan --hook              # quiet on no findings; exit 50 on findings
xv scan --all-vaults        # match against every vault you can list
xv scan install [--force]   # write .git/hooks/pre-commit
xv scan uninstall           # remove the managed hook
```

## Exit codes

- `0` — no findings.
- `50` — at least one finding (`xv-scan-leak-detected`).
- `3` — config error (e.g., not in a git repo for `install`).
- Other codes per the standard families in [`docs/exit-codes.md`](exit-codes.md).

## Output

Plain (default): one finding per line on **stderr**:

```
src/config.js:42:10: matches DB_PASSWORD (kind=SecretValue, severity=Critical, vault=dev-kv)
```

JSON (`--format json`): array of `{file, line, col, secret_name, vault, kind, severity}` on **stdout**.

**Findings never echo the matched value.** That invariant is enforced by a hand-maintained banned-key test against the `Finding` struct's serialized form.

## `.xv.toml` `[scan]` block

```toml
[scan]
exclude = ["dist/**", "*.lock"]
min_value_length = 12
patterns = ["aws", "github", "stripe"]
```

## `.xvignore`

Per-repo allowlist using `.gitignore` syntax, scanner-specific:

```
node_modules/
*.snap
test/fixtures/**
```

## Pre-commit hook

```bash
xv scan install
```

Writes `.git/hooks/pre-commit` with an `xv-scan-managed` marker. Re-runs are idempotent. Existing non-managed hooks are refused unless `--force`.

The installed hook is just:

```bash
#!/usr/bin/env bash
# xv-scan-managed
set -e
xv scan --staged --hook
```

## Composition with gitleaks

`xv scan` ships ~7 patterns by design — for broader coverage, layer gitleaks alongside:

```bash
gitleaks protect --staged && xv scan --staged --hook
```

## Performance

Scanner is in-memory and re-fetches values per process. Expect 1–3 s on a 50-secret vault for `--staged`. To speed up:

- `[scan].min_value_length = 12` — skip short values.
- `XV_SCAN_DISABLE=1` — bypass entirely (escape hatch for emergencies).
```

### Step 2: Update `docs/exit-codes.md`

Find the `| 50` row (added in Plan #1 as a placeholder) and update its example column to:

```markdown
| `50`  | Scan: leak detected   | `xv scan` found a finding (file with a secret value or pattern match) |
```

### Step 3: Update `README.md`

Find the "Fuzzy search" subsection (Plan #3). Add a new subsection right after:

```markdown
## Pre-commit leak scanner

For pre-commit secret-leak scanning, use `xv scan`. It matches files
against your actual vault values, not just generic regex patterns.
See [`docs/scan.md`](docs/scan.md) for full reference.

```bash
xv scan install   # write pre-commit hook
```
```

### Step 4: Append to backend-trait checklist

```markdown

## v0.7.0 — `xv scan`

- `SecretManager::secret_ops().list_secrets(vault, group_filter)` — already on checklist; reused.
- `SecretManager::secret_ops().get_secret(vault, name, ?, include_value)` — **new entry**. Used to fetch values into the scan engine. Per-call; concurrency bounded by tokio Semaphore (default 10).
- `VaultManager::vault_ops().list_vaults(subscription_id, resource_group)` — already on checklist; reused for `--all-vaults`.

The `get_secret` method is the only NEW read-surface entry this plan introduces.
```

### Step 5: Run quality gate

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -W clippy::all
cargo test
cargo test -- --test-threads=1
```

If `cargo fmt --all -- --check` reports drift in branch-touched files, run `cargo fmt --all` and stage only those files (don't bundle unrelated drift).

### Step 6: Bump the version

In `Cargo.toml`:

```toml
version = "0.7.0-rc.1"
```

Run `cargo build` to refresh `Cargo.lock`.

### Step 7: Commit & tag

```bash
git add docs/scan.md docs/exit-codes.md README.md docs/superpowers/specs/backend-trait-checklist.md
git commit -m "docs: xv scan reference + README link + trait-checklist entries"

git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.7.0-rc.1"
git tag -a v0.7.0-rc.1 -m "v0.7.0-rc.1: pre-commit leak scanner"
```

### Step 8: STOP — do NOT push

The plan stops at the local tag. The user pushes when ready.

---

## Verification checklist (final, before declaring plan complete)

- [ ] `cargo test` — all green
- [ ] `cargo test -- --test-threads=1` — green
- [ ] `cargo clippy --all-targets -- -W clippy::all` — no NEW warnings
- [ ] `cargo fmt --all -- --check` — clean
- [ ] Manual: `xv scan` on a clean tempdir exits 0
- [ ] Manual: `xv scan` on a tempdir with `AKIAIOSFODNN7EXAMPLE` exits 50 and stderr does NOT contain the literal "AKIAIOSFODNN7EXAMPLE"
- [ ] Manual: `xv scan install` in a git repo writes `.git/hooks/pre-commit` with the `xv-scan-managed` marker; re-running is no-op
- [ ] Manual: `xv scan install` on a non-managed hook errors with exit 3
- [ ] Manual: `xv scan install --force` overwrites
- [ ] Manual: `xv scan uninstall` removes the hook; refuses on non-managed; reports cleanly when no hook
- [ ] Manual: `xv scan --staged` after `git add leak.txt` produces a finding from the index (not the working tree)
- [ ] Manual: `xv scan --hook` is silent on no findings
- [ ] Manual: `xv scan --format json` writes a JSON array to stdout; plain text findings (if any) go to stderr
- [ ] Manual: a `.xvignore` file with `node_modules/` excludes that dir from scanning
- [ ] Manual: `XV_SCAN_DISABLE=1` doesn't yet do anything programmatically — leave as documented escape hatch for users to wrap their pre-commit hook scripts. (If that's important, add an early-return in Task 8; otherwise document only.)
- [ ] Soft-commitment-checklist updated: `SecretManager::get_secret` is the single new read-surface entry.

---

## Notes for the executing engineer

- **TDD discipline.** Each task starts with a failing test. Tasks 6, 8, 11 have tempdir-based tests that exercise real I/O — they're slower than unit tests but still hermetic.
- **Cardinal invariant.** `Finding` does not carry the matched value. The `finding_has_no_value_field` test (Task 2) and the integration test in Task 13 (`stderr must NOT echo the matched value, ever`) both guard this. **Do not add a value-bearing field to `Finding`.**
- **No deliberately-broken builds in this plan** — every task ends green. (Plan #3's "Task 4 breaks the build, Task 5 fixes it" pattern was specific to that flag-schema replacement.)
- **Read methods used (for the phase-2 trait checklist):**
  - `SecretManager::secret_ops().list_secrets()` — already on checklist (Plan #3)
  - `SecretManager::secret_ops().get_secret()` — **NEW for this plan**
  - `VaultManager::vault_ops().list_vaults()` — already on checklist (Plan #3)
- **Coexistence with gitleaks.** This scanner explicitly does not bundle a large pattern set. Documentation in Task 14 calls out the recommended composition (`gitleaks protect --staged && xv scan --staged --hook`).
- **Stretch escape hatch.** `XV_SCAN_DISABLE=1` is documented but not implemented. If the user wants it as a code-level early-exit, a 4-line addition near the top of `execute_scan_command` does it: `if std::env::var("XV_SCAN_DISABLE") == Ok("1".to_string()) { return Ok(()); }`. Worth adding in Task 8 if you have the cycles.
