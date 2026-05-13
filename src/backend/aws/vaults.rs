//! `AwsVaultBackend` impl `VaultBackend`.

use crate::backend::VaultBackend;
use crate::backend::error::BackendError;
use crate::vault::models::{VaultCreateRequest, VaultProperties, VaultSummary};
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use aws_sdk_secretsmanager::types::Tag;
use chrono::Utc;
use std::collections::HashMap;
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
        request: VaultCreateRequest,
    ) -> Result<VaultProperties, BackendError> {
        use crate::backend::aws::encoding::marker_name;
        use crate::backend::aws::metadata::{TAG_TYPE, TAG_VALUE_VAULT_MARKER};

        let marker = marker_name(&request.name);
        let mut tags: Vec<Tag> = Vec::new();
        tags.push(
            Tag::builder()
                .key(TAG_TYPE)
                .value(TAG_VALUE_VAULT_MARKER)
                .build(),
        );
        tags.push(
            Tag::builder()
                .key("xv:vault_name")
                .value(&request.name)
                .build(),
        );
        tags.push(
            Tag::builder()
                .key("xv:created_at")
                .value(Utc::now().to_rfc3339())
                .build(),
        );
        if let Some(ref user_tags) = request.tags {
            for (k, v) in user_tags {
                if !k.starts_with("xv:") {
                    tags.push(Tag::builder().key(k).value(v).build());
                }
            }
        }

        self.client
            .create_secret()
            .name(&marker)
            .secret_string("{}")
            .description(format!("xv vault marker for '{}'", request.name))
            .set_tags(Some(tags))
            .send()
            .await
            .map_err(super::errors::from_create)?;

        // Build VaultProperties manually with all fields
        let now = Utc::now();
        Ok(VaultProperties {
            id: format!("vault-{}", request.name),
            name: request.name.clone(),
            location: request.location.clone(),
            resource_group: request.resource_group.clone(),
            subscription_id: request.subscription_id.clone(),
            tenant_id: String::new(),
            uri: format!("https://{}.vault.aws.net/", request.name),
            enabled_for_deployment: request.enabled_for_deployment.unwrap_or(false),
            enabled_for_disk_encryption: request.enabled_for_disk_encryption.unwrap_or(false),
            enabled_for_template_deployment: request.enabled_for_template_deployment.unwrap_or(false),
            soft_delete_retention_in_days: request.soft_delete_retention_in_days.unwrap_or(30),
            purge_protection: request.purge_protection.unwrap_or(false),
            sku: request.sku.clone().unwrap_or_else(|| "standard".to_string()),
            access_policies: request.access_policies.clone().unwrap_or_default(),
            created_at: now,
            tags: request.tags.clone().unwrap_or_default(),
            enable_rbac_authorization: Some(false),
        })
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
