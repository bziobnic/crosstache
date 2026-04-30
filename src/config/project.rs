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
