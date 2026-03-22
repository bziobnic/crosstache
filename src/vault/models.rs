//! Vault data models and types
//!
//! This module defines the data structures used for vault management
//! including vault properties, access policies, and role definitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;
/// Display function for Option<String> in tables
fn display_option(opt: &Option<String>) -> String {
    match opt {
        Some(value) => value.clone(),
        None => "-".to_string(),
    }
}

/// Azure Key Vault properties and metadata
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct VaultProperties {
    #[tabled(rename = "ID")]
    pub id: String,
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Location")]
    pub location: String,
    #[tabled(rename = "Resource Group")]
    pub resource_group: String,
    #[tabled(rename = "Subscription")]
    pub subscription_id: String,
    #[tabled(skip)]
    pub tenant_id: String,
    #[tabled(rename = "URI")]
    pub uri: String,
    #[tabled(skip)]
    pub enabled_for_deployment: bool,
    #[tabled(skip)]
    pub enabled_for_disk_encryption: bool,
    #[tabled(skip)]
    pub enabled_for_template_deployment: bool,
    #[tabled(skip)]
    pub soft_delete_retention_in_days: i32,
    #[tabled(skip)]
    pub purge_protection: bool,
    #[tabled(rename = "SKU")]
    pub sku: String,
    #[tabled(skip)]
    pub access_policies: Vec<AccessPolicy>,
    #[tabled(rename = "Created")]
    pub created_at: DateTime<Utc>,
    #[tabled(skip)]
    pub tags: HashMap<String, String>,
    /// Whether the vault uses RBAC authorization (true) or access policies (false/None)
    #[tabled(skip)]
    #[serde(default)]
    pub enable_rbac_authorization: Option<bool>,
}

/// Access policy for a Key Vault
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct AccessPolicy {
    #[tabled(skip)]
    pub tenant_id: String,
    #[tabled(rename = "Object ID")]
    pub object_id: String,
    #[tabled(rename = "Application ID", display_with = "display_option")]
    pub application_id: Option<String>,
    #[tabled(skip)]
    pub permissions: AccessPolicyPermissions,
    #[tabled(rename = "User Email", display_with = "display_option")]
    pub user_email: Option<String>,
}

/// Permissions within an access policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessPolicyPermissions {
    pub keys: Vec<String>,
    pub secrets: Vec<String>,
    pub certificates: Vec<String>,
    pub storage: Vec<String>,
}

/// Access level for simplified role management
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AccessLevel {
    Reader,
    Contributor,
    Admin,
}

impl AccessLevel {
    /// Convert access level to secret permissions
    pub fn to_secret_permissions(&self) -> Vec<String> {
        match self {
            AccessLevel::Reader => vec!["get".to_string(), "list".to_string()],
            AccessLevel::Contributor => vec![
                "get".to_string(),
                "list".to_string(),
                "set".to_string(),
                "delete".to_string(),
                "recover".to_string(),
                "backup".to_string(),
                "restore".to_string(),
            ],
            AccessLevel::Admin => vec![
                "get".to_string(),
                "list".to_string(),
                "set".to_string(),
                "delete".to_string(),
                "recover".to_string(),
                "backup".to_string(),
                "restore".to_string(),
                "purge".to_string(),
            ],
        }
    }

    /// Convert access level to key permissions
    pub fn to_key_permissions(&self) -> Vec<String> {
        match self {
            AccessLevel::Reader => vec!["get".to_string(), "list".to_string()],
            AccessLevel::Contributor => vec![
                "get".to_string(),
                "list".to_string(),
                "create".to_string(),
                "delete".to_string(),
                "recover".to_string(),
                "backup".to_string(),
                "restore".to_string(),
                "decrypt".to_string(),
                "encrypt".to_string(),
                "sign".to_string(),
                "verify".to_string(),
                "wrapKey".to_string(),
                "unwrapKey".to_string(),
            ],
            AccessLevel::Admin => vec![
                "get".to_string(),
                "list".to_string(),
                "create".to_string(),
                "delete".to_string(),
                "recover".to_string(),
                "backup".to_string(),
                "restore".to_string(),
                "decrypt".to_string(),
                "encrypt".to_string(),
                "sign".to_string(),
                "verify".to_string(),
                "wrapKey".to_string(),
                "unwrapKey".to_string(),
                "purge".to_string(),
                "import".to_string(),
                "update".to_string(),
            ],
        }
    }
}

/// Role assignment for Azure RBAC
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct VaultRole {
    #[tabled(skip)]
    pub assignment_id: String,
    #[tabled(skip)]
    pub role_id: String,
    #[tabled(rename = "Role")]
    pub role_name: String,
    #[tabled(skip)]
    pub role_description: String,
    #[tabled(skip)]
    pub principal_id: String,
    #[tabled(rename = "Name")]
    pub principal_name: String,
    #[tabled(rename = "Email")]
    pub email: String,
    #[tabled(skip)]
    pub principal_type: String,
    #[tabled(skip)]
    pub scope: String,
    #[tabled(skip)]
    pub created_on: DateTime<Utc>,
    #[tabled(skip)]
    pub updated_on: DateTime<Utc>,
}

/// Vault creation parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultCreateRequest {
    pub name: String,
    pub location: String,
    pub resource_group: String,
    pub subscription_id: String,
    pub sku: Option<String>,
    pub enabled_for_deployment: Option<bool>,
    pub enabled_for_disk_encryption: Option<bool>,
    pub enabled_for_template_deployment: Option<bool>,
    pub soft_delete_retention_in_days: Option<i32>,
    pub purge_protection: Option<bool>,
    pub tags: Option<HashMap<String, String>>,
    pub access_policies: Option<Vec<AccessPolicy>>,
}

impl Default for VaultCreateRequest {
    fn default() -> Self {
        Self {
            name: String::new(),
            location: "eastus".to_string(),
            resource_group: String::new(),
            subscription_id: String::new(),
            sku: Some("standard".to_string()),
            enabled_for_deployment: Some(false),
            enabled_for_disk_encryption: Some(false),
            enabled_for_template_deployment: Some(false),
            soft_delete_retention_in_days: Some(90),
            purge_protection: Some(false),
            tags: Some(HashMap::new()),
            access_policies: Some(Vec::new()),
        }
    }
}

/// Vault update parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultUpdateRequest {
    pub enabled_for_deployment: Option<bool>,
    pub enabled_for_disk_encryption: Option<bool>,
    pub enabled_for_template_deployment: Option<bool>,
    pub soft_delete_retention_in_days: Option<i32>,
    pub purge_protection: Option<bool>,
    pub tags: Option<HashMap<String, String>>,
    pub access_policies: Option<Vec<AccessPolicy>>,
}

/// Role definition for Azure RBAC
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct RoleDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub role_type: String,
    pub permissions: Vec<RolePermission>,
}

/// Role permission definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct RolePermission {
    pub actions: Vec<String>,
    pub not_actions: Vec<String>,
    pub data_actions: Vec<String>,
    pub not_data_actions: Vec<String>,
}

/// Role assignment request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct RoleAssignmentRequest {
    pub role_definition_id: String,
    pub principal_id: String,
    pub scope: String,
}

/// Vault summary for list operations
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct VaultSummary {
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Location")]
    pub location: String,
    #[tabled(rename = "Resource Group")]
    pub resource_group: String,
    #[tabled(rename = "Status")]
    pub status: String,
    #[tabled(rename = "Created")]
    pub created_at: String,
}

impl VaultProperties {
    /// Convert to vault summary
    pub fn to_summary(&self) -> VaultSummary {
        VaultSummary {
            name: self.name.clone(),
            location: self.location.clone(),
            resource_group: self.resource_group.clone(),
            status: "Active".to_string(),
            created_at: self.created_at.format("%Y-%m-%d %H:%M").to_string(),
        }
    }

    /// Get vault URI
    pub fn get_vault_uri(&self) -> String {
        if self.uri.is_empty() {
            format!("https://{}.vault.azure.net/", self.name)
        } else {
            self.uri.clone()
        }
    }

    /// Check if vault has purge protection enabled
    pub fn has_purge_protection(&self) -> bool {
        self.purge_protection
    }

    /// Get soft delete retention period
    pub fn get_retention_days(&self) -> i32 {
        self.soft_delete_retention_in_days
    }
}

impl AccessPolicy {
    /// Create a new access policy
    pub fn new(
        tenant_id: String,
        object_id: String,
        access_level: AccessLevel,
        application_id: Option<String>,
        user_email: Option<String>,
    ) -> Self {
        Self {
            tenant_id,
            object_id,
            application_id,
            permissions: AccessPolicyPermissions {
                keys: access_level.to_key_permissions(),
                secrets: access_level.to_secret_permissions(),
                certificates: Vec::new(),
                storage: Vec::new(),
            },
            user_email,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vault_properties(name: &str, uri: &str, purge: bool, retention: i32) -> VaultProperties {
        VaultProperties {
            id: format!("/subscriptions/sub/resourceGroups/rg/providers/Microsoft.KeyVault/vaults/{name}"),
            name: name.to_string(),
            location: "eastus".to_string(),
            resource_group: "rg".to_string(),
            subscription_id: "sub-id".to_string(),
            tenant_id: "tenant-id".to_string(),
            uri: uri.to_string(),
            enabled_for_deployment: false,
            enabled_for_disk_encryption: false,
            enabled_for_template_deployment: false,
            soft_delete_retention_in_days: retention,
            purge_protection: purge,
            sku: "standard".to_string(),
            access_policies: Vec::new(),
            created_at: chrono::Utc::now(),
            tags: HashMap::new(),
            enable_rbac_authorization: None,
        }
    }

    // --- AccessLevel::to_secret_permissions ---

    #[test]
    fn test_reader_secret_permissions() {
        let perms = AccessLevel::Reader.to_secret_permissions();
        assert_eq!(perms, vec!["get", "list"]);
    }

    #[test]
    fn test_contributor_secret_permissions() {
        let perms = AccessLevel::Contributor.to_secret_permissions();
        assert!(perms.contains(&"get".to_string()));
        assert!(perms.contains(&"set".to_string()));
        assert!(perms.contains(&"delete".to_string()));
        assert!(!perms.contains(&"purge".to_string()));
    }

    #[test]
    fn test_admin_secret_permissions_includes_purge() {
        let perms = AccessLevel::Admin.to_secret_permissions();
        assert!(perms.contains(&"purge".to_string()));
        assert!(perms.contains(&"get".to_string()));
        assert!(perms.contains(&"set".to_string()));
    }

    // --- AccessLevel::to_key_permissions ---

    #[test]
    fn test_reader_key_permissions() {
        let perms = AccessLevel::Reader.to_key_permissions();
        assert_eq!(perms, vec!["get", "list"]);
    }

    #[test]
    fn test_contributor_key_permissions() {
        let perms = AccessLevel::Contributor.to_key_permissions();
        assert!(perms.contains(&"create".to_string()));
        assert!(perms.contains(&"decrypt".to_string()));
        assert!(!perms.contains(&"purge".to_string()));
        assert!(!perms.contains(&"import".to_string()));
    }

    #[test]
    fn test_admin_key_permissions_includes_purge_and_import() {
        let perms = AccessLevel::Admin.to_key_permissions();
        assert!(perms.contains(&"purge".to_string()));
        assert!(perms.contains(&"import".to_string()));
        assert!(perms.contains(&"update".to_string()));
    }

    // --- VaultProperties methods ---

    #[test]
    fn test_get_vault_uri_returns_stored_uri() {
        let vp = make_vault_properties(
            "myvault",
            "https://myvault.vault.azure.net/",
            false,
            90,
        );
        assert_eq!(vp.get_vault_uri(), "https://myvault.vault.azure.net/");
    }

    #[test]
    fn test_get_vault_uri_constructs_when_empty() {
        let vp = make_vault_properties("myvault", "", false, 90);
        assert_eq!(vp.get_vault_uri(), "https://myvault.vault.azure.net/");
    }

    #[test]
    fn test_has_purge_protection_true() {
        let vp = make_vault_properties("myvault", "", true, 90);
        assert!(vp.has_purge_protection());
    }

    #[test]
    fn test_has_purge_protection_false() {
        let vp = make_vault_properties("myvault", "", false, 90);
        assert!(!vp.has_purge_protection());
    }

    #[test]
    fn test_get_retention_days() {
        let vp = make_vault_properties("myvault", "", false, 7);
        assert_eq!(vp.get_retention_days(), 7);
    }

    #[test]
    fn test_to_summary_active_status() {
        let vp = make_vault_properties(
            "myvault",
            "https://myvault.vault.azure.net/",
            false,
            90,
        );
        let summary = vp.to_summary();
        assert_eq!(summary.name, "myvault");
        assert_eq!(summary.status, "Active");
        assert_eq!(summary.location, "eastus");
        assert_eq!(summary.resource_group, "rg");
    }

    // --- VaultCreateRequest::default ---

    #[test]
    fn test_vault_create_request_default_location() {
        let req = VaultCreateRequest::default();
        assert_eq!(req.location, "eastus");
    }

    #[test]
    fn test_vault_create_request_default_sku() {
        let req = VaultCreateRequest::default();
        assert_eq!(req.sku, Some("standard".to_string()));
    }

    #[test]
    fn test_vault_create_request_default_retention() {
        let req = VaultCreateRequest::default();
        assert_eq!(req.soft_delete_retention_in_days, Some(90));
    }

    #[test]
    fn test_vault_create_request_default_no_purge_protection() {
        let req = VaultCreateRequest::default();
        assert_eq!(req.purge_protection, Some(false));
    }

    // --- AccessPolicy::new ---

    #[test]
    fn test_access_policy_new_reader_has_minimal_perms() {
        let policy = AccessPolicy::new(
            "tenant".to_string(),
            "oid".to_string(),
            AccessLevel::Reader,
            None,
            Some("user@example.com".to_string()),
        );
        assert_eq!(policy.object_id, "oid");
        assert_eq!(policy.tenant_id, "tenant");
        assert_eq!(policy.user_email, Some("user@example.com".to_string()));
        assert_eq!(policy.permissions.secrets, vec!["get", "list"]);
        assert!(policy.permissions.certificates.is_empty());
        assert!(policy.permissions.storage.is_empty());
    }

    #[test]
    fn test_access_policy_new_admin_has_purge() {
        let policy = AccessPolicy::new(
            "tenant".to_string(),
            "oid".to_string(),
            AccessLevel::Admin,
            None,
            None,
        );
        assert!(policy.permissions.secrets.contains(&"purge".to_string()));
        assert!(policy.permissions.keys.contains(&"purge".to_string()));
    }

    // --- AccessLevel PartialEq ---

    #[test]
    fn test_access_level_equality() {
        assert_eq!(AccessLevel::Reader, AccessLevel::Reader);
        assert_ne!(AccessLevel::Reader, AccessLevel::Admin);
        assert_ne!(AccessLevel::Contributor, AccessLevel::Admin);
    }
}
