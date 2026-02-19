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

/// Display function for Option<u32> in tables
fn display_option_u32(opt: &Option<u32>) -> String {
    match opt {
        Some(value) => value.to_string(),
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
    #[tabled(rename = "Assignment ID")]
    pub assignment_id: String,
    #[tabled(skip)]
    pub role_id: String,
    #[tabled(rename = "Role")]
    pub role_name: String,
    #[tabled(skip)]
    pub role_description: String,
    #[tabled(skip)]
    pub principal_id: String,
    #[tabled(rename = "Principal")]
    pub principal_name: String,
    #[tabled(rename = "Type")]
    pub principal_type: String,
    #[tabled(skip)]
    pub scope: String,
    #[tabled(rename = "Created")]
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

/// Vault status enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(dead_code)]
pub enum VaultStatus {
    Active,
    SoftDeleted,
    PendingDeletion,
    Creating,
    Updating,
    Unknown,
}

impl std::fmt::Display for VaultStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultStatus::Active => write!(f, "Active"),
            VaultStatus::SoftDeleted => write!(f, "Soft Deleted"),
            VaultStatus::PendingDeletion => write!(f, "Pending Deletion"),
            VaultStatus::Creating => write!(f, "Creating"),
            VaultStatus::Updating => write!(f, "Updating"),
            VaultStatus::Unknown => write!(f, "Unknown"),
        }
    }
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
    #[tabled(rename = "Secrets", display_with = "display_option_u32")]
    pub secret_count: Option<u32>,
    #[tabled(rename = "Created")]
    pub created_at: String,
}

impl VaultProperties {
    /// Convert to vault summary
    pub fn to_summary(&self, secret_count: Option<u32>) -> VaultSummary {
        VaultSummary {
            name: self.name.clone(),
            location: self.location.clone(),
            resource_group: self.resource_group.clone(),
            status: "Active".to_string(),
            secret_count,
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
