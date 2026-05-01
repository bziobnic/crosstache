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
                    config.azure_credential_priority.clone()
                ).map_err(|e| CrosstacheError::authentication(format!("auth: {e}")))?
            );
            let vm = VaultManager::new(auth, config.subscription_id.clone(), config.no_color)?;
            vm.vault_ops().list_vaults(Some(&config.subscription_id), None).await
        }.await;
        let msg = match result {
            Ok(vaults) => Message::VaultsLoaded(vaults),
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

// STUBS — Tasks 5/6/10 replace each. They take the same parameters as the
// real versions so the runtime in mod.rs can call them today.

pub fn spawn_load_secrets(config: Config, vault: String, tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use crate::auth::provider::DefaultAzureCredentialProvider;
        use crate::secret::manager::SecretManager;
        let result: Result<_, CrosstacheError> = async {
            let auth = std::sync::Arc::new(
                DefaultAzureCredentialProvider::with_credential_priority(
                    config.azure_credential_priority.clone()
                ).map_err(|e| CrosstacheError::authentication(format!("auth: {e}")))?
            );
            let sm = SecretManager::new(auth, config.no_color);
            sm.secret_ops().list_secrets(&vault, None).await
        }.await;
        let msg = match result {
            Ok(secrets) => Message::SecretsLoaded { vault, secrets },
            Err(e) => Message::Error(e),
        };
        let _ = tx.send(msg).await;
    })
}

pub fn spawn_load_value(_config: Config, _vault: String, _name: String, _tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}

pub fn spawn_load_history(_config: Config, _vault: String, _name: String, _tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}

pub fn spawn_load_audit(_config: Config, _vault: String, _name: Option<String>, _tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}
