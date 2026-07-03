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
        /// The fetched secret's content type. Record-types plan (Bugbot
        /// LOW review): the TUI detail pane must gate value-line masking
        /// on the actual content-type marker at reveal time, not just the
        /// list-summary's `xv-type` tag — a record whose `xv-type` tag is
        /// absent/stripped but whose content type still says
        /// `application/vnd.xv.record` is still a record, and printing its
        /// raw envelope JSON would violate "content-type decides
        /// record-ness".
        content_type: String,
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
