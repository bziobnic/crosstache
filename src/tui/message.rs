use crate::secret::manager::{SecretProperties, SecretSummary};
use crate::vault::models::VaultSummary;

#[derive(Debug)]
pub enum Message {
    KeyPress(crossterm::event::KeyEvent),
    VaultsLoaded(Vec<VaultSummary>),
    SecretsLoaded {
        vault: String,
        secrets: Vec<SecretSummary>,
    },
    ValueLoaded {
        vault: String,
        name: String,
        value: zeroize::Zeroizing<String>,
    },
    HistoryLoaded {
        vault: String,
        name: String,
        versions: Vec<SecretProperties>,
    },
    AuditLoaded {
        vault: String,
        name: Option<String>,
        events: Vec<String>,
    },
    Tick,
    Error(crate::error::CrosstacheError),
}
