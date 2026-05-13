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

    /// Update an already-existing secret (upsert path). Filled in Task 19.
    async fn update_existing_secret(
        &self,
        _vault: &str,
        _request: &SecretRequest,
        _aws_full_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        Err(BackendError::Unsupported(
            "aws update path: not yet implemented".into(),
        ))
    }
}

#[async_trait::async_trait]
impl SecretBackend for AwsSecretBackend {
    // Required methods — real impls added in Tasks 13-22.

    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::encoding::{aws_name, validate_secret_name};
        use crate::backend::aws::metadata::{
            TAG_CONTENT_TYPE, TAG_EXPIRES_AT, TAG_FOLDER, TAG_GROUPS, TAG_ORIGINAL_NAME,
        };
        use aws_sdk_secretsmanager::types::Tag;

        validate_secret_name(&request.name)?;
        let aws_full_name = aws_name(vault, &request.name);

        let mut tags: Vec<Tag> = Vec::new();
        tags.push(
            Tag::builder()
                .key(TAG_ORIGINAL_NAME)
                .value(&request.name)
                .build(),
        );
        if let Some(ref groups) = request.groups {
            if !groups.is_empty() {
                tags.push(
                    Tag::builder()
                        .key(TAG_GROUPS)
                        .value(groups.join(","))
                        .build(),
                );
            }
        }
        if let Some(ref f) = request.folder {
            tags.push(Tag::builder().key(TAG_FOLDER).value(f).build());
        }
        if let Some(ref ct) = request.content_type {
            tags.push(Tag::builder().key(TAG_CONTENT_TYPE).value(ct).build());
        }
        if let Some(ref e) = request.expires_on {
            tags.push(
                Tag::builder()
                    .key(TAG_EXPIRES_AT)
                    .value(e.to_rfc3339())
                    .build(),
            );
        }
        if let Some(ref user_tags) = request.tags {
            for (k, v) in user_tags {
                if !k.starts_with("xv:") {
                    tags.push(Tag::builder().key(k).value(v).build());
                }
            }
        }

        let create_result = self
            .client
            .create_secret()
            .name(&aws_full_name)
            .secret_string(request.value.as_str().to_string())
            .description(request.note.clone().unwrap_or_default())
            .set_tags(if tags.is_empty() { None } else { Some(tags) })
            .send()
            .await;

        let version_id = match create_result {
            Ok(out) => out.version_id().unwrap_or("").to_string(),
            Err(e) => match super::errors::from_create(e) {
                BackendError::Conflict(_) => {
                    return self
                        .update_existing_secret(vault, &request, &aws_full_name)
                        .await;
                }
                other => return Err(other),
            },
        };

        Ok(SecretProperties {
            name: request.name.clone(),
            original_name: request.name.clone(),
            value: None,
            version: version_id,
            version_number: None,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: request.expires_on,
            not_before: request.not_before,
            tags: request.tags.unwrap_or_default(),
            content_type: request.content_type.unwrap_or_default(),
            recovery_level: None,
        })
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
