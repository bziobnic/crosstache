//! Local age-encrypted file backend.
//!
//! This module implements [`Backend`](super::Backend) for a purely local,
//! file-based secret store. Secrets are encrypted with [age](https://age-encryption.org/)
//! x25519 keys and stored alongside plaintext metadata in a directory tree.
//!
//! ## Storage layout
//!
//! ```text
//! <store_path>/
//! ├── vaults/
//! │   ├── default/
//! │   │   ├── .vault.json
//! │   │   ├── secrets/
//! │   │   │   ├── <name>.age
//! │   │   │   ├── <name>.meta.json
//! │   │   │   └── .versions/<name>/v<N>.{age,meta.json}
//! │   └── ...
//! ```
//!
//! Key files (`key.txt`, `recipients.txt`) are stored alongside the store
//! or at a user-configured path.

pub mod config;
pub mod crypto;
#[cfg(feature = "file-ops")]
pub mod files;
pub mod opaque;
pub mod paths;
pub mod secrets;
pub mod vaults;

use std::fs;

use crate::utils::helpers::{create_private_dir, write_private};

use async_trait::async_trait;

use super::error::BackendError;
use super::{Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend, VaultBackend};

#[cfg(feature = "file-ops")]
use super::FileBackend;

use self::config::ResolvedLocalConfig;
#[cfg(feature = "file-ops")]
use self::files::LocalFileBackend;
use self::secrets::LocalSecretBackend;
use self::vaults::LocalVaultBackend;

/// The local age-encrypted file backend.
pub struct LocalBackend {
    config: ResolvedLocalConfig,
    secret_backend: LocalSecretBackend,
    vault_backend: LocalVaultBackend,
    #[cfg(feature = "file-ops")]
    file_backend: LocalFileBackend,
}

impl LocalBackend {
    /// Create a new `LocalBackend`.
    ///
    /// Key resolution order:
    /// 1. `AGE_KEY` env var (inline key)
    /// 2. `AGE_KEY_FILE` env var (path to key file)
    /// 3. Config `key_file` path
    /// 4. Default `~/.xv/key.txt`
    ///
    /// If no key file exists at the resolved path, a new keypair is generated.
    pub fn new(
        raw_config: Option<&crate::config::settings::LocalConfig>,
    ) -> Result<Self, BackendError> {
        let config = ResolvedLocalConfig::from_raw(raw_config);

        // Ensure store directory exists
        fs::create_dir_all(&config.store_path).map_err(|e| {
            BackendError::Internal(format!(
                "create store directory {}: {e}",
                config.store_path.display()
            ))
        })?;
        crypto::set_dir_permissions(&config.store_path)?;

        // Ensure vaults directory exists
        let vaults_dir = paths::vaults_dir(&config.store_path);
        create_private_dir(&vaults_dir).map_err(|e| {
            BackendError::Internal(format!(
                "create vaults directory {}: {e}",
                vaults_dir.display()
            ))
        })?;

        // Resolve identity and recipients
        let (identity, recipients) = Self::resolve_keys(&config)?;

        // Create default vault if it doesn't exist
        let default_vault_dir = paths::vault_dir(&config.store_path, &config.default_vault)?;
        if !default_vault_dir.join(".vault.json").exists() {
            create_private_dir(default_vault_dir.join("secrets"))
                .map_err(|e| BackendError::Internal(format!("create default vault: {e}")))?;
            let meta = serde_json::json!({
                "name": config.default_vault,
                "created_at": chrono::Utc::now().to_rfc3339(),
                "tags": {}
            });
            write_private(
                default_vault_dir.join(".vault.json"),
                serde_json::to_string_pretty(&meta)
                    .map_err(|e| BackendError::Internal(format!("serialize vault meta: {e}")))?
                    .as_bytes(),
            )
            .map_err(|e| BackendError::Internal(format!("write default vault meta: {e}")))?;
        }

        let secret_backend = LocalSecretBackend::with_options(
            config.store_path.clone(),
            identity.clone(),
            recipients.clone(),
            config.encrypt_metadata,
            config.opaque_filenames,
        );
        let vault_backend = LocalVaultBackend::new(config.store_path.clone());

        #[cfg(feature = "file-ops")]
        let file_backend = LocalFileBackend::new(config.store_path.clone(), identity, recipients);

        Ok(Self {
            config,
            secret_backend,
            vault_backend,
            #[cfg(feature = "file-ops")]
            file_backend,
        })
    }

    /// Whether this backend was configured to encrypt metadata at rest.
    pub fn encrypt_metadata_enabled(&self) -> bool {
        self.config.encrypt_metadata
    }

    /// Re-encrypt all plaintext secret metadata under the store. See
    /// [`LocalSecretBackend::reencrypt_all_metadata`]. Returns
    /// `(converted, skipped)`.
    pub fn reencrypt_all_metadata(&self, dry_run: bool) -> Result<(usize, usize), BackendError> {
        self.secret_backend.reencrypt_all_metadata(dry_run)
    }

    /// Whether this backend was configured to use opaque on-disk filenames.
    pub fn opaque_filenames_enabled(&self) -> bool {
        self.config.opaque_filenames
    }

    /// Migrate every vault to the opaque-filename layout. See
    /// [`LocalSecretBackend::migrate_all`].
    pub fn migrate_all(
        &self,
        dry_run: bool,
    ) -> Result<self::secrets::MigrationReport, BackendError> {
        self.secret_backend.migrate_all(dry_run)
    }

    /// Resolve age identity and recipients from env vars or files.
    fn resolve_keys(
        config: &ResolvedLocalConfig,
    ) -> Result<(age::x25519::Identity, Vec<age::x25519::Recipient>), BackendError> {
        // 1. AGE_KEY env var — inline identity string
        if let Ok(key_str) = std::env::var("AGE_KEY") {
            let identity: age::x25519::Identity = key_str
                .trim()
                .parse()
                .map_err(|e: &str| BackendError::Internal(format!("parse AGE_KEY: {e}")))?;
            let recipient = identity.to_public();
            return Ok((identity, vec![recipient]));
        }

        // 2. AGE_KEY_FILE env var — path to key file
        if let Ok(path_str) = std::env::var("AGE_KEY_FILE") {
            let path = std::path::PathBuf::from(&path_str);
            let identity = crypto::load_identity(&path)?;
            let recipients = if config.recipients_file.exists() {
                crypto::load_recipients(&config.recipients_file)?
            } else {
                vec![identity.to_public()]
            };
            return Ok((identity, recipients));
        }

        // 3. Config key_file path / default
        let key_path = &config.key_file;
        if key_path.exists() {
            let identity = crypto::load_identity(key_path)?;
            let recipients = if config.recipients_file.exists() {
                crypto::load_recipients(&config.recipients_file)?
            } else {
                vec![identity.to_public()]
            };
            Ok((identity, recipients))
        } else {
            // 4. Generate new keypair
            crypto::generate_keypair(key_path, &config.recipients_file)
        }
    }
}

#[async_trait]
impl Backend for LocalBackend {
    fn name(&self) -> &'static str {
        "local"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Local
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_vaults: true,
            has_file_storage: cfg!(feature = "file-ops"),
            has_rbac: false,
            has_audit: false,
            has_versioning: true,
            has_soft_delete: true,
            has_restore: true,
            has_purge: true,
            has_scheduled_purge: false,
            has_secret_rotation: false,
            has_groups: true,
            has_folders: true,
            has_notes: true,
            has_expiry: true,
            max_secret_size: None,
            max_name_length: Some(255),
            name_charset: NameCharset::Unrestricted,
            max_tags: None,
            max_tag_value_len: None,
        }
    }

    fn secrets(&self) -> &dyn SecretBackend {
        &self.secret_backend
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(&self.vault_backend)
    }

    #[cfg(feature = "file-ops")]
    fn files(&self) -> Option<&dyn FileBackend> {
        Some(&self.file_backend)
    }

    async fn health_check(&self) -> Result<(), BackendError> {
        // Verify store directory is accessible
        if !self.config.store_path.exists() {
            return Err(BackendError::Internal(format!(
                "store directory does not exist: {}",
                self.config.store_path.display()
            )));
        }

        // Verify key file is readable
        if !self.config.key_file.exists() {
            return Err(BackendError::Internal(format!(
                "key file does not exist: {}",
                self.config.key_file.display()
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::settings::LocalConfig;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> LocalConfig {
        LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
        }
    }

    #[test]
    fn new_creates_store_and_default_vault() {
        let tmp = TempDir::new().unwrap();
        let raw = make_config(&tmp);
        let backend = LocalBackend::new(Some(&raw)).unwrap();

        assert!(tmp.path().join("store/vaults/default/.vault.json").exists());
        assert!(tmp.path().join("key.txt").exists());
        assert_eq!(backend.name(), "local");
        assert_eq!(backend.kind(), BackendKind::Local);
    }

    #[test]
    fn capabilities_are_correct() {
        let tmp = TempDir::new().unwrap();
        let raw = make_config(&tmp);
        let backend = LocalBackend::new(Some(&raw)).unwrap();

        let caps = backend.capabilities();
        assert!(caps.has_vaults);
        assert!(caps.has_versioning);
        assert!(caps.has_groups);
        assert!(caps.has_folders);
        assert!(caps.has_notes);
        assert!(caps.has_expiry);
        assert!(caps.has_soft_delete);
        assert!(caps.has_restore);
        assert!(caps.has_purge);
        assert!(!caps.has_scheduled_purge);
        assert!(!caps.has_rbac);
        assert_eq!(caps.max_name_length, Some(255));
        #[cfg(feature = "file-ops")]
        assert!(caps.has_file_storage);
    }

    #[tokio::test]
    async fn health_check_passes() {
        let tmp = TempDir::new().unwrap();
        let raw = make_config(&tmp);
        let backend = LocalBackend::new(Some(&raw)).unwrap();

        backend.health_check().await.unwrap();
    }

    #[tokio::test]
    async fn end_to_end_secret_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let raw = make_config(&tmp);
        let backend = LocalBackend::new(Some(&raw)).unwrap();

        // Create secret
        let request = crate::secret::manager::SecretRequest {
            name: "e2e-test".into(),
            value: zeroize::Zeroizing::new("my-secret-value".into()),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        };

        let props = backend
            .secrets()
            .set_secret("default", request)
            .await
            .unwrap();
        assert_eq!(props.name, "e2e-test");

        // Get secret
        let props = backend
            .secrets()
            .get_secret("default", "e2e-test", true)
            .await
            .unwrap();
        assert_eq!(&*props.value.unwrap(), "my-secret-value");

        // List secrets
        let list = backend
            .secrets()
            .list_secrets("default", None)
            .await
            .unwrap();
        assert_eq!(list.len(), 1);

        // Delete secret
        backend
            .secrets()
            .delete_secret("default", "e2e-test")
            .await
            .unwrap();
        let list = backend
            .secrets()
            .list_secrets("default", None)
            .await
            .unwrap();
        assert!(list.is_empty());
    }
}
