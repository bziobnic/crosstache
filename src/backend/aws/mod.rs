//! AWS Secrets Manager backend.

pub mod auth;
pub mod config;
pub mod encoding;
pub mod errors;
pub mod metadata;
pub mod models;
pub mod secrets;
pub mod vaults;

use std::sync::Arc;

use crate::backend::error::BackendError;
use crate::backend::{
    Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend, VaultBackend,
};
use crate::config::settings::AwsConfig;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;

pub struct AwsBackend {
    secrets_impl: Arc<secrets::AwsSecretBackend>,
    vaults_impl: Arc<vaults::AwsVaultBackend>,
}

impl AwsBackend {
    /// Build a backend from config + per-invocation overrides.
    /// Async because `aws-config::load()` is async.
    pub async fn new(
        aws_cfg: &AwsConfig,
        region_override: Option<String>,
        profile_override: Option<String>,
    ) -> Result<Self, BackendError> {
        let client: SecretsManagerClient =
            auth::build_client(aws_cfg, region_override, profile_override).await?;
        let client = Arc::new(client);
        Ok(Self {
            secrets_impl: Arc::new(secrets::AwsSecretBackend::new(client.clone())),
            vaults_impl: Arc::new(vaults::AwsVaultBackend::new(client)),
        })
    }
}

#[async_trait::async_trait]
impl Backend for AwsBackend {
    fn name(&self) -> &'static str {
        "aws"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Aws
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_vaults: true,
            has_file_storage: false,
            has_rbac: false,
            has_audit: false,
            has_versioning: true,
            has_soft_delete: true,
            has_secret_rotation: false,
            has_groups: true,
            has_folders: true,
            has_notes: true,
            has_expiry: true,
            max_secret_size: Some(65_536),
            max_name_length: Some(encoding::MAX_NAME_LEN),
            name_charset: NameCharset::AwsRelaxed,
        }
    }

    fn secrets(&self) -> &dyn SecretBackend {
        self.secrets_impl.as_ref()
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(self.vaults_impl.as_ref())
    }

    async fn health_check(&self) -> Result<(), BackendError> {
        self.secrets_impl.health_check().await
    }
}
