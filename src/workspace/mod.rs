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

use crate::backend::BackendKind;
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
    /// Synthesized workspace-of-one over the current/default vault when NO
    /// `cx`/`.xv.toml` workspace is configured. This is the degenerate case
    /// that lets every command resolve through the single workspace path
    /// without the caller having attached a real workspace. Distinguished
    /// from a user-configured workspace by [`Workspace::is_configured`] so
    /// presence-gates (`xv context use`, union `ls`, mv/copy, TUI) that mean
    /// "a REAL workspace is attached" keep working.
    Degenerate,
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
    /// `true` when this workspace was explicitly configured by the user — a
    /// personal context workspace ([`WorkspaceSource::Context`]) or a project
    /// `.xv.toml` overlay ([`WorkspaceSource::ProjectToml`]). `false` for the
    /// synthesized degenerate workspace-of-one
    /// ([`WorkspaceSource::Degenerate`]).
    ///
    /// Presence-gates that used to test `resolve_workspace().is_some()` to
    /// mean "a REAL workspace is attached" must test this instead, now that
    /// [`resolve_workspace`] never returns `None` (it returns a degenerate
    /// workspace rather than `None`).
    pub fn is_configured(&self) -> bool {
        !matches!(self.source, WorkspaceSource::Degenerate)
    }

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

/// The registry backend names (built-ins + configured `named_backends` keys)
/// used for the alias/backend-name collision check.
fn known_backend_names(config: &Config) -> Vec<String> {
    let mut backend_names: Vec<String> = BUILTIN_BACKEND_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    for k in config.named_backends.keys() {
        if !backend_names.iter().any(|n| n == k) {
            backend_names.push(k.clone());
        }
    }
    backend_names
}

/// Resolve the CONFIGURED workspace, if any, per the spec's overlay rule: a
/// `.xv.toml` `vaults = [...]` block on the active env profile REPLACES any
/// context workspace entirely (no merging); a personal context workspace is
/// used next; with neither present, returns `Ok(None)`.
///
/// This does **not** synthesize the degenerate workspace-of-one (and so never
/// raises its Azure-no-vault hard-error). It is what PRESENCE-GATES ("is a
/// REAL workspace attached?" — `xv context use`, union `ls`/`find`, mv/copy,
/// TUI) and `run`/`inject` alias resolution consult: they only care whether a
/// user-configured workspace exists, and must NOT fail merely because the
/// degenerate builder can't resolve a default vault (e.g. `xv context use
/// <vault>` in an unconfigured Azure env must succeed so the user can set that
/// very vault). A real `.xv.toml`/validation error still propagates. The
/// secret-resolution seam uses [`resolve_workspace`] instead.
pub async fn resolve_configured_workspace(config: &Config) -> Result<Option<Workspace>> {
    let cwd = std::env::current_dir()?;
    let context_manager = crate::config::ContextManager::load().await?;
    resolve_configured_workspace_from(config, Some(&cwd), &context_manager).await
}

/// Resolve the effective workspace for SECRET RESOLUTION: the configured
/// workspace when present, otherwise a synthesized **degenerate
/// workspace-of-one** ([`WorkspaceSource::Degenerate`]) over the
/// current/default vault.
///
/// **Never returns `None`.** It returns `Some(Workspace)` (configured or
/// degenerate) or propagates `Err` — preserving the no-vault hard-error for
/// an active Azure backend (the same error `resolve_vault_for_trait` /
/// `Config::resolve_vault_name` raise today). The return type stays
/// `Option<Workspace>` only so existing callers compile unchanged; the
/// `None` case is unreachable. This is used ONLY by the resolver seam
/// ([`crate::cli::helpers::resolve_workspace_or_default`]); presence-gates and
/// `run`/`inject` use [`resolve_configured_workspace`]. Callers distinguish a
/// configured workspace from the degenerate one with
/// [`Workspace::is_configured`].
pub async fn resolve_workspace(config: &Config) -> Result<Option<Workspace>> {
    // Audit finding (Bugbot round-4): `.ok()` here would swallow a
    // `current_dir()` failure by treating it as "no cwd", which skips the
    // `.xv.toml` project-overlay lookup entirely and falls through
    // straight to the personal context workspace — the same class of
    // unsafe silent fallback as the `resolve_env` bug this round fixes.
    // `Config::resolve_vault_name` (src/config/settings.rs) propagates
    // `current_dir()` with `?`; this does the same for consistency. The
    // `Option<&Path>` parameter on `resolve_workspace_from` stays (only
    // ever `None` in tests, which can't safely touch the process-global
    // cwd) — production always supplies `Some` here or fails loud.
    let cwd = std::env::current_dir()?;
    let context_manager = crate::config::ContextManager::load().await?;
    resolve_workspace_from(config, Some(&cwd), &context_manager).await
}

/// Core of [`resolve_configured_workspace`], parameterized over `cwd` and the
/// loaded context so it's testable without touching the process-global working
/// directory (which unit tests can't safely sandbox under parallel `cargo
/// test`). Production code always goes through [`resolve_configured_workspace`].
async fn resolve_configured_workspace_from(
    config: &Config,
    cwd: Option<&std::path::Path>,
    context_manager: &crate::config::ContextManager,
) -> Result<Option<Workspace>> {
    let active_backend = config.effective_backend_name().to_string();
    let backend_names = known_backend_names(config);
    let backend_name_refs: Vec<&str> = backend_names.iter().map(|s| s.as_str()).collect();

    // 1. `.xv.toml` project overlay — replaces context entirely.
    //
    // `find_project_config`'s own error (a found `.xv.toml` that fails to
    // parse) is swallowed here, matching the existing precedent in
    // `Config::resolve_vault_name` (src/config/settings.rs) — every other
    // resolver in the codebase treats a parse failure the same way.
    //
    // `resolve_env`'s error is NOT swallowed (Bugbot round-4 fix): post-#334
    // it returns `Ok(None)` only when the project file genuinely defines no
    // `[env.*]` blocks at all (a types-only file, #331) — that case falls
    // through to the context workspace below, correctly. A real `Err` means
    // the file DOES define environments but none was selected, or an
    // explicit `--env`/`XV_ENV` names one that doesn't exist — the same
    // "fail closed" case `resolve_vault_name` and every other resolver
    // already propagates. Silently falling through to the personal context
    // workspace here would let a secret command target personal vaults
    // inside a project directory that clearly intends project-scoped ones.
    if let Some(cwd) = cwd {
        if let Some((_path, proj_cfg)) = crate::config::project::find_project_config(cwd).await? {
            if let Some((_name, profile)) =
                crate::config::project::resolve_env(&proj_cfg, config.env_flag.as_deref())?
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

/// Core of [`resolve_workspace`]: the configured workspace, or the degenerate
/// workspace-of-one when none is configured. Parameterized over `cwd`/context
/// for the same testability reason as [`resolve_configured_workspace_from`].
pub(crate) async fn resolve_workspace_from(
    config: &Config,
    cwd: Option<&std::path::Path>,
    context_manager: &crate::config::ContextManager,
) -> Result<Option<Workspace>> {
    if let Some(ws) = resolve_configured_workspace_from(config, cwd, context_manager).await? {
        return Ok(Some(ws));
    }

    // No configured workspace: synthesize the degenerate workspace-of-one over
    // the current/default vault. This is what makes `resolve_workspace` never
    // return `None`. The vault is derived from the same chain
    // `resolve_vault_for_trait` uses today (project `.xv.toml` env vault →
    // context current vault → `default_vault` → local `default_vault` →
    // `"default"`), preserving the Azure no-vault hard-error as `Err`.
    let active_backend = config.effective_backend_name().to_string();
    let backend_names = known_backend_names(config);
    let backend_name_refs: Vec<&str> = backend_names.iter().map(|s| s.as_str()).collect();

    let vault = degenerate_default_vault(config, cwd, context_manager).await?;
    let alias = degenerate_alias(&vault, &backend_name_refs);
    let degenerate_entry = WorkspaceEntryConfig {
        vault,
        backend: Some(active_backend.clone()),
        alias: Some(alias),
        default: true,
    };
    let ws = build_workspace(
        std::slice::from_ref(&degenerate_entry),
        &active_backend,
        WorkspaceSource::Degenerate,
        &backend_name_refs,
    )?;
    Ok(Some(ws))
}

/// Pick the degenerate workspace-of-one's single alias.
///
/// Prefers the vault name so natural `xv get <vault>:name` addressing works,
/// but only when it is a charset-valid alias that does not collide with a
/// registry backend name (a collision would make [`Workspace::validate`]
/// reject the synthesized workspace and break the never-`None` invariant).
/// Otherwise falls back to a synthetic that is guaranteed to satisfy both
/// constraints. The alias never affects cache-key identity — that is keyed on
/// the entry's `backend` (`config.effective_backend_name()`), not the alias.
fn degenerate_alias(vault: &str, backend_names: &[&str]) -> String {
    if is_valid_alias_charset(vault) && !backend_names.contains(&vault) {
        return vault.to_string();
    }
    let mut candidate = "default".to_string();
    let mut n = 0;
    while backend_names.contains(&candidate.as_str()) {
        n += 1;
        candidate = format!("default-{n}");
    }
    candidate
}

/// `true` when the config's active (top-level) backend is Azure.
///
/// Mirrors `crate::cli::helpers::requested_backend_kind` without depending on
/// the CLI layer: named backends are only ever AWS/Local, so the active
/// backend is Azure exactly when the effective name is not a named-backend
/// key and parses to [`BackendKind::Azure`] (which includes the
/// default-when-unset case, `effective_backend_name() == "azure"`).
fn active_kind_is_azure(config: &Config) -> bool {
    let name = config.effective_backend_name();
    if config.named_backends.contains_key(name) {
        return false;
    }
    matches!(name.parse::<BackendKind>(), Ok(BackendKind::Azure))
}

/// Resolve the vault for the degenerate workspace-of-one, mirroring
/// `Config::resolve_vault_name(None)` (src/config/settings.rs) then the
/// Azure-hard-error / local-fallback rule of `resolve_vault_for_trait`
/// (src/cli/helpers.rs).
///
/// The resolution chain is replicated here (rather than calling
/// `Config::resolve_vault_name`) so it uses the injected `cwd` and
/// `context_manager` — the same parameterization that keeps
/// [`resolve_workspace_from`] hermetic under `cargo test`. The one behavioral
/// difference from `resolve_vault_name` is that the cross-boundary `.xv.toml`
/// notice (a stderr line) is not re-emitted here, since the workspace-overlay
/// pass above already inspected the same project config.
async fn degenerate_default_vault(
    config: &Config,
    cwd: Option<&std::path::Path>,
    context_manager: &crate::config::ContextManager,
) -> Result<String> {
    let resolved: Result<String> = async {
        // 1. Project `.xv.toml` env-profile vault (walk up from cwd).
        if let Some(cwd) = cwd {
            if let Ok(Some((_path, proj_cfg))) =
                crate::config::project::find_project_config(cwd).await
            {
                if let Some((_name, profile)) =
                    crate::config::project::resolve_env(&proj_cfg, config.env_flag.as_deref())?
                {
                    if let Some(v) = profile.vault.as_deref() {
                        return Ok(v.to_string());
                    }
                }
            }
        }

        // 2. Context current vault.
        if let Some(v) = context_manager.current_vault() {
            return Ok(v.to_string());
        }

        // 3. Config default vault.
        if !config.default_vault.is_empty() {
            return Ok(config.default_vault.clone());
        }

        Err(CrosstacheError::config(
            "No vault specified. Use --vault, set context with 'xv context use', or configure default_vault",
        ))
    }
    .await;

    match resolved {
        Ok(name) => Ok(name),
        // Azure keeps the legacy hard-error: no implicit fallback.
        Err(e) if active_kind_is_azure(config) => Err(e),
        // Local / future offline backends fall back to their configured
        // default vault, then the literal `"default"`.
        Err(_) => {
            if let Some(ref local) = config.local {
                if let Some(ref v) = local.default_vault {
                    return Ok(v.clone());
                }
            }
            Ok("default".to_string())
        }
    }
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

    /// A local backend with a configured default vault and NO cx/`.xv.toml`
    /// workspace resolves to the degenerate workspace-of-one — never `None`.
    #[tokio::test]
    async fn resolve_degenerate_for_local_only() {
        use crate::config::settings::LocalConfig;

        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            local: Some(LocalConfig {
                store_path: None,
                key_file: None,
                default_vault: Some("mystore".to_string()),
                encrypt_metadata: None,
                opaque_filenames: None,
            }),
            ..Default::default()
        };
        let context_manager = crate::config::ContextManager::default();

        let resolved = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect("must not error")
            .expect("degenerate workspace-of-one must be synthesized, never None");

        assert_eq!(resolved.source, WorkspaceSource::Degenerate);
        assert!(!resolved.is_configured());
        assert_eq!(resolved.entries.len(), 1);
        let entry = resolved.default_entry().unwrap();
        assert!(entry.default);
        assert_eq!(entry.vault, "mystore");
        // Cache-key identity: the degenerate entry's backend MUST equal
        // `config.effective_backend_name()` so writes invalidate the exact
        // cache entry the no-workspace read path keys on.
        assert_eq!(entry.backend, config.effective_backend_name());
        assert_eq!(entry.backend, "local");
        // Vault name is charset-valid and not a backend name, so it's used
        // verbatim as the alias.
        assert_eq!(entry.alias, "mystore");
    }

    /// A local backend with no vault configured anywhere falls back through
    /// `local.default_vault` → `"default"` (never an error, never `None`).
    #[tokio::test]
    async fn resolve_degenerate_local_falls_back_to_default_literal() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            ..Default::default()
        };
        let context_manager = crate::config::ContextManager::default();

        let resolved = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect("must not error")
            .expect("degenerate workspace-of-one must be synthesized, never None");

        assert_eq!(resolved.source, WorkspaceSource::Degenerate);
        assert_eq!(resolved.default_entry().unwrap().vault, "default");
    }

    /// `resolve_workspace` never yields `Ok(None)`: an active Azure backend
    /// with no vault configured surfaces the no-vault hard-error as `Err`,
    /// not a silent `"default"` vault — preserving the legacy Azure UX.
    #[tokio::test]
    async fn resolve_degenerate_azure_no_vault_errors() {
        let temp = tempfile::tempdir().unwrap();
        // `backend: None` ⇒ effective backend is "azure" (the default).
        let config = Config::default();
        let context_manager = crate::config::ContextManager::default();

        let err = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect_err("Azure + no vault must be an Err, not Ok(None) or a 'default' vault");
        assert!(err.to_string().contains("No vault specified"), "{err}");
    }

    /// `is_configured()` distinguishes the degenerate workspace from a real
    /// context/project one.
    #[tokio::test]
    async fn is_configured_true_for_context_false_for_degenerate() {
        // Degenerate.
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            ..Default::default()
        };
        let degenerate = resolve_workspace_from(
            &config,
            Some(temp.path()),
            &crate::config::ContextManager::default(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!degenerate.is_configured());

        // Context.
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
        let context_ws = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(context_ws.source, WorkspaceSource::Context);
        assert!(context_ws.is_configured());
    }

    /// The degenerate alias avoids colliding with a registry backend name,
    /// which would otherwise fail `Workspace::validate` and break the
    /// never-`None` invariant. Here the resolved vault is literally `"local"`
    /// (a backend name), so a synthetic non-colliding alias is chosen.
    #[tokio::test]
    async fn resolve_degenerate_alias_avoids_backend_name_collision() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            backend: Some("local".to_string()),
            default_vault: "local".to_string(),
            ..Default::default()
        };
        let context_manager = crate::config::ContextManager::default();

        let resolved = resolve_workspace_from(&config, Some(temp.path()), &context_manager)
            .await
            .expect("must not error")
            .expect(
                "must synthesize a valid degenerate workspace despite the vault name colliding",
            );

        let entry = resolved.default_entry().unwrap();
        assert_eq!(entry.vault, "local");
        assert_ne!(
            entry.alias, "local",
            "alias must not collide with a backend name"
        );
        assert_eq!(entry.alias, "default");
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
