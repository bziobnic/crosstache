//! Secret management implementation
//!
//! This module provides comprehensive secret management functionality
//! including name sanitization, group management, and advanced operations.

use async_trait::async_trait;
use azure_core::auth::TokenCredential;
use azure_security_keyvault::{prelude::*, SecretClient};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use tabled::Tabled;

use crate::auth::provider::AzureAuthProvider;
use crate::error::{crosstacheError, Result};
use crate::utils::format::{DisplayUtils, FormattableOutput, OutputFormat, TableFormatter};
use crate::utils::helpers::{generate_uuid, parse_connection_string, validate_folder_path};
use crate::utils::network::{classify_network_error, create_http_client, NetworkConfig};
use crate::utils::sanitizer::{get_secret_name_info, sanitize_secret_name, SecretNameInfo};

/// Secret properties and metadata
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretProperties {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub value: Option<String>,
    #[tabled(rename = "Version")]
    pub version: String,
    #[tabled(rename = "Created")]
    pub created_on: String,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(rename = "Enabled")]
    pub enabled: bool,
    #[tabled(skip)]
    pub expires_on: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub not_before: Option<DateTime<Utc>>,
    #[tabled(skip)]
    pub tags: HashMap<String, String>,
    #[tabled(rename = "Content Type")]
    pub content_type: String,
}

impl FormattableOutput for SecretProperties {}

/// Secret creation/update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequest {
    pub name: String,
    pub value: String,
    pub content_type: Option<String>,
    pub enabled: Option<bool>,
    pub expires_on: Option<DateTime<Utc>>,
    pub not_before: Option<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
    pub groups: Option<Vec<String>>,
    pub note: Option<String>,
    pub folder: Option<String>,
}

/// Secret update request for advanced operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretUpdateRequest {
    pub name: String,
    pub new_name: Option<String>, // For renaming
    pub value: Option<String>,
    pub content_type: Option<String>,
    pub enabled: Option<bool>,
    pub expires_on: Option<DateTime<Utc>>,
    pub not_before: Option<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
    pub groups: Option<Vec<String>>,
    pub note: Option<String>,
    pub folder: Option<String>,
    pub replace_tags: bool,
    pub replace_groups: bool,
}

/// Display function for optional group
fn display_optional_group(option: &Option<String>) -> String {
    option
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

/// Secret summary for list operations
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretSummary {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(skip)]
    pub original_name: String,
    #[tabled(rename = "Note", display_with = "display_optional_group")]
    pub note: Option<String>,
    #[tabled(rename = "Folder", display_with = "display_optional_group")]
    pub folder: Option<String>,
    #[tabled(rename = "Groups", display_with = "display_optional_group")]
    pub groups: Option<String>,
    #[tabled(rename = "Updated")]
    pub updated_on: String,
    #[tabled(skip)]
    pub enabled: bool,
    #[tabled(skip)]
    pub content_type: String,
}

impl FormattableOutput for SecretSummary {}

/// Connection string component
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct ConnectionComponent {
    #[tabled(rename = "Key")]
    pub key: String,
    #[tabled(rename = "Value")]
    pub value: String,
    #[tabled(rename = "Description")]
    pub description: String,
}

/// Secret group information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretGroup {
    pub name: String,
    pub secrets: Vec<SecretSummary>,
    pub total_count: usize,
}

/// Trait for secret operations
#[async_trait]
pub trait SecretOperations: Send + Sync {
    /// Set a secret value
    async fn set_secret(
        &self,
        vault_name: &str,
        request: &SecretRequest,
    ) -> Result<SecretProperties>;

    /// Get a secret value
    async fn get_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        include_value: bool,
    ) -> Result<SecretProperties>;

    /// List secrets in a vault
    async fn list_secrets(
        &self,
        vault_name: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>>;

    /// Delete a secret (soft delete)
    async fn delete_secret(&self, vault_name: &str, secret_name: &str) -> Result<()>;

    /// Update secret properties
    async fn update_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        request: &SecretRequest,
    ) -> Result<SecretProperties>;

    /// Restore a deleted secret
    async fn restore_secret(&self, vault_name: &str, secret_name: &str)
        -> Result<SecretProperties>;

    /// Permanently purge a deleted secret
    async fn purge_secret(&self, vault_name: &str, secret_name: &str) -> Result<()>;

    /// List deleted secrets
    async fn list_deleted_secrets(&self, vault_name: &str) -> Result<Vec<SecretSummary>>;

    /// Check if secret exists
    async fn secret_exists(&self, vault_name: &str, secret_name: &str) -> Result<bool>;

    /// Get secret versions
    async fn get_secret_versions(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<Vec<SecretProperties>>;

    /// Backup secret
    async fn backup_secret(&self, vault_name: &str, secret_name: &str) -> Result<Vec<u8>>;

    /// Restore secret from backup
    async fn restore_secret_from_backup(
        &self,
        vault_name: &str,
        backup_data: &[u8],
    ) -> Result<SecretProperties>;
}

/// Azure Key Vault secret operations implementation
pub struct AzureSecretOperations {
    auth_provider: Arc<dyn AzureAuthProvider>,
}

impl AzureSecretOperations {
    /// Create a new Azure secret operations instance
    pub fn new(auth_provider: Arc<dyn AzureAuthProvider>) -> Self {
        Self { auth_provider }
    }

    /// Create a secret client for the specified vault
    async fn create_secret_client(&self, vault_name: &str) -> Result<SecretClient> {
        let vault_url = format!("https://{}.vault.azure.net/", vault_name);

        // Get the credential from auth provider
        let credential = self.auth_provider.get_token_credential();

        // Create the secret client
        let client = SecretClient::new(&vault_url, credential).map_err(|e| {
            crosstacheError::azure_api(format!("Failed to create SecretClient: {}", e))
        })?;

        Ok(client)
    }

    /// Sanitize secret name and preserve original in tags
    fn prepare_secret_request(
        &self,
        request: &SecretRequest,
    ) -> Result<(String, HashMap<String, String>)> {
        let sanitized_name = sanitize_secret_name(&request.name)?;
        let mut tags = request.tags.clone().unwrap_or_default();

        // Store original name in tags for mapping
        tags.insert("original_name".to_string(), request.name.clone());
        tags.insert("created_by".to_string(), "crosstache".to_string());

        // Handle groups from request
        if let Some(ref groups) = request.groups {
            if !groups.is_empty() {
                // Store all groups as comma-separated list
                tags.insert("groups".to_string(), groups.join(","));
            }
        }

        // Handle note if provided
        if let Some(ref note) = request.note {
            tags.insert("note".to_string(), note.clone());
        }

        // Handle folder if provided (validate first)
        if let Some(ref folder) = request.folder {
            validate_folder_path(folder)?;
            tags.insert("folder".to_string(), folder.clone());
        }

        Ok((sanitized_name, tags))
    }

    /// Get original name from tags or use sanitized name
    fn get_original_name(&self, sanitized_name: &str, tags: &HashMap<String, String>) -> String {
        // Check for original_name tag first (new format)
        if let Some(original_name) = tags.get("original_name") {
            return original_name.clone();
        }

        // Fall back to name tag (legacy format)
        if let Some(name) = tags.get("name") {
            return name.clone();
        }

        // If no tags, use the sanitized name
        sanitized_name.to_string()
    }

    /// Get groups from tags as comma-separated string (returns None if no groups assigned)
    fn get_group_name(&self, _name: &str, tags: &HashMap<String, String>) -> Option<String> {
        // Check for groups tag (comma-separated list)
        if let Some(groups) = tags.get("groups") {
            let trimmed_groups = groups.trim();
            if !trimmed_groups.is_empty() {
                return Some(trimmed_groups.to_string());
            }
        }

        // No groups assigned
        None
    }

    /// Get note from tags (returns None if no note assigned)
    fn get_note(&self, tags: &HashMap<String, String>) -> Option<String> {
        tags.get("note").map(|s| s.clone())
    }

    /// Get folder from tags (returns None if no folder assigned)
    fn get_folder(&self, tags: &HashMap<String, String>) -> Option<String> {
        tags.get("folder").map(|s| s.clone())
    }

    /// Map Azure SDK response to SecretProperties
    fn map_azure_response_to_properties(
        &self,
        response: KeyVaultGetSecretResponse,
        sanitized_name: &str,
        tags: &HashMap<String, String>,
    ) -> Result<SecretProperties> {
        // Extract version from the response ID if available
        let version = response.id.clone();

        // Get the original name from tags or use provided name
        let original_name = self.get_original_name(sanitized_name, tags);

        Ok(SecretProperties {
            name: sanitized_name.to_string(),
            original_name,
            value: Some(response.value), // Set operation always includes the value
            version,
            created_on: response.attributes.created_on.to_string(),
            updated_on: response.attributes.updated_on.to_string(),
            enabled: response.attributes.enabled,
            expires_on: response.attributes.expires_on.map(|dt| {
                chrono::DateTime::from_timestamp(dt.unix_timestamp(), 0)
                    .unwrap_or_else(|| chrono::Utc::now())
            }),
            not_before: None, // not_before field not available in v0.20 response
            tags: tags.clone(),
            content_type: "text/plain".to_string(), // content_type not available in v0.20 response
        })
    }
}

#[async_trait]
impl SecretOperations for AzureSecretOperations {
    async fn set_secret(
        &self,
        vault_name: &str,
        request: &SecretRequest,
    ) -> Result<SecretProperties> {
        let (sanitized_name, tags) = self.prepare_secret_request(request)?;

        // Since Azure SDK v0.20 doesn't properly support tags, we'll use the REST API directly
        let vault_url = format!("https://{}.vault.azure.net", vault_name);
        let secret_url = format!("{}/secrets/{}?api-version=7.4", vault_url, sanitized_name);

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create the request body
        let mut body = serde_json::json!({
            "value": request.value,
        });

        // Add tags if any
        if !tags.is_empty() {
            body["tags"] = serde_json::json!(tags);
        }

        // Add content type if specified
        if let Some(content_type) = &request.content_type {
            body["contentType"] = serde_json::json!(content_type);
        }

        // Add attributes
        let mut attributes = serde_json::json!({});
        if let Some(enabled) = request.enabled {
            attributes["enabled"] = serde_json::json!(enabled);
        }
        if let Some(expires_on) = request.expires_on {
            attributes["exp"] = serde_json::json!(expires_on.timestamp());
        }
        if let Some(not_before) = request.not_before {
            attributes["nbf"] = serde_json::json!(not_before.timestamp());
        }
        if !attributes.as_object().unwrap().is_empty() {
            body["attributes"] = attributes;
        }

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let client = create_http_client(&network_config)?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| crosstacheError::azure_api(format!("Invalid token format: {}", e)))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );

        // Make the REST API call
        let response = client
            .put(&secret_url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &secret_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(crosstacheError::azure_api(format!(
                "Failed to set secret: HTTP {} - {}",
                status, error_text
            )));
        }

        // Parse the response and convert to SecretProperties
        let json: serde_json::Value = response.json().await.map_err(|e| {
            crosstacheError::serialization(format!("Failed to parse set secret response: {}", e))
        })?;

        // Return the created secret properties
        self.get_secret(vault_name, &sanitized_name, true).await
    }

    async fn get_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        include_value: bool,
    ) -> Result<SecretProperties> {
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Since Azure SDK v0.20 doesn't properly return tags with get_secret,
        // we'll use the REST API directly to get full secret details including tags
        let vault_url = format!("https://{}.vault.azure.net", vault_name);
        let secret_url = format!("{}/secrets/{}?api-version=7.4", vault_url, sanitized_name);

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let client = create_http_client(&network_config)?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| crosstacheError::azure_api(format!("Invalid token format: {}", e)))?,
        );

        // Make the REST API call
        let response = client
            .get(&secret_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &secret_url))?;

        if !response.status().is_success() {
            let status = response.status();
            if status == 404 {
                return Err(crosstacheError::SecretNotFound {
                    name: secret_name.to_string(),
                });
            }
            let error_text = response.text().await.unwrap_or_default();
            return Err(crosstacheError::azure_api(format!(
                "Failed to get secret: HTTP {} - {}",
                status, error_text
            )));
        }

        // Parse the response
        let json: serde_json::Value = response.json().await.map_err(|e| {
            crosstacheError::serialization(format!("Failed to parse secret response: {}", e))
        })?;

        // Extract secret properties from JSON response
        let value = if include_value {
            json.get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let version = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let attributes = json.get("attributes").unwrap_or(&serde_json::Value::Null);
        let enabled = attributes
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let created_on = attributes
            .get("created")
            .and_then(|v| v.as_i64())
            .map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let updated_on = attributes
            .get("updated")
            .and_then(|v| v.as_i64())
            .map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());

        // Extract tags
        let mut tags = HashMap::new();
        if let Some(tags_obj) = json.get("tags").and_then(|v| v.as_object()) {
            for (key, value) in tags_obj {
                if let Some(tag_value) = value.as_str() {
                    tags.insert(key.clone(), tag_value.to_string());
                }
            }
        }

        // Get original name from tags
        let original_name = self.get_original_name(&sanitized_name, &tags);

        Ok(SecretProperties {
            name: sanitized_name,
            original_name,
            value,
            version,
            created_on,
            updated_on,
            enabled,
            expires_on: None, // Not extracted from this API
            not_before: None, // Not extracted from this API
            tags,
            content_type: json
                .get("contentType")
                .and_then(|v| v.as_str())
                .unwrap_or("text/plain")
                .to_string(),
        })
    }

    async fn list_secrets(
        &self,
        vault_name: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>> {
        // Since Azure SDK v0.20 doesn't properly support list operations,
        // we'll use the REST API directly
        let vault_url = format!("https://{}.vault.azure.net", vault_name);
        let list_url = format!("{}/secrets?api-version=7.4", vault_url);

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let client = create_http_client(&network_config)?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| crosstacheError::azure_api(format!("Invalid token format: {}", e)))?,
        );

        // Make the REST API call
        let response = client
            .get(&list_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &list_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(crosstacheError::azure_api(format!(
                "Failed to list secrets: HTTP {} - {}",
                status, error_text
            )));
        }

        // Parse the response
        let json: serde_json::Value = response.json().await.map_err(|e| {
            crosstacheError::serialization(format!("Failed to parse list response: {}", e))
        })?;

        let mut secret_summaries = Vec::new();

        // Process the secrets from the response
        if let Some(values) = json.get("value").and_then(|v| v.as_array()) {
            for secret_value in values {
                if let Some(id) = secret_value.get("id").and_then(|v| v.as_str()) {
                    // Extract name from ID
                    let name = id.rsplit('/').next().unwrap_or(id).to_string();

                    // Extract attributes
                    let attributes = secret_value
                        .get("attributes")
                        .unwrap_or(&serde_json::Value::Null);
                    let enabled = attributes
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let updated = attributes
                        .get("updated")
                        .and_then(|v| v.as_i64())
                        .map(|ts| {
                            chrono::DateTime::from_timestamp(ts, 0)
                                .map(|dt| dt.to_string())
                                .unwrap_or_else(|| "Unknown".to_string())
                        })
                        .unwrap_or_else(|| "Unknown".to_string());

                    // For tags and groups, we need to get the full secret details
                    // Make an individual get call to extract tags and original name
                    match self.get_secret(vault_name, &name, false).await {
                        Ok(secret_details) => {
                            // Extract original name, folder, group, and note from tags
                            let original_name = self.get_original_name(&name, &secret_details.tags);
                            let folder = self.get_folder(&secret_details.tags);
                            let group = self.get_group_name(&original_name, &secret_details.tags);
                            let note = self.get_note(&secret_details.tags);

                            let summary = SecretSummary {
                                name: original_name.clone(),
                                original_name,
                                note,
                                folder,
                                groups: group,
                                updated_on: updated,
                                enabled,
                                content_type: secret_details.content_type,
                            };

                            secret_summaries.push(summary);
                        }
                        Err(e) => {
                            // If we can't get details, add with basic info
                            eprintln!(
                                "Warning: Failed to get details for secret '{}': {}",
                                name, e
                            );
                            let summary = SecretSummary {
                                name: name.clone(),
                                original_name: name,
                                note: None,
                                folder: None,
                                groups: None,
                                updated_on: updated,
                                enabled,
                                content_type: "text/plain".to_string(),
                            };

                            secret_summaries.push(summary);
                        }
                    }
                }
            }
        }

        // Handle pagination if there's a nextLink
        if let Some(next_link) = json.get("nextLink").and_then(|v| v.as_str()) {
            // TODO: Implement pagination support
            eprintln!("Warning: Pagination not yet implemented, showing first page only");
        }

        // Apply group filter if specified
        let filtered_summaries: Vec<SecretSummary> = if let Some(filter) = group_filter {
            secret_summaries
                .into_iter()
                .filter(|secret| {
                    match &secret.groups {
                        Some(groups) => {
                            // Check if the filter matches any of the groups in the comma-separated list
                            groups.split(',').any(|g| g.trim() == filter)
                        }
                        None if filter.is_empty() => true,
                        _ => false,
                    }
                })
                .collect()
        } else {
            secret_summaries
        };

        // Sort by name for consistent output
        let mut result = filtered_summaries;
        result.sort_by(|a, b| a.original_name.cmp(&b.original_name));

        Ok(result)
    }

    async fn delete_secret(&self, vault_name: &str, secret_name: &str) -> Result<()> {
        let client = self.create_secret_client(vault_name).await?;
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Delete the secret from Azure Key Vault (soft delete)
        client.delete(&sanitized_name).await.map_err(|e| {
            crosstacheError::azure_api(format!("Failed to delete secret '{}': {}", secret_name, e))
        })?;

        Ok(())
    }

    async fn update_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        request: &SecretRequest,
    ) -> Result<SecretProperties> {
        // For update operations, Azure Key Vault doesn't have a separate update operation
        // Setting a secret with the same name creates a new version
        // We'll use the same implementation as set_secret
        self.set_secret(vault_name, request).await
    }

    async fn restore_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<SecretProperties> {
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Use REST API to restore a deleted secret
        let vault_url = format!("https://{}.vault.azure.net", vault_name);
        let restore_url = format!(
            "{}/deletedsecrets/{}/recover?api-version=7.4",
            vault_url, sanitized_name
        );

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let client = create_http_client(&network_config)?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| crosstacheError::azure_api(format!("Invalid token format: {}", e)))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );

        // Make the REST API call to restore the secret
        let response = client
            .post(&restore_url)
            .headers(headers)
            .json(&serde_json::json!({})) // Empty JSON body to satisfy Content-Length requirement
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &restore_url))?;

        if !response.status().is_success() {
            let status = response.status();
            if status == 404 {
                return Err(crosstacheError::azure_api(format!(
                    "Deleted secret '{}' not found or cannot be restored",
                    secret_name
                )));
            }
            let error_text = response.text().await.unwrap_or_default();
            return Err(crosstacheError::azure_api(format!(
                "Failed to restore secret: HTTP {} - {}",
                status, error_text
            )));
        }

        // Parse the response to get the restored secret properties
        let json: serde_json::Value = response.json().await.map_err(|e| {
            crosstacheError::serialization(format!("Failed to parse restore response: {}", e))
        })?;

        // Extract secret properties from JSON response
        let version = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let attributes = json.get("attributes").unwrap_or(&serde_json::Value::Null);
        let enabled = attributes
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let created_on = attributes
            .get("created")
            .and_then(|v| v.as_i64())
            .map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());
        let updated_on = attributes
            .get("updated")
            .and_then(|v| v.as_i64())
            .map(|ts| {
                chrono::DateTime::from_timestamp(ts, 0)
                    .map(|dt| dt.to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            })
            .unwrap_or_else(|| "Unknown".to_string());

        // Extract tags
        let mut tags = HashMap::new();
        if let Some(tags_obj) = json.get("tags").and_then(|v| v.as_object()) {
            for (key, value) in tags_obj {
                if let Some(tag_value) = value.as_str() {
                    tags.insert(key.clone(), tag_value.to_string());
                }
            }
        }

        // Get original name from tags
        let original_name = self.get_original_name(&sanitized_name, &tags);

        Ok(SecretProperties {
            name: sanitized_name,
            original_name,
            value: None, // Restore operation doesn't return the secret value
            version,
            created_on,
            updated_on,
            enabled,
            expires_on: None, // Not extracted from this API
            not_before: None, // Not extracted from this API
            tags,
            content_type: json
                .get("contentType")
                .and_then(|v| v.as_str())
                .unwrap_or("text/plain")
                .to_string(),
        })
    }

    async fn purge_secret(&self, vault_name: &str, secret_name: &str) -> Result<()> {
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Use REST API to permanently purge a deleted secret
        let vault_url = format!("https://{}.vault.azure.net", vault_name);
        let purge_url = format!(
            "{}/deletedsecrets/{}/purge?api-version=7.4",
            vault_url, sanitized_name
        );

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let client = create_http_client(&network_config)?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| crosstacheError::azure_api(format!("Invalid token format: {}", e)))?,
        );

        // Make the REST API call to permanently purge the secret
        let response = client
            .delete(&purge_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &purge_url))?;

        if !response.status().is_success() {
            let status = response.status();
            if status == 404 {
                return Err(crosstacheError::azure_api(format!(
                    "Deleted secret '{}' not found or cannot be purged",
                    secret_name
                )));
            }
            let error_text = response.text().await.unwrap_or_default();
            return Err(crosstacheError::azure_api(format!(
                "Failed to purge secret: HTTP {} - {}",
                status, error_text
            )));
        }

        Ok(())
    }

    async fn list_deleted_secrets(&self, vault_name: &str) -> Result<Vec<SecretSummary>> {
        let _client = self.create_secret_client(vault_name).await?;

        // Placeholder implementation
        Err(crosstacheError::azure_api(
            "Secret operations not yet fully implemented for Azure SDK v0.20",
        ))
    }

    async fn secret_exists(&self, vault_name: &str, secret_name: &str) -> Result<bool> {
        match self.get_secret(vault_name, secret_name, false).await {
            Ok(_) => Ok(true),
            Err(crosstacheError::SecretNotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn get_secret_versions(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<Vec<SecretProperties>> {
        let _client = self.create_secret_client(vault_name).await?;
        let _sanitized_name = sanitize_secret_name(secret_name)?;

        // Placeholder implementation
        Err(crosstacheError::azure_api(
            "Secret operations not yet fully implemented for Azure SDK v0.20",
        ))
    }

    async fn backup_secret(&self, vault_name: &str, secret_name: &str) -> Result<Vec<u8>> {
        let _client = self.create_secret_client(vault_name).await?;
        let _sanitized_name = sanitize_secret_name(secret_name)?;

        // Placeholder implementation
        Err(crosstacheError::azure_api(
            "Secret operations not yet fully implemented for Azure SDK v0.20",
        ))
    }

    async fn restore_secret_from_backup(
        &self,
        vault_name: &str,
        _backup_data: &[u8],
    ) -> Result<SecretProperties> {
        let _client = self.create_secret_client(vault_name).await?;

        // Placeholder implementation
        Err(crosstacheError::azure_api(
            "Secret operations not yet fully implemented for Azure SDK v0.20",
        ))
    }
}

/// High-level secret manager with user-friendly operations
pub struct SecretManager {
    secret_ops: Arc<dyn SecretOperations>,
    display_utils: DisplayUtils,
    no_color: bool,
}

impl SecretManager {
    /// Create a new secret manager
    pub fn new(auth_provider: Arc<dyn AzureAuthProvider>, no_color: bool) -> Self {
        let secret_ops = Arc::new(AzureSecretOperations::new(auth_provider));
        let display_utils = DisplayUtils::new(no_color);

        Self {
            secret_ops,
            display_utils,
            no_color,
        }
    }

    /// Set a secret with name sanitization and validation
    pub async fn set_secret_safe(
        &self,
        vault_name: &str,
        name: &str,
        value: &str,
        options: Option<SecretRequest>,
    ) -> Result<SecretProperties> {
        // Validate secret name
        self.validate_secret_name(name)?;

        let mut request = options.unwrap_or_else(|| SecretRequest {
            name: name.to_string(),
            value: value.to_string(),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        });

        request.name = name.to_string();
        request.value = value.to_string();

        // Show name sanitization info if needed
        let name_info = get_secret_name_info(name)?;
        if name_info.was_modified {
            self.display_utils.print_warning(&format!(
                "Secret name '{}' will be sanitized to '{}'",
                name_info.original_name, name_info.sanitized_name
            ))?;

            if name_info.is_hashed {
                self.display_utils.print_info(
                    "Long or complex name was hashed - original name preserved in tags",
                )?;
            }
        }

        self.display_utils
            .print_info(&format!("Setting secret '{}'...", name))?;

        let secret = self.secret_ops.set_secret(vault_name, &request).await?;

        self.display_utils.print_success(&format!(
            "Successfully set secret '{}'",
            secret.original_name
        ))?;

        Ok(secret)
    }

    /// Get a secret with optional value display
    pub async fn get_secret_safe(
        &self,
        vault_name: &str,
        secret_name: &str,
        show_value: bool,
        raw_output: bool,
    ) -> Result<SecretProperties> {
        let mut secret = self
            .secret_ops
            .get_secret(vault_name, secret_name, show_value)
            .await?;

        if !raw_output {
            self.display_secret_details(&secret, show_value)?;
        }

        // Clear sensitive data if not showing value
        if !show_value {
            secret.value = None;
        }

        Ok(secret)
    }

    /// List secrets with optional grouping and filtering
    pub async fn list_secrets_formatted(
        &self,
        vault_name: &str,
        group_filter: Option<&str>,
        output_format: OutputFormat,
        group_by: bool,
        show_all: bool,
    ) -> Result<Vec<SecretSummary>> {
        let mut secrets = self
            .secret_ops
            .list_secrets(vault_name, group_filter)
            .await?;

        // Filter out disabled secrets by default
        if !show_all {
            secrets.retain(|secret| secret.enabled);
        }

        // Display vault name header for table output
        if output_format == OutputFormat::Table {
            self.display_vault_header(vault_name)?;
        }

        if secrets.is_empty() {
            let message = if show_all {
                "No secrets found in vault."
            } else {
                "No enabled secrets found in vault. Use --all to show disabled secrets."
            };
            self.display_utils.print_info(message)?;
            return Ok(secrets);
        }

        if group_by {
            self.display_secrets_by_group(&secrets, output_format, vault_name)?;
        } else {
            self.display_secrets_table(&secrets, output_format)?;
        }

        Ok(secrets)
    }

    /// Delete a secret with confirmation
    pub async fn delete_secret_safe(
        &self,
        vault_name: &str,
        secret_name: &str,
        force: bool,
    ) -> Result<()> {
        if !force {
            self.display_utils.print_warning(&format!(
                "This will delete secret '{}' from vault '{}'",
                secret_name, vault_name
            ))?;
            self.display_utils
                .print_info("The secret will be recoverable for the vault's retention period.")?;
        }

        self.secret_ops
            .delete_secret(vault_name, secret_name)
            .await?;

        self.display_utils
            .print_success(&format!("Successfully deleted secret '{}'", secret_name))?;

        Ok(())
    }

    /// Restore a deleted secret with user-friendly interface
    pub async fn restore_secret_safe(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<SecretProperties> {
        self.display_utils.print_info(&format!(
            "Restoring deleted secret '{}' from vault '{}'...",
            secret_name, vault_name
        ))?;

        let restored_secret = self
            .secret_ops
            .restore_secret(vault_name, secret_name)
            .await?;

        self.display_utils.print_success(&format!(
            "Successfully restored secret '{}'",
            restored_secret.original_name
        ))?;

        Ok(restored_secret)
    }

    /// Permanently purge a deleted secret with user-friendly interface
    pub async fn purge_secret_safe(
        &self,
        vault_name: &str,
        secret_name: &str,
        force: bool,
    ) -> Result<()> {
        if !force {
            self.display_utils.print_warning(&format!(
                "This will PERMANENTLY DELETE secret '{}' from vault '{}'",
                secret_name, vault_name
            ))?;
            self.display_utils
                .print_warning("This operation cannot be undone!")?;
            self.display_utils
                .print_info("The secret must be in a deleted state before it can be purged.")?;
        }

        self.display_utils.print_info(&format!(
            "Permanently purging deleted secret '{}' from vault '{}'...",
            secret_name, vault_name
        ))?;

        self.secret_ops
            .purge_secret(vault_name, secret_name)
            .await?;

        self.display_utils
            .print_success(&format!("Successfully purged secret '{}'", secret_name))?;

        Ok(())
    }

    /// Parse and display connection string components
    pub async fn parse_connection_string(
        &self,
        connection_string: &str,
    ) -> Result<Vec<ConnectionComponent>> {
        let components = parse_connection_string(connection_string);
        let mut result = Vec::new();

        for (key, value) in &components {
            let description = self.get_connection_string_description(key);
            result.push(ConnectionComponent {
                key: key.clone(),
                value: value.clone(),
                description,
            });
        }

        // Display formatted table
        let formatter = TableFormatter::new(OutputFormat::Table, self.no_color);
        let table_output = formatter.format_table(&result)?;
        println!("{}", table_output);

        Ok(result)
    }

    /// Get secrets by group
    pub async fn get_secrets_by_group(&self, vault_name: &str) -> Result<Vec<SecretGroup>> {
        let secrets = self.secret_ops.list_secrets(vault_name, None).await?;
        let mut groups: HashMap<String, Vec<SecretSummary>> = HashMap::new();

        // Group secrets by group name (handling comma-separated groups)
        for secret in secrets {
            match &secret.groups {
                Some(groups_str) => {
                    // Split comma-separated groups and add secret to each group
                    for group in groups_str.split(',') {
                        let group_name = group.trim().to_string();
                        if !group_name.is_empty() {
                            groups.entry(group_name).or_default().push(secret.clone());
                        }
                    }
                }
                None => {
                    groups
                        .entry("(No Groups)".to_string())
                        .or_default()
                        .push(secret);
                }
            }
        }

        let mut result = Vec::new();
        for (group_name, group_secrets) in groups {
            result.push(SecretGroup {
                name: group_name,
                total_count: group_secrets.len(),
                secrets: group_secrets,
            });
        }

        // Sort groups by name
        result.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(result)
    }

    /// Validate secret name
    fn validate_secret_name(&self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(crosstacheError::invalid_secret_name(
                "Secret name cannot be empty",
            ));
        }

        if name.len() > 127 {
            return Err(crosstacheError::invalid_secret_name(
                "Secret name too long (max 127 characters)",
            ));
        }

        Ok(())
    }

    /// Display secret details
    fn display_secret_details(&self, secret: &SecretProperties, show_value: bool) -> Result<()> {
        self.display_utils
            .print_header(&format!("Secret: {}", secret.original_name))?;

        let mut details = vec![
            ("Name", secret.name.as_str()),
            ("Original Name", secret.original_name.as_str()),
            ("Version", secret.version.as_str()),
            ("Content Type", secret.content_type.as_str()),
            ("Enabled", if secret.enabled { "Yes" } else { "No" }),
            ("Created", secret.created_on.as_str()),
            ("Updated", secret.updated_on.as_str()),
        ];

        if show_value {
            if let Some(value) = &secret.value {
                details.push(("Value", value.as_str()));
            }
        }

        let formatted_details = self.display_utils.format_key_value_pairs(&details);
        println!("{}", formatted_details);

        if !secret.tags.is_empty() {
            self.display_utils.print_separator()?;
            self.display_utils.print_header("Tags")?;

            let tag_pairs: Vec<(&str, &str)> = secret
                .tags
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let formatted_tags = self.display_utils.format_key_value_pairs(&tag_pairs);
            println!("{}", formatted_tags);
        }

        Ok(())
    }

    /// Display vault name header
    fn display_vault_header(&self, vault_name: &str) -> Result<()> {
        if self.no_color {
            println!("Vault: {}", vault_name);
        } else {
            println!("\x1b[1m\x1b[36mVault: {}\x1b[0m", vault_name);
        }
        println!();
        Ok(())
    }

    /// Display secrets in a table
    fn display_secrets_table(
        &self,
        secrets: &[SecretSummary],
        output_format: OutputFormat,
    ) -> Result<()> {
        let formatter = TableFormatter::new(output_format, self.no_color);
        let table_output = formatter.format_table(secrets)?;
        println!("{}", table_output);
        Ok(())
    }

    /// Display secrets grouped by group name
    fn display_secrets_by_group(
        &self,
        secrets: &[SecretSummary],
        output_format: OutputFormat,
        vault_name: &str,
    ) -> Result<()> {
        // Display vault name header first
        if output_format == OutputFormat::Table {
            self.display_vault_header(vault_name)?;
        }

        let mut groups: HashMap<String, Vec<&SecretSummary>> = HashMap::new();

        // Group secrets by each individual group (since groups can contain multiple comma-separated values)
        for secret in secrets {
            match &secret.groups {
                Some(groups_str) => {
                    // Split comma-separated groups and add secret to each group
                    for group in groups_str.split(',') {
                        let group_name = group.trim().to_string();
                        if !group_name.is_empty() {
                            groups.entry(group_name).or_default().push(secret);
                        }
                    }
                }
                None => {
                    groups
                        .entry("(No Groups)".to_string())
                        .or_default()
                        .push(secret);
                }
            }
        }

        // Display each group
        for (group_name, group_secrets) in groups {
            self.display_utils.print_header(&format!(
                "Group: {} ({} secrets)",
                group_name,
                group_secrets.len()
            ))?;

            let formatter = TableFormatter::new(output_format.clone(), self.no_color);
            let table_output = formatter.format_table(&group_secrets)?;
            println!("{}", table_output);

            self.display_utils.print_separator()?;
        }

        Ok(())
    }

    /// Get description for connection string keys
    fn get_connection_string_description(&self, key: &str) -> String {
        match key.to_lowercase().as_str() {
            "server" | "hostname" => "Database server hostname or IP address".to_string(),
            "database" | "initial catalog" => "Database name".to_string(),
            "user id" | "uid" | "username" => "Username for authentication".to_string(),
            "password" | "pwd" => "Password for authentication".to_string(),
            "port" => "Port number for database connection".to_string(),
            "encrypt" | "ssl" => "Enable SSL/TLS encryption".to_string(),
            "trust server certificate" => "Trust server certificate without validation".to_string(),
            "connection timeout" => "Connection timeout in seconds".to_string(),
            "command timeout" => "Command execution timeout in seconds".to_string(),
            "application name" => "Application name for connection".to_string(),
            _ => "Connection parameter".to_string(),
        }
    }

    /// Enhanced secret update with support for tags, groups, renaming, and notes
    pub async fn update_secret_enhanced(
        &self,
        vault_name: &str,
        update_request: &SecretUpdateRequest,
    ) -> Result<SecretProperties> {
        // Validate secret name
        self.validate_secret_name(&update_request.name)?;

        // Check if secret exists first
        let current_secret = self
            .secret_ops
            .get_secret(vault_name, &update_request.name, false)
            .await?;

        // Handle secret renaming if requested
        if let Some(ref new_name) = update_request.new_name {
            self.validate_secret_name(new_name)?;

            // Check if new name already exists
            if self.secret_ops.secret_exists(vault_name, new_name).await? {
                return Err(crosstacheError::invalid_argument(format!(
                    "Secret with name '{}' already exists",
                    new_name
                )));
            }

            self.display_utils.print_info(&format!(
                "Renaming secret '{}' to '{}'...",
                update_request.name, new_name
            ))?;
        }

        // Handle tags merging/replacement
        let final_tags = if let Some(ref new_tags) = update_request.tags {
            if update_request.replace_tags {
                // Replace all tags
                let mut tags = new_tags.clone();
                // Always preserve original name and created_by tags
                tags.insert(
                    "original_name".to_string(),
                    update_request
                        .new_name
                        .as_ref()
                        .unwrap_or(&update_request.name)
                        .clone(),
                );
                tags.insert("created_by".to_string(), "crosstache".to_string());
                Some(tags)
            } else {
                // Merge with existing tags
                let mut existing_tags = current_secret.tags.clone();
                existing_tags.extend(new_tags.clone());
                Some(existing_tags)
            }
        } else {
            None
        };

        // Handle groups
        let final_groups = if let Some(ref new_groups) = update_request.groups {
            if update_request.replace_groups {
                Some(new_groups.clone())
            } else {
                // Merge with existing groups from tags
                let mut groups = Vec::new();
                if let Some(existing_groups) = current_secret.tags.get("groups") {
                    groups.extend(existing_groups.split(',').map(|g| g.trim().to_string()));
                }
                groups.extend(new_groups.clone());
                groups.dedup();
                Some(groups)
            }
        } else {
            // Preserve existing groups if none specified in update
            current_secret.tags.get("groups").map(|groups_str| {
                groups_str
                    .split(',')
                    .map(|g| g.trim().to_string())
                    .collect()
            })
        };

        // Handle folder - preserve existing if not specified in update
        let final_folder = update_request
            .folder
            .clone()
            .or_else(|| current_secret.tags.get("folder").map(|f| f.clone()));

        // Handle note - preserve existing if not specified in update
        let final_note = update_request
            .note
            .clone()
            .or_else(|| current_secret.tags.get("note").map(|n| n.clone()));

        // Create the enhanced secret request
        let secret_request = SecretRequest {
            name: update_request
                .new_name
                .as_ref()
                .unwrap_or(&update_request.name)
                .clone(),
            value: update_request
                .value
                .as_ref()
                .unwrap_or(&current_secret.value.unwrap_or_default())
                .clone(),
            content_type: update_request.content_type.clone(),
            enabled: update_request.enabled,
            expires_on: update_request.expires_on,
            not_before: update_request.not_before,
            tags: final_tags,
            groups: final_groups,
            note: final_note,
            folder: final_folder,
        };

        // Show update progress
        self.display_utils
            .print_info(&format!("Updating secret '{}'...", update_request.name))?;

        // If renaming, we need to create a new secret and delete the old one
        if update_request.new_name.is_some() {
            // Create new secret with new name
            let new_secret = self
                .secret_ops
                .set_secret(vault_name, &secret_request)
                .await?;

            // Delete old secret
            self.secret_ops
                .delete_secret(vault_name, &update_request.name)
                .await?;

            self.display_utils.print_info(&format!(
                "Successfully renamed secret '{}' to '{}'",
                update_request.name, secret_request.name
            ))?;

            Ok(new_secret)
        } else {
            // Regular update
            let updated_secret = self
                .secret_ops
                .update_secret(vault_name, &update_request.name, &secret_request)
                .await?;

            self.display_utils.print_info(&format!(
                "Successfully updated secret '{}'",
                update_request.name
            ))?;

            Ok(updated_secret)
        }
    }
}

/// Secret manager builder for flexible construction
pub struct SecretManagerBuilder {
    auth_provider: Option<Arc<dyn AzureAuthProvider>>,
    no_color: bool,
}

impl SecretManagerBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            auth_provider: None,
            no_color: false,
        }
    }

    /// Set the authentication provider
    pub fn with_auth_provider(mut self, auth_provider: Arc<dyn AzureAuthProvider>) -> Self {
        self.auth_provider = Some(auth_provider);
        self
    }

    /// Disable colored output
    pub fn with_no_color(mut self, no_color: bool) -> Self {
        self.no_color = no_color;
        self
    }

    /// Build the secret manager
    pub fn build(self) -> Result<SecretManager> {
        let auth_provider = self
            .auth_provider
            .ok_or_else(|| crosstacheError::config("Authentication provider is required"))?;

        Ok(SecretManager::new(auth_provider, self.no_color))
    }
}

impl Default for SecretManagerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_name_validation() {
        let manager = SecretManager::new(
            Arc::new(crate::auth::provider::DefaultAzureCredentialProvider::new().unwrap()),
            true,
        );

        // Valid names
        assert!(manager.validate_secret_name("valid-secret").is_ok());
        assert!(manager.validate_secret_name("secret123").is_ok());
        assert!(manager
            .validate_secret_name("app/database/connection")
            .is_ok());

        // Invalid names
        assert!(manager.validate_secret_name("").is_err());
        assert!(manager.validate_secret_name(&"a".repeat(128)).is_err());
    }
}
