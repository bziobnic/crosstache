//! Secret management implementation
//!
//! This module provides comprehensive secret management functionality
//! including name sanitization, group management, and advanced operations.

use async_trait::async_trait;
use azure_security_keyvault::SecretClient;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use tabled::Tabled;
use zeroize::Zeroizing;

use crate::auth::provider::AzureAuthProvider;
use crate::backend::azure::types::AzureVaultName;
use crate::error::{CrosstacheError, Result};
use crate::secret::models::SecretInfo;
use crate::utils::format::{DisplayUtils, OutputFormat, TableFormatter};
use crate::utils::helpers::{parse_connection_string, validate_folder_path};
use crate::utils::network::{classify_network_error, create_http_client, NetworkConfig};
use crate::utils::output;
use crate::utils::sanitizer::{get_secret_name_info, sanitize_secret_name};

/// Secret properties and metadata
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct SecretProperties {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Original Name")]
    pub original_name: String,
    #[tabled(skip)]
    pub value: Option<Zeroizing<String>>,
    #[tabled(skip)]
    pub version: String,
    /// Human-readable sequential version number (1 = oldest). None when not in a version list context.
    #[tabled(rename = "Version", display_with = "display_version_number")]
    pub version_number: Option<u32>,
    /// Raw Unix timestamp for sorting (not displayed)
    #[tabled(skip)]
    pub created_timestamp: i64,
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
    #[tabled(skip)]
    pub recovery_level: Option<String>,
}

/// Secret creation/update request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequest {
    pub name: String,
    pub value: Zeroizing<String>,
    pub content_type: Option<String>,
    pub enabled: Option<bool>,
    pub expires_on: Option<DateTime<Utc>>,
    pub not_before: Option<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
    pub groups: Option<Vec<String>>,
    pub note: Option<String>,
    pub folder: Option<String>,
}

/// Tri-state update for an optional metadata field: leave it as-is, set a
/// new value, or remove the current value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldUpdate<T> {
    #[default]
    Unchanged,
    Set(T),
    Clear,
}

impl<T> FieldUpdate<T> {
    /// Build from a CLI-style `(value, clear)` flag pair. Supplying both a
    /// value and the clear flag for the same field is an error.
    pub fn from_flags(value: Option<T>, clear: bool, field: &str) -> Result<Self> {
        match (value, clear) {
            (Some(_), true) => Err(CrosstacheError::invalid_argument(format!(
                "Cannot set and clear {field} in the same update"
            ))),
            (Some(v), false) => Ok(FieldUpdate::Set(v)),
            (None, true) => Ok(FieldUpdate::Clear),
            (None, false) => Ok(FieldUpdate::Unchanged),
        }
    }

    /// Resolve against the current value: `Unchanged` preserves it,
    /// `Set` replaces it, `Clear` removes it.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            FieldUpdate::Unchanged => current,
            FieldUpdate::Set(v) => Some(v),
            FieldUpdate::Clear => None,
        }
    }

    pub fn is_unchanged(&self) -> bool {
        matches!(self, FieldUpdate::Unchanged)
    }
}

/// Secret update request for advanced operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretUpdateRequest {
    pub name: String,
    pub new_name: Option<String>, // For renaming
    pub value: Option<Zeroizing<String>>,
    pub content_type: Option<String>,
    pub enabled: Option<bool>,
    pub expires_on: FieldUpdate<DateTime<Utc>>,
    pub not_before: FieldUpdate<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
    pub groups: Option<Vec<String>>,
    pub note: FieldUpdate<String>,
    pub folder: FieldUpdate<String>,
    pub replace_tags: bool,
    pub replace_groups: bool,
}

/// Display function for optional version number (e.g. Some(3) → "v3", None → "-")
fn display_version_number(v: &Option<u32>) -> String {
    match v {
        Some(n) => format!("v{n}"),
        None => "-".to_string(),
    }
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

    /// Get a specific version of a secret
    async fn get_secret_version(
        &self,
        vault_name: &str,
        secret_name: &str,
        version: &str,
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
        _secret_name: &str,
        request: &SecretRequest,
    ) -> Result<SecretProperties>;

    /// Restore a deleted secret
    async fn restore_secret(&self, vault_name: &str, secret_name: &str)
        -> Result<SecretProperties>;

    /// Permanently purge a deleted secret
    async fn purge_secret(&self, vault_name: &str, secret_name: &str) -> Result<()>;

    /// List deleted secrets
    #[allow(dead_code)]
    async fn list_deleted_secrets(&self, vault_name: &str) -> Result<Vec<SecretSummary>>;

    /// Check if secret exists
    async fn secret_exists(&self, vault_name: &str, secret_name: &str) -> Result<bool>;

    /// Get secret versions
    async fn get_secret_versions(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<Vec<SecretProperties>>;

    /// Rollback secret to a specific version
    async fn rollback_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        version: &str,
    ) -> Result<SecretProperties>;

    /// Backup secret
    #[allow(dead_code)]
    async fn backup_secret(&self, vault_name: &str, secret_name: &str) -> Result<Vec<u8>>;

    /// Restore secret from backup
    #[allow(dead_code)]
    async fn restore_secret_from_backup(
        &self,
        vault_name: &str,
        backup_data: &[u8],
    ) -> Result<SecretProperties>;
}

/// Deserialize a JSON response body while enforcing a hard size limit.
///
/// Checks the `Content-Length` header first (fast path) and then verifies
/// the actual byte count after buffering, to guard against oversized responses.
async fn read_json_body<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<T> {
    if let Some(content_length) = response.content_length() {
        if content_length > max_bytes as u64 {
            return Err(CrosstacheError::azure_api(format!(
                "Response body too large: {} bytes (max: {} bytes)",
                content_length, max_bytes
            )));
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to read response body: {e}")))?;
    if bytes.len() > max_bytes {
        return Err(CrosstacheError::azure_api(format!(
            "Response body too large: {} bytes (max: {} bytes)",
            bytes.len(),
            max_bytes
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| CrosstacheError::serialization(format!("Failed to parse JSON response: {e}")))
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

    fn validated_vault_name(&self, vault_name: &str) -> Result<AzureVaultName> {
        AzureVaultName::try_from(vault_name)
    }

    fn key_vault_api_url(
        &self,
        vault_name: &AzureVaultName,
        path_segments: &[&str],
    ) -> Result<String> {
        let mut url = vault_name.key_vault_url()?;
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                CrosstacheError::invalid_url(format!(
                    "Cannot build Key Vault URL for vault '{}'",
                    vault_name.as_str()
                ))
            })?;
            segments.clear();
            segments.extend(path_segments.iter().copied());
        }
        // Key Vault REST API 7.4 is stable and covers the operations we use;
        // keep this explicit so SDK crate version bumps do not silently change
        // the wire contract.
        url.query_pairs_mut().append_pair("api-version", "7.4");
        Ok(url.to_string())
    }

    /// Create a secret client for the specified vault
    async fn create_secret_client(&self, vault_name: &str) -> Result<SecretClient> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let vault_url = vault_name.key_vault_url()?;

        // Get the credential from auth provider
        let credential = self.auth_provider.get_token_credential();

        // Create the secret client
        let client = SecretClient::new(vault_url.as_str(), credential).map_err(|e| {
            CrosstacheError::azure_api(format!("Failed to create SecretClient: {e}"))
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
        tags.get("note").cloned()
    }

    /// Get folder from tags (returns None if no folder assigned)
    fn get_folder(&self, tags: &HashMap<String, String>) -> Option<String> {
        tags.get("folder").cloned()
    }
}

/// Read the body of an HTTP error response for diagnostic messages.
///
/// If reading the body fails (e.g. the connection was dropped), returns a
/// descriptive placeholder rather than a bare empty string so callers always
/// have actionable context in their error messages.
async fn read_error_body(response: reqwest::Response) -> String {
    response
        .text()
        .await
        .unwrap_or_else(|e| format!("(failed to read error body: {e})"))
}

/// Parse one entry of a `GET /deletedsecrets` response page into a
/// [`SecretSummary`].
///
/// Deleted secret items carry the original secret `id`
/// (`https://<vault>.vault.azure.net/secrets/<name>`) alongside their
/// attributes and tags, so the summary can be built without a follow-up
/// `get_secret` call (which would 404 for a deleted secret anyway).
/// Returns `None` when the entry has no usable `id`.
fn parse_deleted_secret_summary(item: &serde_json::Value) -> Option<SecretSummary> {
    let id = item.get("id").and_then(|v| v.as_str())?;
    let name = id.rsplit('/').next().unwrap_or(id).to_string();
    if name.is_empty() {
        return None;
    }

    let attributes = item.get("attributes").unwrap_or(&serde_json::Value::Null);
    let enabled = attributes
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let updated_on = attributes
        .get("updated")
        .and_then(|v| v.as_i64())
        .map(|ts| {
            chrono::DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        })
        .unwrap_or_else(|| "Unknown".to_string());

    let mut tags = HashMap::new();
    if let Some(tags_obj) = item.get("tags").and_then(|v| v.as_object()) {
        for (key, value) in tags_obj {
            if let Some(tag_value) = value.as_str() {
                tags.insert(key.clone(), tag_value.to_string());
            }
        }
    }

    let original_name = tags
        .get("original_name")
        .or_else(|| tags.get("name"))
        .cloned()
        .unwrap_or_else(|| name.clone());
    let groups = tags
        .get("groups")
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty());

    Some(SecretSummary {
        name: original_name.clone(),
        original_name,
        note: tags.get("note").cloned(),
        folder: tags.get("folder").cloned(),
        groups,
        updated_on,
        enabled,
        content_type: item
            .get("contentType")
            .and_then(|v| v.as_str())
            .unwrap_or("text/plain")
            .to_string(),
    })
}

fn json_string_tags(json: &serde_json::Value) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    if let Some(tags_obj) = json.get("tags").and_then(|v| v.as_object()) {
        for (key, value) in tags_obj {
            if let Some(tag_value) = value.as_str() {
                tags.insert(key.clone(), tag_value.to_string());
            }
        }
    }
    tags
}

fn original_name_from_tags(fallback: &str, tags: &HashMap<String, String>) -> String {
    tags.get("original_name")
        .or_else(|| tags.get("name"))
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

fn timestamp_string(attributes: &serde_json::Value, field: &str) -> String {
    attributes
        .get(field)
        .and_then(|v| v.as_i64())
        .map(|ts| {
            DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        })
        .unwrap_or_else(|| "Unknown".to_string())
}

fn optional_timestamp(attributes: &serde_json::Value, field: &str) -> Option<DateTime<Utc>> {
    attributes
        .get(field)
        .and_then(|v| v.as_i64())
        .and_then(|ts| DateTime::from_timestamp(ts, 0))
}

fn parse_secret_properties_bundle(
    json: &serde_json::Value,
    fallback_name: &str,
    include_value: bool,
    default_version: &str,
) -> Result<SecretProperties> {
    let id = json.get("id").and_then(|v| v.as_str());
    let name = id
        .and_then(|id| id.rsplit('/').nth(1))
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_name)
        .to_string();
    let version = id
        .and_then(|id| id.split('/').next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_version)
        .to_string();

    let attributes = json.get("attributes").unwrap_or(&serde_json::Value::Null);
    let enabled = attributes
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let created_ts = attributes
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let tags = json_string_tags(json);
    let original_name = original_name_from_tags(&name, &tags);
    let value = if include_value {
        json.get("value")
            .and_then(|v| v.as_str())
            .map(|s| Zeroizing::new(s.to_string()))
    } else {
        None
    };

    Ok(SecretProperties {
        name,
        original_name,
        value,
        version,
        created_on: timestamp_string(attributes, "created"),
        updated_on: timestamp_string(attributes, "updated"),
        enabled,
        expires_on: optional_timestamp(attributes, "exp"),
        not_before: optional_timestamp(attributes, "nbf"),
        tags,
        content_type: json
            .get("contentType")
            .and_then(|v| v.as_str())
            .unwrap_or("text/plain")
            .to_string(),
        version_number: None,
        created_timestamp: created_ts,
        recovery_level: attributes
            .get("recoveryLevel")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

/// Extract the raw backup bytes from a `POST /secrets/{name}/backup` response.
///
/// Azure returns the backup blob as a base64url-encoded string (RFC 4648 §5)
/// in the `value` field; padding may be absent.
fn decode_backup_value(json: &serde_json::Value) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let value = json
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CrosstacheError::azure_api("Backup response missing 'value' field"))?;
    URL_SAFE_NO_PAD
        .decode(value.trim_end_matches('='))
        .map_err(|e| CrosstacheError::azure_api(format!("Failed to decode backup payload: {e}")))
}

/// Build the JSON body for `POST /secrets/restore` from raw backup bytes.
///
/// The inverse of [`decode_backup_value`]: Azure expects the blob re-encoded
/// as base64url in the `value` field.
fn build_restore_request_body(backup_data: &[u8]) -> serde_json::Value {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    serde_json::json!({ "value": URL_SAFE_NO_PAD.encode(backup_data) })
}

/// Parse the secret bundle returned by `POST /secrets/restore` into
/// [`SecretProperties`].
///
/// The bundle `id` looks like
/// `https://<vault>.vault.azure.net/secrets/<name>/<version>`; the restore
/// response never includes the secret value.
fn parse_restored_secret_properties(json: &serde_json::Value) -> Result<SecretProperties> {
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CrosstacheError::azure_api("Restore response missing secret 'id' field"))?;
    let mut segments = id.rsplit('/');
    let version = segments.next().unwrap_or("").to_string();
    let name = segments.next().unwrap_or("").to_string();
    if name.is_empty() {
        return Err(CrosstacheError::azure_api(format!(
            "Restore response contained unexpected secret id '{id}'"
        )));
    }

    let attributes = json.get("attributes").unwrap_or(&serde_json::Value::Null);
    let enabled = attributes
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let created_ts = attributes
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let created_on = if created_ts > 0 {
        chrono::DateTime::from_timestamp(created_ts, 0)
            .map(|dt| dt.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    } else {
        "Unknown".to_string()
    };
    let updated_on = attributes
        .get("updated")
        .and_then(|v| v.as_i64())
        .map(|ts| {
            chrono::DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        })
        .unwrap_or_else(|| "Unknown".to_string());
    let expires_on = attributes
        .get("exp")
        .and_then(|v| v.as_i64())
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));
    let not_before = attributes
        .get("nbf")
        .and_then(|v| v.as_i64())
        .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));
    let recovery_level = attributes
        .get("recoveryLevel")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut tags = HashMap::new();
    if let Some(tags_obj) = json.get("tags").and_then(|v| v.as_object()) {
        for (key, value) in tags_obj {
            if let Some(tag_value) = value.as_str() {
                tags.insert(key.clone(), tag_value.to_string());
            }
        }
    }

    let original_name = tags
        .get("original_name")
        .or_else(|| tags.get("name"))
        .cloned()
        .unwrap_or_else(|| name.clone());

    Ok(SecretProperties {
        name,
        original_name,
        value: None, // Restore operation doesn't return the secret value
        version,
        created_on,
        updated_on,
        enabled,
        expires_on,
        not_before,
        tags,
        content_type: json
            .get("contentType")
            .and_then(|v| v.as_str())
            .unwrap_or("text/plain")
            .to_string(),
        version_number: None,
        created_timestamp: created_ts,
        recovery_level,
    })
}

#[async_trait]
impl SecretOperations for AzureSecretOperations {
    async fn set_secret(
        &self,
        vault_name: &str,
        request: &SecretRequest,
    ) -> Result<SecretProperties> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let (sanitized_name, tags) = self.prepare_secret_request(request)?;

        // The Azure Key Vault SDK crate does not expose the full tag-bearing
        // SecretBundle shape for this flow, so use REST directly.
        let secret_url = self.key_vault_api_url(&vault_name, &["secrets", &sanitized_name])?;

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
        if attributes.as_object().is_some_and(|obj| !obj.is_empty()) {
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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
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
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to set secret: HTTP {status} - {error_text}"
            )));
        }

        // Parse the response and convert to SecretProperties
        let _json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

        // Return the created secret properties
        self.get_secret(vault_name.as_str(), &sanitized_name, true)
            .await
    }

    async fn get_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        include_value: bool,
    ) -> Result<SecretProperties> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // The Azure Key Vault SDK crate does not expose all tags consistently here,
        // so use REST directly to get full secret details including tags
        let secret_url = self.key_vault_api_url(&vault_name, &["secrets", &sanitized_name])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
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
                return Err(CrosstacheError::SecretNotFound {
                    name: secret_name.to_string(),
                    suggestion: None,
                });
            }
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to get secret: HTTP {status} - {error_text}"
            )));
        }

        // Parse the response
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

        parse_secret_properties_bundle(&json, &sanitized_name, include_value, "")
    }

    async fn get_secret_version(
        &self,
        vault_name: &str,
        secret_name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;
        let secret_url =
            self.key_vault_api_url(&vault_name, &["secrets", &sanitized_name, version])?;

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let http_client = create_http_client(&network_config)?;

        // Make the REST API call
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );

        let response = http_client
            .get(&secret_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &secret_url))?;

        if !response.status().is_success() {
            let status = response.status();
            if status == 404 {
                return Err(CrosstacheError::azure_api(format!(
                    "Secret version '{version}' not found for secret '{secret_name}'"
                )));
            }
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to get secret version: HTTP {} - {}",
                status, error_text
            )));
        }

        // Parse the JSON response
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

        parse_secret_properties_bundle(&json, secret_name, include_value, version)
    }

    async fn list_secrets(
        &self,
        vault_name: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>> {
        let vault_name = self.validated_vault_name(vault_name)?;
        // The Azure Key Vault SDK crate list shape omits the tag details this CLI
        // displays, so use REST directly
        let list_url = self.key_vault_api_url(&vault_name, &["secrets"])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );

        // Bounded concurrency for the per-secret detail fetch below. Azure Key
        // Vault's list response does NOT include tags, so the original_name /
        // groups / folder / note (all stored in tags) require a per-secret GET.
        // We collect the lightweight per-item fields during pagination, then
        // fetch the tag-bearing details concurrently instead of serially — the
        // old code awaited get_secret once per secret in sequence (N+1 latency).
        const LIST_DETAIL_CONCURRENCY: usize = 10;

        // (name, enabled, updated_on) gathered cheaply from the list response.
        let mut pending: Vec<(String, bool, String)> = Vec::new();
        let mut next_url: Option<String> = Some(list_url);
        let mut page_count: usize = 0;

        while let Some(current_url) = next_url.take() {
            page_count += 1;
            if page_count > crate::utils::MAX_PAGES {
                return Err(CrosstacheError::azure_api(format!(
                    "Pagination exceeded maximum of {} pages",
                    crate::utils::MAX_PAGES
                )));
            }

            let response = client
                .get(&current_url)
                .headers(headers.clone())
                .send()
                .await
                .map_err(|e| classify_network_error(&e, &current_url))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = read_error_body(response).await;
                return Err(CrosstacheError::azure_api(format!(
                    "Failed to list secrets: HTTP {status} - {error_text}"
                )));
            }

            let json: serde_json::Value =
                read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

            if let Some(values) = json.get("value").and_then(|v| v.as_array()) {
                for secret_value in values {
                    if let Some(id) = secret_value.get("id").and_then(|v| v.as_str()) {
                        let name = id.rsplit('/').next().unwrap_or(id).to_string();

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

                        // Defer the tag-bearing get_secret to a bounded-concurrency
                        // pass after pagination, instead of awaiting it serially here.
                        pending.push((name, enabled, updated));
                    }
                }
            }

            // Follow pagination nextLink if present
            next_url = json
                .get("nextLink")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }

        // Fetch per-secret details (tags → original_name/groups/folder/note)
        // concurrently with a bounded number of in-flight requests, preserving
        // the original per-secret error degradation (a failed detail fetch
        // falls back to the bare list fields with a warning).
        use futures::stream::StreamExt;
        let vault_for_fetch = vault_name.clone();
        let mut secret_summaries: Vec<SecretSummary> = futures::stream::iter(pending)
            .map(|(name, enabled, updated)| {
                let vault = vault_for_fetch.clone();
                async move {
                    match self.get_secret(vault.as_str(), &name, false).await {
                        Ok(secret_details) => {
                            let original_name = self.get_original_name(&name, &secret_details.tags);
                            let folder = self.get_folder(&secret_details.tags);
                            let group = self.get_group_name(&original_name, &secret_details.tags);
                            let note = self.get_note(&secret_details.tags);

                            SecretSummary {
                                name: original_name.clone(),
                                original_name,
                                note,
                                folder,
                                groups: group,
                                updated_on: updated,
                                enabled,
                                content_type: secret_details.content_type,
                            }
                        }
                        Err(e) => {
                            crate::utils::output::warn(&format!(
                                "Failed to get details for secret '{name}': {e}"
                            ));
                            SecretSummary {
                                name: name.clone(),
                                original_name: name,
                                note: None,
                                folder: None,
                                groups: None,
                                updated_on: updated,
                                enabled,
                                content_type: "text/plain".to_string(),
                            }
                        }
                    }
                }
            })
            .buffer_unordered(LIST_DETAIL_CONCURRENCY)
            .collect()
            .await;
        // buffer_unordered yields completion-order; restore a stable name order
        // so output is deterministic regardless of network timing.
        secret_summaries.sort_by(|a, b| a.name.cmp(&b.name));

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
            CrosstacheError::azure_api(format!("Failed to delete secret '{secret_name}': {e}"))
        })?;

        Ok(())
    }

    async fn update_secret(
        &self,
        vault_name: &str,
        _secret_name: &str,
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
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Use REST API to restore a deleted secret
        let restore_url =
            self.key_vault_api_url(&vault_name, &["deletedsecrets", &sanitized_name, "recover"])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
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
                return Err(CrosstacheError::azure_api(format!(
                    "Deleted secret '{secret_name}' not found or cannot be restored"
                )));
            }
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to restore secret: HTTP {status} - {error_text}"
            )));
        }

        // Parse the response to get the restored secret properties
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

        parse_secret_properties_bundle(&json, &sanitized_name, false, "")
    }

    async fn purge_secret(&self, vault_name: &str, secret_name: &str) -> Result<()> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Use REST API to permanently purge a deleted secret
        let purge_url =
            self.key_vault_api_url(&vault_name, &["deletedsecrets", &sanitized_name, "purge"])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
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
                return Err(CrosstacheError::azure_api(format!(
                    "Deleted secret '{secret_name}' not found or cannot be purged"
                )));
            }
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to purge secret: HTTP {status} - {error_text}"
            )));
        }

        Ok(())
    }

    async fn list_deleted_secrets(&self, vault_name: &str) -> Result<Vec<SecretSummary>> {
        let vault_name = self.validated_vault_name(vault_name)?;

        // Use REST API to list soft-deleted secrets
        let list_url = self.key_vault_api_url(&vault_name, &["deletedsecrets"])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );

        let mut summaries = Vec::new();
        let mut next_url: Option<String> = Some(list_url);
        let mut page_count: usize = 0;

        while let Some(current_url) = next_url.take() {
            page_count += 1;
            if page_count > crate::utils::MAX_PAGES {
                return Err(CrosstacheError::azure_api(format!(
                    "Pagination exceeded maximum of {} pages",
                    crate::utils::MAX_PAGES
                )));
            }

            let response = client
                .get(&current_url)
                .headers(headers.clone())
                .send()
                .await
                .map_err(|e| classify_network_error(&e, &current_url))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = read_error_body(response).await;
                return Err(CrosstacheError::azure_api(format!(
                    "Failed to list deleted secrets: HTTP {status} - {error_text}"
                )));
            }

            let json: serde_json::Value =
                read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

            if let Some(values) = json.get("value").and_then(|v| v.as_array()) {
                for item in values {
                    if let Some(summary) = parse_deleted_secret_summary(item) {
                        summaries.push(summary);
                    }
                }
            }

            // Follow pagination nextLink if present
            next_url = json
                .get("nextLink")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }

        // Sort by name for consistent output
        summaries.sort_by(|a, b| a.original_name.cmp(&b.original_name));

        Ok(summaries)
    }

    async fn secret_exists(&self, vault_name: &str, secret_name: &str) -> Result<bool> {
        match self.get_secret(vault_name, secret_name, false).await {
            Ok(_) => Ok(true),
            Err(CrosstacheError::SecretNotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn get_secret_versions(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<Vec<SecretProperties>> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;
        let versions_url =
            self.key_vault_api_url(&vault_name, &["secrets", &sanitized_name, "versions"])?;

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Create HTTP client with proper timeout configuration
        let network_config = NetworkConfig::default();
        let http_client = create_http_client(&network_config)?;

        // Make the REST API call to list versions
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );

        let mut versions = Vec::new();
        let mut next_url: Option<String> = Some(versions_url);
        let mut page_count: usize = 0;

        while let Some(current_url) = next_url.take() {
            page_count += 1;
            if page_count > crate::utils::MAX_PAGES {
                return Err(CrosstacheError::azure_api(format!(
                    "Pagination exceeded maximum of {} pages",
                    crate::utils::MAX_PAGES
                )));
            }

            let response = http_client
                .get(&current_url)
                .headers(headers.clone())
                .send()
                .await
                .map_err(|e| classify_network_error(&e, &current_url))?;

            if !response.status().is_success() {
                let status = response.status();
                let error_text = read_error_body(response).await;
                return Err(CrosstacheError::azure_api(format!(
                    "Failed to list secret versions: HTTP {} - {}",
                    status, error_text
                )));
            }

            let json: serde_json::Value =
                read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

            if let Some(value_array) = json["value"].as_array() {
                for version_json in value_array {
                    let attributes = version_json
                        .get("attributes")
                        .unwrap_or(&serde_json::Value::Null);

                    let version = version_json["id"]
                        .as_str()
                        .and_then(|id| id.split('/').next_back())
                        .unwrap_or("unknown")
                        .to_string();

                    let enabled = attributes
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    let created_timestamp = attributes
                        .get("created")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let updated_timestamp = attributes
                        .get("updated")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);

                    let created_on = DateTime::from_timestamp(created_timestamp, 0)
                        .unwrap_or_else(Utc::now)
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string();

                    let updated_on = DateTime::from_timestamp(updated_timestamp, 0)
                        .unwrap_or_else(Utc::now)
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string();

                    let recovery_level = attributes
                        .get("recoveryLevel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    versions.push(SecretProperties {
                        name: secret_name.to_string(),
                        original_name: secret_name.to_string(),
                        value: None,
                        version,
                        version_number: None,
                        created_on,
                        updated_on,
                        enabled,
                        expires_on: None,
                        not_before: None,
                        tags: HashMap::new(),
                        content_type: String::new(),
                        created_timestamp,
                        recovery_level,
                    });
                }
            }

            next_url = json
                .get("nextLink")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }

        // Sort ascending by raw Unix timestamp to assign sequential version numbers (oldest = v1)
        versions.sort_by_key(|v| v.created_timestamp);
        for (i, v) in versions.iter_mut().enumerate() {
            v.version_number = Some((i + 1) as u32);
        }
        // Re-sort newest first for display
        versions.sort_by_key(|version| std::cmp::Reverse(version.created_timestamp));
        Ok(versions)
    }

    async fn rollback_secret(
        &self,
        vault_name: &str,
        secret_name: &str,
        version: &str,
    ) -> Result<SecretProperties> {
        // First, get the specific version with its value
        let old_version = self
            .get_secret_version(vault_name, secret_name, version, true)
            .await?;

        // Extract the value - we need it to create the new version
        let value = old_version.value.ok_or_else(|| {
            CrosstacheError::azure_api("Failed to retrieve value from old version")
        })?;

        // Create a new secret version with the old value (this is how "rollback" works in Key Vault)
        let request = SecretRequest {
            name: secret_name.to_string(),
            value,
            content_type: Some(old_version.content_type.clone()),
            enabled: Some(old_version.enabled),
            expires_on: old_version.expires_on,
            not_before: old_version.not_before,
            tags: Some(old_version.tags.clone()),
            groups: None, // Will be extracted from tags by the set_secret method
            note: None,   // Will be extracted from tags by the set_secret method
            folder: None, // Will be extracted from tags by the set_secret method
        };

        self.set_secret(vault_name, &request).await
    }

    async fn backup_secret(&self, vault_name: &str, secret_name: &str) -> Result<Vec<u8>> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // Use REST API to download a protected backup of the secret
        let backup_url =
            self.key_vault_api_url(&vault_name, &["secrets", &sanitized_name, "backup"])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        // Make the REST API call to back up the secret
        let response = client
            .post(&backup_url)
            .headers(headers)
            .json(&serde_json::json!({})) // Empty JSON body to satisfy Content-Length requirement
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &backup_url))?;

        if !response.status().is_success() {
            let status = response.status();
            if status == 404 {
                return Err(CrosstacheError::SecretNotFound {
                    name: secret_name.to_string(),
                    suggestion: None,
                });
            }
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to backup secret: HTTP {status} - {error_text}"
            )));
        }

        // Parse the response and decode the base64url backup blob
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

        decode_backup_value(&json)
    }

    async fn restore_secret_from_backup(
        &self,
        vault_name: &str,
        backup_data: &[u8],
    ) -> Result<SecretProperties> {
        let vault_name = self.validated_vault_name(vault_name)?;

        // Use REST API to restore a secret from a protected backup blob
        let restore_url = self.key_vault_api_url(&vault_name, &["secrets", "restore"])?;

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
                .map_err(|e| CrosstacheError::azure_api(format!("Invalid token format: {e}")))?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        // Make the REST API call to restore the secret from the backup
        let response = client
            .post(&restore_url)
            .headers(headers)
            .json(&build_restore_request_body(backup_data))
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &restore_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = read_error_body(response).await;
            return Err(CrosstacheError::azure_api(format!(
                "Failed to restore secret from backup: HTTP {status} - {error_text}"
            )));
        }

        // Parse the response to get the restored secret properties
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;

        parse_restored_secret_properties(&json)
    }
}

/// High-level secret manager with user-friendly operations
pub struct SecretManager {
    secret_ops: Arc<dyn SecretOperations>,
    no_color: bool,
}

impl SecretManager {
    /// Create a new secret manager
    pub fn new(auth_provider: Arc<dyn AzureAuthProvider>, no_color: bool) -> Self {
        let secret_ops = Arc::new(AzureSecretOperations::new(auth_provider));

        Self {
            secret_ops,
            no_color,
        }
    }

    /// Access to secret operations (for advanced use cases)
    pub fn secret_ops(&self) -> &Arc<dyn SecretOperations> {
        &self.secret_ops
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
            value: Zeroizing::new(value.to_string()),
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
        request.value = Zeroizing::new(value.to_string());

        // Show name sanitization info if needed
        let name_info = get_secret_name_info(name)?;
        if name_info.was_modified {
            output::warn(&format!(
                "Secret name '{}' will be sanitized to '{}'",
                name_info.original_name, name_info.sanitized_name
            ));

            if name_info.is_hashed {
                output::info("Long or complex name was hashed - original name preserved in tags");
                output::hint(&format!(
                    "Access by original name: xv get '{}'",
                    name_info.original_name
                ));
            }
        }

        output::info(&format!("Setting secret '{name}'..."));

        let secret = self.secret_ops.set_secret(vault_name, &request).await?;

        output::success(&format!(
            "Successfully set secret '{}'",
            secret.original_name
        ));

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

    /// Get a secret with optional version support
    pub async fn get_secret_with_version(
        &self,
        vault_name: &str,
        secret_name: &str,
        version: Option<&str>,
        show_value: bool,
        raw_output: bool,
    ) -> Result<SecretProperties> {
        let mut secret = match version {
            Some(ver) => {
                self.secret_ops
                    .get_secret_version(vault_name, secret_name, ver, show_value)
                    .await?
            }
            None => {
                self.secret_ops
                    .get_secret(vault_name, secret_name, show_value)
                    .await?
            }
        };

        if !raw_output {
            self.display_secret_details(&secret, show_value)?;
        }

        // Clear sensitive data if not showing value
        if !show_value {
            secret.value = None;
        }

        Ok(secret)
    }

    /// Get detailed secret information without the secret value
    pub async fn get_secret_info(&self, vault_name: &str, secret_name: &str) -> Result<SecretInfo> {
        let validated_vault_name = AzureVaultName::try_from(vault_name)?;
        // Use the existing get_secret method to fetch properties
        let secret_props = self
            .secret_ops
            .get_secret(vault_name, secret_name, false)
            .await?;

        // Build the vault URI
        let vault_uri = validated_vault_name
            .key_vault_url()?
            .as_str()
            .trim_end_matches('/')
            .to_string();

        // Build the secret ID (simulated since we don't have it from SecretProperties)
        let id = format!(
            "{}/secrets/{}/{}",
            vault_uri, secret_props.name, secret_props.version
        );

        // Extract metadata from tags
        let tags = secret_props.tags.clone();
        let groups = SecretInfo::extract_groups(&tags);
        let folder = SecretInfo::extract_folder(&tags);
        let note = SecretInfo::extract_note(&tags);
        let original_name = SecretInfo::extract_original_name(&tags)
            .or_else(|| Some(secret_props.original_name.clone()));

        // Parse timestamps from the string formats
        let created = if !secret_props.created_on.is_empty() {
            DateTime::parse_from_rfc3339(&secret_props.created_on)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        } else {
            None
        };

        let updated = if !secret_props.updated_on.is_empty() {
            DateTime::parse_from_rfc3339(&secret_props.updated_on)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        } else {
            None
        };

        // Build SecretInfo
        let info = SecretInfo {
            name: secret_props.name.clone(),
            original_name,
            id,
            version: Some(secret_props.version.clone()),
            enabled: secret_props.enabled,
            created,
            updated,
            expires: secret_props.expires_on,
            not_before: secret_props.not_before,
            recovery_level: secret_props.recovery_level.clone(),
            content_type: if secret_props.content_type.is_empty() {
                None
            } else {
                Some(secret_props.content_type.clone())
            },
            tags,
            groups,
            folder,
            note,
            vault_uri,
            version_count: self
                .secret_ops
                .get_secret_versions(vault_name, secret_name)
                .await
                .map(|versions| versions.len())
                .ok(),
        };

        Ok(info)
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
            output::info(message);
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
            output::warn(&format!(
                "This will delete secret '{secret_name}' from vault '{vault_name}'"
            ));
            output::info("The secret will be recoverable for the vault's retention period.");
        }

        self.secret_ops
            .delete_secret(vault_name, secret_name)
            .await?;

        output::success(&format!("Successfully deleted secret '{secret_name}'"));
        output::hint(&format!(
            "Undo with 'xv restore {secret_name}' (before purge retention expires)"
        ));

        Ok(())
    }

    /// Restore a deleted secret with user-friendly interface
    pub async fn restore_secret_safe(
        &self,
        vault_name: &str,
        secret_name: &str,
    ) -> Result<SecretProperties> {
        output::info(&format!(
            "Restoring deleted secret '{secret_name}' from vault '{vault_name}'..."
        ));

        let restored_secret = self
            .secret_ops
            .restore_secret(vault_name, secret_name)
            .await?;

        output::success(&format!(
            "Successfully restored secret '{}'",
            restored_secret.original_name
        ));

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
            output::warn(&format!(
                "This will PERMANENTLY DELETE secret '{secret_name}' from vault '{vault_name}'"
            ));
            output::warn("This operation cannot be undone!");
            output::info("The secret must be in a deleted state before it can be purged.");
        }

        output::info(&format!(
            "Permanently purging deleted secret '{secret_name}' from vault '{vault_name}'..."
        ));

        self.secret_ops
            .purge_secret(vault_name, secret_name)
            .await?;

        output::success(&format!("Successfully purged secret '{secret_name}'"));

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

        Ok(result)
    }

    /// Validate secret name
    fn validate_secret_name(&self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(CrosstacheError::invalid_secret_name(
                "Secret name cannot be empty",
            ));
        }

        if name.len() > 127 {
            return Err(CrosstacheError::invalid_secret_name(
                "Secret name too long (max 127 characters)",
            ));
        }

        Ok(())
    }

    /// Display secret details
    fn display_secret_details(&self, secret: &SecretProperties, show_value: bool) -> Result<()> {
        let du = DisplayUtils::new(self.no_color);
        du.print_header(&format!("Secret: {}", secret.original_name))?;

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

        let formatted_details = du.format_key_value_pairs(&details);
        println!("{formatted_details}");

        if !secret.tags.is_empty() {
            du.print_separator()?;
            du.print_header("Tags")?;

            let tag_pairs: Vec<(&str, &str)> = secret
                .tags
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let formatted_tags = du.format_key_value_pairs(&tag_pairs);
            println!("{formatted_tags}");
        }

        Ok(())
    }

    /// Display vault name header
    fn display_vault_header(&self, vault_name: &str) -> Result<()> {
        if self.no_color {
            println!("Vault: {vault_name}");
        } else {
            println!("\x1b[1m\x1b[36mVault: {vault_name}\x1b[0m");
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
        let formatter = TableFormatter::new(output_format, self.no_color, None, None);
        let table_output = formatter.format_table(secrets)?;
        println!("{table_output}");
        Ok(())
    }

    /// Display secrets grouped by group name
    fn display_secrets_by_group(
        &self,
        secrets: &[SecretSummary],
        output_format: OutputFormat,
        vault_name: &str,
    ) -> Result<()> {
        let du = DisplayUtils::new(self.no_color);
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
            du.print_header(&format!(
                "Group: {} ({} secrets)",
                group_name,
                group_secrets.len()
            ))?;

            let formatter = TableFormatter::new(output_format, self.no_color, None, None);
            let table_output = formatter.format_table(&group_secrets)?;
            println!("{table_output}");

            du.print_separator()?;
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
        // If no new value is provided, we need the current value to preserve it
        let need_value = update_request.value.is_none();
        let current_secret = self
            .secret_ops
            .get_secret(vault_name, &update_request.name, need_value)
            .await?;

        // Handle secret renaming if requested
        if let Some(ref new_name) = update_request.new_name {
            self.validate_secret_name(new_name)?;

            // Check if new name already exists
            if self.secret_ops.secret_exists(vault_name, new_name).await? {
                return Err(CrosstacheError::invalid_argument(format!(
                    "Secret with name '{new_name}' already exists"
                )));
            }

            output::info(&format!(
                "Renaming secret '{}' to '{}'...",
                update_request.name, new_name
            ));
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

        // Handle folder - Unchanged preserves the existing value, Clear removes it
        let final_folder = update_request
            .folder
            .clone()
            .apply(current_secret.tags.get("folder").cloned());

        // Handle note - Unchanged preserves the existing value, Clear removes it
        let final_note = update_request
            .note
            .clone()
            .apply(current_secret.tags.get("note").cloned());

        // Create the enhanced secret request
        let secret_request = SecretRequest {
            name: update_request
                .new_name
                .as_ref()
                .unwrap_or(&update_request.name)
                .clone(),
            value: update_request.value.as_ref().cloned().unwrap_or_else(|| {
                current_secret
                    .value
                    .unwrap_or_else(|| Zeroizing::new(String::new()))
            }),
            content_type: update_request.content_type.clone(),
            enabled: update_request.enabled,
            // Azure updates PUT a new secret version, so attributes omitted
            // from the request are dropped: Unchanged must carry the current
            // value forward explicitly.
            expires_on: update_request
                .expires_on
                .clone()
                .apply(current_secret.expires_on),
            not_before: update_request
                .not_before
                .clone()
                .apply(current_secret.not_before),
            tags: final_tags,
            groups: final_groups,
            note: final_note,
            folder: final_folder,
        };

        // Show update progress
        output::info(&format!("Updating secret '{}'...", update_request.name));

        // If renaming, we need to create a new secret and delete the old one.
        // The backend has no atomic rename, so this is a recoverable two-step
        // operation: create-new first (failure there leaves everything
        // untouched), then delete-old.
        if update_request.new_name.is_some() {
            // Create new secret with new name
            let new_secret = self
                .secret_ops
                .set_secret(vault_name, &secret_request)
                .await?;

            // Delete old secret. On failure the new secret must be left in
            // place — it holds a good copy of the material, and a rollback
            // delete could itself fail or remove the only live copy. Both
            // secrets surviving is the safe outcome; the error tells the
            // user how to finish the rename by hand.
            if let Err(cause) = self
                .secret_ops
                .delete_secret(vault_name, &update_request.name)
                .await
            {
                return Err(CrosstacheError::RenameIncomplete {
                    source: update_request.name.clone(),
                    destination: secret_request.name.clone(),
                    vault: vault_name.to_string(),
                    cause: Box::new(cause),
                });
            }

            output::info(&format!(
                "Successfully renamed secret '{}' to '{}'",
                update_request.name, secret_request.name
            ));

            Ok(new_secret)
        } else {
            // Regular update
            let updated_secret = self
                .secret_ops
                .update_secret(vault_name, &update_request.name, &secret_request)
                .await?;

            output::info(&format!(
                "Successfully updated secret '{}'",
                update_request.name
            ));

            Ok(updated_secret)
        }
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

    #[test]
    fn test_field_update_from_flags() {
        assert_eq!(
            FieldUpdate::from_flags(Some("x".to_string()), false, "note").unwrap(),
            FieldUpdate::Set("x".to_string())
        );
        assert_eq!(
            FieldUpdate::from_flags(None::<String>, true, "note").unwrap(),
            FieldUpdate::Clear
        );
        assert_eq!(
            FieldUpdate::from_flags(None::<String>, false, "note").unwrap(),
            FieldUpdate::Unchanged
        );
        // Set + clear together is an error
        assert!(FieldUpdate::from_flags(Some("x".to_string()), true, "note").is_err());
    }

    #[test]
    fn test_field_update_apply() {
        assert_eq!(FieldUpdate::Unchanged.apply(Some(1)), Some(1));
        assert_eq!(FieldUpdate::<i32>::Unchanged.apply(None), None);
        assert_eq!(FieldUpdate::Set(2).apply(Some(1)), Some(2));
        assert_eq!(FieldUpdate::Set(2).apply(None), Some(2));
        assert_eq!(FieldUpdate::Clear.apply(Some(1)), None);
        assert_eq!(FieldUpdate::<i32>::Clear.apply(None), None);
    }

    // --- Rename recoverability ---

    use std::sync::Mutex;

    fn test_properties(name: &str, value: Option<&str>) -> SecretProperties {
        SecretProperties {
            name: name.to_string(),
            original_name: name.to_string(),
            value: value.map(|v| Zeroizing::new(v.to_string())),
            version: "v1".to_string(),
            version_number: None,
            created_timestamp: 0,
            created_on: String::new(),
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            content_type: String::new(),
            recovery_level: None,
        }
    }

    /// Fake backend that records every write so tests can assert exactly
    /// which secrets a rename touched. `fail_delete` simulates the old
    /// secret's deletion failing after the new secret was created.
    #[derive(Default)]
    struct FakeSecretOps {
        fail_delete: bool,
        /// (name, include_value) of every get_secret call, in order
        get_requests: Mutex<Vec<(String, bool)>>,
        /// (name, value) of every set_secret call, in order
        set_requests: Mutex<Vec<(String, String)>>,
        /// Names passed to delete_secret, whether or not the call succeeded
        delete_attempts: Mutex<Vec<String>>,
        /// Names passed to purge_secret
        purge_attempts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl SecretOperations for FakeSecretOps {
        async fn set_secret(
            &self,
            _vault_name: &str,
            request: &SecretRequest,
        ) -> Result<SecretProperties> {
            self.set_requests
                .lock()
                .unwrap()
                .push((request.name.clone(), request.value.to_string()));
            Ok(test_properties(&request.name, None))
        }

        async fn get_secret(
            &self,
            _vault_name: &str,
            secret_name: &str,
            include_value: bool,
        ) -> Result<SecretProperties> {
            self.get_requests
                .lock()
                .unwrap()
                .push((secret_name.to_string(), include_value));
            Ok(test_properties(
                secret_name,
                include_value.then_some("old-value"),
            ))
        }

        async fn get_secret_version(
            &self,
            _vault_name: &str,
            _secret_name: &str,
            _version: &str,
            _include_value: bool,
        ) -> Result<SecretProperties> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn list_secrets(
            &self,
            _vault_name: &str,
            _group_filter: Option<&str>,
        ) -> Result<Vec<SecretSummary>> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn delete_secret(&self, _vault_name: &str, secret_name: &str) -> Result<()> {
            self.delete_attempts
                .lock()
                .unwrap()
                .push(secret_name.to_string());
            if self.fail_delete {
                return Err(CrosstacheError::network(format!(
                    "simulated outage deleting '{secret_name}'"
                )));
            }
            Ok(())
        }

        async fn update_secret(
            &self,
            _vault_name: &str,
            _secret_name: &str,
            _request: &SecretRequest,
        ) -> Result<SecretProperties> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn restore_secret(
            &self,
            _vault_name: &str,
            _secret_name: &str,
        ) -> Result<SecretProperties> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn purge_secret(&self, _vault_name: &str, secret_name: &str) -> Result<()> {
            self.purge_attempts
                .lock()
                .unwrap()
                .push(secret_name.to_string());
            Ok(())
        }

        async fn list_deleted_secrets(&self, _vault_name: &str) -> Result<Vec<SecretSummary>> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn secret_exists(&self, _vault_name: &str, _secret_name: &str) -> Result<bool> {
            Ok(false)
        }

        async fn get_secret_versions(
            &self,
            _vault_name: &str,
            _secret_name: &str,
        ) -> Result<Vec<SecretProperties>> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn rollback_secret(
            &self,
            _vault_name: &str,
            _secret_name: &str,
            _version: &str,
        ) -> Result<SecretProperties> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn backup_secret(&self, _vault_name: &str, _secret_name: &str) -> Result<Vec<u8>> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }

        async fn restore_secret_from_backup(
            &self,
            _vault_name: &str,
            _backup_data: &[u8],
        ) -> Result<SecretProperties> {
            Err(CrosstacheError::unknown("not implemented in fake"))
        }
    }

    fn rename_request(from: &str, to: &str) -> SecretUpdateRequest {
        SecretUpdateRequest {
            name: from.to_string(),
            new_name: Some(to.to_string()),
            value: None,
            content_type: None,
            enabled: None,
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: None,
            groups: None,
            note: FieldUpdate::Unchanged,
            folder: FieldUpdate::Unchanged,
            replace_tags: false,
            replace_groups: false,
        }
    }

    #[tokio::test]
    async fn test_rename_succeeds_creates_new_then_deletes_old() {
        let ops = Arc::new(FakeSecretOps::default());
        let manager = SecretManager {
            secret_ops: ops.clone(),
            no_color: true,
        };

        let result = manager
            .update_secret_enhanced("test-vault", &rename_request("legacy-name", "modern-name"))
            .await
            .expect("rename should succeed");
        assert_eq!(result.name, "modern-name");

        // A pure rename must fetch the old secret's value so it can be
        // carried over to the new secret.
        assert_eq!(
            ops.get_requests.lock().unwrap().as_slice(),
            [("legacy-name".to_string(), true)]
        );
        let sets = ops.set_requests.lock().unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "modern-name");
        // The old secret's material was carried over to the new secret
        assert_eq!(sets[0].1, "old-value");
        assert_eq!(
            ops.delete_attempts.lock().unwrap().as_slice(),
            ["legacy-name".to_string()]
        );
    }

    #[tokio::test]
    async fn test_rename_delete_failure_keeps_new_secret_and_reports_recovery() {
        let ops = Arc::new(FakeSecretOps {
            fail_delete: true,
            ..Default::default()
        });
        let manager = SecretManager {
            secret_ops: ops.clone(),
            no_color: true,
        };

        let err = manager
            .update_secret_enhanced("test-vault", &rename_request("legacy-name", "modern-name"))
            .await
            .expect_err("rename must fail when the old secret cannot be deleted");

        match &err {
            CrosstacheError::RenameIncomplete {
                source,
                destination,
                vault,
                cause,
            } => {
                assert_eq!(source, "legacy-name");
                assert_eq!(destination, "modern-name");
                assert_eq!(vault, "test-vault");
                assert!(matches!(**cause, CrosstacheError::NetworkError(_)));
            }
            other => panic!("expected RenameIncomplete, got {other:?}"),
        }

        // The message names both secrets and the vault, preserves the
        // underlying failure, and gives concrete recovery steps.
        let msg = err.to_string();
        assert!(msg.contains("'legacy-name'"), "missing source: {msg}");
        assert!(msg.contains("'modern-name'"), "missing destination: {msg}");
        assert!(msg.contains("'test-vault'"), "missing vault: {msg}");
        assert!(msg.contains("simulated outage"), "missing cause: {msg}");
        assert!(msg.contains("Next steps"), "missing recovery plan: {msg}");
        assert!(
            msg.contains("xv get modern-name"),
            "missing verify step: {msg}"
        );
        assert!(
            msg.contains("xv delete legacy-name"),
            "missing manual delete step: {msg}"
        );

        // The new secret was created exactly once and never rolled back:
        // the only delete attempt targeted the old name, and nothing was
        // purged.
        let sets = ops.set_requests.lock().unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].0, "modern-name");
        assert_eq!(
            ops.delete_attempts.lock().unwrap().as_slice(),
            ["legacy-name".to_string()]
        );
        assert!(ops.purge_attempts.lock().unwrap().is_empty());
    }

    fn test_ops() -> AzureSecretOperations {
        AzureSecretOperations::new(Arc::new(
            crate::auth::provider::DefaultAzureCredentialProvider::new().unwrap(),
        ))
    }

    #[test]
    fn test_deleted_backup_restore_url_construction() {
        let ops = test_ops();
        let vault = ops.validated_vault_name("myvault").unwrap();

        assert_eq!(
            ops.key_vault_api_url(&vault, &["deletedsecrets"]).unwrap(),
            "https://myvault.vault.azure.net/deletedsecrets?api-version=7.4"
        );
        assert_eq!(
            ops.key_vault_api_url(&vault, &["secrets", "my-secret", "backup"])
                .unwrap(),
            "https://myvault.vault.azure.net/secrets/my-secret/backup?api-version=7.4"
        );
        assert_eq!(
            ops.key_vault_api_url(&vault, &["secrets", "restore"])
                .unwrap(),
            "https://myvault.vault.azure.net/secrets/restore?api-version=7.4"
        );
    }

    #[test]
    fn test_parse_deleted_secret_summary_full() {
        let item = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/my-secret",
            "recoveryId": "https://myvault.vault.azure.net/deletedsecrets/my-secret",
            "deletedDate": 1_700_000_100,
            "scheduledPurgeDate": 1_707_776_100,
            "contentType": "application/json",
            "attributes": {
                "enabled": false,
                "created": 1_700_000_000,
                "updated": 1_700_000_050,
                "recoveryLevel": "Recoverable+Purgeable"
            },
            "tags": {
                "original_name": "My Secret",
                "groups": "alpha,beta",
                "note": "a note",
                "folder": "apps/web"
            }
        });

        let summary = parse_deleted_secret_summary(&item).unwrap();
        assert_eq!(summary.name, "My Secret");
        assert_eq!(summary.original_name, "My Secret");
        assert_eq!(summary.note.as_deref(), Some("a note"));
        assert_eq!(summary.folder.as_deref(), Some("apps/web"));
        assert_eq!(summary.groups.as_deref(), Some("alpha,beta"));
        assert!(!summary.enabled);
        assert_eq!(summary.content_type, "application/json");
        assert_eq!(
            summary.updated_on,
            chrono::DateTime::from_timestamp(1_700_000_050, 0)
                .unwrap()
                .to_string()
        );
    }

    #[test]
    fn test_parse_deleted_secret_summary_minimal() {
        let item = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/bare-secret"
        });

        let summary = parse_deleted_secret_summary(&item).unwrap();
        assert_eq!(summary.name, "bare-secret");
        assert_eq!(summary.original_name, "bare-secret");
        assert_eq!(summary.note, None);
        assert_eq!(summary.folder, None);
        assert_eq!(summary.groups, None);
        assert!(summary.enabled);
        assert_eq!(summary.content_type, "text/plain");
        assert_eq!(summary.updated_on, "Unknown");
    }

    #[test]
    fn test_parse_deleted_secret_summary_legacy_name_tag_and_empty_groups() {
        let item = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/legacy",
            "tags": { "name": "Legacy Name", "groups": "   " }
        });

        let summary = parse_deleted_secret_summary(&item).unwrap();
        assert_eq!(summary.name, "Legacy Name");
        // Whitespace-only groups tag is treated as no groups
        assert_eq!(summary.groups, None);
    }

    #[test]
    fn test_parse_deleted_secret_summary_missing_id() {
        assert!(parse_deleted_secret_summary(&serde_json::json!({})).is_none());
        assert!(parse_deleted_secret_summary(&serde_json::json!({ "id": 42 })).is_none());
        assert!(parse_deleted_secret_summary(&serde_json::json!({ "id": "" })).is_none());
    }

    #[test]
    fn test_decode_backup_value_roundtrip() {
        let original: Vec<u8> = (0u8..=255).collect();
        let body = build_restore_request_body(&original);
        // The restore body re-encodes exactly what backup decoded
        let decoded = decode_backup_value(&body).unwrap();
        assert_eq!(decoded, original);
        // Encoded value is base64url (no '+', '/', or '=')
        let encoded = body.get("value").unwrap().as_str().unwrap();
        assert!(!encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='));
    }

    #[test]
    fn test_decode_backup_value_accepts_padding() {
        // base64url of [0, 1, 2, 3]; Azure may or may not include padding
        let unpadded = serde_json::json!({ "value": "AAECAw" });
        let padded = serde_json::json!({ "value": "AAECAw==" });
        assert_eq!(decode_backup_value(&unpadded).unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(decode_backup_value(&padded).unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_decode_backup_value_errors() {
        // Missing value field
        let err = decode_backup_value(&serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("missing 'value'"), "{err}");

        // Invalid base64url payload
        let err = decode_backup_value(&serde_json::json!({ "value": "!!!" })).unwrap_err();
        assert!(err.to_string().contains("Failed to decode"), "{err}");
    }

    #[test]
    fn test_parse_restored_secret_properties_full() {
        let json = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/my-secret/abc123def456",
            "contentType": "application/json",
            "attributes": {
                "enabled": true,
                "created": 1_700_000_000,
                "updated": 1_700_000_050,
                "exp": 1_800_000_000,
                "nbf": 1_600_000_000,
                "recoveryLevel": "Recoverable"
            },
            "tags": {
                "original_name": "My Secret",
                "groups": "alpha"
            }
        });

        let props = parse_restored_secret_properties(&json).unwrap();
        assert_eq!(props.name, "my-secret");
        assert_eq!(props.original_name, "My Secret");
        assert_eq!(props.version, "abc123def456");
        assert!(props.value.is_none());
        assert!(props.enabled);
        assert_eq!(props.created_timestamp, 1_700_000_000);
        assert_eq!(
            props.expires_on,
            chrono::DateTime::from_timestamp(1_800_000_000, 0)
        );
        assert_eq!(
            props.not_before,
            chrono::DateTime::from_timestamp(1_600_000_000, 0)
        );
        assert_eq!(props.content_type, "application/json");
        assert_eq!(props.recovery_level.as_deref(), Some("Recoverable"));
        assert_eq!(props.tags.get("groups").map(String::as_str), Some("alpha"));
    }

    #[test]
    fn test_parse_restored_secret_properties_minimal() {
        let json = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/plain/v1"
        });

        let props = parse_restored_secret_properties(&json).unwrap();
        assert_eq!(props.name, "plain");
        assert_eq!(props.original_name, "plain");
        assert_eq!(props.version, "v1");
        assert!(props.enabled);
        assert_eq!(props.created_on, "Unknown");
        assert_eq!(props.updated_on, "Unknown");
        assert_eq!(props.expires_on, None);
        assert_eq!(props.not_before, None);
        assert_eq!(props.content_type, "text/plain");
    }

    #[test]
    fn test_parse_restored_secret_properties_errors() {
        // Missing id entirely
        let err = parse_restored_secret_properties(&serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("missing secret 'id'"), "{err}");

        // Id without enough path segments to carry a secret name
        let err =
            parse_restored_secret_properties(&serde_json::json!({ "id": "abc" })).unwrap_err();
        assert!(err.to_string().contains("unexpected secret id"), "{err}");
    }
}
