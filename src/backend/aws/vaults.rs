//! `AwsVaultBackend` impl `VaultBackend`.

use crate::backend::error::BackendError;
use crate::backend::VaultBackend;
use crate::vault::models::{VaultCreateRequest, VaultProperties, VaultSummary};
use aws_sdk_secretsmanager::types::Tag;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
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
            enabled_for_template_deployment: request
                .enabled_for_template_deployment
                .unwrap_or(false),
            soft_delete_retention_in_days: request.soft_delete_retention_in_days.unwrap_or(30),
            purge_protection: request.purge_protection.unwrap_or(false),
            sku: request
                .sku
                .clone()
                .unwrap_or_else(|| "standard".to_string()),
            access_policies: request.access_policies.clone().unwrap_or_default(),
            created_at: now,
            tags: request.tags.clone().unwrap_or_default(),
            enable_rbac_authorization: Some(false),
        })
    }

    async fn get_vault(
        &self,
        name: &str,
        _resource_group: Option<&str>,
    ) -> Result<VaultProperties, BackendError> {
        use crate::backend::aws::encoding::marker_name;
        let marker = marker_name(name);
        let describe = self
            .client
            .describe_secret()
            .secret_id(&marker)
            .send()
            .await
            .map_err(|e| match super::errors::from_describe(name, e) {
                BackendError::NotFound { .. } => BackendError::VaultNotFound {
                    name: name.to_string(),
                    suggestion: None,
                },
                other => other,
            })?;

        // Build VaultProperties manually from describe output
        let tags_list = describe.tags();
        let mut tags: HashMap<String, String> = HashMap::new();
        let mut vault_name = name.to_string();
        let mut created_at = Utc::now();

        for tag in tags_list {
            if let (Some(key), Some(value)) = (tag.key(), tag.value()) {
                if key == "xv:vault_name" {
                    vault_name = value.to_string();
                } else if key == "xv:created_at" {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(value) {
                        created_at = dt.with_timezone(&Utc);
                    }
                } else if !key.starts_with("xv:") {
                    tags.insert(key.to_string(), value.to_string());
                }
            }
        }

        Ok(VaultProperties {
            id: format!("vault-{}", name),
            name: vault_name,
            location: "aws".to_string(),
            resource_group: "default".to_string(),
            subscription_id: "default".to_string(),
            tenant_id: String::new(),
            uri: format!("https://{}.vault.aws.net/", name),
            enabled_for_deployment: false,
            enabled_for_disk_encryption: false,
            enabled_for_template_deployment: false,
            soft_delete_retention_in_days: 30,
            purge_protection: false,
            sku: "standard".to_string(),
            access_policies: Vec::new(),
            created_at,
            tags,
            enable_rbac_authorization: Some(false),
        })
    }

    async fn list_vaults(
        &self,
        _resource_group: Option<&str>,
    ) -> Result<Vec<VaultSummary>, BackendError> {
        use crate::backend::aws::metadata::{TAG_TYPE, TAG_VALUE_VAULT_MARKER};
        use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

        let mut next_token: Option<String> = None;
        let mut summaries: Vec<VaultSummary> = Vec::new();

        loop {
            let mut req = self
                .client
                .list_secrets()
                .max_results(100)
                .filters(
                    Filter::builder()
                        .key(FilterNameStringType::TagKey)
                        .values(TAG_TYPE)
                        .build(),
                )
                .filters(
                    Filter::builder()
                        .key(FilterNameStringType::TagValue)
                        .values(TAG_VALUE_VAULT_MARKER)
                        .build(),
                );
            if let Some(t) = &next_token {
                req = req.next_token(t.clone());
            }

            let out = req.send().await.map_err(super::errors::from_list)?;
            for entry in out.secret_list() {
                let aws_full_name = entry.name().unwrap_or("");
                if let Some(idx) = aws_full_name.rfind("/.xv-vault") {
                    let vault = &aws_full_name[..idx];
                    summaries.push(VaultSummary {
                        name: vault.to_string(),
                        location: "aws".to_string(),
                        resource_group: "default".to_string(),
                        status: "Active".to_string(),
                        created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string(),
                    });
                }
            }
            next_token = out.next_token().map(|s| s.to_string());
            if next_token.is_none() {
                break;
            }
        }
        Ok(summaries)
    }

    async fn delete_vault(
        &self,
        name: &str,
        _resource_group: Option<&str>,
    ) -> Result<(), BackendError> {
        self.delete_vault_internal(name, false).await
    }

    async fn update_vault(
        &self,
        name: &str,
        _resource_group: Option<&str>,
        request: crate::vault::models::VaultUpdateRequest,
    ) -> Result<VaultProperties, BackendError> {
        use crate::backend::aws::encoding::marker_name;

        let marker = marker_name(name);

        // Update tags if provided
        if let Some(ref new_tags) = request.tags {
            let tag_list: Vec<Tag> = new_tags
                .iter()
                .filter(|(k, _)| !k.starts_with("xv:"))
                .map(|(k, v)| Tag::builder().key(k).value(v).build())
                .collect();
            if !tag_list.is_empty() {
                self.client
                    .tag_resource()
                    .secret_id(&marker)
                    .set_tags(Some(tag_list))
                    .send()
                    .await
                    .map_err(super::errors::from_tag)?;
            }
        }

        // Fetch updated vault properties
        self.get_vault(name, None).await
    }
}

impl AwsVaultBackend {
    pub async fn delete_vault_internal(&self, name: &str, force: bool) -> Result<(), BackendError> {
        use crate::backend::aws::encoding::{is_marker, marker_name, strip_prefix};
        use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

        let prefix = format!("{name}/");

        // Paginate through all secrets in this vault prefix.
        let mut next_token: Option<String> = None;
        let mut non_marker: Vec<String> = Vec::new();
        loop {
            let mut req = self.client.list_secrets().max_results(100).filters(
                Filter::builder()
                    .key(FilterNameStringType::Name)
                    .values(prefix.clone())
                    .build(),
            );
            if let Some(ref t) = next_token {
                req = req.next_token(t.clone());
            }
            let out = req.send().await.map_err(super::errors::from_list)?;

            for entry in out.secret_list() {
                if let Some(n) = entry.name() {
                    if !is_marker(n) && strip_prefix(name, n).is_some() {
                        non_marker.push(n.to_string());
                    }
                }
            }

            next_token = out.next_token().map(|s| s.to_string());
            if next_token.is_none() {
                break;
            }
        }

        if !non_marker.is_empty() && !force {
            return Err(BackendError::Conflict(format!(
                "vault '{name}' contains {} secret(s); pass --force to delete them all",
                non_marker.len()
            )));
        }

        if force {
            for full_name in &non_marker {
                self.client
                    .delete_secret()
                    .secret_id(full_name)
                    .recovery_window_in_days(30)
                    .send()
                    .await
                    .map_err(|e| super::errors::from_delete(full_name, e))?;
            }
        }

        let marker = marker_name(name);
        self.client
            .delete_secret()
            .secret_id(&marker)
            .force_delete_without_recovery(true)
            .send()
            .await
            .map_err(|e| super::errors::from_delete(&marker, e))?;

        Ok(())
    }
}
