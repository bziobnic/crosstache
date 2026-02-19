//! Vault management facade
//!
//! This module provides a high-level interface for vault operations,
//! combining vault operations with RBAC management and providing
//! a unified API for vault management tasks.

use std::sync::Arc;

use super::models::{AccessLevel, VaultCreateRequest, VaultProperties, VaultRole, VaultSummary};
use super::operations::{AzureVaultOperations, VaultOperations};
use crate::auth::provider::AzureAuthProvider;
use crate::error::Result;
use crate::utils::format::{DisplayUtils, OutputFormat, TableFormatter};

/// High-level vault manager
pub struct VaultManager {
    vault_ops: Arc<dyn VaultOperations>,
    display_utils: DisplayUtils,
    no_color: bool,
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(
        auth_provider: Arc<dyn AzureAuthProvider>,
        subscription_id: String,
        no_color: bool,
    ) -> Result<Self> {
        let vault_ops = Arc::new(AzureVaultOperations::new(auth_provider, subscription_id)?);
        let display_utils = DisplayUtils::new(no_color);

        Ok(Self {
            vault_ops,
            display_utils,
            no_color,
        })
    }

    /// Create a new vault with automatic access policy setup
    pub async fn create_vault_with_setup(
        &self,
        name: &str,
        location: &str,
        resource_group: &str,
        additional_options: Option<VaultCreateRequest>,
    ) -> Result<VaultProperties> {
        self.display_utils
            .print_info(&format!("Creating vault '{name}'..."))?;

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
            request.purge_protection = Some(true);
        }

        let vault = self.vault_ops.create_vault(&request).await?;

        self.display_utils.print_success(&format!(
            "Successfully created vault '{}' in {} ({})",
            vault.name, vault.location, vault.resource_group
        ))?;

        Ok(vault)
    }

    /// Get vault properties without displaying them
    pub async fn get_vault_properties(
        &self,
        vault_name: &str,
        resource_group: &str,
    ) -> Result<VaultProperties> {
        self.vault_ops.get_vault(vault_name, resource_group).await
    }

    /// Get vault information and display it
    pub async fn get_vault_info(
        &self,
        vault_name: &str,
        resource_group: &str,
    ) -> Result<VaultProperties> {
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
        let vaults = self
            .vault_ops
            .list_vaults(subscription_id, resource_group)
            .await?;

        if vaults.is_empty() {
            self.display_utils.print_info("No vaults found.")?;
            return Ok(vaults);
        }

        // Format and display results
        let formatter = TableFormatter::new(output_format, self.no_color);
        let table_output = formatter.format_table(&vaults)?;
        println!("{table_output}");

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
                "This will soft-delete vault '{vault_name}' in resource group '{resource_group}'"
            ))?;

            if vault.has_purge_protection() {
                self.display_utils.print_warning(
                    "This vault has purge protection enabled - it cannot be permanently deleted.",
                )?;
            } else {
                self.display_utils.print_warning(&format!(
                    "The vault will be recoverable for {} days after deletion.",
                    vault.get_retention_days()
                ))?;
            }
        }

        self.vault_ops
            .delete_vault(vault_name, resource_group)
            .await?;

        self.display_utils.print_success(&format!(
            "Successfully deleted vault '{vault_name}' (soft delete)"
        ))?;

        Ok(())
    }

    /// Restore a soft-deleted vault
    pub async fn restore_vault(&self, vault_name: &str, location: &str) -> Result<VaultProperties> {
        self.display_utils
            .print_info(&format!("Restoring soft-deleted vault '{vault_name}'..."))?;

        let vault = self.vault_ops.restore_vault(vault_name, location).await?;

        self.display_utils
            .print_success(&format!("Successfully restored vault '{vault_name}'"))?;

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
                "This will PERMANENTLY delete vault '{vault_name}' and all its contents!"
            ))?;
            self.display_utils
                .print_warning("This action cannot be undone.")?;
        }

        self.vault_ops.purge_vault(vault_name, location).await?;

        self.display_utils.print_success(&format!(
            "Successfully purged vault '{vault_name}' (permanent deletion)"
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
            "Granting {access_level_str} access to vault '{vault_name}' for user '{user_display}'..."
        ))?;

        self.vault_ops
            .grant_access(vault_name, resource_group, user_object_id, access_level)
            .await?;

        self.display_utils.print_success(&format!(
            "Successfully granted {access_level_str} access to vault '{vault_name}' for user '{user_display}'"
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
            "Revoking access to vault '{vault_name}' for user '{user_display}'..."
        ))?;

        self.vault_ops
            .revoke_access(vault_name, resource_group, user_object_id)
            .await?;

        self.display_utils.print_success(&format!(
            "Successfully revoked access to vault '{vault_name}' for user '{user_display}'"
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
        let roles = self
            .vault_ops
            .list_access(vault_name, resource_group)
            .await?;

        if roles.is_empty() {
            self.display_utils
                .print_info("No access policies found for this vault.")?;
            return Ok(roles);
        }

        self.display_utils
            .print_header(&format!("Access Policies for Vault '{vault_name}'"))?;

        // Format and display results
        let formatter = TableFormatter::new(output_format, self.no_color);
        let table_output = formatter.format_table(&roles)?;
        println!("{table_output}");

        Ok(roles)
    }

    /// Display detailed vault information
    fn display_vault_details(&self, vault: &VaultProperties) -> Result<()> {
        self.display_utils
            .print_header(&format!("Vault: {}", vault.name))?;

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
            (
                "Purge Protection",
                if vault.purge_protection {
                    "Enabled"
                } else {
                    "Disabled"
                },
            ),
            (
                "Deployment Access",
                if vault.enabled_for_deployment {
                    "Enabled"
                } else {
                    "Disabled"
                },
            ),
            (
                "Disk Encryption Access",
                if vault.enabled_for_disk_encryption {
                    "Enabled"
                } else {
                    "Disabled"
                },
            ),
            (
                "Template Access",
                if vault.enabled_for_template_deployment {
                    "Enabled"
                } else {
                    "Disabled"
                },
            ),
        ];

        let formatted_details = self.display_utils.format_key_value_pairs(&details);
        println!("{formatted_details}");

        if !vault.access_policies.is_empty() {
            self.display_utils.print_separator()?;
            self.display_utils.print_header("Access Policies")?;

            let formatter = TableFormatter::new(OutputFormat::Table, self.no_color);
            let table_output = formatter.format_table(&vault.access_policies)?;
            println!("{table_output}");
        }

        if !vault.tags.is_empty() {
            self.display_utils.print_separator()?;
            self.display_utils.print_header("Tags")?;

            let tag_pairs: Vec<(&str, &str)> = vault
                .tags
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let formatted_tags = self.display_utils.format_key_value_pairs(&tag_pairs);
            println!("{formatted_tags}");
        }

        Ok(())
    }
}
