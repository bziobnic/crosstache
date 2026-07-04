//! `.xv.toml` env profile schema and resolution.
//!
//! This module owns:
//! - The `ProjectConfig` / `EnvProfile` data shapes for the on-disk format.
//! - Parsing (`parse_str` / `parse_file`).
//! - Walk-up traversal (`find_project_config`) with `.xv.boundary` stopper
//!   and `XV_NO_PARENT_CONFIG=1` opt-out.
//! - Active-env selection (`resolve_env`) honoring `XV_ENV` > `--env` flag >
//!   `default_env` field > error.

use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

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

    /// Optional scanner configuration for leak detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanConfig>,

    /// Custom record types declared as `[types.<name>]` blocks, overriding
    /// global config / built-in types of the same name. See
    /// `crate::records::resolve_types`.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub types: std::collections::HashMap<String, crate::records::RecordTypeConfig>,
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
    /// Backend to use for this env. Must be one of: `azure`, `local`, `aws`.
    /// Overrides the global config `backend` key but loses to `--backend` CLI flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

/// Validate that a backend value from an env profile is a known backend.
///
/// Returns `Err` with a descriptive message if `value` is not one of the
/// recognised canonical names (`azure`, `local`, `aws`).
pub fn validate_env_profile_backend(value: &str) -> Result<()> {
    match value {
        "azure" | "local" | "aws" => Ok(()),
        other => Err(CrosstacheError::config(format!(
            "invalid backend {other:?} in .xv.toml env profile — must be one of: azure, local, aws"
        ))),
    }
}

/// Resolve the effective backend name from the four resolution layers.
///
/// Precedence (highest first):
/// 1. `cli_backend`     — explicit `--backend` flag
/// 2. `profile_backend` — active env profile's `backend` field
/// 3. `config_backend`  — global config `backend` key (or `XV_BACKEND`)
/// 4. `"azure"`         — built-in default
pub fn resolve_effective_backend<'a>(
    cli_backend: Option<&'a str>,
    profile_backend: Option<&'a str>,
    config_backend: Option<&'a str>,
) -> &'a str {
    cli_backend
        .or(profile_backend)
        .or(config_backend)
        .unwrap_or("azure")
}

/// Scanner configuration block for leak detection settings.
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

/// Parse a `.xv.toml` blob into a [`ProjectConfig`].
///
/// Empty input returns `Default`. Unknown top-level fields are
/// tolerated (forward-compat). Malformed TOML returns
/// `CrosstacheError::ConfigError`.
pub fn parse_str(s: &str) -> Result<ProjectConfig> {
    if s.trim().is_empty() {
        return Ok(ProjectConfig::default());
    }
    toml::from_str(s).map_err(|e| CrosstacheError::config(format!(".xv.toml parse error: {e}")))
}

/// Parse a `.xv.toml` file from disk asynchronously.
pub async fn parse_file(path: &Path) -> Result<ProjectConfig> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| CrosstacheError::config(format!("failed to read {}: {e}", path.display())))?;
    parse_str(&content)
}

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
pub async fn find_project_config(start: &Path) -> Result<Option<(PathBuf, ProjectConfig)>> {
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

impl ProjectConfig {
    /// Standard header banner prepended to every `.xv.toml` written by
    /// crosstache (both `xv context init` and `ProjectConfig::save`).
    ///
    /// This file is project-scoped and meant to be checked into VCS, so
    /// the header gives team members a discoverable docs link without
    /// requiring them to run `--help`.
    pub const HEADER: &'static str = "# crosstache project config — see https://github.com/bziobnic/crosstache/blob/main/docs/env-profiles.md\n";

    /// Serialize this config to TOML and write it to `path`.
    ///
    /// `.xv.toml` is project-scoped and intended to be checked into the
    /// repository, so we use `tokio::fs::write` (respects umask, typically
    /// 0o644) rather than `write_sensitive_file_async` (forces 0o600).
    /// Bugbot finding on PR #211: forcing 0o600 broke shared-filesystem
    /// workflows.
    ///
    /// The standard `HEADER` banner is always prepended, so successive
    /// `save()` calls don't strip the helpful "see docs" pointer that
    /// `xv context init` writes. Per-user comments inside the file are
    /// still discarded — this is a documented limitation; users wanting
    /// comments should edit the file manually outside of `xv env *`.
    pub async fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self)
            .map_err(|e| CrosstacheError::config(format!(".xv.toml serialize error: {e}")))?;
        let full = format!("{}{body}", Self::HEADER);
        tokio::fs::write(path, full).await.map_err(|e| {
            CrosstacheError::config(format!("failed to write {}: {e}", path.display()))
        })
    }
}

/// Walk up from `cwd` to find the nearest `.xv.toml`. If none is found,
/// return `(cwd/.xv.toml, ProjectConfig::default())` so callers can
/// mutate and then `save()` without a separate existence check.
pub async fn find_or_create_project_config(cwd: &Path) -> Result<(PathBuf, ProjectConfig)> {
    match find_project_config(cwd).await? {
        Some(result) => Ok(result),
        None => Ok((cwd.join(".xv.toml"), ProjectConfig::default())),
    }
}

/// Selection priority for active env: `XV_ENV` env var → `cli_flag`
/// argument → `cfg.default_env` field → no active profile.
///
/// Returns `Ok(Some((env_name, env_profile)))` when an env was resolved.
///
/// Returns `Ok(None)` when there was no `XV_ENV`, no `cli_flag`, and no
/// `default_env` to fall back to, AND `cfg.envs` is empty — i.e. the
/// `.xv.toml` defines zero `[env.*]` blocks and contributes nothing
/// env-related. Types-only project files are a legitimate shape (record
/// types, #321) and must not hard-fail write commands that ask for
/// profile defaults (#331); callers treat `Ok(None)` as "no defaults
/// here, fall through to the next layer."
///
/// Returns `Err(CrosstacheError::EnvNotDefined)` in every other
/// unresolvable case:
/// - An explicit `XV_ENV`/`cli_flag` name that isn't a key in
///   `cfg.envs` (whether or not `cfg.envs` is empty) — the file *was*
///   asked for a specific environment by name.
/// - No `XV_ENV`/`cli_flag`, but `cfg.default_env` names an environment
///   that isn't a key in `cfg.envs`.
/// - No `XV_ENV`/`cli_flag`/`default_env` at all, but `cfg.envs` is
///   non-empty (the file defines environments; the caller must pick
///   one). This is the pre-#331 "(none)" case and is unchanged.
pub fn resolve_env<'a>(
    cfg: &'a ProjectConfig,
    cli_flag: Option<&str>,
) -> Result<Option<(&'a str, &'a EnvProfile)>> {
    let candidate: Option<String> = if let Ok(env_var) = std::env::var("XV_ENV") {
        Some(env_var)
    } else if let Some(flag) = cli_flag {
        Some(flag.to_string())
    } else {
        cfg.default_env.clone()
    };

    let candidate = match candidate {
        Some(c) => c,
        None => {
            if cfg.envs.is_empty() {
                // No selection source AND no envs defined at all: this
                // file contributes nothing env-related. Not an error.
                return Ok(None);
            }
            // File defines envs, but none was selected — unchanged
            // pre-#331 behavior: error with the available list.
            return Err(CrosstacheError::env_not_defined(
                "(none)",
                cfg.envs.keys().cloned().collect(),
            ));
        }
    };

    if let Some((k, v)) = cfg.envs.get_key_value(&candidate) {
        Ok(Some((k.as_str(), v)))
    } else if cfg.envs.is_empty() {
        // An explicit --env/XV_ENV name (or a stale default_env) against
        // a file that defines zero environments — give the clearer
        // "defines no environments" message instead of an empty list.
        Err(CrosstacheError::env_not_defined_no_envs(candidate))
    } else {
        Err(CrosstacheError::env_not_defined(
            candidate,
            cfg.envs.keys().cloned().collect(),
        ))
    }
}

/// One-shot guard — flips true on the first emit. We expose a
/// test-only reset to keep the assertion local; production code
/// never resets it.
static CROSS_BOUNDARY_NOTICE_EMITTED: AtomicBool = AtomicBool::new(false);

/// Format the cross-boundary notice line. Returns the formatted
/// string the *first* time it is called per process; on subsequent
/// calls returns `None` so callers know to skip emitting.
///
/// Used by `Config::resolve_vault_name` (and friends) to print the
/// line to stderr exactly once.
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
pub(crate) fn reset_cross_boundary_notice_for_test() {
    CROSS_BOUNDARY_NOTICE_EMITTED.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Guards tests that read or write the global `XV_ENV` env var so they
    /// don't race each other under cargo's default parallel test runner.
    static XV_ENV_LOCK: Mutex<()> = Mutex::new(());

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
        assert_eq!(p.backend, None);
    }

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
        tokio::fs::write(dir.join(".xv.boundary"), "")
            .await
            .unwrap();
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

    fn build_cfg(default_env: Option<&str>, envs: &[(&str, EnvProfile)]) -> ProjectConfig {
        let mut envs_map = BTreeMap::new();
        for (name, profile) in envs {
            envs_map.insert((*name).to_string(), profile.clone());
        }
        ProjectConfig {
            default_env: default_env.map(String::from),
            envs: envs_map,
            scan: None,
            types: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn resolve_env_uses_default_env_when_no_override() {
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(
            Some("dev"),
            &[
                (
                    "dev",
                    EnvProfile {
                        vault: Some("v".into()),
                        ..Default::default()
                    },
                ),
                ("prod", EnvProfile::default()),
            ],
        );
        // No XV_ENV, no cli flag — must pick "dev" from default_env.
        // Force-clear XV_ENV for the test.
        std::env::remove_var("XV_ENV");
        let (name, _profile) = resolve_env(&cfg, None)
            .expect("must resolve")
            .expect("expected an active profile");
        assert_eq!(name, "dev");
    }

    #[test]
    fn resolve_env_cli_flag_overrides_default_env() {
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(
            Some("dev"),
            &[
                ("dev", EnvProfile::default()),
                ("prod", EnvProfile::default()),
            ],
        );
        std::env::remove_var("XV_ENV");
        let (name, _profile) = resolve_env(&cfg, Some("prod"))
            .expect("must resolve")
            .expect("expected an active profile");
        assert_eq!(name, "prod");
    }

    #[test]
    fn resolve_env_xv_env_overrides_cli_flag() {
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(
            Some("dev"),
            &[
                ("dev", EnvProfile::default()),
                ("prod", EnvProfile::default()),
                ("staging", EnvProfile::default()),
            ],
        );
        std::env::set_var("XV_ENV", "staging");
        let (name, _profile) = resolve_env(&cfg, Some("prod"))
            .expect("must resolve")
            .expect("expected an active profile");
        assert_eq!(name, "staging");
        std::env::remove_var("XV_ENV");
    }

    #[test]
    fn resolve_env_unknown_name_returns_env_not_defined() {
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(None, &[("dev", EnvProfile::default())]);
        std::env::remove_var("XV_ENV");
        let err = resolve_env(&cfg, None).expect_err("must err");
        match err {
            CrosstacheError::EnvNotDefined { available, .. } => {
                assert_eq!(available, vec!["dev".to_string()]);
            }
            other => panic!("expected EnvNotDefined, got {other:?}"),
        }
    }

    // --- #331: types-only / env-less .xv.toml ---

    #[test]
    fn resolve_env_no_envs_no_default_no_override_returns_no_profile() {
        // Zero [env.*] blocks, no default_env, no --env, no XV_ENV: a
        // types-only project file contributes nothing env-related and
        // must not error.
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(None, &[]);
        std::env::remove_var("XV_ENV");
        let resolved = resolve_env(&cfg, None).expect("must not error");
        assert!(resolved.is_none(), "expected no active profile");
    }

    #[test]
    fn resolve_env_no_envs_explicit_flag_errors_with_no_environments_message() {
        // Explicit --env against a file with zero [env.*] blocks must
        // still error (the user asked for a specific env by name), but
        // with a clearer message than an empty "available: " list.
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(None, &[]);
        std::env::remove_var("XV_ENV");
        let err = resolve_env(&cfg, Some("staging")).expect_err("must err");
        match err {
            CrosstacheError::EnvNotDefined {
                ref name,
                ref available,
            } => {
                assert_eq!(name, "staging");
                assert!(available.is_empty());
            }
            other => panic!("expected EnvNotDefined, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("no environments") || msg.contains("no [env"),
            "message should explain the file defines no environments: {msg}"
        );
        assert!(
            !msg.contains("available: "),
            "message should not print an empty available list: {msg}"
        );
    }

    #[test]
    fn resolve_env_no_envs_but_default_env_set_still_errors() {
        // default_env is explicitly configured but no [env.*] blocks
        // exist at all — this is a real misconfiguration (not the "no
        // envs at all" absent-defaults case), and must keep erroring.
        let _guard = XV_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = build_cfg(Some("dev"), &[]);
        std::env::remove_var("XV_ENV");
        let err = resolve_env(&cfg, None).expect_err("must err");
        match err {
            CrosstacheError::EnvNotDefined { name, available } => {
                assert_eq!(name, "dev");
                assert!(available.is_empty());
            }
            other => panic!("expected EnvNotDefined, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn project_config_save_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".xv.toml");
        let mut cfg = ProjectConfig {
            default_env: Some("dev".to_string()),
            ..Default::default()
        };
        cfg.envs.insert(
            "dev".to_string(),
            EnvProfile {
                vault: Some("myvault".to_string()),
                resource_group: Some("myrg".to_string()),
                ..Default::default()
            },
        );
        cfg.save(&path).await.expect("save must succeed");
        let loaded = parse_file(&path).await.expect("reload must parse");
        assert_eq!(loaded.default_env.as_deref(), Some("dev"));
        let dev = loaded.envs.get("dev").expect("env.dev must be present");
        assert_eq!(dev.vault.as_deref(), Some("myvault"));
        assert_eq!(dev.resource_group.as_deref(), Some("myrg"));
    }

    /// PR #211 Bugbot finding (Medium): `save()` must use umask-respecting
    /// permissions, not 0o600. `.xv.toml` is project-scoped and intended
    /// to be committed to VCS / shared on a team filesystem.
    #[tokio::test]
    #[cfg(unix)]
    async fn project_config_save_respects_umask_not_0o600() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".xv.toml");
        let cfg = ProjectConfig::default();
        cfg.save(&path).await.expect("save must succeed");
        let mode = tokio::fs::metadata(&path)
            .await
            .expect("stat must succeed")
            .permissions()
            .mode()
            & 0o777;
        // Default umask is 0o022 → file mode 0o644. Some test environments
        // run with umask 0o002 → 0o664. Both are world/group-readable, which
        // is the property we care about. 0o600 is what we're guarding against.
        assert_ne!(
            mode, 0o600,
            ".xv.toml must NOT be 0o600 — see PR #211 Bugbot finding"
        );
        assert!(
            mode & 0o044 != 0,
            ".xv.toml must be group/world readable (got {mode:o})"
        );
    }

    /// PR #211 Bugbot finding (Medium): `save()` must prepend the standard
    /// HEADER banner so the helpful "see docs" comment written by
    /// `xv context init` survives every subsequent `xv env use/create/delete`.
    #[tokio::test]
    async fn project_config_save_preserves_header() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".xv.toml");
        let cfg = ProjectConfig::default();
        cfg.save(&path).await.expect("save must succeed");
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            content.starts_with(ProjectConfig::HEADER),
            "saved file must start with HEADER banner; got:\n{content}"
        );
        // And re-saving doesn't duplicate the header.
        cfg.save(&path).await.expect("re-save must succeed");
        let content2 = tokio::fs::read_to_string(&path).await.unwrap();
        let header_count = content2.matches(ProjectConfig::HEADER).count();
        assert_eq!(
            header_count, 1,
            "header must appear exactly once after re-save; got {header_count}"
        );
    }

    #[tokio::test]
    async fn find_or_create_returns_existing() {
        let temp = tempfile::tempdir().unwrap();
        write_xv_toml(temp.path()).await;
        let (path, cfg) = find_or_create_project_config(temp.path())
            .await
            .expect("must succeed");
        assert_eq!(path, temp.path().join(".xv.toml"));
        assert_eq!(cfg.default_env.as_deref(), Some("dev"));
    }

    #[tokio::test]
    async fn find_or_create_returns_cwd_default_when_none() {
        // Temp dirs under the system temp root have no .xv.toml ancestors,
        // so find_project_config returns None and we get the fallback path.
        let temp = tempfile::tempdir().unwrap();
        let (path, cfg) = find_or_create_project_config(temp.path())
            .await
            .expect("must succeed");
        assert_eq!(path, temp.path().join(".xv.toml"));
        assert!(cfg.envs.is_empty());
        assert_eq!(cfg.default_env, None);
        // The file must NOT have been created on disk.
        assert!(!path.exists(), "find_or_create must not write the file");
    }

    #[test]
    fn cross_boundary_notice_fires_once() {
        // Reset the guard for the test (the implementation exposes a
        // test-only reset hook).
        reset_cross_boundary_notice_for_test();

        let captured1 = capture_cross_boundary_notice("/path/a/.xv.toml", "dev");
        let captured2 = capture_cross_boundary_notice("/path/b/.xv.toml", "prod");
        assert_eq!(
            captured1,
            Some("using config from /path/a/.xv.toml (env: dev)".to_string()),
        );
        assert_eq!(captured2, None, "second call must be no-op");
    }

    // --- backend field tests ---

    #[test]
    fn backend_field_parses() {
        let toml = r#"
[env.dev]
vault = "v"
resource_group = "rg"
backend = "aws"
"#;
        let cfg = parse_str(toml).expect("must parse");
        let dev = cfg.envs.get("dev").unwrap();
        assert_eq!(dev.backend.as_deref(), Some("aws"));
    }

    #[test]
    fn backend_field_defaults_to_none() {
        let toml = r#"
[env.dev]
vault = "v"
resource_group = "rg"
"#;
        let cfg = parse_str(toml).expect("must parse");
        let dev = cfg.envs.get("dev").unwrap();
        assert_eq!(dev.backend, None);
    }

    #[test]
    fn backend_all_valid_values_accepted() {
        for name in &["azure", "local", "aws"] {
            assert!(
                validate_env_profile_backend(name).is_ok(),
                "expected {name:?} to be valid"
            );
        }
    }

    #[test]
    fn backend_invalid_value_rejected() {
        let err = validate_env_profile_backend("gcp").expect_err("must err");
        assert!(
            err.to_string().contains("gcp"),
            "error should name the bad value; got: {err}"
        );
        assert!(
            err.to_string().contains("azure") || err.to_string().contains("must be"),
            "error should name valid options; got: {err}"
        );
    }

    #[test]
    fn resolve_effective_backend_cli_wins_over_all() {
        assert_eq!(
            resolve_effective_backend(Some("local"), Some("aws"), Some("azure")),
            "local"
        );
    }

    #[test]
    fn resolve_effective_backend_profile_wins_over_config() {
        assert_eq!(
            resolve_effective_backend(None, Some("aws"), Some("azure")),
            "aws"
        );
    }

    #[test]
    fn resolve_effective_backend_config_wins_over_default() {
        assert_eq!(
            resolve_effective_backend(None, None, Some("local")),
            "local"
        );
    }

    #[test]
    fn resolve_effective_backend_falls_back_to_azure() {
        assert_eq!(resolve_effective_backend(None, None, None), "azure");
    }

    #[test]
    fn parse_str_with_scan_block() {
        let toml = r#"
[scan]
exclude = ["dist/**", "*.lock"]
min_value_length = 12
patterns = ["aws-access-key-id", "github-token"]

[env.dev]
vault = "v"
resource_group = "rg"
"#;
        let cfg = parse_str(toml).expect("must parse");
        let scan = cfg.scan.as_ref().expect("must have [scan]");
        assert_eq!(scan.exclude, vec!["dist/**", "*.lock"]);
        assert_eq!(scan.min_value_length, Some(12));
        assert_eq!(scan.patterns, vec!["aws-access-key-id", "github-token"]);
    }
}
