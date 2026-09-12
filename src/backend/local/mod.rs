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

mod anchored;
pub mod audit;
pub mod config;
pub mod crypto;
#[cfg(feature = "file-ops")]
pub mod files;
pub mod git;
mod name_collision;
pub mod opaque;
pub mod paths;
pub mod secrets;
mod transfer_namespace;
pub mod vaults;

use std::fs;

use crate::utils::helpers::{create_private_dir, write_private};

use async_trait::async_trait;

use super::error::BackendError;
use super::{
    AuditBackend, Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend,
    VaultBackend,
};

#[cfg(feature = "file-ops")]
use super::FileBackend;

use self::audit::LocalAuditLog;
use self::config::ResolvedLocalConfig;
#[cfg(feature = "file-ops")]
use self::files::LocalFileBackend;
use self::git::LocalGitStore;
use self::secrets::LocalSecretBackend;
use self::vaults::LocalVaultBackend;

use std::sync::Arc;

/// The local age-encrypted file backend.
pub struct LocalBackend {
    config: ResolvedLocalConfig,
    secret_backend: LocalSecretBackend,
    vault_backend: LocalVaultBackend,
    #[cfg(feature = "file-ops")]
    file_backend: LocalFileBackend,
    /// Hash-chained audit log. `Some` only when `[local].audit` is on, which is
    /// also what drives the `has_audit` capability flag — so the flag can never
    /// claim an audit trail that is not actually being written.
    audit_log: Option<Arc<LocalAuditLog>>,
    /// Git store for the versioned-store commands. `Some` only when
    /// `[local].git` is on.
    git_store: Option<Arc<LocalGitStore>>,
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
        Self::construct(raw_config, true)
    }

    fn construct(
        raw_config: Option<&crate::config::settings::LocalConfig>,
        initialize: bool,
    ) -> Result<Self, BackendError> {
        let config = ResolvedLocalConfig::from_raw(raw_config);
        config.validate()?;

        if initialize {
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
        } else if anchored::open_configured_store_with_mode(&config.store_path, false, false)?
            .is_none()
        {
            return Err(BackendError::Unsupported(
                "local store does not exist; initialize it explicitly before attachment transfer"
                    .into(),
            ));
        }

        // Resolve identity and recipients
        let (identity, recipients) = Self::resolve_keys(&config, initialize)?;

        // Create default vault if it doesn't exist
        let default_vault_dir = paths::vault_dir(&config.store_path, &config.default_vault)?;
        if initialize && !default_vault_dir.join(".vault.json").exists() {
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

        let mut secret_backend = LocalSecretBackend::with_options(
            config.store_path.clone(),
            identity.clone(),
            recipients.clone(),
            config.encrypt_metadata,
            config.opaque_filenames,
        );

        let audit_log = if config.audit {
            let log = Arc::new(LocalAuditLog::new(config.store_path.clone(), &identity));
            secret_backend = secret_backend.with_audit_log(Arc::clone(&log));
            Some(log)
        } else {
            None
        };

        let git_store = if config.git {
            let git = Arc::new(LocalGitStore::new(
                config.store_path.clone(),
                config.key_file.clone(),
                config.recipients_file.clone(),
            ));
            // Initialize eagerly so a misconfiguration (e.g. key_file inside the
            // store, or git missing from PATH) surfaces at startup rather than
            // midway through the first write.
            if initialize {
                git.ensure_repo()?;
            }
            secret_backend = secret_backend.with_git_store(Arc::clone(&git));
            Some(git)
        } else {
            None
        };

        let vault_backend = LocalVaultBackend::new(config.store_path.clone());

        #[cfg(feature = "file-ops")]
        let file_backend = LocalFileBackend::new(config.store_path.clone(), identity, recipients);

        Ok(Self {
            config,
            secret_backend,
            vault_backend,
            #[cfg(feature = "file-ops")]
            file_backend,
            audit_log,
            git_store,
        })
    }

    /// Open configured local custody without initializing directories or keys.
    pub fn open_existing(
        raw_config: Option<&crate::config::settings::LocalConfig>,
    ) -> Result<Self, BackendError> {
        Self::construct(raw_config, false)
    }

    /// The hash-chained audit log, when `[local].audit` is enabled.
    ///
    /// Exposed for `xv audit --verify`, which needs the concrete type's
    /// `verify_chain` rather than the backend-agnostic [`AuditBackend`] trait.
    pub fn audit_log(&self) -> Option<&Arc<LocalAuditLog>> {
        self.audit_log.as_ref()
    }

    /// The git store, when `[local].git` is enabled.
    pub fn git_store(&self) -> Option<&Arc<LocalGitStore>> {
        self.git_store.as_ref()
    }

    /// A git store for this configuration regardless of whether `[local].git`
    /// is enabled.
    ///
    /// `xv git init` has to work *before* the flag is turned on — otherwise
    /// enabling versioning would be a chicken-and-egg problem.
    pub fn git_store_unconditional(&self) -> LocalGitStore {
        LocalGitStore::new(
            self.config.store_path.clone(),
            self.config.key_file.clone(),
            self.config.recipients_file.clone(),
        )
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
        initialize: bool,
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
        } else if !initialize {
            Err(BackendError::Unsupported("local identity is missing; initialize local custody explicitly before attachment transfer".into()))
        } else {
            // 4. Generate new keypair
            crypto::generate_keypair(key_path, &config.recipients_file)
        }
    }
}

/// Canonicalize existing ancestors without creating a recovery directory.
/// Resolve components in order so symlink/parent traversal cannot hide overlap.
fn resolved_recovery_path(path: &std::path::Path) -> Result<std::path::PathBuf, BackendError> {
    use std::path::Component;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| BackendError::Internal(format!("resolve recovery path: {e}")))?
            .join(path)
    };
    let mut resolved = std::path::PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Prefix(_) => {
                resolved.push(component.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => {
                resolved.push(component.as_os_str());
            }
        }
        match fs::symlink_metadata(&resolved) {
            Ok(meta) => {
                if !meta.is_dir() && !meta.file_type().is_symlink() {
                    return Err(BackendError::InvalidArgument(
                        "recovery path contains a non-directory".into(),
                    ));
                }
                resolved = fs::canonicalize(&resolved).map_err(|e| {
                    BackendError::Internal(format!("resolve recovery ancestor: {e}"))
                })?;
                if !resolved.is_dir() {
                    return Err(BackendError::InvalidArgument(
                        "recovery path contains a non-directory".into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(BackendError::Internal(format!(
                    "inspect recovery ancestor: {error}"
                )))
            }
        }
    }
    Ok(resolved)
}

fn validate_recovery_path(
    store: &std::path::Path,
    recovery: &std::path::Path,
) -> Result<(), BackendError> {
    let store = resolved_recovery_path(store)?;
    let recovery = crate::utils::recovery_path::resolve(recovery)
        .map_err(|error| BackendError::InvalidArgument(error.to_string()))?;
    // Compare canonical ancestors on both sides (including Windows device
    // prefixes/casing). Storage still opens the no-follow normalized path.
    let recovery = resolved_recovery_path(&recovery)?;
    if recovery.starts_with(&store) || store.starts_with(&recovery) {
        return Err(BackendError::InvalidArgument(
            "transfer recovery must be outside the backend store".into(),
        ));
    }
    if crate::utils::recovery_path::in_git(&recovery)
        .map_err(|error| BackendError::InvalidArgument(error.to_string()))?
    {
        return Err(BackendError::InvalidArgument(
            "transfer recovery must be outside Git worktrees; use --recovery-dir or XV_TRANSFER_RECOVERY_DIR to select a private directory outside Git and backend stores".into(),
        ));
    }
    Ok(())
}

#[async_trait]
impl Backend for LocalBackend {
    async fn validate_transfer_recovery_path(
        &self,
        vault: &str,
        path: &std::path::Path,
    ) -> Result<(), BackendError> {
        paths::validate_vault_name(vault)?;
        validate_recovery_path(&self.config.store_path, path)
    }

    async fn transfer_location(
        &self,
        vault: &str,
    ) -> Result<super::TransferLocation, BackendError> {
        transfer_namespace::transfer_location(&self.config.store_path, vault)
    }

    async fn transfer_secret_namespace(&self, vault: &str) -> Result<String, BackendError> {
        transfer_namespace::secret_namespace(&self.config.store_path, vault)
    }

    async fn transfer_secret_physical_namespace(
        &self,
        vault: &str,
    ) -> Result<String, BackendError> {
        transfer_namespace::physical_secret_namespace(&self.config.store_path, vault)
    }

    async fn transfer_file_physical_namespace(&self, vault: &str) -> Result<String, BackendError> {
        transfer_namespace::physical_file_namespace(&self.config.store_path, vault)
    }

    async fn transfer_secret_names_collide(
        &self,
        vault: &str,
        left: &str,
        right: &str,
    ) -> Result<bool, BackendError> {
        paths::validate_vault_name(vault)?;
        let left = self.secret_backend.active_stem(left);
        let right = self.secret_backend.active_stem(right);
        if left == right {
            return Ok(true);
        }
        if self.config.opaque_filenames || !left.eq_ignore_ascii_case(&right) {
            return Ok(false);
        }
        name_collision::case_insensitive(&self.config.store_path, vault)
    }

    async fn prepare_transfer_destination(
        &self,
        vault: &str,
        expected: &super::TransferLocation,
    ) -> Result<super::TransferLocation, BackendError> {
        transfer_namespace::prepare(&self.config.store_path, vault, expected)
    }

    fn name(&self) -> &'static str {
        "local"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Local
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_atomic_record_conversion: true,
            has_conditional_record_conversion: true,
            has_atomic_rename: true,
            has_atomic_file_create: cfg!(feature = "file-ops"),
            has_enable_disable: true,
            has_vaults: true,
            has_file_storage: cfg!(feature = "file-ops"),
            has_rbac: false,
            // Only true when the log is actually being written — see the
            // `audit_log` field. A blanket `true` here would repeat the
            // `has_audit` inconsistency closed for Azure in v0.21.
            has_audit: self.audit_log.is_some(),
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

    async fn attachment_names(&self, vault: &str, name: &str) -> Result<Vec<String>, BackendError> {
        let vault_directory = paths::vault_dir(&self.config.store_path, vault)?;
        match fs::symlink_metadata(&vault_directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(BackendError::Internal(format!(
                    "inspect attachment vault directory: {error}"
                )))
            }
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(BackendError::Internal(
                    "unsafe attachment vault directory".into(),
                ))
            }
        }
        match self.secret_backend.get_secret(vault, name, false).await {
            Ok(properties) => ensure_exact_attachment_owner(name, &properties.name)?,
            Err(BackendError::NotFound { .. }) => {}
            Err(error) => return Err(error),
        }
        #[cfg(feature = "file-ops")]
        {
            // Recover interrupted file transactions through the normal local
            // file path first. The direct metadata scan below is still needed
            // in every build and rejects malformed entries user listing skips.
            self.file_backend
                .list_files(
                    vault,
                    crate::blob::models::FileListRequest {
                        prefix: Some(format!("attachments/{name}/")),
                        groups: None,
                        limit: None,
                        delimiter: None,
                    },
                )
                .await?;
        }
        persisted_attachment_names(&self.config.store_path, vault, name)
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(&self.vault_backend)
    }

    fn audit(&self) -> Option<&dyn AuditBackend> {
        self.audit_log
            .as_deref()
            .map(|log| log as &dyn AuditBackend)
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

/// Reject a physical secret alias only when this store actually resolves it.
fn ensure_exact_attachment_owner(requested: &str, actual: &str) -> Result<(), BackendError> {
    if requested != actual {
        return Err(BackendError::InvalidArgument(format!(
            "attachment ownership is ambiguous: local name '{requested}' resolves to stored secret '{actual}'; use the exact stored name"
        )));
    }
    Ok(())
}

/// Read-only persisted inventory, also usable while atomic rename owns the vault
/// lock. Physical metadata probes preserve case-sensitive filesystem semantics.
fn persisted_attachment_names(
    store_path: &std::path::Path,
    vault: &str,
    name: &str,
) -> Result<Vec<String>, BackendError> {
    let directory = paths::files_dir(store_path, vault)?;
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(BackendError::Internal(format!(
                "refusing unsafe local file metadata directory {}",
                directory.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(BackendError::Internal(format!(
                "inspect local file metadata directory {}: {error}",
                directory.display()
            )))
        }
    };
    let entries = fs::read_dir(&directory)
        .map_err(|error| {
            BackendError::Internal(format!(
                "read local file metadata directory {}: {error}",
                directory.display()
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| {
            BackendError::Internal(format!("read local file metadata entry: {error}"))
        })?;
    let entry_names = entries
        .iter()
        .map(|entry| entry.file_name())
        .collect::<std::collections::HashSet<_>>();

    for entry in &entries {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            return Err(BackendError::Internal(
                "local file metadata entry name is not valid UTF-8".into(),
            ));
        };
        if let Some(stem) = file_name.strip_suffix(".age") {
            if !entry_names.contains(&std::ffi::OsString::from(format!("{stem}.meta.json"))) {
                return Err(BackendError::Internal(format!(
                    "local file body '{}' has no matching metadata",
                    entry.path().display()
                )));
            }
        }
        if file_name == ".transactions" {
            let mut transactions = fs::read_dir(entry.path()).map_err(|error| {
                BackendError::Internal(format!("inspect local file transactions: {error}"))
            })?;
            if transactions
                .next()
                .transpose()
                .map_err(|error| {
                    BackendError::Internal(format!("inspect local file transaction: {error}"))
                })?
                .is_some()
            {
                return Err(BackendError::Internal(
                    "local file storage has an unrecovered transaction; attachment visibility is ambiguous"
                        .into(),
                ));
            }
        }
    }

    let mut persisted = Vec::new();
    for entry in entries {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            return Err(BackendError::Internal(
                "local file metadata entry name is not valid UTF-8".into(),
            ));
        };
        if !file_name.ends_with(".meta.json") {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| BackendError::Internal(format!("inspect file metadata: {error}")))?
            .is_file()
        {
            return Err(BackendError::Internal(format!(
                "local file metadata entry is not a regular file: {}",
                entry.path().display()
            )));
        }
        let bytes = fs::read(entry.path()).map_err(|error| {
            BackendError::Internal(format!("read local file metadata: {error}"))
        })?;
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            BackendError::Internal(format!("parse local file metadata: {error}"))
        })?;
        let persisted_name = parsed
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                BackendError::Internal("local file metadata has no valid name".into())
            })?;
        let prefix = format!("attachments/{name}/");
        if persisted_name.starts_with(&prefix) {
            persisted.push(persisted_name.to_string());
        } else if let Some(rest) = persisted_name.strip_prefix("attachments/") {
            // Historical logical owners may contain slashes. Check each possible
            // boundary without assuming the filesystem's case/Unicode rules.
            for (boundary, _) in rest.match_indices('/') {
                let candidate = format!("attachments/{name}/{}", &rest[boundary + 1..]);
                if candidate.len() > paths::PLATFORM_SAFE_NAME_MAX {
                    continue; // This cannot be a supported local object key.
                }
                let stem = paths::file_storage_stem(&candidate)?;
                let candidate_path = directory.join(format!("{stem}.meta.json"));
                match fs::symlink_metadata(&candidate_path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(BackendError::Internal(format!(
                            "inspect attachment alias metadata: {error}"
                        )))
                    }
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                    Ok(_) => {
                        return Err(BackendError::Internal(
                            "unsafe attachment alias metadata".into(),
                        ))
                    }
                }
                let bytes = fs::read(&candidate_path).map_err(|error| {
                    BackendError::Internal(format!("read attachment alias metadata: {error}"))
                })?;
                let metadata: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        BackendError::Internal(format!("parse attachment alias metadata: {error}"))
                    })?;
                if metadata.get("name").and_then(serde_json::Value::as_str)
                    != Some(candidate.as_str())
                {
                    return Err(BackendError::InvalidArgument(format!(
                        "attachment ownership is ambiguous: local object '{candidate}' resolves to metadata with a different stored name"
                    )));
                }
            }
        }
    }
    super::validate_attachment_names(name, persisted)
}

#[cfg(test)]
mod tests {
    #[test]
    fn transfer_recovery_path_rejects_store_and_git_overlap_without_creation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir(&store).unwrap();
        assert!(super::validate_recovery_path(&store, &store.join("missing/recovery")).is_err());
        assert!(super::validate_recovery_path(&store, tmp.path()).is_err());
        let outside = tmp.path().join("outside/missing");
        super::validate_recovery_path(&store, &outside).unwrap();
        assert!(!outside.exists());
        let git = tmp.path().join("worktree");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(git.join(".git"), "gitdir: elsewhere").unwrap();
        assert!(super::validate_recovery_path(&store, &git.join("missing/recovery")).is_err());
        assert!(super::validate_recovery_path(&store, &store.join("../store/missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn transfer_recovery_path_resolves_symlink_aliases() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&store, &alias).unwrap();
        assert!(super::validate_recovery_path(&store, &alias.join("missing")).is_err());
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let alias = tmp.path().join("outside-alias");
        std::os::unix::fs::symlink(&outside, &alias).unwrap();
        assert!(super::validate_recovery_path(&store, &alias.join("missing")).is_err());
        assert!(super::validate_recovery_path(&store, &alias.join("../safe")).is_err());
        assert!(!outside.join("missing").exists());
    }

    use super::*;
    use crate::config::settings::LocalConfig;
    use tempfile::TempDir;

    #[test]
    fn transfer_open_existing_missing_store_never_initializes() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        assert!(LocalBackend::open_existing(Some(&config)).is_err());
        assert!(!tmp.path().join("store").exists());
        assert!(!tmp.path().join("key.txt").exists());
    }

    #[test]
    fn transfer_open_existing_missing_identity_never_generates_custody() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        fs::create_dir(tmp.path().join("store")).unwrap();
        assert!(LocalBackend::open_existing(Some(&config)).is_err());
        assert!(!tmp.path().join("key.txt").exists());
        assert!(!tmp.path().join("store/vaults").exists());
        assert!(fs::read_dir(tmp.path().join("store"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn transfer_open_existing_never_initializes_git_or_repairs_modes() {
        let tmp = TempDir::new().unwrap();
        let mut config = make_config(&tmp);
        LocalBackend::new(Some(&config)).unwrap();
        config.git = Some(true);
        let store = std::path::Path::new(config.store_path.as_ref().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(store, fs::Permissions::from_mode(0o755)).unwrap();
        }
        LocalBackend::open_existing(Some(&config)).unwrap();
        assert!(!store.join(".git").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    fn make_config(tmp: &TempDir) -> LocalConfig {
        LocalConfig {
            store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
            key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
            default_vault: Some("default".into()),
            encrypt_metadata: None,
            opaque_filenames: None,
            audit: None,
            git: None,
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

    #[tokio::test]
    async fn transfer_name_collisions_match_layout_and_filesystem_without_provisioning() {
        for opaque in [false, true] {
            let tmp = TempDir::new().unwrap();
            let mut raw = make_config(&tmp);
            raw.opaque_filenames = Some(opaque);
            let backend = LocalBackend::new(Some(&raw)).unwrap();
            // The fixture may probe; the production operation must remain read-only.
            let parent = tmp.path().join("store/vaults");
            fs::write(parent.join("case-probe"), b"").unwrap();
            let insensitive = parent.join("CASE-PROBE").exists();
            fs::remove_file(parent.join("case-probe")).unwrap();
            match backend
                .transfer_secret_names_collide("absent", "a", "A")
                .await
            {
                Ok(collides) => assert_eq!(collides, !opaque && insensitive),
                Err(BackendError::Unsupported(_)) if !opaque && !cfg!(target_os = "macos") => {}
                other => panic!("unexpected collision result: {other:?}"),
            }
            assert!(backend
                .transfer_secret_names_collide("absent", "a", "a")
                .await
                .unwrap());
            assert!(!backend
                .transfer_secret_names_collide("absent", "a", "b")
                .await
                .unwrap());
            assert!(!parent.join("absent").exists());
        }
    }

    #[test]
    fn capabilities_are_correct() {
        let tmp = TempDir::new().unwrap();
        let raw = make_config(&tmp);
        let backend = LocalBackend::new(Some(&raw)).unwrap();

        let caps = backend.capabilities();
        assert!(caps.has_vaults);
        assert!(caps.has_conditional_record_conversion);
        assert!(caps.has_atomic_rename);
        assert_eq!(caps.has_atomic_file_create, cfg!(feature = "file-ops"));
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
    async fn attachment_names_reads_persisted_metadata_without_file_operations() {
        let tmp = TempDir::new().unwrap();
        let backend = LocalBackend::new(Some(&make_config(&tmp))).unwrap();
        let files = tmp.path().join("store/vaults/default/files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(
            files.join("source.meta.json"),
            br#"{"name":"attachments/source/proof.txt"}"#,
        )
        .unwrap();
        std::fs::write(
            files.join("sibling.meta.json"),
            br#"{"name":"attachments/source-copy/proof.txt"}"#,
        )
        .unwrap();

        assert_eq!(
            backend.attachment_names("default", "source").await.unwrap(),
            vec!["attachments/source/proof.txt"]
        );
    }

    #[tokio::test]
    async fn local_missing_vault_attachment_inventory_remains_empty() {
        let tmp = TempDir::new().unwrap();
        let backend = LocalBackend::new(Some(&make_config(&tmp))).unwrap();
        assert!(backend
            .attachment_names("missing", "source")
            .await
            .unwrap()
            .is_empty());
        assert!(!tmp.path().join("store/vaults/missing").exists());
    }

    #[tokio::test]
    async fn local_case_alias_orphan_inventory_follows_actual_filesystem_lookup() {
        let tmp = TempDir::new().unwrap();
        let backend = LocalBackend::new(Some(&make_config(&tmp))).unwrap();
        let files = tmp.path().join("store/vaults/default/files");
        fs::create_dir_all(&files).unwrap();
        let original = files.join("attachments%2Fsource%2Fproof.txt.meta.json");
        let alias = files.join("attachments%2FSOURCE%2Fproof.txt.meta.json");
        fs::write(&original, br#"{"name":"attachments/source/proof.txt"}"#).unwrap();
        if !alias.exists() {
            // Distinct case is safe on a case-sensitive store, until an alias
            // actually resolves to the original object's metadata.
            assert!(backend
                .attachment_names("default", "SOURCE")
                .await
                .unwrap()
                .is_empty());
            fs::hard_link(&original, &alias).unwrap();
        }
        let error = backend
            .attachment_names("default", "SOURCE")
            .await
            .expect_err("orphan attachment alias must not imply absence");
        assert!(error.to_string().contains("attachment"), "{error}");
    }

    #[tokio::test]
    async fn attachment_names_fails_closed_on_malformed_persisted_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = LocalBackend::new(Some(&make_config(&tmp))).unwrap();
        let files = tmp.path().join("store/vaults/default/files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("broken.meta.json"), b"not-json").unwrap();

        let error = backend
            .attachment_names("default", "source")
            .await
            .expect_err("unknown persisted file metadata cannot imply absence");
        assert!(error.to_string().contains("file metadata"), "{error}");
    }

    #[tokio::test]
    async fn attachment_names_fails_closed_on_body_without_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = LocalBackend::new(Some(&make_config(&tmp))).unwrap();
        let files = tmp.path().join("store/vaults/default/files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("unknown.age"), b"ciphertext").unwrap();

        let error = backend
            .attachment_names("default", "source")
            .await
            .expect_err("an unclassifiable persisted object cannot imply absence");
        assert!(error.to_string().contains("matching metadata"), "{error}");
    }

    #[tokio::test]
    async fn end_to_end_secret_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let raw = make_config(&tmp);
        let backend = LocalBackend::new(Some(&raw)).unwrap();

        // Create secret
        let request = crate::secret::domain::SecretRequest {
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

    #[cfg(feature = "file-ops")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attachment_upload_racing_rename_never_orphans_the_old_namespace() {
        use crate::blob::models::{FileListRequest, FileUploadRequest};
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::Barrier;

        let tmp = TempDir::new().unwrap();
        let backend = Arc::new(LocalBackend::new(Some(&make_config(&tmp))).unwrap());
        backend
            .secrets()
            .set_secret(
                "default",
                crate::secret::domain::SecretRequest {
                    name: "source".into(),
                    value: zeroize::Zeroizing::new("value".into()),
                    content_type: None,
                    enabled: None,
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: None,
                    folder: None,
                },
            )
            .await
            .unwrap();
        let snapshot = backend
            .secrets()
            .get_secret_snapshot("default", "source", false)
            .await
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));

        let rename_backend = Arc::clone(&backend);
        let rename_barrier = Arc::clone(&barrier);
        let rename = tokio::spawn(async move {
            rename_barrier.wait().await;
            rename_backend
                .secrets()
                .rename_secret_if_revision("default", "source", "destination", &snapshot.revision)
                .await
        });

        let upload_backend = Arc::clone(&backend);
        let upload_barrier = Arc::clone(&barrier);
        let upload = tokio::spawn(async move {
            upload_barrier.wait().await;
            upload_backend
                .files()
                .unwrap()
                .upload_file(
                    "default",
                    FileUploadRequest {
                        name: crate::secret::attachments::attachment_blob_name(
                            "source",
                            "proof.txt",
                        ),
                        content: b"encrypted-attachment".to_vec(),
                        content_type: Some("application/octet-stream".into()),
                        groups: Vec::new(),
                        metadata: HashMap::new(),
                        tags: HashMap::new(),
                    },
                    None,
                )
                .await
        });

        barrier.wait().await;
        let rename_result = rename.await.unwrap();
        let upload_result = upload.await.unwrap();
        assert_ne!(
            rename_result.is_ok(),
            upload_result.is_ok(),
            "exactly one operation may win: {rename_result:?} / {upload_result:?}"
        );

        let old_attachments = backend
            .files()
            .unwrap()
            .list_files(
                "default",
                FileListRequest {
                    prefix: Some(crate::secret::attachments::attachment_prefix("source")),
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        if rename_result.is_ok() {
            assert!(
                old_attachments.is_empty(),
                "rename success must never leave an old-name attachment namespace"
            );
        } else {
            assert_eq!(old_attachments.len(), 1);
            assert!(backend
                .secrets()
                .secret_exists("default", "source")
                .await
                .unwrap());
        }
    }
}
