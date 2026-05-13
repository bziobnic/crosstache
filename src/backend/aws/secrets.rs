//! `AwsSecretBackend` impl `SecretBackend`.

use crate::backend::SecretBackend;
use crate::backend::error::BackendError;
use crate::secret::manager::{SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest};
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
use std::collections::HashMap;
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

    /// Update an already-existing secret (upsert path). Implemented in Task 19.
    async fn update_existing_secret(
        &self,
        vault: &str,
        request: &SecretRequest,
        aws_full_name: &str,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::metadata::{
            TAG_CONTENT_TYPE, TAG_EXPIRES_AT, TAG_FOLDER, TAG_GROUPS, TAG_ORIGINAL_NAME,
        };
        use aws_sdk_secretsmanager::types::Tag;

        // Step 1: Put new value as new version.
        let put_out = self
            .client
            .put_secret_value()
            .secret_id(aws_full_name)
            .secret_string(request.value.as_str().to_string())
            .send()
            .await
            .map_err(|e| super::errors::from_put_value(&request.name, e))?;

        // Step 2: Update description if provided.
        if let Some(ref note) = request.note {
            self.client
                .update_secret()
                .secret_id(aws_full_name)
                .description(note)
                .send()
                .await
                .map_err(|e| super::errors::from_update(&request.name, e))?;
        }

        // Step 3: Replace all tags (describe -> untag all -> re-tag).
        let describe = self
            .client
            .describe_secret()
            .secret_id(aws_full_name)
            .send()
            .await
            .map_err(|e| super::errors::from_describe(&request.name, e))?;

        let existing_keys: Vec<String> = describe
            .tags()
            .iter()
            .filter_map(|t| t.key().map(|k| k.to_string()))
            .collect();
        if !existing_keys.is_empty() {
            self.client
                .untag_resource()
                .secret_id(aws_full_name)
                .set_tag_keys(Some(existing_keys))
                .send()
                .await
                .map_err(super::errors::from_untag)?;
        }

        let mut new_tags: Vec<Tag> = Vec::new();
        new_tags.push(
            Tag::builder()
                .key(TAG_ORIGINAL_NAME)
                .value(&request.name)
                .build(),
        );
        if let Some(ref groups) = request.groups {
            if !groups.is_empty() {
                new_tags.push(
                    Tag::builder()
                        .key(TAG_GROUPS)
                        .value(groups.join(","))
                        .build(),
                );
            }
        }
        if let Some(ref f) = request.folder {
            new_tags.push(Tag::builder().key(TAG_FOLDER).value(f).build());
        }
        if let Some(ref ct) = request.content_type {
            new_tags.push(Tag::builder().key(TAG_CONTENT_TYPE).value(ct).build());
        }
        if let Some(ref e) = request.expires_on {
            new_tags.push(
                Tag::builder()
                    .key(TAG_EXPIRES_AT)
                    .value(e.to_rfc3339())
                    .build(),
            );
        }
        if let Some(ref user_tags) = request.tags {
            for (k, v) in user_tags {
                if !k.starts_with("xv:") {
                    new_tags.push(Tag::builder().key(k).value(v).build());
                }
            }
        }
        self.client
            .tag_resource()
            .secret_id(aws_full_name)
            .set_tags(Some(new_tags))
            .send()
            .await
            .map_err(super::errors::from_tag)?;

        let version = put_out.version_id().unwrap_or("").to_string();
        Ok(SecretProperties {
            name: request.name.clone(),
            original_name: request.name.clone(),
            value: None,
            version,
            version_number: None,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: request.expires_on,
            not_before: request.not_before,
            tags: request.tags.clone().unwrap_or_default(),
            content_type: request.content_type.clone().unwrap_or_default(),
            recovery_level: None,
        })
    }

    /// List all versions of a secret from the AWS API.
    ///
    /// Maps version info to `SecretProperties` entries (without values).
    async fn list_versions_impl(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);

        let out = self
            .client
            .list_secret_version_ids()
            .secret_id(&aws_full_name)
            .include_deprecated(true)
            .send()
            .await
            .map_err(|e| super::errors::from_list_versions(name, e))?;

        let mut versions: Vec<SecretProperties> = Vec::new();
        for v in out.versions() {
            let version_id = v.version_id().unwrap_or("").to_string();
            let created_timestamp = v
                .created_date()
                .map(|d| d.secs())
                .unwrap_or(0);
            let created_on = if created_timestamp > 0 {
                chrono::DateTime::from_timestamp(created_timestamp, 0)
                    .map(|dt| dt.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            // Collect version stages as a tag.
            let stages_str = v
                .version_stages()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let mut tags: HashMap<String, String> = HashMap::new();
            if !stages_str.is_empty() {
                tags.insert("aws:stages".to_string(), stages_str);
            }

            versions.push(SecretProperties {
                name: name.to_string(),
                original_name: name.to_string(),
                value: None,
                version: version_id,
                version_number: None,
                created_timestamp,
                created_on,
                updated_on: String::new(),
                enabled: true,
                expires_on: None,
                not_before: None,
                tags,
                content_type: String::new(),
                recovery_level: None,
            });
        }
        Ok(versions)
    }

    /// Build a `SecretProperties` from a `DescribeSecretOutput`.
    ///
    /// The `fallback_name` is the user-facing name used when the `xv:original_name`
    /// tag is absent.
    fn props_from_describe(
        &self,
        describe: &DescribeSecretOutput,
        fallback_name: &str,
    ) -> SecretProperties {
        use crate::backend::aws::metadata::{
            TAG_CONTENT_TYPE, TAG_EXPIRES_AT, TAG_FOLDER, TAG_GROUPS, TAG_ORIGINAL_NAME,
        };

        // Collect the AWS Tag list into a flat vec of (key, value) pairs.
        let raw_tags: Vec<(String, String)> = describe
            .tags()
            .iter()
            .filter_map(|t| {
                let k = t.key()?;
                let v = t.value()?;
                Some((k.to_string(), v.to_string()))
            })
            .collect();

        // Decode xv: tags into semantic fields; everything else becomes a user tag.
        let mut original_name: Option<String> = None;
        let mut groups: Vec<String> = Vec::new();
        let mut _folder: Option<String> = None;
        let mut content_type: Option<String> = None;
        let mut expires_on: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut user_tags: HashMap<String, String> = HashMap::new();

        for (k, v) in &raw_tags {
            match k.as_str() {
                TAG_ORIGINAL_NAME => original_name = Some(v.clone()),
                TAG_GROUPS => {
                    groups = v
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                }
                TAG_FOLDER => _folder = Some(v.clone()),
                TAG_CONTENT_TYPE => content_type = Some(v.clone()),
                TAG_EXPIRES_AT => {
                    expires_on = chrono::DateTime::parse_from_rfc3339(v)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc));
                }
                _ if !k.starts_with("xv:") => {
                    user_tags.insert(k.clone(), v.clone());
                }
                _ => {} // unknown xv: tag — skip
            }
        }

        // Store groups back into the user_tags map so the rest of the codebase
        // (which reads tags["groups"]) can still find them.
        if !groups.is_empty() {
            user_tags.insert("groups".to_string(), groups.join(","));
        }

        let name = original_name.clone().unwrap_or_else(|| fallback_name.to_string());

        // Extract timestamps from the describe output.
        let created_date = describe.created_date();
        let created_timestamp = created_date
            .map(|d| d.secs())
            .unwrap_or(0);
        let created_on = if created_timestamp > 0 {
            chrono::DateTime::from_timestamp(created_timestamp, 0)
                .map(|dt| dt.to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let updated_on = describe
            .last_changed_date()
            .and_then(|d| chrono::DateTime::from_timestamp(d.secs(), 0))
            .map(|dt| dt.to_string())
            .unwrap_or_default();

        // version_ids_to_stages: find the version with AWSCURRENT label.
        let version = describe
            .version_ids_to_stages()
            .and_then(|map| {
                map.iter().find_map(|(vid, stages)| {
                    if stages.iter().any(|s| s == "AWSCURRENT") {
                        Some(vid.clone())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_default();

        SecretProperties {
            name: name.clone(),
            original_name: original_name.unwrap_or_else(|| fallback_name.to_string()),
            value: None,
            version,
            version_number: None,
            created_timestamp,
            created_on,
            updated_on,
            enabled: true,
            expires_on,
            not_before: None,
            tags: user_tags,
            content_type: content_type.unwrap_or_default(),
            recovery_level: None,
        }
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
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);

        if !include_value {
            let describe = self
                .client
                .describe_secret()
                .secret_id(&aws_full_name)
                .send()
                .await
                .map_err(|e| super::errors::from_describe(name, e))?;
            return Ok(self.props_from_describe(&describe, name));
        }

        // include_value: run describe + get_secret_value concurrently.
        let describe_fut = self.client.describe_secret().secret_id(&aws_full_name).send();
        let value_fut = self.client.get_secret_value().secret_id(&aws_full_name).send();

        let (describe, value) = tokio::join!(describe_fut, value_fut);
        let describe = describe.map_err(|e| super::errors::from_describe(name, e))?;
        let value = value.map_err(|e| super::errors::from_get_value(name, e))?;

        let mut props = self.props_from_describe(&describe, name);
        props.value = value
            .secret_string()
            .map(|s| zeroize::Zeroizing::new(s.to_string()));
        Ok(props)
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);

        // If value not requested, find it in the version list.
        if !include_value {
            let mut versions = self.list_versions(vault, name).await?;
            return versions
                .drain(..)
                .find(|p| p.version == version)
                .ok_or_else(|| BackendError::NotFound {
                    name: format!("{name} (version {version})"),
                    suggestion: None,
                });
        }

        // Get the secret value for this specific version.
        let out = self
            .client
            .get_secret_value()
            .secret_id(&aws_full_name)
            .version_id(version)
            .send()
            .await
            .map_err(|e| super::errors::from_get_value(name, e))?;

        // Build SecretProperties manually with the version-specific data.
        let mut tags: HashMap<String, String> = HashMap::new();
        tags.insert("aws:stages".to_string(), "[current]".to_string());

        let version_id = out.version_id().unwrap_or("").to_string();
        let secret_value = out
            .secret_string()
            .map(|s| zeroize::Zeroizing::new(s.to_string()));

        Ok(SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: secret_value,
            version: version_id,
            version_number: None,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags,
            content_type: String::new(),
            recovery_level: None,
        })
    }

    async fn list_versions(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        self.list_versions_impl(vault, name).await
    }

    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        use crate::backend::aws::encoding::{is_marker, strip_prefix};
        use crate::backend::aws::metadata::TAG_GROUPS;
        use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

        let prefix = format!("{vault}/");
        let mut next_token: Option<String> = None;
        let mut summaries = Vec::new();

        loop {
            let mut req = self
                .client
                .list_secrets()
                .max_results(100)
                .filters(
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
                let aws_full_name = entry.name().unwrap_or("");
                // Skip entries that don't belong to this vault prefix.
                let secret_name = match strip_prefix(vault, aws_full_name) {
                    Some(n) => n,
                    None => continue,
                };
                // Skip vault marker secrets.
                if is_marker(aws_full_name) {
                    continue;
                }
                // Apply group filter if requested.
                if let Some(group_want) = group_filter {
                    let groups_str = entry
                        .tags()
                        .iter()
                        .find(|t| t.key() == Some(TAG_GROUPS))
                        .and_then(|t| t.value())
                        .unwrap_or("");
                    let groups: Vec<&str> = groups_str
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !groups.contains(&group_want) {
                        continue;
                    }
                }
                // Extract groups tag for summary.
                let groups_val = entry
                    .tags()
                    .iter()
                    .find(|t| t.key() == Some(TAG_GROUPS))
                    .and_then(|t| t.value())
                    .map(|s| s.to_string());

                summaries.push(SecretSummary {
                    name: secret_name.clone(),
                    original_name: secret_name,
                    note: None,
                    folder: None,
                    groups: groups_val,
                    updated_on: String::new(),
                    enabled: true,
                    content_type: String::new(),
                });
            }

            next_token = out.next_token().map(|s| s.to_string());
            if next_token.is_none() {
                break;
            }
        }

        Ok(summaries)
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);
        self.client
            .delete_secret()
            .secret_id(&aws_full_name)
            .recovery_window_in_days(30)
            .send()
            .await
            .map_err(|e| super::errors::from_delete(name, e))?;
        Ok(())
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        use crate::backend::aws::metadata::{TAG_FOLDER, TAG_GROUPS};
        use aws_sdk_secretsmanager::types::Tag;

        let aws_full_name = aws_name(vault, name);

        // Update description (note) if provided.
        if let Some(ref new_note) = request.note {
            self.client
                .update_secret()
                .secret_id(&aws_full_name)
                .description(new_note)
                .send()
                .await
                .map_err(|e| super::errors::from_update(name, e))?;
        }

        // Compute tag deltas.
        let mut tags_to_set: Vec<Tag> = Vec::new();
        let mut keys_to_remove: Vec<String> = Vec::new();

        if let Some(ref groups) = request.groups {
            if groups.is_empty() {
                keys_to_remove.push(TAG_GROUPS.into());
            } else {
                tags_to_set.push(
                    Tag::builder()
                        .key(TAG_GROUPS)
                        .value(groups.join(","))
                        .build(),
                );
            }
        }
        if let Some(ref f) = request.folder {
            if f.is_empty() {
                keys_to_remove.push(TAG_FOLDER.into());
            } else {
                tags_to_set.push(Tag::builder().key(TAG_FOLDER).value(f).build());
            }
        }
        if let Some(ref user_tags) = request.tags {
            for (k, v) in user_tags {
                if v.is_empty() {
                    keys_to_remove.push(k.clone());
                } else if !k.starts_with("xv:") {
                    tags_to_set.push(Tag::builder().key(k).value(v).build());
                }
            }
        }

        if !keys_to_remove.is_empty() {
            self.client
                .untag_resource()
                .secret_id(&aws_full_name)
                .set_tag_keys(Some(keys_to_remove))
                .send()
                .await
                .map_err(super::errors::from_untag)?;
        }
        if !tags_to_set.is_empty() {
            self.client
                .tag_resource()
                .secret_id(&aws_full_name)
                .set_tags(Some(tags_to_set))
                .send()
                .await
                .map_err(super::errors::from_tag)?;
        }

        self.get_secret(vault, name, false).await
    }

    async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);
        self.client
            .delete_secret()
            .secret_id(&aws_full_name)
            .force_delete_without_recovery(true)
            .send()
            .await
            .map_err(|e| super::errors::from_delete(name, e))?;
        Ok(())
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);
        match self
            .client
            .describe_secret()
            .secret_id(&aws_full_name)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => match super::errors::from_describe(name, e) {
                BackendError::NotFound { .. } => Ok(false),
                other => Err(other),
            },
        }
    }

    async fn rollback(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);

        // Find which version is currently AWSCURRENT
        let listed = self
            .client
            .list_secret_version_ids()
            .secret_id(&aws_full_name)
            .include_deprecated(true)
            .send()
            .await
            .map_err(|e| super::errors::from_list_versions(name, e))?;

        let current_version = listed
            .versions()
            .iter()
            .find(|v| {
                v.version_stages()
                    .iter()
                    .any(|s| s.as_str() == "AWSCURRENT")
            })
            .and_then(|v| v.version_id())
            .map(|s| s.to_string());

        // Move AWSCURRENT label to target version
        self.client
            .update_secret_version_stage()
            .secret_id(&aws_full_name)
            .version_stage("AWSCURRENT")
            .move_to_version_id(version)
            .set_remove_from_version_id(current_version)
            .send()
            .await
            .map_err(|e| super::errors::from_update_stage(name, e))?;

        // Fetch and return the current secret properties
        self.get_secret(vault, name, false).await
    }

    async fn restore_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretProperties, BackendError> {
        use crate::backend::aws::encoding::aws_name;
        let aws_full_name = aws_name(vault, name);
        self.client
            .restore_secret()
            .secret_id(&aws_full_name)
            .send()
            .await
            .map_err(|e| super::errors::from_restore(name, e))?;
        // After restore, fetch the metadata
        self.get_secret(vault, name, false).await
    }

    async fn list_deleted_secrets(
        &self,
        vault: &str,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        use crate::backend::aws::encoding::{is_marker, strip_prefix};
        use aws_sdk_secretsmanager::types::{Filter, FilterNameStringType};

        let prefix = format!("{vault}/");
        let out = self
            .client
            .list_secrets()
            .max_results(100)
            .include_planned_deletion(true)
            .filters(
                Filter::builder()
                    .key(FilterNameStringType::Name)
                    .values(prefix.clone())
                    .build(),
            )
            .send()
            .await
            .map_err(super::errors::from_list)?;

        let mut summaries: Vec<SecretSummary> = Vec::new();
        for entry in out.secret_list() {
            let aws_full_name = entry.name().unwrap_or("");
            if entry.deleted_date().is_none() {
                continue;
            }
            let secret_name = match strip_prefix(vault, aws_full_name) {
                Some(n) => n,
                None => continue,
            };
            if is_marker(aws_full_name) {
                continue;
            }
            summaries.push(SecretSummary {
                name: secret_name.clone(),
                original_name: secret_name,
                note: None,
                folder: None,
                groups: None,
                updated_on: String::new(),
                enabled: true,
                content_type: String::new(),
            });
        }
        Ok(summaries)
    }
}
