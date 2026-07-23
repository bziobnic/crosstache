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
use crate::utils::helpers::{parse_connection_string, validate_folder_path};
use crate::utils::network::{classify_network_error, create_http_client, NetworkConfig};
use crate::utils::sanitizer::sanitize_secret_name;

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

/// Attribute/tag-only update for [`SecretOperations::update_secret_attributes`].
///
/// `None` fields are left unchanged by the backend. `tags`, when `Some`,
/// replaces the entire tag map (Azure `PATCH /secrets/{name}` semantics), so
/// callers must supply the full desired map including crosstache's metadata
/// tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretAttributesUpdate {
    pub enabled: Option<bool>,
    pub content_type: Option<String>,
    pub expires_on: Option<DateTime<Utc>>,
    pub not_before: Option<DateTime<Utc>>,
    pub tags: Option<HashMap<String, String>>,
}

/// Secret update request for advanced operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretUpdateRequest {
    pub name: String,
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
    /// Full tag map, used to derive record-types metadata (`xv-type`,
    /// `f.*` fields) for `ls --type` filtering and JSON field lifting
    /// (record-types plan Task 10). `#[serde(default)]` so summaries
    /// deserialized from an older cache entry (written before this field
    /// existed) still parse.
    #[tabled(skip)]
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

/// Summary of a soft-deleted secret awaiting purge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedSecretSummary {
    pub name: String,
    pub original_name: String,
    /// When the secret was deleted (backend-formatted timestamp), when known.
    pub deleted_on: Option<String>,
    /// When the backend will permanently purge it (None = no schedule).
    pub scheduled_purge_on: Option<String>,
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

/// Human-readable description for a connection-string key. Pure string
/// mapping — no manager/backend state required.
pub fn connection_string_key_description(key: &str) -> String {
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

/// Parse a connection string into described components, without needing a
/// [`SecretManager`]. Wraps [`crate::utils::helpers::parse_connection_string`]
/// (the raw key/value parser) and annotates each pair with a description.
pub fn parse_connection_components(connection_string: &str) -> Vec<ConnectionComponent> {
    parse_connection_string(connection_string)
        .into_iter()
        .map(|(key, value)| ConnectionComponent {
            description: connection_string_key_description(&key),
            key,
            value,
        })
        .collect()
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

    /// Update only the attributes/tags of an existing secret — no new
    /// version, no value read or write.
    ///
    /// Azure implements this with `PATCH {vault}/secrets/{name}` (the
    /// UpdateSecret operation), which works on *disabled* secrets — the
    /// full-write path cannot, because reading or confirming the value of a
    /// disabled secret returns HTTP 403 `SecretDisabled`. Implementors
    /// without a dedicated attributes call keep this default error so
    /// callers can fall back to a full write.
    async fn update_secret_attributes(
        &self,
        _vault_name: &str,
        _secret_name: &str,
        _update: &SecretAttributesUpdate,
    ) -> Result<SecretProperties> {
        Err(CrosstacheError::azure_api(
            "attribute-only secret updates are not supported by this backend",
        ))
    }

    /// Restore a deleted secret
    async fn restore_secret(&self, vault_name: &str, secret_name: &str)
        -> Result<SecretProperties>;

    /// Permanently purge a deleted secret
    async fn purge_secret(&self, vault_name: &str, secret_name: &str) -> Result<()>;

    /// List soft-deleted secrets awaiting purge.
    async fn list_deleted_secrets(&self, vault_name: &str) -> Result<Vec<DeletedSecretSummary>>;

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
        tags.insert(
            crate::backend::TAG_ORIGINAL_NAME.to_string(),
            request.name.clone(),
        );
        tags.insert(
            crate::backend::TAG_CREATED_BY.to_string(),
            "crosstache".to_string(),
        );

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

/// Parse one item from Azure's `GET {vault}/deletedsecrets` (api 7.4).
/// `deletedDate`/`scheduledPurgeDate` are top-level epoch-second fields on
/// the deleted-secret item (not under `attributes`).
fn parse_deleted_secret_summary(item: &serde_json::Value) -> Option<DeletedSecretSummary> {
    let id = item.get("id").and_then(|v| v.as_str())?;
    let name = id.rsplit('/').next().unwrap_or(id).to_string();
    if name.is_empty() {
        return None;
    }

    let epoch_string = |key: &str| {
        item.get(key)
            .and_then(|v| v.as_i64())
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt| dt.to_string())
    };

    let original_name = item
        .get("tags")
        .and_then(|v| v.as_object())
        .and_then(|tags| {
            tags.get("original_name")
                .or_else(|| tags.get("name"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_string)
        .unwrap_or_else(|| name.clone());

    Some(DeletedSecretSummary {
        name,
        original_name,
        deleted_on: epoch_string("deletedDate"),
        scheduled_purge_on: epoch_string("scheduledPurgeDate"),
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

        // Parse the response and convert to SecretProperties. The PUT
        // response is a full secret bundle (id, value, attributes, tags), so
        // build the result from it directly instead of a confirmation GET —
        // a follow-up GET would return HTTP 403 `SecretDisabled` when this
        // write just disabled the secret (enabled=false), failing the
        // operation *after* the write succeeded.
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;
        parse_secret_properties_bundle(&json, &sanitized_name, true, "")
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
                                tags: secret_details.tags,
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
                                tags: HashMap::new(),
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

    async fn update_secret_attributes(
        &self,
        vault_name: &str,
        secret_name: &str,
        update: &SecretAttributesUpdate,
    ) -> Result<SecretProperties> {
        let vault_name = self.validated_vault_name(vault_name)?;
        let sanitized_name = sanitize_secret_name(secret_name)?;

        // `PATCH {vault}/secrets/{name}?api-version=7.4` (UpdateSecret):
        // updates attributes/tags of the latest version without reading or
        // writing the value, so it works on disabled secrets and never
        // creates a new version. Omitted body fields are left unchanged.
        let secret_url = self.key_vault_api_url(&vault_name, &["secrets", &sanitized_name])?;

        // Get an access token for Key Vault
        let token = self
            .auth_provider
            .get_token(&["https://vault.azure.net/.default"])
            .await?;

        // Build the request body from only the fields being changed.
        let mut body = serde_json::json!({});
        let mut attributes = serde_json::json!({});
        if let Some(enabled) = update.enabled {
            attributes["enabled"] = serde_json::json!(enabled);
        }
        if let Some(expires_on) = update.expires_on {
            attributes["exp"] = serde_json::json!(expires_on.timestamp());
        }
        if let Some(not_before) = update.not_before {
            attributes["nbf"] = serde_json::json!(not_before.timestamp());
        }
        if attributes.as_object().is_some_and(|obj| !obj.is_empty()) {
            body["attributes"] = attributes;
        }
        if let Some(ref content_type) = update.content_type {
            body["contentType"] = serde_json::json!(content_type);
        }
        if let Some(ref tags) = update.tags {
            if let Some(folder) = tags.get("folder") {
                validate_folder_path(folder)?;
            }
            body["tags"] = serde_json::json!(tags);
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
            .patch(&secret_url)
            .headers(headers)
            .json(&body)
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
                "Failed to update secret attributes: HTTP {status} - {error_text}"
            )));
        }

        // The PATCH response is a secret bundle without the value.
        let json: serde_json::Value =
            read_json_body(response, crate::utils::MAX_RESPONSE_BYTES).await?;
        parse_secret_properties_bundle(&json, &sanitized_name, false, "")
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

        // Use REST API to permanently purge a deleted secret.
        // The purge operation is `DELETE {vault}/deletedsecrets/{name}` —
        // no trailing `/purge` segment; that returns HTTP 400 BadParameter
        // ("Method DELETE does not allow operation 'purge'").
        let purge_url =
            self.key_vault_api_url(&vault_name, &["deletedsecrets", &sanitized_name])?;

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

    async fn list_deleted_secrets(&self, vault_name: &str) -> Result<Vec<DeletedSecretSummary>> {
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
        let value = json
            .get("value")
            .and_then(|value| value.as_str())
            .ok_or_else(|| CrosstacheError::azure_api("Backup response missing 'value' field"))?;
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        URL_SAFE_NO_PAD
            .decode(value.trim_end_matches('='))
            .map_err(|error| {
                CrosstacheError::azure_api(format!("Failed to decode backup payload: {error}"))
            })
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

        // Make the REST API call to restore the secret from the backup.
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let restore_body = serde_json::json!({ "value": URL_SAFE_NO_PAD.encode(backup_data) });
        let response = client
            .post(&restore_url)
            .headers(headers)
            .json(&restore_body)
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
        let id = json
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                CrosstacheError::azure_api("Restore response missing secret 'id' field")
            })?;
        if id.rsplit('/').nth(1).is_none_or(str::is_empty) {
            return Err(CrosstacheError::azure_api(format!(
                "Restore response contained unexpected secret id '{id}'"
            )));
        }
        parse_secret_properties_bundle(&json, "", false, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_ops() -> AzureSecretOperations {
        AzureSecretOperations::new(Arc::new(
            crate::auth::provider::DefaultAzureCredentialProvider::new().unwrap(),
        ))
    }

    /// Bugbot HIGH review, round 3: `prepare_secret_request` is the exact
    /// function the record write-back paths (execute_record_field_update's
    /// secret-field-edit branch, execute_record_type_conversion,
    /// execute_record_untype — all value-changing, so Azure's full-PUT
    /// path) run through. It only re-adds groups/note/folder tags when the
    /// corresponding `SecretRequest` field is `Some` — there is no "carry
    /// forward the existing tag" fallback here, unlike the tri-state
    /// `FieldUpdate` used elsewhere. This pins the positive case: all three
    /// present and correctly encoded.
    #[test]
    fn prepare_secret_request_emits_groups_note_folder_when_present() {
        let ops = test_ops();
        let request = SecretRequest {
            name: "cred".to_string(),
            value: Zeroizing::new("v".to_string()),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: None,
            groups: Some(vec!["prod".to_string(), "team-a".to_string()]),
            note: Some("rotate monthly".to_string()),
            folder: Some("app/db".to_string()),
        };
        let (_, tags) = ops.prepare_secret_request(&request).unwrap();
        assert_eq!(tags.get("groups").map(String::as_str), Some("prod,team-a"));
        assert_eq!(tags.get("note").map(String::as_str), Some("rotate monthly"));
        assert_eq!(tags.get("folder").map(String::as_str), Some("app/db"));
    }

    /// Companion negative test, documenting the exact PUT semantics that
    /// bit the record write-back paths (Bugbot review, round 3): a
    /// value-changing `SecretRequest` (this is the full-PUT path, not the
    /// attributes-only PATCH) with `groups: None` must NOT emit a `groups`
    /// tag — `prepare_secret_request` has no "None means leave the
    /// existing tag alone" fallback the way `FieldUpdate::Unchanged` does
    /// for note/folder elsewhere in the update flow. A caller that
    /// forgets this (as the record write-back paths did before the fix)
    /// silently erases group membership on Azure, even though the same
    /// `groups: None` is a safe no-op on the delta-based local/AWS
    /// backends.
    #[test]
    fn prepare_secret_request_omits_groups_tag_when_groups_is_none_even_with_a_value() {
        let ops = test_ops();
        let request = SecretRequest {
            name: "cred".to_string(),
            value: Zeroizing::new("v".to_string()),
            content_type: None,
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        };
        let (_, tags) = ops.prepare_secret_request(&request).unwrap();
        assert!(
            !tags.contains_key("groups"),
            "groups tag must not appear when SecretRequest.groups is None: {tags:?}"
        );
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
            "id": "https://myvault.vault.azure.net/deletedsecrets/my-secret",
            "deletedDate": 1_700_000_100,
            "scheduledPurgeDate": 1_707_776_100,
            "tags": {
                "original_name": "My Secret"
            }
        });

        let summary = parse_deleted_secret_summary(&item).unwrap();
        assert_eq!(summary.name, "my-secret");
        assert_eq!(summary.original_name, "My Secret");
        assert_eq!(
            summary.deleted_on,
            Some(
                chrono::DateTime::from_timestamp(1_700_000_100, 0)
                    .unwrap()
                    .to_string()
            )
        );
        assert_eq!(
            summary.scheduled_purge_on,
            Some(
                chrono::DateTime::from_timestamp(1_707_776_100, 0)
                    .unwrap()
                    .to_string()
            )
        );
    }

    #[test]
    fn test_parse_deleted_secret_summary_minimal() {
        let item = serde_json::json!({
            "id": "https://myvault.vault.azure.net/deletedsecrets/bare-secret"
        });

        let summary = parse_deleted_secret_summary(&item).unwrap();
        assert_eq!(summary.name, "bare-secret");
        assert_eq!(summary.original_name, "bare-secret");
        assert_eq!(summary.deleted_on, None);
        assert_eq!(summary.scheduled_purge_on, None);
    }

    #[test]
    fn test_parse_deleted_secret_summary_legacy_name_tag_and_empty_groups() {
        let item = serde_json::json!({
            "id": "https://myvault.vault.azure.net/deletedsecrets/legacy",
            "tags": { "name": "Legacy Name", "groups": "   " }
        });

        let summary = parse_deleted_secret_summary(&item).unwrap();
        assert_eq!(summary.name, "legacy");
        assert_eq!(summary.original_name, "Legacy Name");
    }

    #[test]
    fn test_parse_deleted_secret_summary_missing_id() {
        assert!(parse_deleted_secret_summary(&serde_json::json!({})).is_none());
        assert!(parse_deleted_secret_summary(&serde_json::json!({ "id": 42 })).is_none());
        assert!(parse_deleted_secret_summary(&serde_json::json!({ "id": "" })).is_none());
    }

    #[test]
    fn test_parse_secret_properties_bundle_full_without_value() {
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

        let props = parse_secret_properties_bundle(&json, "", false, "").unwrap();
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
    fn test_parse_secret_properties_bundle_minimal_without_value() {
        let json = serde_json::json!({
            "id": "https://myvault.vault.azure.net/secrets/plain/v1"
        });

        let props = parse_secret_properties_bundle(&json, "", false, "").unwrap();
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
}
