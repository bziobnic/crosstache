//! Multi-vault workspaces: a set of attached vaults, potentially spanning
//! several backends, open simultaneously.
//!
//! See `docs/superpowers/specs/2026-07-04-multi-vault-workspaces-design.md`
//! for the full design. Phase A (this module) covers the workspace model,
//! persistence (context + `.xv.toml` `vaults` overlay), and colon-address
//! parsing. **No workspace attached ⇒ every command behaves exactly as it
//! did before this module existed** — the workspace layer is only
//! consulted when [`resolve_workspace`] returns `Some`.

pub mod address;
pub mod resolve;

pub use address::parse_address;
pub use resolve::{resolve_secret_target, TargetMode};

use crate::config::settings::Config;
use crate::error::{CrosstacheError, Result};
use serde::{Deserialize, Serialize};

/// Where a resolved [`Workspace`] came from. Surfaced by `xv cx ls` so users
/// can tell a project-level `.xv.toml` overlay from their personal context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSource {
    /// Loaded from the (local or global) vault context store.
    Context,
    /// Loaded from a `.xv.toml` `[env.<name>] vaults = [...]` overlay. When
    /// present this REPLACES any context workspace entirely — no merging.
    ProjectToml,
}

/// One attached vault in a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    /// Unique-within-workspace short name used in colon addressing
    /// (`alias:path`) and `xv cx` output.
    pub alias: String,
    /// Registry backend name (`"azure"`, `"local"`, a named-backend key
    /// like `"aws-east"`, ...).
    pub backend: String,
    /// Vault name on that backend.
    pub vault: String,
    /// Whether this entry is the workspace's default (write target for
    /// unqualified writes). Exactly one entry in a valid workspace has
    /// `default: true`.
    pub default: bool,
}

/// A resolved set of attached vaults plus the default write target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub entries: Vec<WorkspaceEntry>,
    /// Alias of the default (write) entry. Always present in a validated
    /// workspace — a matching entry is guaranteed by construction.
    pub default_alias: String,
    pub source: WorkspaceSource,
}

impl Workspace {
    /// Look up an attached entry by alias.
    pub fn entry(&self, alias: &str) -> Option<&WorkspaceEntry> {
        self.entries.iter().find(|e| e.alias == alias)
    }

    /// The workspace's default (write) entry.
    ///
    /// `default_alias` matching an entry is an invariant maintained by
    /// construction (`build_workspace` / `resolve_workspace`), but this is
    /// a shared resolution layer reachable from hand-built `Workspace`
    /// values (tests, future callers) — so a violation returns a config
    /// error naming the missing alias instead of panicking.
    pub fn default_entry(&self) -> Result<&WorkspaceEntry> {
        self.entry(&self.default_alias).ok_or_else(|| {
            CrosstacheError::config(format!(
                "workspace invariant violated: default alias '{}' does not match any attached entry",
                self.default_alias
            ))
        })
    }

    /// Fail-closed structural validation, following the `[types.*]`
    /// precedent (`crate::records::resolve_types`): reject rather than
    /// silently coerce.
    ///
    /// Checks:
    /// - every alias is non-empty and charset-valid (same charset as vault
    ///   names: `[a-zA-Z0-9-]`);
    /// - aliases are unique within the workspace;
    /// - no alias collides with a known registry backend name (would make
    ///   `xv://alias/...` ambiguous with `xv://backend:vault/...` parsing);
    /// - exactly one entry is marked `default`.
    pub fn validate(&self, backend_names: &[&str]) -> Result<()> {
        if self.entries.is_empty() {
            return Err(CrosstacheError::config(
                "workspace must have at least one attached vault",
            ));
        }

        let mut seen = std::collections::HashSet::new();
        for e in &self.entries {
            if !is_valid_alias_charset(&e.alias) {
                return Err(CrosstacheError::config(format!(
                    "invalid workspace alias '{}': must be non-empty and use only \
                     letters, digits, and hyphens (same charset as vault names)",
                    e.alias
                )));
            }
            if !seen.insert(e.alias.as_str()) {
                return Err(CrosstacheError::config(format!(
                    "duplicate workspace alias '{}': aliases must be unique within a workspace",
                    e.alias
                )));
            }
            if backend_names.contains(&e.alias.as_str()) {
                return Err(CrosstacheError::config(format!(
                    "workspace alias '{}' collides with a registry backend name; \
                     choose a different alias (`--as <alias>`) to avoid ambiguity in xv:// addressing",
                    e.alias
                )));
            }
        }

        let default_count = self.entries.iter().filter(|e| e.default).count();
        match default_count {
            1 => {}
            0 => {
                return Err(CrosstacheError::config(
                    "workspace has no default vault: exactly one attached vault must be marked default",
                ));
            }
            _ => {
                return Err(CrosstacheError::config(
                    "workspace has multiple default vaults: exactly one attached vault may be marked default",
                ));
            }
        }

        Ok(())
    }
}

/// Serde shape for one workspace entry, shared by both the context store
/// (JSON) and the `.xv.toml` project overlay (TOML).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEntryConfig {
    pub vault: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

impl WorkspaceEntryConfig {
    /// The alias to use for this entry: the explicit `alias`, or the vault
    /// name when omitted.
    pub fn resolved_alias(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.vault.clone())
    }
}

/// Workspace state persisted in the context store (`ContextManager`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    #[serde(default)]
    pub entries: Vec<WorkspaceEntryConfig>,
}

/// Alias charset: identical to the vault-name charset crosstache already
/// enforces for Azure Key Vault names (`[a-zA-Z0-9-]`), non-empty.
pub fn is_valid_alias_charset(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Build and validate a [`Workspace`] from raw entry configs.
///
/// - Resolves each entry's alias (`alias` field or vault name) and backend
///   (`backend` field or `active_backend`).
/// - A single entry with no explicit `default` becomes the implicit
///   default (so `xv cx add <first-vault>` "just works" without `--default`).
/// - Runs [`Workspace::validate`] and returns its error unchanged on
///   failure (fail-closed, no silent coercion).
pub fn build_workspace(
    configs: &[WorkspaceEntryConfig],
    active_backend: &str,
    source: WorkspaceSource,
    backend_names: &[&str],
) -> Result<Workspace> {
    let mut entries: Vec<WorkspaceEntry> = configs
        .iter()
        .map(|c| WorkspaceEntry {
            alias: c.resolved_alias(),
            backend: c
                .backend
                .clone()
                .unwrap_or_else(|| active_backend.to_string()),
            vault: c.vault.clone(),
            default: c.default,
        })
        .collect();

    if entries.len() == 1 && !entries[0].default {
        entries[0].default = true;
    }

    let default_alias = entries
        .iter()
        .find(|e| e.default)
        .map(|e| e.alias.clone())
        .unwrap_or_default();

    let ws = Workspace {
        entries,
        default_alias,
        source,
    };
    ws.validate(backend_names)?;
    Ok(ws)
}

/// The known, always-valid registry backend kind names, used for the
/// alias/backend-name collision check regardless of what's configured.
pub const BUILTIN_BACKEND_NAMES: [&str; 3] = ["azure", "local", "aws"];

/// Resolve the active workspace, if any, per the spec's overlay rule:
/// a `.xv.toml` `vaults = [...]` block on the active env profile REPLACES
/// any context workspace entirely (no merging); with neither present,
/// returns `Ok(None)` — the degenerate, byte-identical-with-today case.
pub async fn resolve_workspace(config: &Config) -> Result<Option<Workspace>> {
    let cwd = std::env::current_dir().ok();
    let context_manager = crate::config::ContextManager::load()
        .await
        .unwrap_or_default();
    resolve_workspace_from(config, cwd.as_deref(), &context_manager).await
}

/// Core of [`resolve_workspace`], parameterized over `cwd` and the loaded
/// context so it's testable without touching the process-global working
/// directory (which unit tests can't safely sandbox under parallel `cargo
/// test`). Production code always goes through [`resolve_workspace`].
async fn resolve_workspace_from(
    config: &Config,
    cwd: Option<&std::path::Path>,
    context_manager: &crate::config::ContextManager,
) -> Result<Option<Workspace>> {
    let active_backend = config.effective_backend_name().to_string();

    let mut backend_names: Vec<String> = BUILTIN_BACKEND_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    for k in config.named_backends.keys() {
        if !backend_names.iter().any(|n| n == k) {
            backend_names.push(k.clone());
        }
    }
    let backend_name_refs: Vec<&str> = backend_names.iter().map(|s| s.as_str()).collect();

    // 1. `.xv.toml` project overlay — replaces context entirely.
    if let Some(cwd) = cwd {
        if let Ok(Some((_path, proj_cfg))) = crate::config::project::find_project_config(cwd).await
        {
            if let Ok(Some((_name, profile))) =
                crate::config::project::resolve_env(&proj_cfg, config.env_flag.as_deref())
            {
                if !profile.vaults.is_empty() {
                    let ws = build_workspace(
                        &profile.vaults,
                        &active_backend,
                        WorkspaceSource::ProjectToml,
                        &backend_name_refs,
                    )?;
                    return Ok(Some(ws));
                }
            }
        }
    }

    // 2. Context workspace.
    if let Some(ws_state) = &context_manager.workspace {
        if !ws_state.entries.is_empty() {
            let ws = build_workspace(
                &ws_state.entries,
                &active_backend,
                WorkspaceSource::Context,
                &backend_name_refs,
            )?;
            return Ok(Some(ws));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(alias: &str, backend: &str, vault: &str, default: bool) -> WorkspaceEntry {
        WorkspaceEntry {
            alias: alias.to_string(),
            backend: backend.to_string(),
            vault: vault.to_string(),
            default,
        }
    }

    fn ws(entries: Vec<WorkspaceEntry>, default_alias: &str) -> Workspace {
        Workspace {
            entries,
            default_alias: default_alias.to_string(),
            source: WorkspaceSource::Context,
        }
    }

    #[test]
    fn workspace_validate_rejects_duplicate_alias() {
        let w = ws(
            vec![
                entry("work", "azure", "work-kv", true),
                entry("work", "local", "personal", false),
            ],
            "work",
        );
        let err = w.validate(&["azure", "local", "aws"]).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn workspace_validate_rejects_multi_default() {
        let w = ws(
            vec![
                entry("work", "azure", "work-kv", true),
                entry("stage", "aws", "stage-sm", true),
            ],
            "work",
        );
        let err = w.validate(&["azure", "local", "aws"]).unwrap_err();
        assert!(err.to_string().contains("multiple default"), "{err}");
    }

    #[test]
    fn workspace_validate_rejects_no_default_multi_entry() {
        let w = ws(
            vec![
                entry("work", "azure", "work-kv", false),
                entry("stage", "aws", "stage-sm", false),
            ],
            "",
        );
        let err = w.validate(&["azure", "local", "aws"]).unwrap_err();
        assert!(err.to_string().contains("no default"), "{err}");
    }

    #[test]
    fn workspace_validate_rejects_alias_matching_backend_name() {
        let w = ws(vec![entry("azure", "aws", "prod-sm", true)], "azure");
        let err = w.validate(&["azure", "local", "aws"]).unwrap_err();
        assert!(err.to_string().contains("collides"), "{err}");
    }

    #[test]
    fn single_entry_implicit_default() {
        let configs = vec![WorkspaceEntryConfig {
            vault: "work-kv".to_string(),
            backend: Some("azure".to_string()),
            alias: Some("work".to_string()),
            default: false,
        }];
        let built = build_workspace(
            &configs,
            "azure",
            WorkspaceSource::Context,
            &["azure", "local", "aws"],
        )
        .expect("single entry must build with implicit default");
        assert_eq!(built.default_alias, "work");
        assert!(built.entries[0].default);
    }

    #[test]
    fn entry_config_alias_defaults_to_vault_name() {
        let c = WorkspaceEntryConfig {
            vault: "myproj-dev-kv".to_string(),
            backend: None,
            alias: None,
            default: false,
        };
        assert_eq!(c.resolved_alias(), "myproj-dev-kv");

        let c2 = WorkspaceEntryConfig {
            alias: Some("dev".to_string()),
            ..c
        };
        assert_eq!(c2.resolved_alias(), "dev");
    }

    #[test]
    fn build_workspace_multi_entry_requires_explicit_default() {
        let configs = vec![
            WorkspaceEntryConfig {
                vault: "work-kv".to_string(),
                backend: Some("azure".to_string()),
                alias: Some("work".to_string()),
                default: false,
            },
            WorkspaceEntryConfig {
                vault: "stage-sm".to_string(),
                backend: Some("aws".to_string()),
                alias: Some("stage".to_string()),
                default: false,
            },
        ];
        let err = build_workspace(
            &configs,
            "azure",
            WorkspaceSource::Context,
            &["azure", "local", "aws"],
        )
        .unwrap_err();
        assert!(err.to_string().contains("no default"), "{err}");
    }

    #[test]
    fn entry_and_default_entry_lookup() {
        let configs = vec![
            WorkspaceEntryConfig {
                vault: "work-kv".to_string(),
                backend: Some("azure".to_string()),
                alias: Some("work".to_string()),
                default: true,
            },
            WorkspaceEntryConfig {
                vault: "stage-sm".to_string(),
                backend: Some("aws".to_string()),
                alias: Some("stage".to_string()),
                default: false,
            },
        ];
        let built = build_workspace(
            &configs,
            "azure",
            WorkspaceSource::Context,
            &["azure", "local", "aws"],
        )
        .expect("must build");
        assert_eq!(built.default_entry().unwrap().alias, "work");
        assert_eq!(built.entry("stage").unwrap().vault, "stage-sm");
        assert!(built.entry("nope").is_none());
    }

    /// `default_entry()` must return a `Result` (config error naming the
    /// missing alias), not panic, when a hand-built `Workspace` violates
    /// the default_alias-matches-an-entry invariant — this resolution
    /// layer is reachable from more than just `build_workspace`'s own
    /// guaranteed-valid output.
    #[test]
    fn default_entry_on_broken_invariant_errors_instead_of_panicking() {
        let broken = ws(
            vec![entry("work", "azure", "work-kv", true)],
            "no-such-alias",
        );
        let err = broken.default_entry().unwrap_err();
        assert!(err.to_string().contains("no-such-alias"), "{err}");
    }

    #[tokio::test]
    async fn resolve_none_when_no_workspace_anywhere() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::default();
        let context_manager = crate::config::ContextManager::default();
        let resolved = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect("must not error");
        assert!(resolved.is_none());
    }

    #[tokio::test]
    async fn resolve_prefers_project_over_context() {
        let temp = tempfile::tempdir().unwrap();
        let toml = r#"
default_env = "dev"

[env.dev]
vault = "ignored"
vaults = [
  { vault = "project-vault", backend = "azure", alias = "proj", default = true },
]
"#;
        std::fs::write(temp.path().join(".xv.toml"), toml).unwrap();

        let config = Config {
            backend: Some("azure".to_string()),
            ..Default::default()
        };

        let context_manager = crate::config::ContextManager {
            workspace: Some(WorkspaceState {
                entries: vec![WorkspaceEntryConfig {
                    vault: "context-vault".to_string(),
                    backend: Some("local".to_string()),
                    alias: Some("ctx".to_string()),
                    default: true,
                }],
            }),
            ..Default::default()
        };

        let resolved = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect("must not error")
            .expect("project overlay must produce a workspace");

        assert_eq!(resolved.source, WorkspaceSource::ProjectToml);
        assert_eq!(resolved.entries.len(), 1);
        assert_eq!(resolved.entries[0].alias, "proj");
        assert!(
            resolved.entry("ctx").is_none(),
            "project overlay must REPLACE context entries, not merge them"
        );
    }

    #[tokio::test]
    async fn resolve_falls_back_to_context_when_no_project_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config::default();

        let context_manager = crate::config::ContextManager {
            workspace: Some(WorkspaceState {
                entries: vec![WorkspaceEntryConfig {
                    vault: "context-vault".to_string(),
                    backend: Some("local".to_string()),
                    alias: Some("ctx".to_string()),
                    default: true,
                }],
            }),
            ..Default::default()
        };

        let resolved = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect("must not error")
            .expect("context workspace must resolve");
        assert_eq!(resolved.source, WorkspaceSource::Context);
        assert_eq!(resolved.entries[0].alias, "ctx");
    }
}
