//! Azure environment detection utilities
//!
//! This module provides utilities for detecting Azure CLI installation,
//! available subscriptions, and environment configuration.

use crate::error::{CrosstacheError, Result};
use serde_json::Value;
use std::process::Command;

/// Azure environment detection information
#[derive(Debug, Clone)]
pub struct AzureEnvironment {
    /// Whether Azure CLI is installed and available
    pub cli_available: bool,
    /// Whether user is logged into Azure CLI
    pub cli_logged_in: bool,
    /// Current Azure CLI version
    pub cli_version: Option<String>,
    /// Available subscriptions
    pub subscriptions: Vec<AzureSubscription>,
    /// Current default subscription
    pub current_subscription: Option<AzureSubscription>,
    /// Current tenant information
    pub tenant_info: Option<AzureTenant>,
}

/// Azure subscription information
#[derive(Debug, Clone)]
pub struct AzureSubscription {
    /// Subscription ID
    pub id: String,
    /// Subscription name
    pub name: String,
    /// Tenant ID
    pub tenant_id: String,
    /// Whether this is the current default subscription
    pub is_default: bool,
    /// Subscription state (e.g., "Enabled")
    #[allow(dead_code)]
    pub state: String,
}

/// Azure tenant information
#[derive(Debug, Clone)]
pub struct AzureTenant {
    /// Tenant ID
    pub id: String,
    /// Tenant domain name
    pub domain: Option<String>,
    /// Display name
    pub display_name: Option<String>,
}

/// Azure environment detector
pub struct AzureDetector;

impl AzureDetector {
    /// Detect the current Azure environment
    pub async fn detect_environment() -> Result<AzureEnvironment> {
        let cli_available = Self::check_cli_available();
        
        if !cli_available {
            return Ok(AzureEnvironment {
                cli_available: false,
                cli_logged_in: false,
                cli_version: None,
                subscriptions: Vec::new(),
                current_subscription: None,
                tenant_info: None,
            });
        }

        let cli_version = Self::get_cli_version();
        let cli_logged_in = Self::check_cli_logged_in();
        
        if !cli_logged_in {
            return Ok(AzureEnvironment {
                cli_available: true,
                cli_logged_in: false,
                cli_version,
                subscriptions: Vec::new(),
                current_subscription: None,
                tenant_info: None,
            });
        }

        let subscriptions = Self::get_subscriptions().await?;
        let current_subscription = subscriptions.iter().find(|s| s.is_default).cloned();
        let tenant_info = Self::get_tenant_info().await?;

        Ok(AzureEnvironment {
            cli_available: true,
            cli_logged_in: true,
            cli_version,
            subscriptions,
            current_subscription,
            tenant_info,
        })
    }

    /// Check if Azure CLI is available
    fn check_cli_available() -> bool {
        Command::new("az")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Get Azure CLI version
    fn get_cli_version() -> Option<String> {
        let output = Command::new("az")
            .arg("--version")
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let version_output = String::from_utf8_lossy(&output.stdout);
        // Parse the first line which typically contains "azure-cli 2.x.x"
        version_output
            .lines()
            .next()
            .and_then(|line| {
                line.split_whitespace()
                    .nth(1)
                    .map(|v| v.to_string())
            })
    }

    /// Check if user is logged into Azure CLI
    fn check_cli_logged_in() -> bool {
        Command::new("az")
            .args(["account", "show"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Get available Azure subscriptions
    async fn get_subscriptions() -> Result<Vec<AzureSubscription>> {
        let output = Command::new("az")
            .args(["account", "list", "--output", "json"])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(CrosstacheError::config(format!(
                "Azure CLI command failed: {error_msg}"
            )));
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let subscriptions_json: Value = serde_json::from_str(&output_str)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to parse Azure CLI output: {e}"
            )))?;

        let mut subscriptions = Vec::new();
        
        if let Some(subs_array) = subscriptions_json.as_array() {
            for sub_json in subs_array {
                if let Some(subscription) = Self::parse_subscription(sub_json) {
                    subscriptions.push(subscription);
                }
            }
        }

        Ok(subscriptions)
    }

    /// Parse a subscription from JSON
    fn parse_subscription(sub_json: &Value) -> Option<AzureSubscription> {
        let id = sub_json.get("id")?.as_str()?.to_string();
        let name = sub_json.get("name")?.as_str()?.to_string();
        let tenant_id = sub_json.get("tenantId")?.as_str()?.to_string();
        let is_default = sub_json.get("isDefault")?.as_bool().unwrap_or(false);
        let state = sub_json.get("state")?.as_str().unwrap_or("Unknown").to_string();

        Some(AzureSubscription {
            id,
            name,
            tenant_id,
            is_default,
            state,
        })
    }

    /// Get current tenant information
    async fn get_tenant_info() -> Result<Option<AzureTenant>> {
        let output = Command::new("az")
            .args(["account", "show", "--output", "json"])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            return Ok(None);
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let account_json: Value = serde_json::from_str(&output_str)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to parse Azure CLI output: {e}"
            )))?;

        let tenant_id = account_json
            .get("tenantId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if let Some(id) = tenant_id {
            // Try to get additional tenant info
            let tenant_details = Self::get_tenant_details(&id).await;
            
            Ok(Some(AzureTenant {
                id,
                domain: tenant_details.as_ref().and_then(|t| t.domain.clone()),
                display_name: tenant_details.and_then(|t| t.display_name),
            }))
        } else {
            Ok(None)
        }
    }

    /// Get detailed tenant information
    async fn get_tenant_details(tenant_id: &str) -> Option<AzureTenant> {
        let output = Command::new("az")
            .args(["rest", "--method", "GET", "--url", 
                   &format!("https://graph.microsoft.com/v1.0/organization?$filter=id eq '{tenant_id}'")])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let org_json: Value = serde_json::from_str(&output_str).ok()?;

        let value_array = org_json.get("value")?.as_array()?;
        let org = value_array.first()?;

        let domain = org
            .get("verifiedDomains")?
            .as_array()?
            .iter()
            .find(|domain| {
                domain.get("isDefault")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            })
            .and_then(|domain| domain.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let display_name = org
            .get("displayName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Some(AzureTenant {
            id: tenant_id.to_string(),
            domain,
            display_name,
        })
    }

    /// Get available resource groups for a subscription
    pub async fn get_resource_groups(subscription_id: &str) -> Result<Vec<String>> {
        let output = Command::new("az")
            .args([
                "group", "list",
                "--subscription", subscription_id,
                "--query", "[].name",
                "--output", "json"
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(CrosstacheError::config(format!(
                "Failed to list resource groups: {error_msg}"
            )));
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let groups_json: Value = serde_json::from_str(&output_str)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to parse resource groups: {e}"
            )))?;

        let mut resource_groups = Vec::new();
        if let Some(groups_array) = groups_json.as_array() {
            for group in groups_array {
                if let Some(name) = group.as_str() {
                    resource_groups.push(name.to_string());
                }
            }
        }

        resource_groups.sort();
        Ok(resource_groups)
    }

    /// Get available locations for a subscription
    pub async fn get_locations(subscription_id: &str) -> Result<Vec<String>> {
        let output = Command::new("az")
            .args([
                "account", "list-locations",
                "--subscription", subscription_id,
                "--query", "[].name",
                "--output", "json"
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(CrosstacheError::config(format!(
                "Failed to list locations: {error_msg}"
            )));
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let locations_json: Value = serde_json::from_str(&output_str)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to parse locations: {e}"
            )))?;

        let mut locations = Vec::new();
        if let Some(locations_array) = locations_json.as_array() {
            for location in locations_array {
                if let Some(name) = location.as_str() {
                    locations.push(name.to_string());
                }
            }
        }

        locations.sort();
        Ok(locations)
    }

    /// Check if a resource group exists
    pub async fn resource_group_exists(subscription_id: &str, resource_group: &str) -> Result<bool> {
        let output = Command::new("az")
            .args([
                "group", "exists",
                "--subscription", subscription_id,
                "--name", resource_group
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            return Ok(false);
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let trimmed = output_str.trim();
        Ok(trimmed == "true")
    }

    /// Create a resource group
    pub async fn create_resource_group(
        subscription_id: &str, 
        resource_group: &str, 
        location: &str
    ) -> Result<()> {
        let output = Command::new("az")
            .args([
                "group", "create",
                "--subscription", subscription_id,
                "--name", resource_group,
                "--location", location
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(CrosstacheError::config(format!(
                "Failed to create resource group: {error_msg}"
            )));
        }

        Ok(())
    }

    /// Get storage accounts in a resource group
    pub async fn get_storage_accounts(subscription_id: &str, resource_group: &str) -> Result<Vec<String>> {
        let output = Command::new("az")
            .args([
                "storage", "account", "list",
                "--subscription", subscription_id,
                "--resource-group", resource_group,
                "--query", "[].name",
                "--output", "json"
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            let error_msg = String::from_utf8_lossy(&output.stderr);
            return Err(CrosstacheError::config(format!(
                "Failed to list storage accounts: {error_msg}"
            )));
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let accounts_json: Value = serde_json::from_str(&output_str)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to parse storage accounts: {e}"
            )))?;

        let mut storage_accounts = Vec::new();
        if let Some(accounts_array) = accounts_json.as_array() {
            for account in accounts_array {
                if let Some(name) = account.as_str() {
                    storage_accounts.push(name.to_string());
                }
            }
        }

        storage_accounts.sort();
        Ok(storage_accounts)
    }

    /// Check if a storage account exists
    #[allow(dead_code)]
    pub async fn storage_account_exists(subscription_id: &str, storage_account: &str) -> Result<bool> {
        let output = Command::new("az")
            .args([
                "storage", "account", "show",
                "--subscription", subscription_id,
                "--name", storage_account,
                "--query", "name",
                "--output", "json"
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        Ok(output.status.success())
    }

    /// Check if a container exists in a storage account
    pub async fn container_exists(subscription_id: &str, storage_account: &str, container_name: &str) -> Result<bool> {
        let output = Command::new("az")
            .args([
                "storage", "container", "exists",
                "--subscription", subscription_id,
                "--account-name", storage_account,
                "--name", container_name,
                "--output", "json"
            ])
            .output()
            .map_err(|e| CrosstacheError::config(format!("Failed to execute Azure CLI: {e}")))?;

        if !output.status.success() {
            return Ok(false);
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let exists_json: Value = serde_json::from_str(&output_str)
            .map_err(|e| CrosstacheError::serialization(format!(
                "Failed to parse container existence check: {e}"
            )))?;

        Ok(exists_json.get("exists")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }
}

impl AzureEnvironment {
    /// Check if the environment is ready for use
    pub fn is_ready(&self) -> bool {
        self.cli_available && self.cli_logged_in && !self.subscriptions.is_empty()
    }

    /// Get setup status message
    pub fn get_status_message(&self) -> String {
        if !self.cli_available {
            "Azure CLI is not installed or not available in PATH".to_string()
        } else if !self.cli_logged_in {
            "Azure CLI is available but you are not logged in".to_string()
        } else if self.subscriptions.is_empty() {
            "Azure CLI is available but no subscriptions found".to_string()
        } else {
            format!("Azure CLI ready with {} subscription(s)", self.subscriptions.len())
        }
    }

    /// Get setup instructions based on current state
    pub fn get_setup_instructions(&self) -> Vec<String> {
        let mut instructions = Vec::new();

        if !self.cli_available {
            instructions.push("Install Azure CLI: https://docs.microsoft.com/en-us/cli/azure/install-azure-cli".to_string());
        } else if !self.cli_logged_in {
            instructions.push("Log in to Azure: az login".to_string());
        } else if self.subscriptions.is_empty() {
            instructions.push("Ensure you have access to at least one Azure subscription".to_string());
        }

        instructions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_azure_detection() {
        // This test will only pass if Azure CLI is available and configured
        let env = AzureDetector::detect_environment().await.unwrap();
        
        // At minimum, we should be able to detect CLI availability
        assert!(env.cli_available == AzureDetector::check_cli_available());
    }

    #[test]
    fn test_environment_status() {
        let env = AzureEnvironment {
            cli_available: false,
            cli_logged_in: false,
            cli_version: None,
            subscriptions: Vec::new(),
            current_subscription: None,
            tenant_info: None,
        };

        assert!(!env.is_ready());
        assert!(env.get_status_message().contains("not installed"));
        assert!(!env.get_setup_instructions().is_empty());
    }
}