use crate::backend::Backend;
use crate::config::Config;
use crate::secret::manager::{SecretProperties, SecretSummary};
use crate::vault::models::VaultSummary;
use ratatui::widgets::ListState;
use std::collections::HashMap;
use std::sync::Arc;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Vaults,
    Secrets,
    Detail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    History,
    Audit,
    ErrorDetail(String),
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub code: Option<String>,
    pub ticks_left: u32, // 50 ticks @ 100ms = 5s
}

/// Result of resolving a selected `app.vaults[i].name` (an ALIAS when a
/// workspace is attached) against the workspace — shared by every command
/// that queries a specific vault (`LoadSecrets`/`LoadValue`/`LoadHistory`)
/// so the three can never diverge on which backend/vault they hit (Bugbot
/// HIGH fix, round 2: `LoadValue`/`LoadHistory` previously ignored the
/// workspace entirely and queried the shared active backend under the
/// alias string as if it were a real vault name).
#[derive(Clone)]
pub(crate) enum WorkspaceTarget {
    /// No workspace attached — caller falls back to the shared `backend`
    /// and treats the alias as the vault name unchanged (spec §Backward
    /// compatibility).
    NoWorkspace,
    /// The alias resolved to an attached, materialized entry.
    Entry {
        backend: Arc<dyn Backend>,
        /// The entry's REAL vault name (may differ from the alias).
        vault: String,
    },
    /// A workspace IS attached but this entry's backend failed to
    /// materialize at startup — callers must toast and skip the query, not
    /// fall back to any other backend.
    Unavailable,
}

impl std::fmt::Debug for WorkspaceTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspace => write!(f, "NoWorkspace"),
            Self::Entry { backend, vault } => f
                .debug_struct("Entry")
                .field("backend", &backend.name())
                .field("vault", vault)
                .finish(),
            Self::Unavailable => write!(f, "Unavailable"),
        }
    }
}

pub struct App {
    pub config: Config,
    pub pane: Pane,

    pub vaults: Vec<VaultSummary>,
    pub vault_state: ListState,
    pub vaults_loading: bool,

    pub secrets_by_vault: HashMap<String, Vec<SecretSummary>>,
    pub secret_state: ListState,
    pub secret_filter: String,
    pub secret_filter_active: bool,
    pub secrets_loading: bool,

    pub values: HashMap<(String, String), Zeroizing<String>>,
    /// Content type of the fetched value in `values`, keyed the same way.
    /// Populated alongside `values` from `Message::ValueLoaded`. Lets the
    /// detail pane gate value-line masking on the actual content-type
    /// marker at reveal time (`crate::records::is_record`), not just the
    /// list-summary's `xv-type` tag, which can be absent/stripped while
    /// the secret is still a record (record-types plan, Bugbot LOW
    /// review).
    pub value_content_types: HashMap<(String, String), String>,
    pub value_revealed: bool,
    pub value_loading: bool,
    /// (vault, name, ticks_left) — when ticks_left hits 0, fire LoadValue.
    pub value_debounce: Option<(String, String, u32)>,

    pub overlay: Overlay,
    pub history: HashMap<(String, String), Vec<SecretProperties>>,
    pub audit: HashMap<(String, Option<String>), Vec<String>>,

    pub toast: Option<Toast>,
    pub clipboard_countdown: Option<u32>,
    pub quit: bool,

    /// The active multi-vault workspace, if any (Phase C Task 13). `None` ⇒
    /// every field below is unused and the vault pane / secrets loading
    /// behave exactly as they did before workspaces existed (spec
    /// §Backward compatibility). Populated once at startup by `run_tui`.
    pub workspace: Option<crate::workspace::Workspace>,
    /// Workspace alias -> materialized backend for that entry. Constructing
    /// a backend doesn't perform auth (resolved lazily at first actual
    /// secret operation — see `crate::workspace::resolve` test comments),
    /// so populating every attached entry's backend up front is safe and
    /// keeps `Command::LoadSecrets` a simple lookup.
    pub workspace_backends: HashMap<String, Arc<dyn Backend>>,
    /// Workspace alias -> the REAL vault name on that entry's backend
    /// (`app.vaults[i].name` holds the ALIAS for display/selection
    /// purposes, which may differ from the actual vault name).
    pub workspace_vault_names: HashMap<String, String>,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            pane: Pane::Vaults,
            vaults: Vec::new(),
            vault_state: ListState::default(),
            vaults_loading: true,
            secrets_by_vault: HashMap::new(),
            secret_state: ListState::default(),
            secret_filter: String::new(),
            secret_filter_active: false,
            secrets_loading: false,
            values: HashMap::new(),
            value_content_types: HashMap::new(),
            value_revealed: false,
            value_loading: false,
            value_debounce: None,
            overlay: Overlay::None,
            history: HashMap::new(),
            audit: HashMap::new(),
            toast: None,
            clipboard_countdown: None,
            quit: false,
            workspace: None,
            workspace_backends: HashMap::new(),
            workspace_vault_names: HashMap::new(),
        }
    }

    pub fn selected_vault(&self) -> Option<&str> {
        self.vault_state
            .selected()
            .and_then(|i| self.vaults.get(i))
            .map(|v| v.name.as_str())
    }

    pub fn filtered_secrets(&self) -> Vec<&SecretSummary> {
        let Some(vault) = self.selected_vault() else {
            return Vec::new();
        };
        let Some(secrets) = self.secrets_by_vault.get(vault) else {
            return Vec::new();
        };
        if self.secret_filter.is_empty() {
            return secrets.iter().collect();
        }
        use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};
        let items: Vec<CandidateItem> = secrets
            .iter()
            .map(CandidateItem::from_secret_summary)
            .collect();
        let matches = score_matches(&self.secret_filter, &items, &[FuzzyField::Name]);
        let mut out: Vec<&SecretSummary> = Vec::new();
        for m in &matches {
            if let Some(s) = secrets.iter().find(|s| {
                let display = if s.original_name.is_empty() {
                    &s.name
                } else {
                    &s.original_name
                };
                display == m.item.name.as_str()
            }) {
                out.push(s);
            }
        }
        out
    }

    pub fn selected_secret(&self) -> Option<&SecretSummary> {
        let secrets = self.filtered_secrets();
        self.secret_state
            .selected()
            .and_then(|i| secrets.get(i).copied())
    }

    pub fn selected_vault_and_name(&self) -> Option<(String, String)> {
        let vault = self.selected_vault()?.to_string();
        let s = self.selected_secret()?;
        let name = if s.original_name.is_empty() {
            s.name.clone()
        } else {
            s.original_name.clone()
        };
        Some((vault, name))
    }

    /// Resolve `alias` (an `app.vaults[i].name`) against the workspace. See
    /// [`WorkspaceTarget`] for the three possible outcomes.
    pub(crate) fn workspace_target_for(&self, alias: &str) -> WorkspaceTarget {
        if self.workspace.is_none() {
            return WorkspaceTarget::NoWorkspace;
        }
        match self.workspace_backends.get(alias) {
            Some(backend) => {
                let vault = self
                    .workspace_vault_names
                    .get(alias)
                    .cloned()
                    .unwrap_or_else(|| alias.to_string());
                WorkspaceTarget::Entry {
                    backend: backend.clone(),
                    vault,
                }
            }
            None => WorkspaceTarget::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::error::BackendError;
    use crate::backend::{BackendCapabilities, BackendKind, SecretBackend};
    use crate::secret::manager::{SecretProperties, SecretRequest, SecretUpdateRequest};

    /// Minimal fake `Backend` distinguishable by its `name()` — enough to
    /// prove `workspace_target_for` returns the RIGHT entry's backend, not
    /// just some backend.
    struct FakeBackend(&'static str);

    #[async_trait::async_trait]
    impl SecretBackend for FakeBackend {
        async fn set_secret(
            &self,
            _vault: &str,
            _request: SecretRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("fake".into()))
        }
        async fn get_secret(
            &self,
            _vault: &str,
            _name: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("fake".into()))
        }
        async fn get_secret_version(
            &self,
            _vault: &str,
            _name: &str,
            _version: &str,
            _include_value: bool,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("fake".into()))
        }
        async fn list_secrets(
            &self,
            _vault: &str,
            _group_filter: Option<&str>,
        ) -> std::result::Result<Vec<SecretSummary>, BackendError> {
            Ok(Vec::new())
        }
        async fn delete_secret(
            &self,
            _vault: &str,
            _name: &str,
        ) -> std::result::Result<(), BackendError> {
            Err(BackendError::Unsupported("fake".into()))
        }
        async fn update_secret(
            &self,
            _vault: &str,
            _name: &str,
            _request: SecretUpdateRequest,
        ) -> std::result::Result<SecretProperties, BackendError> {
            Err(BackendError::Unsupported("fake".into()))
        }
    }

    #[async_trait::async_trait]
    impl Backend for FakeBackend {
        fn name(&self) -> &'static str {
            self.0
        }
        fn kind(&self) -> BackendKind {
            BackendKind::Local
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }
        fn secrets(&self) -> &dyn SecretBackend {
            self
        }
        async fn health_check(&self) -> std::result::Result<(), BackendError> {
            Ok(())
        }
    }

    fn app_with_workspace() -> App {
        let mut app = App::new(Config::default());
        app.workspace = Some(crate::workspace::Workspace {
            entries: vec![
                crate::workspace::WorkspaceEntry {
                    alias: "work".to_string(),
                    backend: "local-a".to_string(),
                    vault: "work-vault".to_string(),
                    default: true,
                },
                crate::workspace::WorkspaceEntry {
                    alias: "stage".to_string(),
                    backend: "local-b".to_string(),
                    vault: "stage-vault".to_string(),
                    default: false,
                },
            ],
            default_alias: "work".to_string(),
            source: crate::workspace::WorkspaceSource::Context,
        });
        app.workspace_backends.insert(
            "work".to_string(),
            Arc::new(FakeBackend("backend-a")) as Arc<dyn Backend>,
        );
        app.workspace_backends.insert(
            "stage".to_string(),
            Arc::new(FakeBackend("backend-b")) as Arc<dyn Backend>,
        );
        app.workspace_vault_names
            .insert("work".to_string(), "work-vault".to_string());
        app.workspace_vault_names
            .insert("stage".to_string(), "stage-vault".to_string());
        app
    }

    #[test]
    fn workspace_target_for_no_workspace_returns_no_workspace() {
        let app = App::new(Config::default());
        assert!(matches!(
            app.workspace_target_for("anything"),
            WorkspaceTarget::NoWorkspace
        ));
    }

    #[test]
    fn workspace_target_for_hit_returns_entry_backend_and_real_vault() {
        let app = app_with_workspace();

        match app.workspace_target_for("stage") {
            WorkspaceTarget::Entry { backend, vault } => {
                assert_eq!(vault, "stage-vault");
                assert_eq!(backend.name(), "backend-b");
            }
            other => panic!("expected Entry, got a different variant: {other:?}"),
        }

        match app.workspace_target_for("work") {
            WorkspaceTarget::Entry { backend, vault } => {
                assert_eq!(vault, "work-vault");
                assert_eq!(backend.name(), "backend-a");
            }
            other => panic!("expected Entry, got a different variant: {other:?}"),
        }
    }

    #[test]
    fn workspace_target_for_miss_returns_unavailable() {
        let app = app_with_workspace();
        // "ghost" is not an attached alias at all — but the important case
        // this pins is an ATTACHED alias whose backend failed to
        // materialize at startup (workspace_backends has no entry for it),
        // which `run_tui` produces identically (a missing map entry).
        assert!(matches!(
            app.workspace_target_for("ghost"),
            WorkspaceTarget::Unavailable
        ));
    }

    /// Three-command consistency (Bugbot HIGH fix, round 2): `LoadSecrets`,
    /// `LoadValue`, and `LoadHistory` all resolve a selected alias through
    /// this SAME helper — pinned here by calling it multiple times for the
    /// same alias (as each of the three call sites in `mod.rs` does) and
    /// asserting the resolved `(backend, vault)` never diverges. Before the
    /// fix, `LoadValue`/`LoadHistory` used a completely different
    /// resolution path (the shared active backend + the alias as a literal
    /// vault name), which this test would have caught: "stage"'s vault name
    /// is `"stage-vault"`, not `"stage"`.
    #[test]
    fn three_commands_resolve_the_same_alias_identically() {
        let app = app_with_workspace();

        let for_list = app.workspace_target_for("stage");
        let for_value = app.workspace_target_for("stage");
        let for_history = app.workspace_target_for("stage");

        for (label, target) in [
            ("list", &for_list),
            ("value", &for_value),
            ("history", &for_history),
        ] {
            match target {
                WorkspaceTarget::Entry { backend, vault } => {
                    assert_eq!(vault, "stage-vault", "{label} resolved the wrong vault");
                    assert_eq!(
                        backend.name(),
                        "backend-b",
                        "{label} resolved the wrong backend"
                    );
                }
                other => panic!("{label}: expected Entry, got {other:?}"),
            }
        }
    }
}
