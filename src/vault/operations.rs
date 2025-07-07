//! Vault operations implementation
//!
//! This module provides core vault management operations including
//! creation, deletion, access control, and metadata management.

use async_trait::async_trait;
use reqwest::{header::HeaderMap, Client};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::models::{
    AccessLevel, AccessPolicy, BuiltInRoles, RoleAssignmentRequest, VaultCreateRequest,
    VaultProperties, VaultRole, VaultStatus, VaultSummary, VaultUpdateRequest,
};
use crate::auth::provider::AzureAuthProvider;
use crate::error::{crosstacheError, Result};
use crate::utils::network::{classify_network_error, create_http_client, NetworkConfig};
use crate::utils::retry::retry_with_backoff;

/// Trait for vault operations
#[async_trait]
pub trait VaultOperations: Send + Sync {
    /// Create a new vault
    async fn create_vault(&self, request: &VaultCreateRequest) -> Result<VaultProperties>;

    /// Get vault details
    async fn get_vault(&self, vault_name: &str, resource_group: &str) -> Result<VaultProperties>;

    /// List vaults in subscription
    async fn list_vaults(
        &self,
        subscription_id: Option<&str>,
        resource_group: Option<&str>,
    ) -> Result<Vec<VaultSummary>>;

    /// Update vault properties
    async fn update_vault(
        &self,
        vault_name: &str,
        resource_group: &str,
        request: &VaultUpdateRequest,
    ) -> Result<VaultProperties>;

    /// Delete vault (soft delete)
    async fn delete_vault(&self, vault_name: &str, resource_group: &str) -> Result<()>;

    /// Restore soft-deleted vault
    async fn restore_vault(&self, vault_name: &str, location: &str) -> Result<VaultProperties>;

    /// Permanently purge vault
    async fn purge_vault(&self, vault_name: &str, location: &str) -> Result<()>;

    /// List deleted vaults
    async fn list_deleted_vaults(&self, subscription_id: &str) -> Result<Vec<VaultSummary>>;

    /// Grant access to vault
    async fn grant_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        user_object_id: &str,
        access_level: AccessLevel,
    ) -> Result<()>;

    /// Revoke access from vault
    async fn revoke_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        user_object_id: &str,
    ) -> Result<()>;

    /// List vault access assignments
    async fn list_access(&self, vault_name: &str, resource_group: &str) -> Result<Vec<VaultRole>>;

    /// Check vault existence
    async fn vault_exists(&self, vault_name: &str, resource_group: &str) -> Result<bool>;

    /// Get vault tags
    async fn get_vault_tags(
        &self,
        vault_name: &str,
        resource_group: &str,
    ) -> Result<HashMap<String, String>>;

    /// Update vault tags
    async fn update_vault_tags(
        &self,
        vault_name: &str,
        resource_group: &str,
        tags: HashMap<String, String>,
    ) -> Result<()>;
}

/// Azure vault operations implementation
pub struct AzureVaultOperations {
    auth_provider: Arc<dyn AzureAuthProvider>,
    http_client: Client,
    subscription_id: String,
}

impl AzureVaultOperations {
    /// Create a new Azure vault operations instance
    pub fn new(auth_provider: Arc<dyn AzureAuthProvider>, subscription_id: String) -> Result<Self> {
        let network_config = NetworkConfig::default();
        let http_client = create_http_client(&network_config)?;

        Ok(Self {
            auth_provider,
            http_client,
            subscription_id,
        })
    }

    /// Get access token for Azure Resource Manager
    async fn get_management_token(&self) -> Result<String> {
        let token = self
            .auth_provider
            .get_token(&["https://management.azure.com/.default"])
            .await?;
        Ok(token.token.secret().to_string())
    }

    /// Create authorized headers for Azure REST API
    async fn create_headers(&self) -> Result<HeaderMap> {
        let token = self.get_management_token().await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", token).parse().map_err(|e| {
                crosstacheError::authentication(format!("Invalid token format: {}", e))
            })?,
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        Ok(headers)
    }

    /// Build Azure Resource Manager URL
    fn build_arm_url(&self, path: &str) -> String {
        format!("https://management.azure.com{}", path)
    }

    /// Get vault ARM resource ID
    fn get_vault_resource_id(&self, vault_name: &str, resource_group: &str) -> String {
        format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.KeyVault/vaults/{}",
            self.subscription_id, resource_group, vault_name
        )
    }

    /// Parse Azure error response
    fn parse_azure_error(&self, status: u16, body: &str) -> crosstacheError {
        if let Ok(error_json) = serde_json::from_str::<Value>(body) {
            if let Some(error) = error_json.get("error") {
                if let Some(message) = error.get("message").and_then(|m| m.as_str()) {
                    return crosstacheError::azure_api(format!("HTTP {}: {}", status, message));
                }
            }
        }
        crosstacheError::azure_api(format!("HTTP {}: {}", status, body))
    }

    /// Retry wrapper for Azure operations
    async fn execute_with_retry<F, Fut, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let retry_options = crate::utils::retry::RetryOptions {
            max_retries: 3,
            initial_interval: std::time::Duration::from_millis(1000),
            max_interval: std::time::Duration::from_millis(10000),
            multiplier: 2.0,
        };
        retry_with_backoff(operation, retry_options).await
    }
}

#[async_trait]
impl VaultOperations for AzureVaultOperations {
    async fn create_vault(&self, request: &VaultCreateRequest) -> Result<VaultProperties> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let resource_id = self.get_vault_resource_id(&request.name, &request.resource_group);
            let url = self.build_arm_url(&format!("{}?api-version=2023-07-01", resource_id));

            let tenant_id = self.auth_provider.get_tenant_id().await?;
            let current_user_object_id = self.auth_provider.get_object_id().await?;

            // Build access policies including current user as admin
            let mut access_policies = request.access_policies.clone().unwrap_or_default();
            if !access_policies
                .iter()
                .any(|p| p.object_id == current_user_object_id)
            {
                access_policies.push(AccessPolicy::new(
                    tenant_id.clone(),
                    current_user_object_id,
                    AccessLevel::Admin,
                    None,
                    None,
                ));
            }

            let body = json!({
                "location": request.location,
                "properties": {
                    "tenantId": tenant_id,
                    "sku": {
                        "family": "A",
                        "name": request.sku.as_ref().unwrap_or(&"standard".to_string())
                    },
                    "accessPolicies": access_policies,
                    "enabledForDeployment": request.enabled_for_deployment.unwrap_or(false),
                    "enabledForDiskEncryption": request.enabled_for_disk_encryption.unwrap_or(false),
                    "enabledForTemplateDeployment": request.enabled_for_template_deployment.unwrap_or(false),
                    "enableSoftDelete": true,
                    "softDeleteRetentionInDays": request.soft_delete_retention_in_days.unwrap_or(90),
                    "enablePurgeProtection": request.purge_protection.unwrap_or(false)
                },
                "tags": request.tags.as_ref().unwrap_or(&HashMap::new())
            });

            let response = self
                .http_client
                .put(&url)
                .headers(headers)
                .json(&body)
                .send()
                .await
                .map_err(|e| classify_network_error(&e, &url))?;

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            let vault_data: Value = response.json().await.map_err(|e| {
                crosstacheError::serialization(format!("Failed to parse vault response: {}", e))
            })?;

            self.parse_vault_properties(&vault_data)
        };

        self.execute_with_retry(operation).await
    }

    async fn get_vault(&self, vault_name: &str, resource_group: &str) -> Result<VaultProperties> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let resource_id = self.get_vault_resource_id(vault_name, resource_group);
            let url = self.build_arm_url(&format!("{}?api-version=2023-07-01", resource_id));

            let response = self
                .http_client
                .get(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| classify_network_error(&e, &url))?;

            if response.status().as_u16() == 404 {
                return Err(crosstacheError::vault_not_found(vault_name));
            }

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            let vault_data: Value = response.json().await.map_err(|e| {
                crosstacheError::serialization(format!("Failed to parse vault response: {}", e))
            })?;

            self.parse_vault_properties(&vault_data)
        };

        self.execute_with_retry(operation).await
    }

    async fn list_vaults(
        &self,
        subscription_id: Option<&str>,
        resource_group: Option<&str>,
    ) -> Result<Vec<VaultSummary>> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let sub_id = subscription_id.unwrap_or(&self.subscription_id);

            let url = if let Some(rg) = resource_group {
                self.build_arm_url(&format!(
                    "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.KeyVault/vaults?api-version=2023-07-01",
                    sub_id, rg
                ))
            } else {
                self.build_arm_url(&format!(
                    "/subscriptions/{}/providers/Microsoft.KeyVault/vaults?api-version=2023-07-01",
                    sub_id
                ))
            };

            let response = self
                .http_client
                .get(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| crosstacheError::network(format!("Failed to list vaults: {}", e)))?;

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            let response_data: Value = response.json().await.map_err(|e| {
                crosstacheError::serialization(format!("Failed to parse vaults response: {}", e))
            })?;

            let mut vaults = Vec::new();
            if let Some(vault_array) = response_data.get("value").and_then(|v| v.as_array()) {
                for vault_value in vault_array {
                    if let Ok(vault_props) = self.parse_vault_properties(vault_value) {
                        vaults.push(vault_props.to_summary(None));
                    }
                }
            }

            Ok(vaults)
        };

        self.execute_with_retry(operation).await
    }

    async fn update_vault(
        &self,
        vault_name: &str,
        resource_group: &str,
        request: &VaultUpdateRequest,
    ) -> Result<VaultProperties> {
        let operation = || async {
            // First get the current vault to merge properties
            let current_vault = self.get_vault(vault_name, resource_group).await?;

            let headers = self.create_headers().await?;
            let resource_id = self.get_vault_resource_id(vault_name, resource_group);
            let url = self.build_arm_url(&format!("{}?api-version=2023-07-01", resource_id));

            let properties = json!({
                "tenantId": current_vault.tenant_id,
                "sku": {
                    "family": "A",
                    "name": current_vault.sku
                },
                "accessPolicies": request.access_policies.as_ref().unwrap_or(&current_vault.access_policies),
                "enabledForDeployment": request.enabled_for_deployment.unwrap_or(current_vault.enabled_for_deployment),
                "enabledForDiskEncryption": request.enabled_for_disk_encryption.unwrap_or(current_vault.enabled_for_disk_encryption),
                "enabledForTemplateDeployment": request.enabled_for_template_deployment.unwrap_or(current_vault.enabled_for_template_deployment),
                "enableSoftDelete": true,
                "softDeleteRetentionInDays": request.soft_delete_retention_in_days.unwrap_or(current_vault.soft_delete_retention_in_days),
                "enablePurgeProtection": request.purge_protection.unwrap_or(current_vault.purge_protection)
            });

            let body = json!({
                "location": current_vault.location,
                "properties": properties,
                "tags": request.tags.as_ref().unwrap_or(&current_vault.tags)
            });

            let response = self
                .http_client
                .put(&url)
                .headers(headers)
                .json(&body)
                .send()
                .await
                .map_err(|e| crosstacheError::network(format!("Failed to update vault: {}", e)))?;

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            let vault_data: Value = response.json().await.map_err(|e| {
                crosstacheError::serialization(format!("Failed to parse vault response: {}", e))
            })?;

            self.parse_vault_properties(&vault_data)
        };

        self.execute_with_retry(operation).await
    }

    async fn delete_vault(&self, vault_name: &str, resource_group: &str) -> Result<()> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let resource_id = self.get_vault_resource_id(vault_name, resource_group);
            let url = self.build_arm_url(&format!("{}?api-version=2023-07-01", resource_id));

            let response = self
                .http_client
                .delete(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| crosstacheError::network(format!("Failed to delete vault: {}", e)))?;

            if response.status().as_u16() == 404 {
                return Err(crosstacheError::vault_not_found(vault_name));
            }

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            Ok(())
        };

        self.execute_with_retry(operation).await
    }

    async fn restore_vault(&self, vault_name: &str, location: &str) -> Result<VaultProperties> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let url = self.build_arm_url(&format!(
                "/subscriptions/{}/providers/Microsoft.KeyVault/locations/{}/deletedVaults/{}/recover?api-version=2023-07-01",
                self.subscription_id, location, vault_name
            ));

            let response = self
                .http_client
                .post(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| crosstacheError::network(format!("Failed to restore vault: {}", e)))?;

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            // After restore, wait a bit and then get the vault details
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            // We need to find the resource group - this is a limitation of the restore API
            // For now, we'll return a basic vault properties structure
            Ok(VaultProperties {
                id: format!(
                    "/subscriptions/{}/providers/Microsoft.KeyVault/vaults/{}",
                    self.subscription_id, vault_name
                ),
                name: vault_name.to_string(),
                location: location.to_string(),
                resource_group: "restored".to_string(), // Placeholder
                subscription_id: self.subscription_id.clone(),
                tenant_id: self.auth_provider.get_tenant_id().await?,
                uri: format!("https://{}.vault.azure.net/", vault_name),
                enabled_for_deployment: false,
                enabled_for_disk_encryption: false,
                enabled_for_template_deployment: false,
                soft_delete_retention_in_days: 90,
                purge_protection: false,
                sku: "standard".to_string(),
                access_policies: Vec::new(),
                created_at: chrono::Utc::now(),
                tags: HashMap::new(),
            })
        };

        self.execute_with_retry(operation).await
    }

    async fn purge_vault(&self, vault_name: &str, location: &str) -> Result<()> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let url = self.build_arm_url(&format!(
                "/subscriptions/{}/providers/Microsoft.KeyVault/locations/{}/deletedVaults/{}/purge?api-version=2023-07-01",
                self.subscription_id, location, vault_name
            ));

            let response = self
                .http_client
                .post(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| crosstacheError::network(format!("Failed to purge vault: {}", e)))?;

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            Ok(())
        };

        self.execute_with_retry(operation).await
    }

    async fn list_deleted_vaults(&self, subscription_id: &str) -> Result<Vec<VaultSummary>> {
        let operation = || async {
            let headers = self.create_headers().await?;
            let url = self.build_arm_url(&format!(
                "/subscriptions/{}/providers/Microsoft.KeyVault/deletedVaults?api-version=2023-07-01",
                subscription_id
            ));

            let response = self
                .http_client
                .get(&url)
                .headers(headers)
                .send()
                .await
                .map_err(|e| {
                    crosstacheError::network(format!("Failed to list deleted vaults: {}", e))
                })?;

            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let error_body = response.text().await.unwrap_or_default();
                return Err(self.parse_azure_error(status_code, &error_body));
            }

            let response_data: Value = response.json().await.map_err(|e| {
                crosstacheError::serialization(format!(
                    "Failed to parse deleted vaults response: {}",
                    e
                ))
            })?;

            let mut vaults = Vec::new();
            if let Some(vault_array) = response_data.get("value").and_then(|v| v.as_array()) {
                for vault_value in vault_array {
                    if let Some(properties) = vault_value.get("properties") {
                        let name = vault_value
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let location = properties
                            .get("location")
                            .and_then(|l| l.as_str())
                            .unwrap_or("unknown");
                        let deletion_date = properties
                            .get("deletionDate")
                            .and_then(|d| d.as_str())
                            .unwrap_or("unknown");

                        vaults.push(VaultSummary {
                            name: name.to_string(),
                            location: location.to_string(),
                            resource_group: "deleted".to_string(),
                            status: "Soft Deleted".to_string(),
                            secret_count: None,
                            created_at: deletion_date.to_string(),
                        });
                    }
                }
            }

            Ok(vaults)
        };

        self.execute_with_retry(operation).await
    }

    async fn grant_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        user_object_id: &str,
        access_level: AccessLevel,
    ) -> Result<()> {
        // Get current vault to update access policies
        let mut current_vault = self.get_vault(vault_name, resource_group).await?;

        // Remove existing policy for this user if any
        current_vault
            .access_policies
            .retain(|p| p.object_id != user_object_id);

        // Add new policy
        let tenant_id = self.auth_provider.get_tenant_id().await?;
        let new_policy = AccessPolicy::new(
            tenant_id,
            user_object_id.to_string(),
            access_level,
            None,
            None,
        );
        current_vault.access_policies.push(new_policy);

        // Update vault with new access policies
        let update_request = VaultUpdateRequest {
            enabled_for_deployment: None,
            enabled_for_disk_encryption: None,
            enabled_for_template_deployment: None,
            soft_delete_retention_in_days: None,
            purge_protection: None,
            tags: None,
            access_policies: Some(current_vault.access_policies),
        };

        self.update_vault(vault_name, resource_group, &update_request)
            .await?;
        Ok(())
    }

    async fn revoke_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        user_object_id: &str,
    ) -> Result<()> {
        // Get current vault to update access policies
        let mut current_vault = self.get_vault(vault_name, resource_group).await?;

        // Remove policy for this user
        let original_count = current_vault.access_policies.len();
        current_vault
            .access_policies
            .retain(|p| p.object_id != user_object_id);

        if current_vault.access_policies.len() == original_count {
            return Err(crosstacheError::permission_denied(
                "User does not have access to this vault",
            ));
        }

        // Update vault with new access policies
        let update_request = VaultUpdateRequest {
            enabled_for_deployment: None,
            enabled_for_disk_encryption: None,
            enabled_for_template_deployment: None,
            soft_delete_retention_in_days: None,
            purge_protection: None,
            tags: None,
            access_policies: Some(current_vault.access_policies),
        };

        self.update_vault(vault_name, resource_group, &update_request)
            .await?;
        Ok(())
    }

    async fn list_access(&self, vault_name: &str, resource_group: &str) -> Result<Vec<VaultRole>> {
        let operation = || async {
            let vault = self.get_vault(vault_name, resource_group).await?;
            let mut roles = Vec::new();

            for policy in &vault.access_policies {
                // Convert access policy to vault role for display
                let role = VaultRole {
                    assignment_id: Uuid::new_v4().to_string(),
                    role_id: "access-policy".to_string(),
                    role_name: self.determine_access_level_from_permissions(&policy.permissions),
                    role_description: "Access Policy".to_string(),
                    principal_id: policy.object_id.clone(),
                    principal_name: policy
                        .user_email
                        .clone()
                        .unwrap_or_else(|| policy.object_id.clone()),
                    principal_type: if policy.application_id.is_some() {
                        "ServicePrincipal"
                    } else {
                        "User"
                    }
                    .to_string(),
                    scope: vault.id.clone(),
                    created_on: vault.created_at,
                    updated_on: vault.created_at,
                };
                roles.push(role);
            }

            Ok(roles)
        };

        self.execute_with_retry(operation).await
    }

    async fn vault_exists(&self, vault_name: &str, resource_group: &str) -> Result<bool> {
        match self.get_vault(vault_name, resource_group).await {
            Ok(_) => Ok(true),
            Err(crosstacheError::VaultNotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn get_vault_tags(
        &self,
        vault_name: &str,
        resource_group: &str,
    ) -> Result<HashMap<String, String>> {
        let vault = self.get_vault(vault_name, resource_group).await?;
        Ok(vault.tags)
    }

    async fn update_vault_tags(
        &self,
        vault_name: &str,
        resource_group: &str,
        tags: HashMap<String, String>,
    ) -> Result<()> {
        let update_request = VaultUpdateRequest {
            enabled_for_deployment: None,
            enabled_for_disk_encryption: None,
            enabled_for_template_deployment: None,
            soft_delete_retention_in_days: None,
            purge_protection: None,
            tags: Some(tags),
            access_policies: None,
        };

        self.update_vault(vault_name, resource_group, &update_request)
            .await?;
        Ok(())
    }
}

impl AzureVaultOperations {
    /// Parse Azure ARM vault response into VaultProperties
    fn parse_vault_properties(&self, vault_data: &Value) -> Result<VaultProperties> {
        let properties = vault_data.get("properties").ok_or_else(|| {
            crosstacheError::serialization("Missing properties in vault response")
        })?;

        let id = vault_data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let name = vault_data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let location = vault_data
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Extract resource group from ID
        let resource_group = id.split('/').nth(4).unwrap_or_default().to_string();

        let subscription_id = id
            .split('/')
            .nth(2)
            .unwrap_or(&self.subscription_id)
            .to_string();

        let tenant_id = properties
            .get("tenantId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let uri = properties
            .get("vaultUri")
            .and_then(|v| v.as_str())
            .unwrap_or(&format!("https://{}.vault.azure.net/", name))
            .to_string();

        let sku = properties
            .get("sku")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("standard")
            .to_string();

        // Parse access policies
        let mut access_policies = Vec::new();
        if let Some(policies_array) = properties.get("accessPolicies").and_then(|v| v.as_array()) {
            for policy_value in policies_array {
                if let Ok(policy) = self.parse_access_policy(policy_value) {
                    access_policies.push(policy);
                }
            }
        }

        // Parse tags
        let mut tags = HashMap::new();
        if let Some(tags_obj) = vault_data.get("tags").and_then(|v| v.as_object()) {
            for (key, value) in tags_obj {
                if let Some(val_str) = value.as_str() {
                    tags.insert(key.clone(), val_str.to_string());
                }
            }
        }

        Ok(VaultProperties {
            id,
            name,
            location,
            resource_group,
            subscription_id,
            tenant_id,
            uri,
            enabled_for_deployment: properties
                .get("enabledForDeployment")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            enabled_for_disk_encryption: properties
                .get("enabledForDiskEncryption")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            enabled_for_template_deployment: properties
                .get("enabledForTemplateDeployment")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            soft_delete_retention_in_days: properties
                .get("softDeleteRetentionInDays")
                .and_then(|v| v.as_i64())
                .unwrap_or(90) as i32,
            purge_protection: properties
                .get("enablePurgeProtection")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            sku,
            access_policies,
            created_at: chrono::Utc::now(), // Azure doesn't provide creation time in ARM response
            tags,
        })
    }

    /// Parse access policy from Azure ARM response
    fn parse_access_policy(&self, policy_value: &Value) -> Result<AccessPolicy> {
        let tenant_id = policy_value
            .get("tenantId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let object_id = policy_value
            .get("objectId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let application_id = policy_value
            .get("applicationId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let permissions = policy_value.get("permissions").ok_or_else(|| {
            crosstacheError::serialization("Missing permissions in access policy")
        })?;

        let keys = permissions
            .get("keys")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let secrets = permissions
            .get("secrets")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let certificates = permissions
            .get("certificates")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let storage = permissions
            .get("storage")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(AccessPolicy {
            tenant_id,
            object_id,
            application_id,
            permissions: super::models::AccessPolicyPermissions {
                keys,
                secrets,
                certificates,
                storage,
            },
            user_email: None, // This would need to be resolved via Graph API
        })
    }

    /// Determine access level from permissions
    fn determine_access_level_from_permissions(
        &self,
        permissions: &super::models::AccessPolicyPermissions,
    ) -> String {
        if permissions.secrets.contains(&"purge".to_string()) {
            "Admin".to_string()
        } else if permissions.secrets.contains(&"set".to_string()) {
            "Contributor".to_string()
        } else if permissions.secrets.contains(&"get".to_string()) {
            "Reader".to_string()
        } else {
            "Custom".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::provider::DefaultAzureCredentialProvider;

    #[tokio::test]
    async fn test_vault_operations_creation() {
        // This is a basic test to ensure the structure compiles
        // Real tests would require Azure credentials and resources
        let auth_provider = DefaultAzureCredentialProvider::new().unwrap();
        let vault_ops =
            AzureVaultOperations::new(Arc::new(auth_provider), "test-subscription-id".to_string())
                .unwrap();

        // Test resource ID generation
        let resource_id = vault_ops.get_vault_resource_id("test-vault", "test-rg");
        assert!(resource_id.contains("test-vault"));
        assert!(resource_id.contains("test-rg"));
    }
}
