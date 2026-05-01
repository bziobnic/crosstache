use crate::config::Config;
use crate::error::CrosstacheError;
use crate::tui::message::Message;
use tokio::sync::mpsc::Sender;

pub fn spawn_load_vaults(config: Config, tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use crate::auth::provider::DefaultAzureCredentialProvider;
        use crate::vault::manager::VaultManager;
        let result: Result<_, CrosstacheError> = async {
            let auth = std::sync::Arc::new(
                DefaultAzureCredentialProvider::with_credential_priority(
                    config.azure_credential_priority.clone(),
                )
                .map_err(|e| CrosstacheError::authentication(format!("auth: {e}")))?,
            );
            let vm = VaultManager::new(auth, config.subscription_id.clone(), config.no_color)?;
            vm.vault_ops()
                .list_vaults(Some(&config.subscription_id), None)
                .await
        }
        .await;
        let msg = match result {
            Ok(vaults) => Message::VaultsLoaded(vaults),
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

// STUBS — Tasks 5/6/10 replace each. They take the same parameters as the
// real versions so the runtime in mod.rs can call them today.

pub fn spawn_load_secrets(
    config: Config,
    vault: String,
    tx: Sender<Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use crate::auth::provider::DefaultAzureCredentialProvider;
        use crate::secret::manager::SecretManager;
        let result: Result<_, CrosstacheError> = async {
            let auth = std::sync::Arc::new(
                DefaultAzureCredentialProvider::with_credential_priority(
                    config.azure_credential_priority.clone(),
                )
                .map_err(|e| CrosstacheError::authentication(format!("auth: {e}")))?,
            );
            let sm = SecretManager::new(auth, config.no_color);
            sm.secret_ops().list_secrets(&vault, None).await
        }
        .await;
        let msg = match result {
            Ok(secrets) => Message::SecretsLoaded { vault, secrets },
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

pub fn spawn_load_value(
    config: Config,
    vault: String,
    name: String,
    tx: Sender<Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use crate::auth::provider::DefaultAzureCredentialProvider;
        use crate::secret::manager::SecretManager;
        let result: Result<_, CrosstacheError> = async {
            let auth = std::sync::Arc::new(
                DefaultAzureCredentialProvider::with_credential_priority(
                    config.azure_credential_priority.clone(),
                )
                .map_err(|e| CrosstacheError::authentication(format!("auth: {e}")))?,
            );
            let sm = SecretManager::new(auth, config.no_color);
            sm.secret_ops().get_secret(&vault, &name, true).await
        }
        .await;
        let msg = match result {
            Ok(props) => match props.value {
                Some(v) => Message::ValueLoaded {
                    vault,
                    name,
                    value: zeroize::Zeroizing::new(v.as_str().to_string()),
                },
                None => Message::Error(CrosstacheError::config(format!(
                    "secret {name} has no value"
                ))),
            },
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

pub fn spawn_load_history(
    config: Config,
    vault: String,
    name: String,
    tx: Sender<Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use crate::auth::provider::DefaultAzureCredentialProvider;
        use crate::secret::manager::SecretManager;
        let result: Result<_, CrosstacheError> = async {
            let auth = std::sync::Arc::new(
                DefaultAzureCredentialProvider::with_credential_priority(
                    config.azure_credential_priority.clone(),
                )
                .map_err(|e| CrosstacheError::authentication(format!("auth: {e}")))?,
            );
            let sm = SecretManager::new(auth, config.no_color);
            sm.secret_ops().get_secret_versions(&vault, &name).await
        }
        .await;
        let msg = match result {
            Ok(versions) => Message::HistoryLoaded {
                vault,
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
