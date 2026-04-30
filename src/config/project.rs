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
}
