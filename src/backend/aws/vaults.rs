//! `AwsVaultBackend` impl `VaultBackend`.

use crate::backend::VaultBackend;
use crate::backend::error::BackendError;
use crate::vault::models::{VaultCreateRequest, VaultProperties, VaultSummary};
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use std::sync::Arc;

pub struct AwsVaultBackend {
    pub(crate) client: Arc<SecretsManagerClient>,
}

impl AwsVaultBackend {
    pub fn new(client: Arc<SecretsManagerClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl VaultBackend for AwsVaultBackend {
    // Required methods — real impls added in Tasks 23-26.

    async fn create_vault(
        &self,
        _request: VaultCreateRequest,
    ) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("create_vault not yet implemented".into()))
    }

    async fn get_vault(&self, _name: &str) -> Result<VaultProperties, BackendError> {
        Err(BackendError::Unsupported("get_vault not yet implemented".into()))
    }

    async fn list_vaults(&self) -> Result<Vec<VaultSummary>, BackendError> {
        Err(BackendError::Unsupported("list_vaults not yet implemented".into()))
    }

    async fn delete_vault(&self, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("delete_vault not yet implemented".into()))
    }
}
