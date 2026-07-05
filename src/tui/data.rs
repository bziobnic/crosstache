use std::sync::Arc;

use crate::backend::Backend;
use crate::config::Config;
use crate::error::CrosstacheError;
use crate::tui::message::Message;
use tokio::sync::mpsc::Sender;

/// Resolve the backend the TUI data tasks read through: the one handed in when
/// the shared registry built at startup, otherwise constructed on demand from
/// config (mirrors the CLI's option-A rebuild — a startup init failure surfaces
/// as a clean error). Every TUI read then goes through the `Backend` trait,
/// with no legacy `SecretManager`/`VaultManager` construction.
async fn resolve_tui_backend(
    backend: Option<Arc<dyn Backend>>,
    config: &Config,
) -> Result<Arc<dyn Backend>, CrosstacheError> {
    match backend {
        Some(be) => Ok(be),
        None => {
            let registry = crate::backend::BackendRegistry::from_config(config)
                .map_err(|e| CrosstacheError::config(e.to_string()))?;
            Ok(registry.active_arc())
        }
    }
}

pub fn spawn_load_vaults(
    config: Config,
    tx: Sender<Message>,
    backend: Option<Arc<dyn Backend>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let be = match resolve_tui_backend(backend, &config).await {
            Ok(be) => be,
            Err(e) => {
                let _ = tx.send(Message::Error(e)).await;
                return;
            }
        };
        let result = match be.vaults() {
            Some(vb) => vb.list_vaults(None).await.map_err(CrosstacheError::from),
            None => Err(CrosstacheError::config(
                "active backend does not support vault listing",
            )),
        };
        let msg = match result {
            Ok(vaults) => Message::VaultsLoaded(vaults),
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

/// `vault` is the actual name queried against the backend; `key` is what the
/// resulting `Message::SecretsLoaded` is tagged with — normally the same
/// string, but in a workspace they diverge: `key` is the workspace ALIAS
/// (`app.vaults`/`secrets_by_vault` are keyed by alias, since two entries
/// can share the same real vault name on different backends), while `vault`
/// is that entry's real vault name on its own backend.
pub fn spawn_load_secrets(
    config: Config,
    vault: String,
    key: String,
    tx: Sender<Message>,
    backend: Option<Arc<dyn Backend>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let be = match resolve_tui_backend(backend, &config).await {
            Ok(be) => be,
            Err(e) => {
                let _ = tx.send(Message::Error(e)).await;
                return;
            }
        };
        let result = be
            .secrets()
            .list_secrets(&vault, None)
            .await
            .map_err(CrosstacheError::from);
        let msg = match result {
            Ok(secrets) => Message::SecretsLoaded {
                vault: key,
                secrets,
            },
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

/// `vault` is the actual name queried against the backend; `key` is what the
/// resulting `Message::ValueLoaded` is tagged with (mirrors
/// `spawn_load_secrets`'s vault/key split — Bugbot HIGH fix, round 2: this
/// used to have no such split at all, so a workspace entry's value was
/// queried against the ALIAS as if it were the real vault name).
pub fn spawn_load_value(
    config: Config,
    vault: String,
    key: String,
    name: String,
    tx: Sender<Message>,
    backend: Option<Arc<dyn Backend>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let be = match resolve_tui_backend(backend, &config).await {
            Ok(be) => be,
            Err(e) => {
                let _ = tx.send(Message::Error(e)).await;
                return;
            }
        };
        let result = be
            .secrets()
            .get_secret(&vault, &name, true)
            .await
            .map_err(CrosstacheError::from);
        let msg = match result {
            Ok(props) => {
                let content_type = props.content_type.clone();
                match props.value {
                    Some(v) => Message::ValueLoaded {
                        vault: key,
                        name,
                        value: zeroize::Zeroizing::new(v.as_str().to_string()),
                        content_type,
                    },
                    None => Message::Error(CrosstacheError::config(format!(
                        "secret {name} has no value"
                    ))),
                }
            }
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

/// `vault` is the actual name queried against the backend; `key` is what the
/// resulting `Message::HistoryLoaded` is tagged with (same split as
/// `spawn_load_value`/`spawn_load_secrets` — Bugbot HIGH fix, round 2).
pub fn spawn_load_history(
    config: Config,
    vault: String,
    key: String,
    name: String,
    tx: Sender<Message>,
    backend: Option<Arc<dyn Backend>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let be = match resolve_tui_backend(backend, &config).await {
            Ok(be) => be,
            Err(e) => {
                let _ = tx.send(Message::Error(e)).await;
                return;
            }
        };
        let result = be
            .secrets()
            .list_versions(&vault, &name)
            .await
            .map_err(CrosstacheError::from);
        let msg = match result {
            Ok(versions) => Message::HistoryLoaded {
                vault: key,
                name,
                versions,
            },
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

/// Audit is a placeholder for v0.7.0; real Activity Log access lands in v0.7.1.
pub fn spawn_load_audit(
    _config: Config,
    vault: String,
    name: Option<String>,
    tx: Sender<Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = tx
            .send(Message::AuditLoaded {
                vault,
                name,
                events: vec![
                    "Audit log integration is not yet wired up — see docs/tui.md".to_string(),
                ],
            })
            .await;
    })
}
