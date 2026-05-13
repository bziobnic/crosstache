//! `AwsSecretBackend` impl `SecretBackend`.

use crate::backend::SecretBackend;
use crate::backend::error::BackendError;
use crate::secret::manager::{SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest};
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use std::sync::Arc;

pub struct AwsSecretBackend {
    pub(crate) client: Arc<SecretsManagerClient>,
}

impl AwsSecretBackend {
    pub fn new(client: Arc<SecretsManagerClient>) -> Self {
        Self { client }
    }

    /// Lightweight health check: list secrets with limit=1.
    pub async fn health_check(&self) -> Result<(), BackendError> {
        self.client
            .list_secrets()
            .max_results(1)
            .send()
            .await
            .map_err(|e| BackendError::Network(format!("aws health check: {e}")))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SecretBackend for AwsSecretBackend {
    // Required methods — real impls added in Tasks 13-22.

    async fn set_secret(
        &self,
        _vault: &str,
        _request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("set_secret not yet implemented".into()))
    }

    async fn get_secret(
        &self,
        _vault: &str,
        _name: &str,
        _include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("get_secret not yet implemented".into()))
    }

    async fn get_secret_version(
        &self,
        _vault: &str,
        _name: &str,
        _version: &str,
        _include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("get_secret_version not yet implemented".into()))
    }

    async fn list_secrets(
        &self,
        _vault: &str,
        _group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        Err(BackendError::Unsupported("list_secrets not yet implemented".into()))
    }

    async fn delete_secret(&self, _vault: &str, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("delete_secret not yet implemented".into()))
    }

    async fn update_secret(
        &self,
        _vault: &str,
        _name: &str,
        _request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported("update_secret not yet implemented".into()))
    }
}
