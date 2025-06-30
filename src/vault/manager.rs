//! Vault management facade
//! 
//! This module provides a high-level interface for vault operations,
//! combining vault operations with RBAC management and providing
//! a unified API for vault management tasks.

use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::provider::AzureAuthProvider;
use crate::error::{crosstacheError, Result};
use crate::utils::format::{TableFormatter, DisplayUtils, OutputFormat};
use super::models::{
    VaultProperties, VaultCreateRequest, VaultUpdateRequest, VaultSummary,
    AccessLevel, VaultRole, AccessPolicy
};
use super::operations::{VaultOperations, AzureVaultOperations};

/// High-level vault manager
pub struct VaultManager {
    vault_ops: Arc<dyn VaultOperations>,
    display_utils: DisplayUtils,
    no_color: bool,
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(auth_provider: Arc<dyn AzureAuthProvider>, subscription_id: String, no_color: bool) -> Self {
        let vault_ops = Arc::new(AzureVaultOperations::new(auth_provider, subscription_id));
        let display_utils = DisplayUtils::new(no_color);

        Self {
            vault_ops,
            display_utils,
            no_color,
        }
    }

    /// Create a new vault with automatic access policy setup
    pub async fn create_vault_with_setup(
        &self,
        name: &str,
        location: &str,
        resource_group: &str,
        additional_options: Option<VaultCreateRequest>,
    ) -> Result<VaultProperties> {
        self.display_utils.print_info(&format!("Creating vault '{}'...", name))?;

        let mut request = additional_options.unwrap_or_default();
        request.name = name.to_string();
        request.location = location.to_string();
        request.resource_group = resource_group.to_string();

        // Set sensible defaults if not provided
        if request.sku.is_none() {
            request.sku = Some("standard".to_string());
        }
        if request.soft_delete_retention_in_days.is_none() {
            request.soft_delete_retention_in_days = Some(90);
        }
        if request.purge_protection.is_none() {
            request.purge_protection = Some(false);
        }

        let vault = self.vault_ops.create_vault(&request).await?;

        self.display_utils.print_success(&format!(
            "Successfully created vault '{}' in {} ({})",
            vault.name, vault.location, vault.resource_group
        ))?;

        Ok(vault)
    }

    /// Get vault properties without displaying them
    pub async fn get_vault_properties(&self, vault_name: &str, resource_group: &str) -> Result<VaultProperties> {
        self.vault_ops.get_vault(vault_name, resource_group).await
    }

    /// Get vault information and display it
    pub async fn get_vault_info(&self, vault_name: &str, resource_group: &str) -> Result<VaultProperties> {
        let vault = self.vault_ops.get_vault(vault_name, resource_group).await?;

        // Display vault information
        self.display_vault_details(&vault)?;

        Ok(vault)
    }

    /// List vaults with formatted output
    pub async fn list_vaults_formatted(
        &self,
        subscription_id: Option<&str>,
        resource_group: Option<&str>,
        output_format: OutputFormat,
    ) -> Result<Vec<VaultSummary>> {
        let vaults = self.vault_ops.list_vaults(subscription_id, resource_group).await?;

        if vaults.is_empty() {
            self.display_utils.print_info("No vaults found.")?;
            return Ok(vaults);
        }

        // Format and display results
        let formatter = TableFormatter::new(output_format, self.no_color);
        let table_output = formatter.format_table(&vaults)?;
        println!("{}", table_output);

        Ok(vaults)
    }

    /// Delete vault with confirmation
    pub async fn delete_vault_safe(
        &self,
        vault_name: &str,
        resource_group: &str,
        force: bool,
    ) -> Result<()> {
        // Check if vault exists and get its details
        let vault = self.vault_ops.get_vault(vault_name, resource_group).await?;

        if !force {
            self.display_utils.print_warning(&format!(
                "This will soft-delete vault '{}' in resource group '{}'",
                vault_name, resource_group
            ))?;

            if vault.has_purge_protection() {
                self.display_utils.print_warning("This vault has purge protection enabled - it cannot be permanently deleted.")?;
            } else {
                self.display_utils.print_warning(&format!(
                    "The vault will be recoverable for {} days after deletion.",
                    vault.get_retention_days()
                ))?;
            }
        }

        self.vault_ops.delete_vault(vault_name, resource_group).await?;

        self.display_utils.print_success(&format!(
            "Successfully deleted vault '{}' (soft delete)",
            vault_name
        ))?;

        Ok(())
    }

    /// Restore a soft-deleted vault
    pub async fn restore_vault(&self, vault_name: &str, location: &str) -> Result<VaultProperties> {
        self.display_utils.print_info(&format!(
            "Restoring soft-deleted vault '{}'...",
            vault_name
        ))?;

        let vault = self.vault_ops.restore_vault(vault_name, location).await?;

        self.display_utils.print_success(&format!(
            "Successfully restored vault '{}'",
            vault_name
        ))?;

        Ok(vault)
    }

    /// Permanently purge a soft-deleted vault
    pub async fn purge_vault_permanent(
        &self,
        vault_name: &str,
        location: &str,
        force: bool,
    ) -> Result<()> {
        if !force {
            self.display_utils.print_warning(&format!(
                "This will PERMANENTLY delete vault '{}' and all its contents!",
                vault_name
            ))?;
            self.display_utils.print_warning("This action cannot be undone.")?;
        }

        self.vault_ops.purge_vault(vault_name, location).await?;

        self.display_utils.print_success(&format!(
            "Successfully purged vault '{}' (permanent deletion)",
            vault_name
        ))?;

        Ok(())
    }

    /// Grant access to a vault with user-friendly interface
    pub async fn grant_vault_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        user_object_id: &str,
        access_level: AccessLevel,
        user_email: Option<&str>,
    ) -> Result<()> {
        let access_level_str = match access_level {
            AccessLevel::Reader => "Reader",
            AccessLevel::Contributor => "Contributor",
            AccessLevel::Admin => "Administrator",
        };

        let user_display = user_email.unwrap_or(user_object_id);

        self.display_utils.print_info(&format!(
            "Granting {} access to vault '{}' for user '{}'...",
            access_level_str, vault_name, user_display
        ))?;

        self.vault_ops.grant_access(vault_name, resource_group, user_object_id, access_level).await?;

        self.display_utils.print_success(&format!(
            "Successfully granted {} access to vault '{}' for user '{}'",
            access_level_str, vault_name, user_display
        ))?;

        Ok(())
    }

    /// Revoke access from a vault
    pub async fn revoke_vault_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        user_object_id: &str,
        user_email: Option<&str>,
    ) -> Result<()> {
        let user_display = user_email.unwrap_or(user_object_id);

        self.display_utils.print_info(&format!(
            "Revoking access to vault '{}' for user '{}'...",
            vault_name, user_display
        ))?;

        self.vault_ops.revoke_access(vault_name, resource_group, user_object_id).await?;

        self.display_utils.print_success(&format!(
            "Successfully revoked access to vault '{}' for user '{}'",
            vault_name, user_display
        ))?;

        Ok(())
    }

    /// List vault access with formatted output
    pub async fn list_vault_access(
        &self,
        vault_name: &str,
        resource_group: &str,
        output_format: OutputFormat,
    ) -> Result<Vec<VaultRole>> {
        let roles = self.vault_ops.list_access(vault_name, resource_group).await?;

        if roles.is_empty() {
            self.display_utils.print_info("No access policies found for this vault.")?;
            return Ok(roles);
        }

        self.display_utils.print_header(&format!("Access Policies for Vault '{}'", vault_name))?;

        // Format and display results
        let formatter = TableFormatter::new(output_format, self.no_color);
        let table_output = formatter.format_table(&roles)?;
        println!("{}", table_output);

        Ok(roles)
    }

    /// Update vault tags with validation
    pub async fn update_vault_tags(
        &self,
        vault_name: &str,
        resource_group: &str,
        tags: HashMap<String, String>,
        merge_with_existing: bool,
    ) -> Result<()> {
        let final_tags = if merge_with_existing {
            let mut existing_tags = self.vault_ops.get_vault_tags(vault_name, resource_group).await?;
            existing_tags.extend(tags);
            existing_tags
        } else {
            tags
        };

        self.vault_ops.update_vault_tags(vault_name, resource_group, final_tags).await?;

        self.display_utils.print_success(&format!(
            "Successfully updated tags for vault '{}'",
            vault_name
        ))?;

        Ok(())
    }

    /// Check vault health and connectivity
    pub async fn check_vault_health(&self, vault_name: &str, resource_group: &str) -> Result<VaultHealthStatus> {
        let vault = match self.vault_ops.get_vault(vault_name, resource_group).await {
            Ok(v) => v,
            Err(_) => {
                return Ok(VaultHealthStatus {
                    vault_name: vault_name.to_string(),
                    exists: false,
                    accessible: false,
                    has_secrets: None,
                    issues: vec!["Vault does not exist or is not accessible".to_string()],
                });
            }
        };

        let mut issues = Vec::new();
        let mut accessible = true;

        // Check basic accessibility
        if vault.access_policies.is_empty() {
            issues.push("No access policies configured".to_string());
            accessible = false;
        }

        // Check soft delete configuration
        if vault.soft_delete_retention_in_days < 7 {
            issues.push("Soft delete retention period is less than 7 days".to_string());
        }

        // Additional health checks could be added here
        // For example: checking connectivity to the vault endpoint

        Ok(VaultHealthStatus {
            vault_name: vault_name.to_string(),
            exists: true,
            accessible,
            has_secrets: None, // Would need secret operations to check this
            issues,
        })
    }

    /// Display detailed vault information
    fn display_vault_details(&self, vault: &VaultProperties) -> Result<()> {
        self.display_utils.print_header(&format!("Vault: {}", vault.name))?;

        let vault_uri = vault.get_vault_uri();
        let retention_days = format!("{} days", vault.soft_delete_retention_in_days);
        
        let details = vec![
            ("Resource ID", vault.id.as_str()),
            ("Location", vault.location.as_str()),
            ("Resource Group", vault.resource_group.as_str()),
            ("Subscription", vault.subscription_id.as_str()),
            ("Vault URI", vault_uri.as_str()),
            ("SKU", vault.sku.as_str()),
            ("Soft Delete Retention", retention_days.as_str()),
            ("Purge Protection", if vault.purge_protection { "Enabled" } else { "Disabled" }),
            ("Deployment Access", if vault.enabled_for_deployment { "Enabled" } else { "Disabled" }),
            ("Disk Encryption Access", if vault.enabled_for_disk_encryption { "Enabled" } else { "Disabled" }),
            ("Template Access", if vault.enabled_for_template_deployment { "Enabled" } else { "Disabled" }),
        ];

        let formatted_details = self.display_utils.format_key_value_pairs(&details);
        println!("{}", formatted_details);

        if !vault.access_policies.is_empty() {
            self.display_utils.print_separator()?;
            self.display_utils.print_header("Access Policies")?;
            
            let formatter = TableFormatter::new(OutputFormat::Table, self.no_color);
            let table_output = formatter.format_table(&vault.access_policies)?;
            println!("{}", table_output);
        }

        if !vault.tags.is_empty() {
            self.display_utils.print_separator()?;
            self.display_utils.print_header("Tags")?;
            
            let tag_pairs: Vec<(&str, &str)> = vault.tags.iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let formatted_tags = self.display_utils.format_key_value_pairs(&tag_pairs);
            println!("{}", formatted_tags);
        }

        Ok(())
    }

    /// Export vault configuration to JSON
    pub async fn export_vault_config(&self, vault_name: &str, resource_group: &str) -> Result<String> {
        let vault = self.vault_ops.get_vault(vault_name, resource_group).await?;
        
        let config = serde_json::to_string_pretty(&vault)
            .map_err(|e| crosstacheError::serialization(format!("Failed to serialize vault config: {}", e)))?;

        Ok(config)
    }

    /// Validate vault name according to Azure requirements
    pub fn validate_vault_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(crosstacheError::invalid_argument("Vault name cannot be empty"));
        }

        if name.len() < 3 || name.len() > 24 {
            return Err(crosstacheError::invalid_argument(
                "Vault name must be between 3 and 24 characters"
            ));
        }

        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(crosstacheError::invalid_argument(
                "Vault name can only contain alphanumeric characters and hyphens"
            ));
        }

        if name.starts_with('-') || name.ends_with('-') {
            return Err(crosstacheError::invalid_argument(
                "Vault name cannot start or end with a hyphen"
            ));
        }

        if name.contains("--") {
            return Err(crosstacheError::invalid_argument(
                "Vault name cannot contain consecutive hyphens"
            ));
        }

        Ok(())
    }
}

/// Vault health status information
#[derive(Debug, Clone)]
pub struct VaultHealthStatus {
    pub vault_name: String,
    pub exists: bool,
    pub accessible: bool,
    pub has_secrets: Option<bool>,
    pub issues: Vec<String>,
}

impl VaultHealthStatus {
    /// Check if vault is healthy
    pub fn is_healthy(&self) -> bool {
        self.exists && self.accessible && self.issues.is_empty()
    }

    /// Get health status summary
    pub fn get_status_summary(&self) -> String {
        if self.is_healthy() {
            "Healthy".to_string()
        } else if !self.exists {
            "Not Found".to_string()
        } else if !self.accessible {
            "Inaccessible".to_string()
        } else {
            format!("Issues: {}", self.issues.len())
        }
    }
}

/// Vault management operations builder
pub struct VaultManagerBuilder {
    auth_provider: Option<Arc<dyn AzureAuthProvider>>,
    subscription_id: Option<String>,
    no_color: bool,
}

impl VaultManagerBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            auth_provider: None,
            subscription_id: None,
            no_color: false,
        }
    }

    /// Set the authentication provider
    pub fn with_auth_provider(mut self, auth_provider: Arc<dyn AzureAuthProvider>) -> Self {
        self.auth_provider = Some(auth_provider);
        self
    }

    /// Set the subscription ID
    pub fn with_subscription_id(mut self, subscription_id: String) -> Self {
        self.subscription_id = Some(subscription_id);
        self
    }

    /// Disable colored output
    pub fn with_no_color(mut self, no_color: bool) -> Self {
        self.no_color = no_color;
        self
    }

    /// Build the vault manager
    pub fn build(self) -> Result<VaultManager> {
        let auth_provider = self.auth_provider
            .ok_or_else(|| crosstacheError::config("Authentication provider is required"))?;
        let subscription_id = self.subscription_id
            .ok_or_else(|| crosstacheError::config("Subscription ID is required"))?;

        Ok(VaultManager::new(auth_provider, subscription_id, self.no_color))
    }
}

impl Default for VaultManagerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_name_validation() {
        // Valid names
        assert!(VaultManager::validate_vault_name("valid-vault-123").is_ok());
        assert!(VaultManager::validate_vault_name("test123").is_ok());
        assert!(VaultManager::validate_vault_name("a-b-c").is_ok());

        // Invalid names
        assert!(VaultManager::validate_vault_name("").is_err());
        assert!(VaultManager::validate_vault_name("ab").is_err()); // too short
        assert!(VaultManager::validate_vault_name(&"a".repeat(25)).is_err()); // too long
        assert!(VaultManager::validate_vault_name("-invalid").is_err()); // starts with hyphen
        assert!(VaultManager::validate_vault_name("invalid-").is_err()); // ends with hyphen
        assert!(VaultManager::validate_vault_name("invalid--name").is_err()); // consecutive hyphens
        assert!(VaultManager::validate_vault_name("invalid_name").is_err()); // underscore
        assert!(VaultManager::validate_vault_name("invalid.name").is_err()); // dot
    }

    #[test]
    fn test_vault_health_status() {
        let healthy_status = VaultHealthStatus {
            vault_name: "test-vault".to_string(),
            exists: true,
            accessible: true,
            has_secrets: Some(true),
            issues: vec![],
        };
        assert!(healthy_status.is_healthy());
        assert_eq!(healthy_status.get_status_summary(), "Healthy");

        let unhealthy_status = VaultHealthStatus {
            vault_name: "test-vault".to_string(),
            exists: true,
            accessible: false,
            has_secrets: None,
            issues: vec!["No access policies".to_string()],
        };
        assert!(!unhealthy_status.is_healthy());
        assert_eq!(unhealthy_status.get_status_summary(), "Inaccessible");
    }
}