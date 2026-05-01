use crate::config::Config;
use crate::secret::manager::{SecretProperties, SecretSummary};
use crate::vault::models::VaultSummary;
use ratatui::widgets::ListState;
use std::collections::HashMap;
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
            value_revealed: false,
            value_loading: false,
            value_debounce: None,
            overlay: Overlay::None,
            history: HashMap::new(),
            audit: HashMap::new(),
            toast: None,
            clipboard_countdown: None,
            quit: false,
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
}
