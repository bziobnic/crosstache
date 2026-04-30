# Env Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `.xv.toml` env profiles with walk-up resolution so users can keep per-project vault/RG/group/folder defaults in a checked-in file. Adds `xv context init/envs`, extends `xv context show`, adds `--env` global flag and `XV_ENV` env var, adds new error code `xv-env-not-defined`. Foundation for v0.6.0-rc.2.

**Architecture:** New `src/config/project.rs` module owns the `.xv.toml` schema (`ProjectConfig` + `EnvProfile`), parsing, the walk-up algorithm (`find_project_config`), boundary-stop support (`.xv.boundary` file or `XV_NO_PARENT_CONFIG=1`), and active-env resolution (`XV_ENV` → `--env` flag → `default_env` → error). The existing `Config::resolve_vault_name` / `resolve_resource_group` consult the project config when no CLI flag is given; CLI flags still override. The legacy `.xv/context` JSON path keeps working for one minor with a one-time deprecation warning. New CLI commands add interactive scaffolding (`xv context init`) and visibility (`xv context envs`, extended `xv context show`).

**Tech Stack:** Rust 2021, `toml = "0.8"` (already in deps), `serde` (already in deps), `tokio` for async I/O. No new heavyweight deps. Uses Plan #1's `CrosstacheError` + `code()` + `exit_code()` infrastructure.

**Reference spec:** `docs/superpowers/specs/2026-04-29-strategic-improvements-phase-1-design.md` §3.2 (env profiles design) and §4.2 (release milestone — env profiles + errors together = v0.6.0).

---

## File Structure

**Created:**

| Path | Responsibility |
|------|----------------|
| `src/config/project.rs` | `ProjectConfig`, `EnvProfile` types; `parse()` from path; `find_project_config()` walk-up; `resolve_env()` env-selection algorithm. |
| `tests/env_profiles_tests.rs` | Integration tests over tempdir trees with `.xv.toml` at varied depths, boundary files, and env-resolution priority. |
| `docs/env-profiles.md` | User-facing reference: schema, walk-up rules, env selection priority, examples. |

**Modified:**

| Path | Change |
|------|--------|
| `src/error.rs` | Add `EnvNotDefined { name: String, available: Vec<String> }` variant. Update `code()`, `exit_code()`, `print_user_friendly_error` paths via the existing exhaustive matches. Add hint in `error_hints.rs`. Update value-leak invariant test list. |
| `src/utils/error_hints.rs` | Add `xv-env-not-defined` → "Run 'xv context envs' to see defined environments." |
| `src/config/mod.rs` | Add `pub mod project;`. |
| `src/config/context.rs` | `ContextManager::load_project_config()` helper; legacy `.xv/context` load now emits one-time deprecation warning. |
| `src/config/settings.rs` | `resolve_vault_name` and `resolve_resource_group` consult `ProjectConfig` (after CLI flag, before global context). |
| `src/cli/commands.rs` | Add `pub env: Option<String>` global flag on `Cli`. Extend `ContextCommands` with `Init { interactive: bool, env: Option<String>, vault: Option<String>, resource_group: Option<String> }` and `Envs`. |
| `src/cli/config_ops.rs` | New executors: `execute_context_init`, `execute_context_envs`, extend `execute_context_show`. |
| `src/main.rs` | Thread `cli.env` into the run path (currently only `cli.format` is captured). Print one-time cross-boundary notice to stderr when `.xv.toml` is discovered above cwd. |
| `docs/exit-codes.md` | Add row for `xv-env-not-defined` (exit 3 — config family). |
| `README.md` | Add subsection "Env profiles" linking to `docs/env-profiles.md`. |
| `Cargo.toml` | Bump version to `0.6.0-rc.2` (Task 14). |

---

## Task 1: Add `EnvNotDefined` error variant

**Files:**
- Modify: `src/error.rs`
- Modify: `src/utils/error_hints.rs`

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` in `src/error.rs` (near the other variant tests):

```rust
// --- EnvNotDefined ---

#[test]
fn test_env_not_defined_constructor() {
    let err = CrosstacheError::env_not_defined(
        "staging",
        vec!["dev".to_string(), "prod".to_string()],
    );
    assert!(matches!(
        err,
        CrosstacheError::EnvNotDefined { ref name, ref available }
            if name == "staging" && available == &vec!["dev".to_string(), "prod".to_string()]
    ));
    assert_eq!(err.code(), "xv-env-not-defined");
    assert_eq!(err.exit_code(), 3);
}

#[test]
fn test_env_not_defined_display_lists_available() {
    let err = CrosstacheError::env_not_defined(
        "staging",
        vec!["dev".to_string(), "prod".to_string()],
    );
    let s = err.to_string();
    assert!(s.contains("staging"), "message must include the missing env name");
    assert!(s.contains("dev"), "message must list 'dev' as available");
    assert!(s.contains("prod"), "message must list 'prod' as available");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib error::tests::test_env_not_defined`
Expected: compile error — `EnvNotDefined` variant and `env_not_defined` constructor missing.

- [ ] **Step 3: Add the variant**

In `src/error.rs`, find the variants block. Add this variant right after `InvalidSecretName`:

```rust
    #[error("Environment '{name}' not defined in .xv.toml; available: {}", available.join(", "))]
    EnvNotDefined {
        name: String,
        available: Vec<String>,
    },
```

The custom `#[error]` attribute uses `available.join(", ")` so the `to_string()` output includes the list — matching the second test.

- [ ] **Step 4: Add the constructor**

In `impl CrosstacheError`, add (next to `secret_not_found`):

```rust
    pub fn env_not_defined<S: Into<String>>(name: S, available: Vec<String>) -> Self {
        Self::EnvNotDefined {
            name: name.into(),
            available,
        }
    }
```

- [ ] **Step 5: Add the variant to `code()` and `exit_code()`**

In `code()`, add:

```rust
            Self::EnvNotDefined { .. } => "xv-env-not-defined",
```

In `exit_code()`, add `Self::EnvNotDefined { .. }` to the config family arm — group it with `ConfigError`/`ConfigLoadError` so it returns `3`:

```rust
            Self::ConfigError(_)
            | Self::ConfigLoadError(_)
            | Self::EnvNotDefined { .. } => 3,
```

- [ ] **Step 6: Update value-leak invariant test**

In `src/error.rs::no_variant_has_a_secret_value_field`, find the `variant_field_names` array and add:

```rust
        ("EnvNotDefined", vec!["name", "available"]),
```

(Place it alphabetically or near the other similar entries — order doesn't matter, just don't omit it.)

- [ ] **Step 7: Add the hint in `error_hints.rs`**

In `src/utils/error_hints.rs`, inside the `match code` block in `hint_for`, add:

```rust
        "xv-env-not-defined" => "Run 'xv context envs' to see defined environments.",
```

Also add `"xv-env-not-defined"` to the test array in `hints_are_one_line`:

```rust
    for code in [
        "xv-vault-not-found",
        "xv-secret-not-found",
        "xv-permission-denied",
        "xv-network-dns",
        "xv-network-timeout",
        "xv-config-invalid",
        "xv-env-not-defined",
    ] {
```

And add a known-codes assertion:

```rust
    #[test]
    fn known_codes_have_hints() {
        assert!(hint_for("xv-vault-not-found").is_some());
        assert!(hint_for("xv-secret-not-found").is_some());
        assert!(hint_for("xv-permission-denied").is_some());
        assert!(hint_for("xv-network-dns").is_some());
        assert!(hint_for("xv-config-invalid").is_some());
        assert!(hint_for("xv-env-not-defined").is_some());
    }
```

- [ ] **Step 8: Run all error tests**

Run: `cargo test --lib error`
Expected: all PASS, including the 2 new tests.

Run: `cargo test --lib utils::error_hints`
Expected: all PASS.

- [ ] **Step 9: Commit**

```bash
git add src/error.rs src/utils/error_hints.rs
git commit -m "feat(error): add EnvNotDefined variant for missing .xv.toml environments

Stable code 'xv-env-not-defined', exit code 3 (config family). Display
includes the requested env name and the list of available envs from
the resolved .xv.toml, so users can fix the typo immediately.
"
```

---

## Task 2: Define `ProjectConfig` and `EnvProfile` types

**Files:**
- Create: `src/config/project.rs`
- Modify: `src/config/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `src/config/project.rs` with test scaffolding only (NO implementation yet):

```rust
//! `.xv.toml` env profile schema and resolution.
//!
//! This module owns:
//! - The `ProjectConfig` / `EnvProfile` data shapes for the on-disk format.
//! - Parsing (`parse_str` / `parse_file`).
//! - Walk-up traversal (`find_project_config`) with `.xv.boundary` stopper
//!   and `XV_NO_PARENT_CONFIG=1` opt-out.
//! - Active-env selection (`resolve_env`) honoring `XV_ENV` > `--env` flag >
//!   `default_env` field > error.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_config_default_is_empty() {
        let c = ProjectConfig::default();
        assert!(c.envs.is_empty());
        assert_eq!(c.default_env, None);
    }

    #[test]
    fn env_profile_default_is_empty() {
        let p = EnvProfile::default();
        assert_eq!(p.vault, None);
        assert_eq!(p.resource_group, None);
        assert_eq!(p.group, None);
        assert_eq!(p.folder, None);
    }
}
```

Add to `src/config/mod.rs`. Find the existing `pub mod ...;` declarations and add (alphabetically):

```rust
pub mod project;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib config::project`
Expected: compile error — `ProjectConfig` and `EnvProfile` not defined.

- [ ] **Step 3: Define the types**

Add to `src/config/project.rs` (above the `#[cfg(test)]` block):

```rust
/// On-disk `.xv.toml` project configuration.
///
/// All non-`envs` fields use `#[serde(default)]` so future fields
/// (output-format defaults, mask lists, file-storage prefix) can be
/// added in v0.7.x without breaking existing files.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Default environment name to use when no `--env` flag and no
    /// `XV_ENV` env var are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_env: Option<String>,

    /// Map of environment name → profile. Stored as `BTreeMap` for
    /// deterministic serialization order and stable test snapshots.
    #[serde(default, rename = "env", skip_serializing_if = "BTreeMap::is_empty")]
    pub envs: BTreeMap<String, EnvProfile>,
}

/// One environment's defaults. All fields optional; a missing field
/// means "no default at this layer — fall through to global context
/// or error if required."
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}
```

Notes on choices:
- `BTreeMap` (not `HashMap`) for deterministic ordering when serializing.
- `#[serde(rename = "env")]` so the TOML reads `[env.dev]` (singular `env`) per the spec, while the Rust field is the more readable plural `envs`.
- Every field is `Option` so a missing `[env.dev].vault` doesn't crash — it just doesn't override anything.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::project`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config/project.rs src/config/mod.rs
git commit -m "feat(config): add ProjectConfig and EnvProfile types for .xv.toml

Schema for project-scoped env defaults. All optional fields with
serde(default) so the schema can grow additively in v0.7.x.
"
```

---

## Task 3: Parse `.xv.toml` from string and from file

**Files:**
- Modify: `src/config/project.rs`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/config/project.rs`:

```rust
    #[test]
    fn parse_str_basic_two_envs() {
        let toml = r#"
default_env = "dev"

[env.dev]
vault = "myproj-dev-kv"
resource_group = "myproj-rg"

[env.prod]
vault = "myproj-prod-kv"
resource_group = "myproj-prod-rg"
"#;
        let cfg = parse_str(toml).expect("must parse");
        assert_eq!(cfg.default_env.as_deref(), Some("dev"));
        assert_eq!(cfg.envs.len(), 2);
        let dev = cfg.envs.get("dev").unwrap();
        assert_eq!(dev.vault.as_deref(), Some("myproj-dev-kv"));
        assert_eq!(dev.resource_group.as_deref(), Some("myproj-rg"));
        let prod = cfg.envs.get("prod").unwrap();
        assert_eq!(prod.vault.as_deref(), Some("myproj-prod-kv"));
    }

    #[test]
    fn parse_str_with_optional_group_and_folder() {
        let toml = r#"
[env.dev]
vault = "v"
resource_group = "rg"
group = "backend"
folder = "app/database"
"#;
        let cfg = parse_str(toml).expect("must parse");
        let dev = cfg.envs.get("dev").unwrap();
        assert_eq!(dev.group.as_deref(), Some("backend"));
        assert_eq!(dev.folder.as_deref(), Some("app/database"));
    }

    #[test]
    fn parse_str_empty_returns_default() {
        let cfg = parse_str("").expect("empty toml is valid");
        assert!(cfg.envs.is_empty());
        assert_eq!(cfg.default_env, None);
    }

    #[test]
    fn parse_str_unknown_top_level_field_is_ignored() {
        // Tolerance for forward-compat: unknown fields don't crash.
        let toml = r#"
default_env = "dev"
mystery_field = 42

[env.dev]
vault = "v"
resource_group = "rg"
"#;
        let cfg = parse_str(toml).expect("unknown fields tolerated");
        assert_eq!(cfg.default_env.as_deref(), Some("dev"));
    }

    #[test]
    fn parse_str_malformed_returns_error() {
        let bad = r#"this is not = valid = toml [["#;
        let result = parse_str(bad);
        assert!(result.is_err(), "malformed TOML must error");
    }

    #[tokio::test]
    async fn parse_file_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".xv.toml");
        let toml = r#"
default_env = "dev"

[env.dev]
vault = "myvault"
resource_group = "myrg"
"#;
        tokio::fs::write(&path, toml).await.unwrap();
        let cfg = parse_file(&path).await.expect("must parse from file");
        assert_eq!(cfg.default_env.as_deref(), Some("dev"));
        assert_eq!(cfg.envs.len(), 1);
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib config::project`
Expected: compile error — `parse_str` and `parse_file` not defined.

- [ ] **Step 3: Implement `parse_str` and `parse_file`**

Add to `src/config/project.rs` (above the `#[cfg(test)]` block):

```rust
use crate::error::{CrosstacheError, Result};
use std::path::Path;

/// Parse a `.xv.toml` blob into a [`ProjectConfig`].
///
/// Empty input returns `Default`. Unknown top-level fields are
/// tolerated (forward-compat). Malformed TOML returns
/// `CrosstacheError::ConfigError`.
pub fn parse_str(s: &str) -> Result<ProjectConfig> {
    if s.trim().is_empty() {
        return Ok(ProjectConfig::default());
    }
    toml::from_str(s).map_err(|e| {
        CrosstacheError::config(format!(".xv.toml parse error: {e}"))
    })
}

/// Parse a `.xv.toml` file from disk asynchronously.
pub async fn parse_file(path: &Path) -> Result<ProjectConfig> {
    let content = tokio::fs::read_to_string(path).await.map_err(|e| {
        CrosstacheError::config(format!(
            "failed to read {}: {e}",
            path.display()
        ))
    })?;
    parse_str(&content)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::project`
Expected: 8 tests PASS (2 type tests + 6 parse tests).

- [ ] **Step 5: Commit**

```bash
git add src/config/project.rs
git commit -m "feat(config): add .xv.toml parsing (parse_str + async parse_file)

Tolerates empty input, unknown fields, and reports malformed TOML
as ConfigError. Async file path uses tokio::fs.
"
```

---

## Task 4: Walk-up algorithm with boundary support

**Files:**
- Modify: `src/config/project.rs`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/config/project.rs`:

```rust
    use std::path::PathBuf;

    /// Helper: write a minimal valid `.xv.toml` at `path/.xv.toml`.
    async fn write_xv_toml(dir: &Path) -> PathBuf {
        let path = dir.join(".xv.toml");
        let toml = r#"
default_env = "dev"

[env.dev]
vault = "v"
resource_group = "rg"
"#;
        tokio::fs::write(&path, toml).await.unwrap();
        path
    }

    /// Helper: create a `.xv.boundary` marker file.
    async fn write_boundary(dir: &Path) {
        tokio::fs::write(dir.join(".xv.boundary"), "").await.unwrap();
    }

    #[tokio::test]
    async fn find_project_config_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let xv_path = write_xv_toml(temp.path()).await;
        let result = find_project_config(temp.path()).await.expect("ok");
        let (path, cfg) = result.expect("must find config in cwd");
        assert_eq!(path, xv_path);
        assert_eq!(cfg.default_env.as_deref(), Some("dev"));
    }

    #[tokio::test]
    async fn find_project_config_walks_up_two_levels() {
        let temp = tempfile::tempdir().unwrap();
        let xv_path = write_xv_toml(temp.path()).await;
        let nested = temp.path().join("a").join("b");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        let result = find_project_config(&nested).await.expect("ok");
        let (path, _cfg) = result.expect("must find ancestor config");
        assert_eq!(path, xv_path);
    }

    #[tokio::test]
    async fn find_project_config_stops_at_boundary() {
        let temp = tempfile::tempdir().unwrap();
        // .xv.toml at root
        write_xv_toml(temp.path()).await;
        // .xv.boundary at intermediate dir — must block walk-up past it
        let mid = temp.path().join("a");
        tokio::fs::create_dir_all(&mid).await.unwrap();
        write_boundary(&mid).await;
        let nested = mid.join("b");
        tokio::fs::create_dir_all(&nested).await.unwrap();

        let result = find_project_config(&nested).await.expect("ok");
        assert!(
            result.is_none(),
            "boundary at intermediate dir must block discovery of ancestor .xv.toml"
        );
    }

    #[tokio::test]
    async fn find_project_config_none_when_no_xv_toml() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a").join("b");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        let result = find_project_config(&nested).await.expect("ok");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_project_config_xv_toml_in_cwd_wins_over_boundary() {
        // If both .xv.toml and .xv.boundary are in the same dir, .xv.toml
        // takes precedence — the boundary only blocks ancestor discovery.
        let temp = tempfile::tempdir().unwrap();
        write_xv_toml(temp.path()).await;
        write_boundary(temp.path()).await;
        let result = find_project_config(temp.path()).await.expect("ok");
        let (_path, cfg) = result.expect("local .xv.toml wins");
        assert_eq!(cfg.default_env.as_deref(), Some("dev"));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib config::project`
Expected: compile error — `find_project_config` not defined.

- [ ] **Step 3: Implement `find_project_config`**

Add to `src/config/project.rs` (above the `#[cfg(test)]` block):

```rust
use std::path::PathBuf;

/// Walk up from `start` to filesystem root looking for `.xv.toml`.
///
/// Stops early at a `.xv.boundary` marker file in any ancestor
/// directory — useful for marking "do not cross this line" between
/// sibling projects in a monorepo.
///
/// Honors `XV_NO_PARENT_CONFIG=1`: when set, only the cwd itself is
/// inspected; no walk-up.
///
/// Returns `Ok(None)` if no `.xv.toml` was found before hitting the
/// root (or a boundary). Returns `Err` only if a found `.xv.toml`
/// fails to parse.
pub async fn find_project_config(
    start: &Path,
) -> Result<Option<(PathBuf, ProjectConfig)>> {
    let no_walk = std::env::var("XV_NO_PARENT_CONFIG")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mut current: Option<&Path> = Some(start);
    while let Some(dir) = current {
        // Check for .xv.toml first — local config wins even if a
        // boundary marker is also in the same dir (boundaries only
        // block *ancestor* discovery).
        let candidate = dir.join(".xv.toml");
        if tokio::fs::metadata(&candidate).await.is_ok() {
            let cfg = parse_file(&candidate).await?;
            return Ok(Some((candidate, cfg)));
        }

        if no_walk {
            return Ok(None);
        }

        // Then check for boundary — if present, do not climb further.
        let boundary = dir.join(".xv.boundary");
        if tokio::fs::metadata(&boundary).await.is_ok() {
            return Ok(None);
        }

        current = dir.parent();
    }
    Ok(None)
}
```

Notes:
- Order matters: check `.xv.toml` BEFORE the boundary, so a config-and-boundary in the same dir picks the config.
- `XV_NO_PARENT_CONFIG` is checked once per call — not on every iteration — so the env var doesn't prevent finding `.xv.toml` in cwd itself.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::project`
Expected: 13 tests PASS (8 prior + 5 new).

- [ ] **Step 5: Commit**

```bash
git add src/config/project.rs
git commit -m "feat(config): add find_project_config walk-up with boundary support

Walks from start dir up to root looking for .xv.toml. .xv.boundary
file in any ancestor blocks further climbing. XV_NO_PARENT_CONFIG=1
disables walk-up entirely (only cwd is inspected). Local config in
cwd wins over a co-located boundary marker.
"
```

---

## Task 5: Resolve active env

**Files:**
- Modify: `src/config/project.rs`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/config/project.rs`:

```rust
    fn build_cfg(default_env: Option<&str>, envs: &[(&str, EnvProfile)]) -> ProjectConfig {
        let mut envs_map = BTreeMap::new();
        for (name, profile) in envs {
            envs_map.insert((*name).to_string(), profile.clone());
        }
        ProjectConfig {
            default_env: default_env.map(String::from),
            envs: envs_map,
        }
    }

    #[test]
    fn resolve_env_uses_default_env_when_no_override() {
        let cfg = build_cfg(
            Some("dev"),
            &[
                ("dev", EnvProfile { vault: Some("v".into()), ..Default::default() }),
                ("prod", EnvProfile::default()),
            ],
        );
        // No XV_ENV, no cli flag — must pick "dev" from default_env.
        // Force-clear XV_ENV for the test.
        std::env::remove_var("XV_ENV");
        let (name, _profile) = resolve_env(&cfg, None).expect("must resolve");
        assert_eq!(name, "dev");
    }

    #[test]
    fn resolve_env_cli_flag_overrides_default_env() {
        let cfg = build_cfg(
            Some("dev"),
            &[
                ("dev", EnvProfile::default()),
                ("prod", EnvProfile::default()),
            ],
        );
        std::env::remove_var("XV_ENV");
        let (name, _profile) = resolve_env(&cfg, Some("prod")).expect("must resolve");
        assert_eq!(name, "prod");
    }

    #[test]
    fn resolve_env_xv_env_overrides_cli_flag() {
        let cfg = build_cfg(
            Some("dev"),
            &[
                ("dev", EnvProfile::default()),
                ("prod", EnvProfile::default()),
                ("staging", EnvProfile::default()),
            ],
        );
        std::env::set_var("XV_ENV", "staging");
        let (name, _profile) = resolve_env(&cfg, Some("prod")).expect("must resolve");
        assert_eq!(name, "staging");
        std::env::remove_var("XV_ENV");
    }

    #[test]
    fn resolve_env_unknown_name_returns_env_not_defined() {
        let cfg = build_cfg(
            Some("dev"),
            &[
                ("dev", EnvProfile::default()),
                ("prod", EnvProfile::default()),
            ],
        );
        std::env::remove_var("XV_ENV");
        let err = resolve_env(&cfg, Some("staging")).expect_err("must err");
        match err {
            CrosstacheError::EnvNotDefined { name, available } => {
                assert_eq!(name, "staging");
                assert_eq!(available, vec!["dev".to_string(), "prod".to_string()]);
            }
            other => panic!("expected EnvNotDefined, got {other:?}"),
        }
    }

    #[test]
    fn resolve_env_no_default_no_override_errors_helpfully() {
        let cfg = build_cfg(
            None,
            &[("dev", EnvProfile::default())],
        );
        std::env::remove_var("XV_ENV");
        let err = resolve_env(&cfg, None).expect_err("must err");
        // Should still surface as EnvNotDefined with a sentinel name
        // ("(none)" or similar) so the error printer's hint applies.
        match err {
            CrosstacheError::EnvNotDefined { available, .. } => {
                assert_eq!(available, vec!["dev".to_string()]);
            }
            other => panic!("expected EnvNotDefined, got {other:?}"),
        }
    }
```

> **Test-isolation caveat:** these tests mutate process-global env state via `std::env::set_var` / `remove_var`. They MUST run with `--test-threads=1` to be reliable. The CI command for this module uses that flag; see Step 4.

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib config::project::tests::resolve_env -- --test-threads=1`
Expected: compile error — `resolve_env` not defined.

- [ ] **Step 3: Implement `resolve_env`**

Add to `src/config/project.rs` (above the `#[cfg(test)]` block):

```rust
/// Selection priority for active env: `XV_ENV` env var → `cli_flag`
/// argument → `cfg.default_env` field → error.
///
/// Returns `(env_name, env_profile)` on success. Returns
/// `CrosstacheError::EnvNotDefined` if the resolved name isn't a key
/// in `cfg.envs`, OR if no name could be resolved at all (in that
/// case the missing-name field is `"(none)"`, indicating "no
/// default_env, no flag, no XV_ENV").
pub fn resolve_env<'a>(
    cfg: &'a ProjectConfig,
    cli_flag: Option<&str>,
) -> Result<(&'a str, &'a EnvProfile)> {
    let candidate: String = if let Ok(env_var) = std::env::var("XV_ENV") {
        env_var
    } else if let Some(flag) = cli_flag {
        flag.to_string()
    } else if let Some(default) = cfg.default_env.as_deref() {
        default.to_string()
    } else {
        // No source of truth at all.
        return Err(CrosstacheError::env_not_defined(
            "(none)",
            cfg.envs.keys().cloned().collect(),
        ));
    };

    if let Some((k, v)) = cfg.envs.get_key_value(&candidate) {
        Ok((k.as_str(), v))
    } else {
        Err(CrosstacheError::env_not_defined(
            candidate,
            cfg.envs.keys().cloned().collect(),
        ))
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::project -- --test-threads=1`
Expected: 18 tests PASS (13 prior + 5 new).

(Single-threaded mandatory for the env-var manipulating tests.)

- [ ] **Step 5: Commit**

```bash
git add src/config/project.rs
git commit -m "feat(config): add resolve_env with XV_ENV > flag > default priority

Returns EnvNotDefined when the resolved name isn't a key in cfg.envs,
listing available envs so the user can fix the typo from the error
alone. Sentinel '(none)' surfaced when no source supplies a name.
"
```

---

## Task 6: Add `--env` global CLI flag

**Files:**
- Modify: `src/cli/commands.rs`

- [ ] **Step 1: Add the global flag**

In `src/cli/commands.rs`, find the `pub struct Cli { ... }` definition (around line 76). Add a new field below `pub format: OutputFormat`:

```rust
    /// Active environment from the resolved .xv.toml (overrides default_env).
    /// Lower priority than the XV_ENV env var.
    #[arg(long, global = true, hide = should_hide_options())]
    pub env: Option<String>,
```

Place it adjacent to other global flags (e.g., right after `pub format: OutputFormat` is fine).

- [ ] **Step 2: Verify build**

Run: `cargo build`
Expected: clean. (The flag is now declared but not yet consulted — that's wired in Task 7.)

- [ ] **Step 3: Quick smoke test**

Run: `cargo run -- --env dev --help 2>&1 | head -20`
Expected: clap accepts the flag without error (the `--help` exits 0 before any execution).

Run: `cargo run -- --env dev list 2>&1 | head -5`
Expected: doesn't fail clap parsing. (May fail downstream if no vault — that's fine; the flag itself just needs to be accepted.)

- [ ] **Step 4: Commit**

```bash
git add src/cli/commands.rs
git commit -m "feat(cli): add --env global flag (consumed in Task 7)

Optional string flag, lower priority than XV_ENV, that selects the
active environment from a resolved .xv.toml.
"
```

---

## Task 7: Apply project config to `resolve_vault_name` and `resolve_resource_group`

**Files:**
- Modify: `src/config/settings.rs`
- Modify: `src/main.rs`

This is the wiring task — `--env`, `XV_ENV`, and the resolved `.xv.toml` env profile all become defaults for vault/RG resolution. CLI flags still override.

- [ ] **Step 1: Extend `resolve_vault_name` and `resolve_resource_group` to consult project config**

The existing `Cli::execute(self, config)` method has `self.env` available. We thread `self.env.as_deref()` through to the call sites where `resolve_vault_name` / `resolve_resource_group` are invoked, so no change to `main.rs` is needed for env-flag plumbing.

Now the actual changes:

In `src/config/settings.rs`, find `resolve_vault_name`:

```rust
    pub async fn resolve_vault_name(&self, vault_arg: Option<String>) -> Result<String> {
```

Change the signature to:

```rust
    pub async fn resolve_vault_name(
        &self,
        vault_arg: Option<String>,
        env_flag: Option<&str>,
    ) -> Result<String> {
```

Replace the body with:

```rust
    pub async fn resolve_vault_name(
        &self,
        vault_arg: Option<String>,
        env_flag: Option<&str>,
    ) -> Result<String> {
        use crate::config::{project, ContextManager};

        // 1. Command line argument takes precedence
        if let Some(vault) = vault_arg {
            return Ok(vault);
        }

        // 2. Project config (.xv.toml) — walk up from cwd
        let cwd = std::env::current_dir()?;
        if let Ok(Some((_path, cfg))) = project::find_project_config(&cwd).await {
            // resolve_env returns Err on unknown-env — propagate so the
            // user sees the helpful EnvNotDefined message with the list
            // of available envs.
            let (_name, profile) = project::resolve_env(&cfg, env_flag)?;
            if let Some(v) = profile.vault.as_deref() {
                return Ok(v.to_string());
            }
            // Profile defines no vault — fall through to context/config.
        }

        // 3. Check local/global context
        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(vault_name) = context_manager.current_vault() {
            return Ok(vault_name.to_string());
        }

        // 4. Fall back to config default
        if !self.default_vault.is_empty() {
            return Ok(self.default_vault.clone());
        }

        Err(CrosstacheError::config(
            "No vault specified. Use --vault, set context with 'xv context use', or configure default_vault"
        ))
    }
```

Apply an analogous change to `resolve_resource_group`:

```rust
    #[allow(dead_code)]
    pub async fn resolve_resource_group(
        &self,
        rg_arg: Option<String>,
        env_flag: Option<&str>,
    ) -> Result<String> {
        use crate::config::{project, ContextManager};

        if let Some(rg) = rg_arg {
            return Ok(rg);
        }

        let cwd = std::env::current_dir()?;
        if let Ok(Some((_path, cfg))) = project::find_project_config(&cwd).await {
            let (_name, profile) = project::resolve_env(&cfg, env_flag)?;
            if let Some(rg) = profile.resource_group.as_deref() {
                return Ok(rg.to_string());
            }
        }

        let context_manager = ContextManager::load().await.unwrap_or_default();
        if let Some(rg) = context_manager.current_resource_group() {
            return Ok(rg.to_string());
        }

        if !self.default_resource_group.is_empty() {
            return Ok(self.default_resource_group.clone());
        }

        Err(CrosstacheError::config("No resource group specified"))
    }
```

- [ ] **Step 2: Update all call sites**

Run:

```bash
grep -rn "resolve_vault_name\|resolve_resource_group" src/ tests/
```

For each call site, add the new `env_flag` argument. The cleanest path is usually `config.resolve_vault_name(vault, cli.env.as_deref())` — but the `cli` value isn't always in scope at the call site.

Recommended approach: thread `cli.env` (or its cloned `Option<String>`) through the same execution path that already threads `vault: Option<String>`. The CLI execution layer holds `cli` and knows the env flag; the config layer needs to receive it.

Concretely, inspect the existing call sites and add a parallel `env_flag: Option<&str>` parameter:
- `execute_secret_get` (in `src/cli/secret_ops.rs`)
- `execute_vault_info` (in `src/cli/vault_ops.rs`)
- Anywhere else that calls `resolve_vault_name` or `resolve_resource_group`.

For these CLI execute functions, plumb the env flag down from `Cli::execute` (in `src/cli/commands.rs` around line 1196) through each `Commands::Foo { ... }` arm. The execute function in `commands.rs:Cli::execute(self, config)` has `self` (the `Cli`), so it has `self.env` available — pass it down.

If a call site is in test code (e.g., a doctest), pass `None`:

```rust
config.resolve_vault_name(vault, None).await?;
```

Run `cargo build` after each fix to surface the next compile error. Repeat until clean.

- [ ] **Step 3: Run all tests**

Run: `cargo test --lib -- --test-threads=1`
Expected: all PASS.

Run: `cargo build`
Expected: clean — no new warnings.

- [ ] **Step 4: Manual smoke test (optional but recommended)**

Set up a temp dir with a `.xv.toml` pointing at a known vault:

```bash
mkdir -p /tmp/xv-smoke && cd /tmp/xv-smoke
cat > .xv.toml <<EOF
default_env = "dev"

[env.dev]
vault = "$DEFAULT_VAULT"
resource_group = "$DEFAULT_RESOURCE_GROUP"
EOF
xv list 2>&1 | head -5
```

Expected: `xv list` resolves the vault from `.xv.toml` without `--vault`. Test passes if output matches what `xv list --vault $DEFAULT_VAULT` would produce.

Then test the env override:

```bash
xv list --env nope 2>&1 | head -5
echo "exit=$?"
```

Expected: exit 3, error `error[xv-env-not-defined]: Environment 'nope' not defined in .xv.toml; available: dev`.

- [ ] **Step 5: Commit**

```bash
git add src/config/settings.rs src/cli/secret_ops.rs src/cli/vault_ops.rs src/cli/commands.rs
# plus any other files touched in step 2
git commit -m "feat(config): consult .xv.toml in resolve_vault_name/resource_group

Resolution priority becomes: CLI flag > .xv.toml env profile (via
walk-up + active-env resolution) > .xv/context > config default. CLI
flags still override. Unknown env names surface as the new
xv-env-not-defined error with the available list.
"
```

---

## Task 8: Cross-boundary stderr notice (one-time-per-process)

**Files:**
- Modify: `src/config/project.rs`
- Modify: `src/main.rs`

When `find_project_config` discovers a `.xv.toml` in an **ancestor** directory (not in cwd itself), the user should see a one-time stderr line so they know which config is winning.

- [ ] **Step 1: Write the failing test**

The notice fires on the first call per process and never again. We test the underlying `OnceLock`-style guard. Add to `mod tests` in `src/config/project.rs`:

```rust
    #[test]
    fn cross_boundary_notice_fires_once() {
        // Reset the guard for the test (the implementation exposes a
        // test-only reset hook).
        #[cfg(test)]
        reset_cross_boundary_notice_for_test();

        let captured1 = capture_cross_boundary_notice("/path/a/.xv.toml", "dev");
        let captured2 = capture_cross_boundary_notice("/path/b/.xv.toml", "prod");
        assert_eq!(
            captured1,
            Some("using config from /path/a/.xv.toml (env: dev)".to_string()),
        );
        assert_eq!(captured2, None, "second call must be no-op");
    }
```

- [ ] **Step 2: Implement the guard and the helper**

Add to `src/config/project.rs`:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// One-shot guard — flips true on the first emit. We expose a
/// test-only reset to keep the assertion local; production code
/// never resets it.
static CROSS_BOUNDARY_NOTICE_EMITTED: AtomicBool = AtomicBool::new(false);

/// Format the cross-boundary notice line. Returns the formatted
/// string the *first* time it is called per process; on subsequent
/// calls returns `None` so callers know to skip emitting.
///
/// Used by `main.rs` to print the line to stderr exactly once.
pub fn capture_cross_boundary_notice(
    config_path: impl AsRef<std::path::Path>,
    env_name: &str,
) -> Option<String> {
    if CROSS_BOUNDARY_NOTICE_EMITTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        Some(format!(
            "using config from {} (env: {env_name})",
            config_path.as_ref().display()
        ))
    } else {
        None
    }
}

#[cfg(test)]
fn reset_cross_boundary_notice_for_test() {
    CROSS_BOUNDARY_NOTICE_EMITTED.store(false, Ordering::SeqCst);
}
```

(We use `compare_exchange` rather than just `swap` so the test can reason about it cleanly. `OnceLock` would also work but `AtomicBool` keeps the API smaller and the test reset trivial.)

- [ ] **Step 3: Wire the notice into the resolution path**

In `src/config/settings.rs::resolve_vault_name` and `resolve_resource_group`, after a successful `find_project_config(&cwd).await` call, check if the discovered `.xv.toml` path is in an ancestor (not cwd itself), and if so emit the notice:

In both functions, replace:

```rust
        if let Ok(Some((_path, cfg))) = project::find_project_config(&cwd).await {
            let (_name, profile) = project::resolve_env(&cfg, env_flag)?;
            ...
        }
```

with:

```rust
        if let Ok(Some((path, cfg))) = project::find_project_config(&cwd).await {
            let (name, profile) = project::resolve_env(&cfg, env_flag)?;
            // Emit the cross-boundary notice if the .xv.toml lives
            // above cwd. Suppressed by XV_NO_PARENT_CONFIG=1 since
            // walk-up wouldn't have reached the ancestor anyway —
            // but keep this branch defensive.
            if path.parent().map(|p| p != cwd).unwrap_or(false) {
                if let Some(line) = project::capture_cross_boundary_notice(&path, name) {
                    eprintln!("{line}");
                }
            }
            if let Some(v) = profile.vault.as_deref() {
                return Ok(v.to_string());
            }
        }
```

(Same change in `resolve_resource_group` — but the second call won't actually emit because of the one-shot guard. That's intentional; the message has the same content either way.)

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib config::project -- --test-threads=1`
Expected: all PASS, including `cross_boundary_notice_fires_once`.

- [ ] **Step 5: Manual smoke test**

```bash
mkdir -p /tmp/xv-smoke-cross/sub && cd /tmp/xv-smoke-cross
cat > .xv.toml <<EOF
default_env = "dev"

[env.dev]
vault = "myvault"
resource_group = "myrg"
EOF
cd sub
xv list 2>&1 | head -3
```

Expected stderr: `using config from /tmp/xv-smoke-cross/.xv.toml (env: dev)` (printed once).

Run `xv list` again from the same shell session in a different process — the message should appear (each process is its own one-shot). Run two commands in the same shell — first prints, second doesn't because the guard is process-scoped.

Verify that putting `.xv.toml` directly in cwd does NOT emit the notice:

```bash
cd /tmp/xv-smoke-cross
xv list 2>&1 | head -3
```

Expected: no `using config from ...` line on stderr.

Verify suppression:

```bash
cd /tmp/xv-smoke-cross/sub
XV_NO_PARENT_CONFIG=1 xv list 2>&1 | head -3
```

Expected: no `using config from ...` line; `xv` falls through to `.xv/context` or config default.

- [ ] **Step 6: Commit**

```bash
git add src/config/project.rs src/config/settings.rs
git commit -m "feat(config): one-time stderr notice when .xv.toml found in ancestor

Process-scoped AtomicBool gate: first call returns the formatted
'using config from <path> (env: <name>)' line; subsequent calls
return None. Suppressed automatically by XV_NO_PARENT_CONFIG=1
(walk-up disabled). Co-located .xv.toml in cwd doesn't emit.
"
```

---

## Task 9: Legacy `.xv/context` deprecation warning

**Files:**
- Modify: `src/config/context.rs`

When the legacy `.xv/context` JSON file is loaded (no `.xv.toml` found at any walk-up level), emit a one-time stderr deprecation warning so users know to migrate.

- [ ] **Step 1: Add the one-shot guard**

In `src/config/context.rs`, add near the top of the file (below the existing `use` lines):

```rust
use std::sync::atomic::{AtomicBool, Ordering};

static LEGACY_CONTEXT_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

fn maybe_warn_legacy_context(path: &std::path::Path) {
    if LEGACY_CONTEXT_WARN_EMITTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        eprintln!(
            "warning: legacy .xv/context loaded from {}; consider migrating to .xv.toml — see docs/env-profiles.md",
            path.display()
        );
    }
}
```

- [ ] **Step 2: Call the warner from `load_local_context`**

Find `load_local_context` (around line 107). After successfully parsing the JSON content and before the `Ok(context)` return, add a call:

```rust
        let mut context: ContextManager = serde_json::from_str(&content)?;
        context.context_file = Some(context_path.clone());
        context.is_local = true;

        if let Some(ref path) = context.context_file {
            debug!("Loaded local context from: {}", path.display());
        }
        maybe_warn_legacy_context(&context_path);
        Ok(context)
```

(Note: `context_path` is a `PathBuf` constructed at line 108. Clone it before moving into `Some(...)` so we can also pass it to `maybe_warn_legacy_context`.)

- [ ] **Step 3: Manual smoke test**

```bash
mkdir -p /tmp/xv-smoke-legacy/.xv && cd /tmp/xv-smoke-legacy
echo '{"current":null,"recent":[]}' > .xv/context
xv list 2>&1 | grep -i warning | head -3
```

Expected: `warning: legacy .xv/context loaded from /tmp/xv-smoke-legacy/.xv/context; consider migrating to .xv.toml — see docs/env-profiles.md`.

Run `xv list` again — the warning should NOT repeat (process boundary or no, the in-process guard has flipped).

Now drop a `.xv.toml` adjacent and re-run:

```bash
cat > .xv.toml <<EOF
[env.dev]
vault = "v"
resource_group = "rg"
EOF
xv list 2>&1 | grep -i warning | head -3
```

Expected: no warning — the new path takes precedence (assuming Tasks 7/8 took effect; the legacy path is only loaded as fallback).

- [ ] **Step 4: Run unit tests**

Run: `cargo test --lib config::context`
Expected: existing tests still pass. (The new `maybe_warn_legacy_context` is best tested via the manual smoke; unit-testing stderr emission requires `assert_cmd` infrastructure that's overkill for a one-shot warning.)

- [ ] **Step 5: Commit**

```bash
git add src/config/context.rs
git commit -m "feat(config): one-time deprecation warning when legacy .xv/context loads

Pointer to docs/env-profiles.md so users know to migrate. Suppressed
when .xv.toml is present (the legacy path is only consulted as
fallback, see Task 7). Process-scoped AtomicBool guard.
"
```

---

## Task 10: `xv context envs` command

**Files:**
- Modify: `src/cli/commands.rs`
- Modify: `src/cli/config_ops.rs`

Lists the envs defined in the resolved `.xv.toml`, marking the active one.

- [ ] **Step 1: Add the subcommand**

In `src/cli/commands.rs`, find the `pub enum ContextCommands { ... }` block (around line 889). Add a new variant:

```rust
    /// List environment profiles in the resolved .xv.toml
    Envs,
```

Place it after `List` for grouping.

- [ ] **Step 2: Wire the executor**

In `src/cli/commands.rs`, find the `Commands::Context { command }` arm (around line 1196). Inspect how the inner `command` is dispatched — existing variants like `Show`, `Use`, `List`, `Clear` map to `execute_context_*` functions in `src/cli/config_ops.rs`. Add a parallel arm for `Envs`:

```rust
            ContextCommands::Envs => {
                crate::cli::config_ops::execute_context_envs(self.env.as_deref()).await?;
            }
```

- [ ] **Step 3: Implement `execute_context_envs`**

In `src/cli/config_ops.rs`, add a new public function:

```rust
pub(crate) async fn execute_context_envs(env_flag: Option<&str>) -> crate::error::Result<()> {
    use crate::config::project;

    let cwd = std::env::current_dir()?;
    let Some((path, cfg)) = project::find_project_config(&cwd).await? else {
        crate::utils::output::warn(
            "no .xv.toml found in cwd or any ancestor (within boundary)",
        );
        crate::utils::output::info("hint: run 'xv context init' to create one");
        return Ok(());
    };

    // Resolve active env (best-effort — don't error out, just leave
    // the active marker absent if resolution fails).
    let active = project::resolve_env(&cfg, env_flag)
        .ok()
        .map(|(name, _)| name.to_string());

    println!("config: {}", path.display());
    if let Some(d) = cfg.default_env.as_deref() {
        println!("default_env: {d}");
    }
    println!();
    if cfg.envs.is_empty() {
        println!("(no envs defined)");
        return Ok(());
    }
    println!("envs:");
    for (name, profile) in &cfg.envs {
        let marker = if active.as_deref() == Some(name.as_str()) { "*" } else { " " };
        let vault = profile.vault.as_deref().unwrap_or("(unset)");
        let rg = profile.resource_group.as_deref().unwrap_or("(unset)");
        println!("  {marker} {name}  vault={vault}  rg={rg}");
    }
    Ok(())
}
```

- [ ] **Step 4: Smoke test**

```bash
cd /tmp/xv-smoke && cat > .xv.toml <<EOF
default_env = "dev"

[env.dev]
vault = "dev-vault"
resource_group = "dev-rg"

[env.prod]
vault = "prod-vault"
resource_group = "prod-rg"
EOF
xv context envs 2>&1
```

Expected output (something like):

```
config: /tmp/xv-smoke/.xv.toml
default_env: dev

envs:
  * dev  vault=dev-vault  rg=dev-rg
    prod  vault=prod-vault  rg=prod-rg
```

Now test override:

```bash
xv --env prod context envs 2>&1
```

Expected: `*` shifts to `prod`.

Test no-config case:

```bash
cd /tmp && xv context envs 2>&1 | head -3
```

Expected: warning about no `.xv.toml`, hint to run `xv context init`.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib`
Expected: all PASS.

Run: `cargo build`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs src/cli/config_ops.rs
git commit -m "feat(cli): add 'xv context envs' to list .xv.toml envs

Shows the resolved config path, default_env, and each env's
vault/rg with a star marker on the active one. Honors --env and
XV_ENV. Helpful empty-state output when no .xv.toml is found.
"
```

---

## Task 11: Extend `xv context show`

**Files:**
- Modify: `src/cli/config_ops.rs`

Existing `xv context show` displays current vault/RG from `.xv/context` plus scope. After this task, when a `.xv.toml` resolves, the output ALSO includes "active env: dev (from /path/to/.xv.toml)" plus the resolved defaults.

- [ ] **Step 1: Locate `execute_context_show`**

Run:

```bash
grep -n "execute_context_show\|pub.*context_show\|fn execute_context_show" src/cli/config_ops.rs
```

Open the function and read it. Note the existing print structure so the new lines fit the style.

- [ ] **Step 2: Add the env-profile section**

Modify the function so that, after the existing scope/vault/RG output, it also tries to load `.xv.toml` and prints the active-env block:

```rust
    // (existing scope + current vault + RG output stays)

    // New: project-config (.xv.toml) section.
    let cwd = std::env::current_dir()?;
    if let Ok(Some((path, cfg))) = crate::config::project::find_project_config(&cwd).await {
        match crate::config::project::resolve_env(&cfg, env_flag) {
            Ok((name, profile)) => {
                println!();
                println!("active env: {name} (from {})", path.display());
                if let Some(v) = &profile.vault {
                    println!("  vault: {v}");
                }
                if let Some(rg) = &profile.resource_group {
                    println!("  resource_group: {rg}");
                }
                if let Some(g) = &profile.group {
                    println!("  group: {g}");
                }
                if let Some(f) = &profile.folder {
                    println!("  folder: {f}");
                }
            }
            Err(e) => {
                println!();
                println!("project config: {} (error: {e})", path.display());
            }
        }
    }
```

The function will need a new `env_flag: Option<&str>` parameter — match the pattern from Task 10 (`execute_context_envs`).

- [ ] **Step 3: Wire `env_flag` through the dispatch**

In `src/cli/commands.rs`, find the `ContextCommands::Show` arm in `Cli::execute`. Update the call site to pass `self.env.as_deref()`:

```rust
            ContextCommands::Show => {
                crate::cli::config_ops::execute_context_show(self.env.as_deref(), &config).await?;
            }
```

(Adjust the existing call signature — read the current call site and add the parameter consistently with how `execute_context_envs` does.)

- [ ] **Step 4: Smoke test**

```bash
cd /tmp/xv-smoke
xv context show 2>&1
```

Expected output (something like):

```
[existing scope/vault/RG block]

active env: dev (from /tmp/xv-smoke/.xv.toml)
  vault: dev-vault
  resource_group: dev-rg
```

With `--env prod`:

```bash
xv --env prod context show 2>&1
```

Expected: `active env: prod ...` block.

With an unknown env:

```bash
xv --env nope context show 2>&1
```

Expected: `project config: /tmp/xv-smoke/.xv.toml (error: Environment 'nope' not defined in .xv.toml; available: dev, prod)`.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib`
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs src/cli/config_ops.rs
git commit -m "feat(cli): extend 'xv context show' with active env from .xv.toml

When a .xv.toml resolves, append 'active env: <name> (from <path>)'
plus the env's vault/rg/group/folder defaults. On resolution error
(e.g., bad --env), show the path and the parse error so the user
can fix it.
"
```

---

## Task 12: `xv context init` interactive scaffolder

**Files:**
- Modify: `src/cli/commands.rs`
- Modify: `src/cli/config_ops.rs`

Creates a `.xv.toml` in cwd. Default to interactive prompts; non-interactive fallback via flags so CI and scripts can scaffold cleanly.

- [ ] **Step 1: Add the subcommand**

In `src/cli/commands.rs::ContextCommands`, add (after `Envs`):

```rust
    /// Create a new .xv.toml in the current directory
    Init {
        /// Env name to create (default: "dev")
        #[arg(long, default_value = "dev")]
        env: String,
        /// Vault for the env (skips interactive prompt if provided)
        #[arg(long)]
        vault: Option<String>,
        /// Resource group for the env (skips interactive prompt if provided)
        #[arg(long)]
        resource_group: Option<String>,
        /// Skip prompts entirely; require --vault and --resource-group
        #[arg(long)]
        non_interactive: bool,
        /// Overwrite an existing .xv.toml
        #[arg(long)]
        force: bool,
    },
```

- [ ] **Step 2: Wire the executor**

In `src/cli/commands.rs`, dispatch:

```rust
            ContextCommands::Init { env, vault, resource_group, non_interactive, force } => {
                crate::cli::config_ops::execute_context_init(
                    env, vault, resource_group, non_interactive, force, &config,
                ).await?;
            }
```

- [ ] **Step 3: Implement `execute_context_init`**

In `src/cli/config_ops.rs`:

```rust
pub(crate) async fn execute_context_init(
    env_name: String,
    vault_arg: Option<String>,
    rg_arg: Option<String>,
    non_interactive: bool,
    force: bool,
    config: &crate::config::Config,
) -> crate::error::Result<()> {
    use crate::config::project::{EnvProfile, ProjectConfig};
    use crate::error::CrosstacheError;
    use std::collections::BTreeMap;

    let cwd = std::env::current_dir()?;
    let path = cwd.join(".xv.toml");
    if path.exists() && !force {
        return Err(CrosstacheError::config(format!(
            ".xv.toml already exists at {} (use --force to overwrite)",
            path.display()
        )));
    }

    // Resolve vault/RG: explicit flag → interactive prompt → config default
    let (vault, resource_group) = if non_interactive {
        let vault = vault_arg.ok_or_else(|| {
            CrosstacheError::invalid_argument(
                "--non-interactive requires --vault",
            )
        })?;
        let rg = rg_arg.ok_or_else(|| {
            CrosstacheError::invalid_argument(
                "--non-interactive requires --resource-group",
            )
        })?;
        (vault, rg)
    } else {
        use crate::utils::interactive::InteractivePrompt;
        let prompt = InteractivePrompt::new();
        let vault = match vault_arg {
            Some(v) => v,
            None => prompt.input_text(
                &format!("Vault for env '{env_name}'"),
                if !config.default_vault.is_empty() {
                    Some(config.default_vault.as_str())
                } else {
                    None
                },
            )?,
        };
        let rg = match rg_arg {
            Some(r) => r,
            None => prompt.input_text(
                &format!("Resource group for env '{env_name}'"),
                if !config.default_resource_group.is_empty() {
                    Some(config.default_resource_group.as_str())
                } else {
                    None
                },
            )?,
        };
        (vault, rg)
    };

    let mut envs = BTreeMap::new();
    envs.insert(
        env_name.clone(),
        EnvProfile {
            vault: Some(vault),
            resource_group: Some(resource_group),
            group: None,
            folder: None,
        },
    );

    let cfg = ProjectConfig {
        default_env: Some(env_name.clone()),
        envs,
    };

    let body = toml::to_string_pretty(&cfg).map_err(|e| {
        CrosstacheError::config(format!("failed to serialize .xv.toml: {e}"))
    })?;

    // Helpful header
    let header = "# crosstache project config — see https://github.com/bziobnic/crosstache/blob/main/docs/env-profiles.md\n";
    let full = format!("{header}{body}");

    tokio::fs::write(&path, full).await?;
    crate::utils::output::success(&format!(
        ".xv.toml written to {} (env: {env_name})",
        path.display()
    ));
    Ok(())
}
```

> **Interactive prompt helper:** uses `crate::utils::interactive::InteractivePrompt::input_text(message, default)` (already in the codebase, see `src/utils/interactive.rs:43`). The struct also has `confirm()` if you want to add a "Add another env?" loop in a follow-up — out of scope for this task.

- [ ] **Step 4: Smoke test**

Non-interactive (CI-friendly):

```bash
cd /tmp/xv-init-smoke && rm -f .xv.toml
xv context init --non-interactive --vault myvault --resource-group myrg 2>&1
cat .xv.toml
```

Expected: file written; cat shows the rendered TOML matching the schema.

Re-run without `--force`:

```bash
xv context init --non-interactive --vault other --resource-group other 2>&1
echo "exit=$?"
```

Expected: exits with code 3 (config error: "already exists, use --force").

Re-run with `--force`:

```bash
xv context init --non-interactive --force --vault new --resource-group newrg 2>&1
cat .xv.toml | grep vault
```

Expected: file rewritten; `vault = "new"`.

Interactive smoke test (manual, not in CI):

```bash
xv context init  # follow prompts
```

Expected: prompts for vault and RG, defaults shown from config if set.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib`
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs src/cli/config_ops.rs
git commit -m "feat(cli): add 'xv context init' to scaffold a .xv.toml

Defaults to interactive prompts seeded from the user's existing
config; --non-interactive + --vault/--resource-group skips prompts
for CI; --force overwrites an existing file. Always serializes
through ProjectConfig so the on-disk format is canonical.
"
```

---

## Task 13: Docs — exit-code row, env-profiles reference, README link

**Files:**
- Modify: `docs/exit-codes.md`
- Create: `docs/env-profiles.md`
- Modify: `README.md`

- [ ] **Step 1: Add the exit-code row**

In `docs/exit-codes.md`, find the table. After the existing config-family entry (around `| 3   | Configuration error ...`), the same exit-3 family also covers the new env-not-defined error. Add a sub-bullet OR a new row clarifying:

Update the row for exit `3` to read:

```markdown
| `3`   | Configuration error   | missing required config; invalid config file; env not defined in `.xv.toml` |
```

Then under the "Error codes" section, add a small example for the new code:

````markdown
For env-resolution failures specifically:

```bash
xv get DB_PASSWORD --env staging
# error[xv-env-not-defined]: Environment 'staging' not defined in .xv.toml; available: dev, prod
# exit 3
```
````

- [ ] **Step 2: Create `docs/env-profiles.md`**

```markdown
# Env Profiles (`.xv.toml`)

`xv` looks for a `.xv.toml` in the current directory and walks up to
the filesystem root. The first one it finds wins. Drop a `.xv.boundary`
file in any directory to stop the walk-up before that point — useful
in monorepos to prevent leaking parent config into a sibling project.

## Schema

```toml
default_env = "dev"

[env.dev]
vault = "myproj-dev-kv"
resource_group = "myproj-rg"
group = "backend"          # optional
folder = "app/database"    # optional

[env.prod]
vault = "myproj-prod-kv"
resource_group = "myproj-prod-rg"
```

All fields except `[env.<name>]` blocks are optional. New fields (output
defaults, mask lists, etc.) will be added in v0.7.x without breaking
existing files.

## Active env selection

Priority (highest first):

1. `XV_ENV` environment variable
2. `--env <name>` CLI flag
3. `default_env` field in `.xv.toml`
4. Error: `xv-env-not-defined` (exit `3`) listing the available envs.

## How env defaults are applied

Each command's `--vault` / `--resource-group` / `--group` / `--folder`
flag still overrides everything. When the flag is absent, `xv`
resolves in this order:

1. CLI flag (if provided)
2. The active env's field in `.xv.toml`
3. The legacy `.xv/context` JSON (deprecated; see below)
4. The user's global config default

## Cross-boundary notice

When a `.xv.toml` is found in an ancestor directory (not in cwd),
the first command in a process prints a one-time stderr line:

```
using config from /path/to/.xv.toml (env: dev)
```

To opt out of walk-up entirely, set `XV_NO_PARENT_CONFIG=1` in your
environment. With that set, only a `.xv.toml` directly in cwd will
be considered.

## Migration from `.xv/context`

The legacy `.xv/context` JSON file (created by `xv context use`) keeps
working as a fallback when no `.xv.toml` is found. You'll see this
warning the first time it loads in any process:

```
warning: legacy .xv/context loaded from <path>; consider migrating to .xv.toml — see docs/env-profiles.md
```

Migrating: run `xv context init` in the project root and answer the
prompts (or pass `--vault` / `--resource-group` non-interactively).
You can then delete `.xv/context`.

The legacy fallback is removed in v0.8.

## Commands

| Command | What it does |
|---------|--------------|
| `xv context init` | Creates `.xv.toml` in cwd. Interactive by default; pass `--non-interactive --vault X --resource-group Y` for scripts. `--force` to overwrite. |
| `xv context envs` | Lists envs in the resolved `.xv.toml` with the active one starred. |
| `xv context show` | Existing command; now also shows the active env block when a `.xv.toml` resolves. |
| `xv --env <name> <command>` | Override the active env for one command. |

## How `xv env` differs

`xv env create / use / list / pull / push` manage **global, user-scoped**
profiles in your user config (one set of named profiles per machine,
per user). `.xv.toml` env profiles are **project-scoped**, checked into
the repo, shared across the team. They coexist; when both are present,
the project `.xv.toml` wins.

```

- [ ] **Step 3: Add the README link**

In `README.md`, find the `## Scripting & exit codes` subsection added in Plan #1's Task 12. Add a new subsection right after it (or before, your call — match document flow):

```markdown
## Env profiles

For per-project vault/resource-group defaults, use a `.xv.toml`
at the project root. See [`docs/env-profiles.md`](docs/env-profiles.md)
for the full reference.
```

- [ ] **Step 4: Commit**

```bash
git add docs/exit-codes.md docs/env-profiles.md README.md
git commit -m "docs: env-profiles reference + exit-code update

User-facing reference for the .xv.toml format, walk-up rules,
active-env selection, cross-boundary notice, and migration from
.xv/context. README pointer alongside the exit-codes reference.
"
```

---

## Task 14: Cut v0.6.0-rc.2

**Files:**
- Modify: `Cargo.toml` (version bump)
- Tag: git tag

- [ ] **Step 1: Run the full quality gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -W clippy::all 2>&1 | grep -E "warning:|error:" | head -20
cargo test
cargo test -- --test-threads=1
```

All must pass. Address any drift; do not bundle unrelated reformats — if `cargo fmt --all -- --check` reports drift in files this branch did NOT modify, leave them alone (commit a separate fmt sweep if needed).

- [ ] **Step 2: Bump the version**

In `Cargo.toml`, change `version = "0.6.0-rc.1"` to `version = "0.6.0-rc.2"`.

Run: `cargo build` to refresh `Cargo.lock`.

- [ ] **Step 3: Commit & tag**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.6.0-rc.2"
git tag -a v0.6.0-rc.2 -m "v0.6.0-rc.2: env profiles + walk-up resolution"
```

- [ ] **Step 4: Stop here**

Do NOT push. The tag and commit are local. Stop and ask the user to confirm before running `git push --tags origin <branch>` and creating the PR.

---

## Verification checklist (final, before declaring plan complete)

- [ ] `cargo test` (all green; honor `--test-threads=1` for env-var-mutating tests in `config::project`)
- [ ] `cargo clippy --all-targets -- -W clippy::all` — no NEW warnings vs. v0.6.0-rc.1 baseline
- [ ] `cargo fmt --all -- --check` — clean
- [ ] Manual: `cd /tmp/xv-smoke && xv list` resolves vault from `.xv.toml` without `--vault` flag
- [ ] Manual: `xv --env nope list` exits 3 with `error[xv-env-not-defined]:` and lists available envs
- [ ] Manual: `XV_ENV=prod xv list` overrides `--env dev` (XV_ENV wins)
- [ ] Manual: `cd subdir && xv list` emits `using config from <ancestor>/.xv.toml (env: ...)` once per process on stderr; same command in cwd does NOT emit
- [ ] Manual: `XV_NO_PARENT_CONFIG=1 xv list` from a subdir does NOT discover the ancestor `.xv.toml`
- [ ] Manual: `xv context init --non-interactive --vault X --resource-group Y` creates a parseable file
- [ ] Manual: `xv context envs` lists envs with `*` on the active one
- [ ] Manual: `xv context show` includes the new "active env: ..." block
- [ ] Manual: legacy `.xv/context`-only project emits the deprecation warning once per process
- [ ] Soft-commitment-checklist: this plan adds NO new manager-method usages — nothing new to log in `docs/superpowers/specs/backend-trait-checklist.md`. (See spec §3.2.6: "Pure config plumbing. No backend reads.")

---

## Notes for the executing engineer

- **TDD discipline.** Each task starts with a failing test. Resist writing implementation before the test fails for the right reason.
- **Commit per task.** Each task ends with one commit. Don't bundle.
- **Test isolation.** Tasks 5 and 8 mutate process-global env state (`XV_ENV`, `XV_NO_PARENT_CONFIG`) and a process-global `AtomicBool`. Run those tests with `--test-threads=1`. The full suite should still tolerate parallel execution because the env-mutating tests use `remove_var` defensively.
- **Soft-commitment checklist:** this plan adds zero backend-method usages — pure config plumbing, no read-surface growth.
- **Coexistence with `xv env`:** the existing global `xv env <subcommand>` namespace is **untouched**. We're adding to `xv context`, not `xv env`. Don't confuse the two.
