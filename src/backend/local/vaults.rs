//! Local vault backend — directory-based vault management.
//!
//! Each vault is a directory under `<store>/vaults/<name>/` containing a
//! `.vault.json` metadata file and a `secrets/` subdirectory.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::error::BackendError;
use crate::backend::local::paths;
use crate::backend::vault::VaultBackend;
use crate::vault::models::{VaultCreateRequest, VaultProperties, VaultSummary};

// ---------------------------------------------------------------------------
// On-disk vault metadata
// ---------------------------------------------------------------------------

/// On-disk vault metadata (`.vault.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMeta {
    pub name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// LocalVaultBackend
// ---------------------------------------------------------------------------

/// File-backed vault operations.
pub struct LocalVaultBackend {
    store_path: PathBuf,
}

impl LocalVaultBackend {
    pub fn new(store_path: PathBuf) -> Self {
        Self { store_path }
    }

    fn vaults_dir(&self) -> PathBuf {
        paths::vaults_dir(&self.store_path)
    }

    fn vault_dir(&self, name: &str) -> Result<PathBuf, BackendError> {
        paths::vault_dir(&self.store_path, name)
    }

    fn vault_json_path(&self, name: &str) -> Result<PathBuf, BackendError> {
        Ok(self.vault_dir(name)?.join(".vault.json"))
    }

    fn read_vault_meta(&self, name: &str) -> Result<VaultMeta, BackendError> {
        let path = self.vault_json_path(name)?;
        if !path.exists() {
            return Err(BackendError::VaultNotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }
        let data = fs::read_to_string(&path)
            .map_err(|e| BackendError::Internal(format!("read vault meta: {e}")))?;
        serde_json::from_str(&data)
            .map_err(|e| BackendError::Internal(format!("parse vault meta: {e}")))
    }

    fn vault_meta_to_properties(&self, meta: &VaultMeta) -> Result<VaultProperties, BackendError> {
        Ok(VaultProperties {
            id: format!("local:{}", meta.name),
            name: meta.name.clone(),
            location: "local".to_string(),
            resource_group: String::new(),
            subscription_id: String::new(),
            tenant_id: String::new(),
            uri: format!("file://{}", self.vault_dir(&meta.name)?.display()),
            enabled_for_deployment: false,
            enabled_for_disk_encryption: false,
            enabled_for_template_deployment: false,
            soft_delete_retention_in_days: 0,
            purge_protection: false,
            sku: "local".to_string(),
            access_policies: Vec::new(),
            created_at: meta.created_at,
            tags: meta.tags.clone(),
            enable_rbac_authorization: Some(false),
        })
    }
}

#[async_trait]
impl VaultBackend for LocalVaultBackend {
    async fn create_vault(
        &self,
        request: VaultCreateRequest,
    ) -> Result<VaultProperties, BackendError> {
        let name = &request.name;
        if name.is_empty() {
            return Err(BackendError::Internal("vault name cannot be empty".into()));
        }

        let vault_dir = self.vault_dir(name)?;
        if vault_dir.join(".vault.json").exists() {
            return Err(BackendError::Conflict(format!(
                "vault '{name}' already exists"
            )));
        }

        // Create directory structure
        fs::create_dir_all(vault_dir.join("secrets"))
            .map_err(|e| BackendError::Internal(format!("create vault directory: {e}")))?;

        let meta = VaultMeta {
            name: name.clone(),
            created_at: Utc::now(),
            tags: request.tags.unwrap_or_default(),
        };

        let json = serde_json::to_string_pretty(&meta)
            .map_err(|e| BackendError::Internal(format!("serialize vault meta: {e}")))?;
        fs::write(vault_dir.join(".vault.json"), json)
            .map_err(|e| BackendError::Internal(format!("write vault meta: {e}")))?;

        self.vault_meta_to_properties(&meta)
    }

    async fn get_vault(&self, name: &str) -> Result<VaultProperties, BackendError> {
        let meta = self.read_vault_meta(name)?;
        self.vault_meta_to_properties(&meta)
    }

    async fn list_vaults(&self) -> Result<Vec<VaultSummary>, BackendError> {
        let vaults_dir = self.vaults_dir();
        if !vaults_dir.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let entries = fs::read_dir(&vaults_dir)
            .map_err(|e| BackendError::Internal(format!("read vaults dir: {e}")))?;

        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let vault_json = self.vault_json_path(&name)?;
            if !vault_json.exists() {
                continue;
            }

            match self.read_vault_meta(&name) {
                Ok(meta) => {
                    results.push(VaultSummary {
                        name: meta.name.clone(),
                        location: "local".to_string(),
                        resource_group: String::new(),
                        status: "Active".to_string(),
                        created_at: meta.created_at.format("%Y-%m-%d %H:%M").to_string(),
                    });
                }
                Err(_) => continue,
            }
        }

        results.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(results)
    }

    async fn delete_vault(&self, name: &str) -> Result<(), BackendError> {
        let vault_dir = self.vault_dir(name)?;
        let vault_json = self.vault_json_path(name)?;

        if !vault_json.exists() {
            return Err(BackendError::VaultNotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        // Check if vault has secrets
        let secrets_dir = vault_dir.join("secrets");
        if secrets_dir.exists() {
            let has_secrets = fs::read_dir(&secrets_dir)
                .map_err(|e| BackendError::Internal(format!("read secrets dir: {e}")))?
                .flatten()
                .any(|e| {
                    let fname = e.file_name().to_string_lossy().to_string();
                    fname.ends_with(".meta.json")
                });

            if has_secrets {
                return Err(BackendError::Conflict(format!(
                    "vault '{name}' still contains secrets — delete them first or use --force"
                )));
            }
        }

        fs::remove_dir_all(&vault_dir)
            .map_err(|e| BackendError::Internal(format!("remove vault dir: {e}")))?;

        Ok(())
    }
}

// _vault_dir_path removed; use self.vault_dir() method instead.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_vault_backend() -> (LocalVaultBackend, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let backend = LocalVaultBackend::new(store);
        (backend, tmp)
    }

    fn create_request(name: &str) -> VaultCreateRequest {
        VaultCreateRequest {
            name: name.to_string(),
            location: "local".to_string(),
            resource_group: String::new(),
            subscription_id: String::new(),
            sku: None,
            enabled_for_deployment: None,
            enabled_for_disk_encryption: None,
            enabled_for_template_deployment: None,
            soft_delete_retention_in_days: None,
            purge_protection: None,
            tags: Some(HashMap::from([("env".into(), "test".into())])),
            access_policies: None,
        }
    }

    #[tokio::test]
    async fn create_and_get_vault() {
        let (backend, _tmp) = test_vault_backend();

        let props = backend
            .create_vault(create_request("myvault"))
            .await
            .unwrap();
        assert_eq!(props.name, "myvault");
        assert_eq!(props.location, "local");
        assert_eq!(props.sku, "local");
        assert!(props.tags.contains_key("env"));

        let got = backend.get_vault("myvault").await.unwrap();
        assert_eq!(got.name, "myvault");
    }

    #[tokio::test]
    async fn rejects_traversal_vault_names() {
        let (backend, tmp) = test_vault_backend();
        let outside = tmp.path().join("outside");

        let result = backend.create_vault(create_request("../../outside")).await;
        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert!(!outside.exists());

        let result = backend.delete_vault("../../outside").await;
        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
    }

    #[tokio::test]
    async fn rejects_separator_vault_names() {
        let (backend, _tmp) = test_vault_backend();

        for name in ["nested/vault", "nested\\vault", "/absolute"] {
            let result = backend.create_vault(create_request(name)).await;
            assert!(
                matches!(result, Err(BackendError::InvalidArgument(_))),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn create_duplicate_vault_fails() {
        let (backend, _tmp) = test_vault_backend();

        backend.create_vault(create_request("dup")).await.unwrap();
        let result = backend.create_vault(create_request("dup")).await;
        assert!(matches!(result, Err(BackendError::Conflict(_))));
    }

    #[tokio::test]
    async fn list_vaults() {
        let (backend, _tmp) = test_vault_backend();

        backend.create_vault(create_request("alpha")).await.unwrap();
        backend.create_vault(create_request("beta")).await.unwrap();

        let vaults = backend.list_vaults().await.unwrap();
        assert_eq!(vaults.len(), 2);
        assert_eq!(vaults[0].name, "alpha");
        assert_eq!(vaults[1].name, "beta");
    }

    #[tokio::test]
    async fn delete_empty_vault() {
        let (backend, _tmp) = test_vault_backend();

        backend
            .create_vault(create_request("to-del"))
            .await
            .unwrap();
        backend.delete_vault("to-del").await.unwrap();

        let result = backend.get_vault("to-del").await;
        assert!(matches!(result, Err(BackendError::VaultNotFound { .. })));
    }

    #[tokio::test]
    async fn delete_nonexistent_vault() {
        let (backend, _tmp) = test_vault_backend();

        let result = backend.delete_vault("nope").await;
        assert!(matches!(result, Err(BackendError::VaultNotFound { .. })));
    }

    #[tokio::test]
    async fn get_nonexistent_vault() {
        let (backend, _tmp) = test_vault_backend();

        let result = backend.get_vault("nope").await;
        assert!(matches!(result, Err(BackendError::VaultNotFound { .. })));
    }
}
