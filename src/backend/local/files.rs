//! Local file/blob backend — age-encrypted file storage.
//!
//! Each file is stored as two files inside
//! `<store>/vaults/<vault>/files/`:
//!
//! - `<encoded_name>.age`       — age-encrypted file content
//! - `<encoded_name>.meta.json` — plaintext metadata

use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::error::BackendError;
use crate::backend::file::FileBackend;
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::progress::ProgressReporter;

use super::anchored::{open_configured_store_with_mode, AnchoredDir};
#[cfg(test)]
use super::transfer_namespace::transfer_location;
use super::{crypto, paths};

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

use super::paths::{
    file_storage_stem as storage_stem, validate_logical_file_name, LONGEST_ACTIVE_SUFFIX_BYTES,
    PLATFORM_SAFE_NAME_MAX,
};

#[cfg(test)]
fn files_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::files_dir(store_path, vault)
}

#[cfg(test)]
fn file_age_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = storage_stem(name)?;
    Ok(files_dir(store_path, vault)?.join(format!("{enc}.age")))
}

#[cfg(test)]
fn file_meta_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = storage_stem(name)?;
    Ok(files_dir(store_path, vault)?.join(format!("{enc}.meta.json")))
}

// ---------------------------------------------------------------------------
// Metadata persisted alongside each file
// ---------------------------------------------------------------------------

#[cfg(test)]
fn sync_directory(path: &Path) -> Result<(), BackendError> {
    // Directory fsync is a Unix durability primitive with no Windows analogue —
    // and `File::open` on a directory fails outright there without
    // FILE_FLAG_BACKUP_SEMANTICS. Skip the sync off Unix but still record the
    // event, so the ordering assertions in these tests stay meaningful on every
    // platform. Mirrors `sync_directory` in `super::secrets`.
    #[cfg(unix)]
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| BackendError::Internal(format!("sync directory {}: {e}", path.display())))?;
    tests::record_file_event("sync-dir", path);
    Ok(())
}

#[cfg(test)]
fn ensure_private_real_directory(path: &Path) -> Result<(), BackendError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(BackendError::Internal(format!(
                "refusing unsafe file transaction directory {}",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                BackendError::Internal(format!("create test transaction directory: {error}"))
            })?;
        }
        Err(error) => {
            return Err(BackendError::Internal(format!(
                "inspect test transaction directory: {error}"
            )))
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            BackendError::Internal(format!("set test transaction permissions: {error}"))
        })?;
    }
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

struct LocalFileChain {
    files_dir: PathBuf,
    _store: AnchoredDir,
    _vaults: AnchoredDir,
    vault: AnchoredDir,
    files: AnchoredDir,
}

fn local_file_chain_paths(store_path: &Path, vault: &str) -> Result<[PathBuf; 4], BackendError> {
    let vault_dir = paths::vault_dir(store_path, vault)?;
    Ok([
        store_path.to_path_buf(),
        paths::vaults_dir(store_path),
        vault_dir.clone(),
        vault_dir.join("files"),
    ])
}

fn open_configured_store(
    store_path: &Path,
    create: bool,
) -> Result<Option<AnchoredDir>, BackendError> {
    open_configured_store_with_mode(store_path, create, true)
}

fn existing_local_file_chain(
    store_path: &Path,
    vault: &str,
) -> Result<Option<LocalFileChain>, BackendError> {
    let chain = local_file_chain_paths(store_path, vault)?;
    let Some(store) = open_configured_store(store_path, false)? else {
        return Ok(None);
    };
    local_file_chain_from_store(chain, store, false)
}

fn ensure_writable_local_file_chain(
    store_path: &Path,
    vault: &str,
) -> Result<LocalFileChain, BackendError> {
    let chain = local_file_chain_paths(store_path, vault)?;
    let store = open_configured_store(store_path, true)?
        .ok_or_else(|| BackendError::Internal("configured store parent does not exist".into()))?;
    local_file_chain_from_store(chain, store, true)?
        .ok_or_else(|| BackendError::Internal("failed to create local file chain".into()))
}

fn local_file_chain_from_store(
    chain: [PathBuf; 4],
    store: AnchoredDir,
    create: bool,
) -> Result<Option<LocalFileChain>, BackendError> {
    let vaults = match store.open_dir("vaults")? {
        Some(directory) => directory,
        None if create => store.open_or_create_private_dir("vaults")?,
        None => return Ok(None),
    };
    vaults.repair_private_mode()?;
    let vault_name = chain[2]
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BackendError::Internal("vault name is not valid UTF-8".into()))?;
    let vault = match vaults.open_dir(vault_name)? {
        Some(directory) => directory,
        None if create => vaults.open_or_create_private_dir(vault_name)?,
        None => return Ok(None),
    };
    vault.repair_private_mode()?;
    let files = match vault.open_dir("files")? {
        Some(directory) => directory,
        None if create => vault.open_or_create_private_dir("files")?,
        None => return Ok(None),
    };
    files.repair_private_mode()?;
    Ok(Some(LocalFileChain {
        files_dir: chain[3].clone(),
        _store: store,
        _vaults: vaults,
        vault,
        files,
    }))
}

fn lock_local_file_chain(
    store_path: &Path,
    vault: &str,
    chain: &LocalFileChain,
) -> Result<fs::File, BackendError> {
    let _ = (store_path, vault);
    let lock_file = chain
        .vault
        .open_file_with_flags(".lock", libc::O_RDWR | libc::O_CREAT, 0o600)?
        .ok_or_else(|| BackendError::Internal("open local file lock failed".into()))?;
    lock_file.lock_exclusive().map_err(|error| {
        BackendError::Internal(format!("local file vault lock failed: {error}"))
    })?;
    Ok(lock_file)
}

#[derive(Debug, Serialize, Deserialize)]
struct FileUploadJournal {
    version: u8,
    had_age: bool,
    had_meta: bool,
}

#[cfg(test)]
struct FileTransactionPaths {
    dir: PathBuf,
    staged_age: PathBuf,
    staged_meta: PathBuf,
    active_age: PathBuf,
    active_meta: PathBuf,
}

#[cfg(test)]
impl FileTransactionPaths {
    fn new(store_path: &Path, vault: &str, name: &str) -> Result<Self, BackendError> {
        let fdir = files_dir(store_path, vault)?;
        let stem = storage_stem(name)?;
        Self::from_stem(&fdir, &stem)
    }

    fn from_stem(fdir: &Path, stem: &str) -> Result<Self, BackendError> {
        if stem.is_empty() || stem == "." || stem == ".." || stem.contains('/') {
            return Err(BackendError::Internal(
                "invalid local file transaction stem".into(),
            ));
        }
        let dir = fdir.join(".transactions").join(stem);
        Ok(Self {
            staged_age: dir.join("new.age"),
            staged_meta: dir.join("new.meta.json"),
            active_age: fdir.join(format!("{stem}.age")),
            active_meta: fdir.join(format!("{stem}.meta.json")),
            dir,
        })
    }
}

// ---------------------------------------------------------------------------
// LocalFileBackend
// ---------------------------------------------------------------------------

/// File-backed file/blob operations using age encryption.
///
/// The target vault is supplied per call (see [`FileBackend`]), so one
/// instance serves every vault in the store.
pub struct LocalFileBackend {
    store_path: PathBuf,
    identity: age::x25519::Identity,
    recipients: Vec<age::x25519::Recipient>,
}

impl LocalFileBackend {
    /// Create a new `LocalFileBackend`.
    pub fn new(
        store_path: PathBuf,
        identity: age::x25519::Identity,
        recipients: Vec<age::x25519::Recipient>,
    ) -> Self {
        Self {
            store_path,
            identity,
            recipients,
        }
    }

    fn attachment_secret_name(name: &str) -> Option<&str> {
        name.strip_prefix("attachments/")
            .and_then(|rest| rest.split_once('/'))
            .map(|(secret, _)| secret)
            .filter(|secret| !secret.is_empty())
    }

    fn active_secret_exists_in_chain(
        &self,
        chain: &LocalFileChain,
        name: &str,
    ) -> Result<bool, BackendError> {
        let Some(secrets) = chain.vault.open_dir("secrets")? else {
            return Ok(false);
        };
        for entry in secrets.entry_names()? {
            if !entry.ends_with(".meta.json") {
                continue;
            }
            let raw = secrets.read_file(&entry)?.ok_or_else(|| {
                BackendError::Internal("active secret metadata disappeared".into())
            })?;
            if super::secrets::secret_name_from_metadata_bytes(&raw, &self.identity)? == name {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[cfg(test)]
    fn file_crash(&self, stage: u8) -> Result<(), BackendError> {
        tests::run_file_crash_hook(&self.store_path, stage)
    }

    #[cfg(not(test))]
    fn file_crash(&self, _stage: u8) -> Result<(), BackendError> {
        Ok(())
    }

    #[cfg(test)]
    fn file_swap_barrier(&self, stage: &'static str, files_dir: &Path) -> Result<(), BackendError> {
        tests::run_file_swap_hook(&self.store_path, stage, files_dir)
    }

    #[cfg(not(test))]
    fn file_swap_barrier(
        &self,
        _stage: &'static str,
        _files_dir: &Path,
    ) -> Result<(), BackendError> {
        Ok(())
    }

    fn remove_transaction_dir(
        root: &AnchoredDir,
        transaction: &AnchoredDir,
        stem: &str,
    ) -> Result<(), BackendError> {
        for entry in transaction.entry_names()? {
            transaction.remove_file(&entry)?;
        }
        root.remove_dir(stem)?;
        root.sync()
    }

    fn restore_entry(
        files: &AnchoredDir,
        transaction: &AnchoredDir,
        backup: &str,
        active: &str,
        existed: bool,
    ) -> Result<(), BackendError> {
        if existed {
            let digest = Sha256::digest(active.as_bytes());
            let temp = format!(".recovering-{digest:x}");
            transaction.copy_file_to(backup, files, &temp)?;
            files.rename_to(&temp, files, active)?;
        } else if files.file_exists(active)? {
            files.remove_file(active)?;
        }
        Ok(())
    }

    fn recover_transaction_locked(
        &self,
        chain: &LocalFileChain,
        root: &AnchoredDir,
        stem: &str,
    ) -> Result<(), BackendError> {
        let Some(transaction) = root.open_dir(stem)? else {
            return Ok(());
        };
        transaction.repair_private_mode()?;
        let Some(journal_bytes) = transaction.read_file("journal.json")? else {
            return Self::remove_transaction_dir(root, &transaction, stem);
        };
        let journal: FileUploadJournal = serde_json::from_slice(&journal_bytes)
            .map_err(|e| BackendError::Internal(format!("parse file upload journal: {e}")))?;
        if journal.version != 1
            || (journal.had_age && !transaction.file_exists("old.age")?)
            || (journal.had_meta && !transaction.file_exists("old.meta.json")?)
        {
            return Err(BackendError::Internal(
                "invalid local file upload journal".into(),
            ));
        }
        let active_age = format!("{stem}.age");
        let active_meta = format!("{stem}.meta.json");
        Self::restore_entry(
            &chain.files,
            &transaction,
            "old.age",
            &active_age,
            journal.had_age,
        )?;
        Self::restore_entry(
            &chain.files,
            &transaction,
            "old.meta.json",
            &active_meta,
            journal.had_meta,
        )?;
        chain.files.sync()?;
        transaction.remove_file("journal.json")?;
        transaction.sync()?;
        Self::remove_transaction_dir(root, &transaction, stem)
    }

    fn recover_all_locked(&self, chain: &LocalFileChain) -> Result<(), BackendError> {
        self.file_swap_barrier("before-recovery", &chain.files_dir)?;
        let Some(root) = chain.files.open_dir(".transactions")? else {
            return Ok(());
        };
        root.repair_private_mode()?;
        for stem in root.entry_names()? {
            self.recover_transaction_locked(chain, &root, &stem)?;
        }
        Ok(())
    }

    fn delete_transaction_locked(
        &self,
        chain: &LocalFileChain,
        name: &str,
        expected_etag: &str,
    ) -> Result<(), BackendError> {
        self.recover_all_locked(chain)?;
        let stem = storage_stem(name)?;
        let active_age = format!("{stem}.age");
        let active_meta = format!("{stem}.meta.json");
        let metadata =
            chain
                .files
                .read_file(&active_meta)?
                .ok_or_else(|| BackendError::NotFound {
                    name: name.into(),
                    suggestion: None,
                })?;
        let info: FileInfo = serde_json::from_slice(&metadata)
            .map_err(|e| BackendError::Internal(format!("parse file metadata: {e}")))?;
        if info.etag != expected_etag {
            return Err(BackendError::Conflict(
                "source file generation changed".into(),
            ));
        }
        if !chain.files.file_exists(&active_age)? {
            return Err(BackendError::Internal(
                "local file pair is incomplete".into(),
            ));
        }
        let root = chain.files.open_or_create_private_dir(".transactions")?;
        let transaction = root.open_or_create_private_dir(&stem)?;
        chain
            .files
            .copy_file_to(&active_age, &transaction, "old.age")?;
        chain
            .files
            .copy_file_to(&active_meta, &transaction, "old.meta.json")?;
        transaction.sync()?;
        self.file_crash(10)?;
        let journal = serde_json::to_vec(&FileUploadJournal {
            version: 1,
            had_age: true,
            had_meta: true,
        })
        .map_err(|e| BackendError::Internal(format!("serialize delete journal: {e}")))?;
        transaction.create_private_file("journal.tmp", &journal)?;
        transaction.rename_to("journal.tmp", &transaction, "journal.json")?;
        transaction.sync()?;
        self.file_crash(11)?;
        chain.files.remove_file(&active_age)?;
        chain.files.sync()?;
        self.file_crash(12)?;
        chain.files.remove_file(&active_meta)?;
        chain.files.sync()?;
        self.file_crash(13)?;
        transaction.remove_file("journal.json")?;
        transaction.sync()?;
        self.file_crash(14)?;
        Self::remove_transaction_dir(&root, &transaction, &stem)
    }

    fn upload_transaction_locked(
        &self,
        chain: &LocalFileChain,
        request: FileUploadRequest,
        create_only: bool,
    ) -> Result<FileInfo, BackendError> {
        self.recover_all_locked(chain)?;
        let original_size = request.content.len() as u64;
        let stem = storage_stem(&request.name)?;
        #[cfg(test)]
        let paths = FileTransactionPaths::from_stem(&chain.files_dir, &stem)?;
        let active_age = format!("{stem}.age");
        let active_meta = format!("{stem}.meta.json");
        let had_age = chain.files.file_exists(&active_age)?;
        let had_meta = chain.files.file_exists(&active_meta)?;
        if had_age != had_meta {
            return Err(BackendError::Internal(
                "local file content and metadata are inconsistent".into(),
            ));
        }
        if create_only && had_age {
            return Err(BackendError::DestinationExists { name: request.name });
        }

        let now = Utc::now();
        let info = FileInfo {
            name: request.name.clone(),
            size: original_size,
            content_type: request
                .content_type
                .unwrap_or_else(|| "application/octet-stream".into()),
            last_modified: now,
            etag: format!("\"{}\"", uuid::Uuid::new_v4()),
            groups: request.groups,
            metadata: request.metadata,
            tags: request.tags,
        };
        let transaction_root = chain.files.open_or_create_private_dir(".transactions")?;
        if transaction_root.open_dir(&stem)?.is_some() {
            self.recover_transaction_locked(chain, &transaction_root, &stem)?;
        }
        let transaction = transaction_root.open_or_create_private_dir(&stem)?;

        let staged = (|| {
            let output = transaction.create_private_file_handle("new.age")?;
            let output =
                crypto::encrypt_to_file_handle(output, &request.content, &self.recipients)?;
            output
                .sync_all()
                .map_err(|error| BackendError::Internal(format!("sync ciphertext: {error}")))?;
            let metadata = serde_json::to_vec_pretty(&info)
                .map_err(|e| BackendError::Internal(format!("serialize file meta: {e}")))?;
            transaction.create_private_file("new.meta.json", &metadata)?;
            if had_age {
                chain
                    .files
                    .copy_file_to(&active_age, &transaction, "old.age")?;
                chain
                    .files
                    .copy_file_to(&active_meta, &transaction, "old.meta.json")?;
            }
            transaction.sync()
        })();
        if let Err(error) = staged {
            let _ = Self::remove_transaction_dir(&transaction_root, &transaction, &stem);
            return Err(error);
        }
        self.file_crash(0)?;

        let journal = FileUploadJournal {
            version: 1,
            had_age,
            had_meta,
        };
        let journal_bytes = serde_json::to_vec(&journal)
            .map_err(|e| BackendError::Internal(format!("serialize file upload journal: {e}")))?;
        let published = (|| {
            transaction.create_private_file("journal.tmp", &journal_bytes)?;
            transaction.rename_to("journal.tmp", &transaction, "journal.json")?;
            transaction.sync()
        })();
        if let Err(error) = published {
            if transaction.file_exists("journal.json")? {
                self.recover_transaction_locked(chain, &transaction_root, &stem)?;
            } else {
                Self::remove_transaction_dir(&transaction_root, &transaction, &stem)?;
            }
            return Err(error);
        }
        self.file_crash(1)?;
        self.file_swap_barrier("before-active-rename", &chain.files_dir)?;

        let activated = (|| {
            #[cfg(test)]
            tests::record_file_event("active-rename", &paths.active_age);
            transaction.rename_to("new.age", &chain.files, &active_age)?;
            chain.files.sync()?;
            Ok::<(), BackendError>(())
        })();
        if let Err(error) = activated {
            self.recover_transaction_locked(chain, &transaction_root, &stem)?;
            return Err(error);
        }
        self.file_crash(2)?;

        let activated = (|| {
            #[cfg(test)]
            tests::record_file_event("active-rename", &paths.active_meta);
            transaction.rename_to("new.meta.json", &chain.files, &active_meta)?;
            chain.files.sync()?;
            Ok::<(), BackendError>(())
        })();
        if let Err(error) = activated {
            self.recover_transaction_locked(chain, &transaction_root, &stem)?;
            return Err(error);
        }
        self.file_crash(3)?;

        transaction.remove_file("journal.json")?;
        transaction.sync()?;
        chain.files.sync()?;
        self.file_crash(4)?;
        Self::remove_transaction_dir(&transaction_root, &transaction, &stem)?;
        Ok(info)
    }

    fn upload_with_policy(
        &self,
        vault: &str,
        request: FileUploadRequest,
        create_only: bool,
    ) -> Result<FileInfo, BackendError> {
        let chain = ensure_writable_local_file_chain(&self.store_path, vault)?;
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.recover_all_locked(&chain)?;
        if let Some(secret_name) = Self::attachment_secret_name(&request.name) {
            self.file_swap_barrier("before-attachment-validation", &chain.vault.display)?;
            if !self.active_secret_exists_in_chain(&chain, secret_name)? {
                return Err(BackendError::NotFound {
                    name: secret_name.to_string(),
                    suggestion: None,
                });
            }
        }
        self.upload_transaction_locked(&chain, request, create_only)
    }
}

#[async_trait]
impl FileBackend for LocalFileBackend {
    fn validate_transfer_request(
        &self,
        request: &crate::backend::file::FileTransferRequest<'_>,
    ) -> Result<(), BackendError> {
        paths::validate_vault_name(request.vault)?;
        self.validate_file_name(request.name)
    }

    async fn transfer_file_names_collide(
        &self,
        vault: &str,
        left: &str,
        right: &str,
    ) -> Result<bool, BackendError> {
        paths::validate_vault_name(vault)?;
        let left = storage_stem(left)?;
        let right = storage_stem(right)?;
        if left == right {
            return Ok(true);
        }
        if !left.eq_ignore_ascii_case(&right) {
            return Ok(false);
        }
        // File stems never use the secret backend's opaque filename setting.
        super::name_collision::case_insensitive_for_child(&self.store_path, vault, "files")
    }

    async fn get_file_restore_info(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<FileInfo, BackendError> {
        self.get_file_info(vault, name).await
    }

    async fn restore_file(
        &self,
        vault: &str,
        request: FileUploadRequest,
    ) -> Result<FileInfo, BackendError> {
        self.upload_file(vault, request, None).await
    }

    fn validate_file_name(&self, name: &str) -> Result<(), BackendError> {
        validate_logical_file_name(name)?;
        let stem = storage_stem(name)?;
        if stem.len() + LONGEST_ACTIVE_SUFFIX_BYTES > PLATFORM_SAFE_NAME_MAX {
            return Err(BackendError::InvalidArgument(
                "local file key cannot be represented safely".into(),
            ));
        }
        Ok(())
    }

    fn supports_atomic_create(&self) -> bool {
        true
    }

    async fn upload_file(
        &self,
        vault: &str,
        request: FileUploadRequest,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        self.validate_file_name(&request.name)?;
        self.upload_with_policy(vault, request, false)
    }

    async fn upload_file_if_absent(
        &self,
        vault: &str,
        request: FileUploadRequest,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<FileInfo, BackendError> {
        self.validate_file_name(&request.name)?;
        self.upload_with_policy(vault, request, true)
    }

    async fn download_file(
        &self,
        vault: &str,
        name: &str,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<Vec<u8>, BackendError> {
        self.validate_file_name(name)?;
        let Some(chain) = existing_local_file_chain(&self.store_path, vault)? else {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        };
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.recover_all_locked(&chain)?;
        self.file_swap_barrier("before-download-read", &chain.files_dir)?;
        let stem = storage_stem(name)?;
        let active_age = format!("{stem}.age");
        let Some(ciphertext) = chain.files.open_file(&active_age)? else {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        };
        let mut plaintext = crypto::decrypt_from_reader(ciphertext, &self.identity)?;
        let output = crypto::take_plaintext_allocation(&mut plaintext);
        debug_assert!(plaintext.is_empty());
        Ok(output)
    }

    async fn download_file_snapshot(
        &self,
        vault: &str,
        name: &str,
        _reporter: Option<&dyn ProgressReporter>,
    ) -> Result<crate::backend::file::FileDownloadSnapshot, BackendError> {
        self.validate_file_name(name)?;
        let missing = || BackendError::NotFound {
            name: name.into(),
            suggestion: None,
        };
        let chain = existing_local_file_chain(&self.store_path, vault)?.ok_or_else(missing)?;
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.recover_all_locked(&chain)?;
        let stem = storage_stem(name)?;
        let metadata = chain
            .files
            .read_file(&format!("{stem}.meta.json"))?
            .ok_or_else(missing)?;
        let info: FileInfo = serde_json::from_slice(&metadata)
            .map_err(|error| BackendError::Internal(format!("parse file metadata: {error}")))?;
        // The lock spans both reads and the handles retain the same directory
        // even if its pathname is replaced between metadata and body reads.
        self.file_swap_barrier("snapshot-after-metadata", &chain.files_dir)?;
        let ciphertext = chain
            .files
            .open_file(&format!("{stem}.age"))?
            .ok_or_else(missing)?;
        let mut plaintext = crypto::decrypt_from_reader(ciphertext, &self.identity)?;
        Ok(crate::backend::file::FileDownloadSnapshot {
            content: crypto::take_plaintext_allocation(&mut plaintext),
            metadata: info.metadata,
        })
    }

    async fn list_files(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<FileInfo>, BackendError> {
        let Some(chain) = existing_local_file_chain(&self.store_path, vault)? else {
            return Ok(Vec::new());
        };
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.recover_all_locked(&chain)?;

        let mut results = Vec::new();
        for fname in chain.files.entry_names()? {
            if !fname.ends_with(".meta.json") {
                continue;
            }
            let info: FileInfo = match chain
                .files
                .read_file(&fname)?
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                Some(info) => info,
                None => continue,
            };

            // Apply prefix filter
            if let Some(ref prefix) = request.prefix {
                if !info.name.starts_with(prefix) {
                    continue;
                }
            }

            // Apply groups filter
            if let Some(ref groups) = request.groups {
                if !groups.iter().any(|g| info.groups.contains(g)) {
                    continue;
                }
            }

            results.push(info);
        }

        // Sort by name for deterministic output
        results.sort_by(|a, b| a.name.cmp(&b.name));

        // Apply limit
        if let Some(limit) = request.limit {
            results.truncate(limit);
        }

        Ok(results)
    }

    fn supports_conditional_delete(&self) -> bool {
        true
    }

    async fn delete_file_if_etag(
        &self,
        vault: &str,
        name: &str,
        expected_etag: &str,
    ) -> Result<(), BackendError> {
        self.validate_file_name(name)?;
        let chain = existing_local_file_chain(&self.store_path, vault)?.ok_or_else(|| {
            BackendError::NotFound {
                name: name.into(),
                suggestion: None,
            }
        })?;
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.delete_transaction_locked(&chain, name, expected_etag)
    }

    async fn delete_file(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.validate_file_name(name)?;
        let Some(chain) = existing_local_file_chain(&self.store_path, vault)? else {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        };
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.recover_all_locked(&chain)?;
        let stem = storage_stem(name)?;
        let active_age = format!("{stem}.age");
        let active_meta = format!("{stem}.meta.json");

        let has_age = chain.files.file_exists(&active_age)?;
        let has_meta = chain.files.file_exists(&active_meta)?;
        if !has_age && !has_meta {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        if has_age {
            chain.files.remove_file(&active_age)?;
        }
        if has_meta {
            chain.files.remove_file(&active_meta)?;
        }
        chain.files.sync()?;

        Ok(())
    }

    async fn get_file_info(&self, vault: &str, name: &str) -> Result<FileInfo, BackendError> {
        self.validate_file_name(name)?;
        let Some(chain) = existing_local_file_chain(&self.store_path, vault)? else {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        };
        let _lock = lock_local_file_chain(&self.store_path, vault, &chain)?;
        self.recover_all_locked(&chain)?;
        let stem = storage_stem(name)?;
        let active_meta = format!("{stem}.meta.json");
        let Some(metadata) = chain.files.read_file(&active_meta)? else {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        };
        serde_json::from_slice(&metadata)
            .map_err(|error| BackendError::Internal(format!("parse file metadata: {error}")))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::backend::local::crypto::generate_keypair;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn file_crash_hooks() -> &'static Mutex<HashMap<PathBuf, u8>> {
        static HOOKS: OnceLock<Mutex<HashMap<PathBuf, u8>>> = OnceLock::new();
        HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    #[derive(Clone)]
    struct FileSwapHook {
        external: PathBuf,
        parked: PathBuf,
    }

    fn file_swap_hooks() -> &'static Mutex<HashMap<(PathBuf, &'static str), FileSwapHook>> {
        static HOOKS: OnceLock<Mutex<HashMap<(PathBuf, &'static str), FileSwapHook>>> =
            OnceLock::new();
        HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn configured_link_swap_hooks() -> &'static Mutex<HashMap<PathBuf, PathBuf>> {
        static HOOKS: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
        HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    #[cfg(unix)]
    fn install_configured_link_swap(link: &Path, replacement: &Path) {
        configured_link_swap_hooks()
            .lock()
            .unwrap()
            .insert(link.to_path_buf(), replacement.to_path_buf());
    }

    #[cfg(unix)]
    pub(in crate::backend::local) fn run_configured_link_swap_hook(
        link: &Path,
    ) -> Result<(), BackendError> {
        let Some(replacement) = configured_link_swap_hooks().lock().unwrap().remove(link) else {
            return Ok(());
        };
        fs::remove_file(link)
            .map_err(|error| BackendError::Internal(format!("test remove symlink: {error}")))?;
        std::os::unix::fs::symlink(replacement, link)
            .map_err(|error| BackendError::Internal(format!("test replace symlink: {error}")))
    }

    fn file_events() -> &'static Mutex<Vec<(String, PathBuf)>> {
        static EVENTS: OnceLock<Mutex<Vec<(String, PathBuf)>>> = OnceLock::new();
        EVENTS.get_or_init(|| Mutex::new(Vec::new()))
    }

    #[derive(Default)]
    struct TransferStats {
        max_chunk: usize,
        full_reads: Vec<String>,
    }

    fn transfer_stats() -> &'static Mutex<TransferStats> {
        static STATS: OnceLock<Mutex<TransferStats>> = OnceLock::new();
        STATS.get_or_init(|| Mutex::new(TransferStats::default()))
    }

    pub(in crate::backend::local) fn record_transfer_chunk(count: usize) {
        let mut stats = transfer_stats().lock().unwrap();
        stats.max_chunk = stats.max_chunk.max(count);
    }

    pub(in crate::backend::local) fn record_full_file_read(name: &str) {
        transfer_stats()
            .lock()
            .unwrap()
            .full_reads
            .push(name.to_string());
    }

    fn reset_transfer_stats() {
        *transfer_stats().lock().unwrap() = TransferStats::default();
    }

    fn take_transfer_stats() -> TransferStats {
        std::mem::take(&mut *transfer_stats().lock().unwrap())
    }

    fn file_event_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    pub(in crate::backend::local) fn record_file_event(kind: &str, path: &Path) {
        file_events()
            .lock()
            .unwrap()
            .push((kind.to_string(), path.to_path_buf()));
    }

    fn take_file_events() -> Vec<(String, PathBuf)> {
        std::mem::take(&mut *file_events().lock().unwrap())
    }

    fn install_file_crash(store_path: &Path, stage: u8) {
        file_crash_hooks()
            .lock()
            .unwrap()
            .insert(store_path.to_path_buf(), stage);
    }

    fn install_file_swap(store_path: &Path, stage: &'static str, external: &Path, parked: &Path) {
        file_swap_hooks().lock().unwrap().insert(
            (store_path.to_path_buf(), stage),
            FileSwapHook {
                external: external.to_path_buf(),
                parked: parked.to_path_buf(),
            },
        );
    }

    pub(super) fn run_file_swap_hook(
        store_path: &Path,
        stage: &'static str,
        files_dir: &Path,
    ) -> Result<(), BackendError> {
        let Some(hook) = file_swap_hooks()
            .lock()
            .unwrap()
            .remove(&(store_path.to_path_buf(), stage))
        else {
            return Ok(());
        };
        fs::rename(files_dir, &hook.parked).map_err(|error| {
            BackendError::Internal(format!("test park files directory: {error}"))
        })?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&hook.external, files_dir).map_err(|error| {
            BackendError::Internal(format!("test swap files directory: {error}"))
        })?;
        #[cfg(not(unix))]
        return Err(BackendError::Internal(
            "directory swap hook is Unix-only".into(),
        ));
        Ok(())
    }

    pub(super) fn run_file_crash_hook(store_path: &Path, stage: u8) -> Result<(), BackendError> {
        let should_crash = file_crash_hooks()
            .lock()
            .unwrap()
            .get(store_path)
            .is_some_and(|expected| *expected == stage);
        if should_crash {
            file_crash_hooks().lock().unwrap().remove(store_path);
            return Err(BackendError::Internal(format!(
                "simulated crash after local file upload stage {stage}"
            )));
        }
        Ok(())
    }

    fn test_file_backend() -> (LocalFileBackend, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        // Create default vault
        let vault_dir = store.join("vaults").join("default");
        fs::create_dir_all(vault_dir.join("files")).unwrap();
        let vault_meta = serde_json::json!({
            "name": "default",
            "created_at": Utc::now().to_rfc3339(),
            "tags": {}
        });
        fs::write(
            vault_dir.join(".vault.json"),
            serde_json::to_string_pretty(&vault_meta).unwrap(),
        )
        .unwrap();

        let backend = LocalFileBackend::new(store, identity, recipients);
        (backend, tmp)
    }

    #[tokio::test]
    async fn destination_file_identity_uses_files_case_semantics_and_hashes_read_only() {
        let (backend, tmp) = test_file_backend();
        let parent = tmp.path().join("vaults/default");
        // A secret-directory alias must not affect independent file semantics.
        #[cfg(unix)]
        std::os::unix::fs::symlink(".", parent.join("secrets")).unwrap();
        for existing in [true, false] {
            if !existing {
                fs::remove_dir(parent.join("files")).unwrap();
            }
            let directory = if existing {
                parent.join("files")
            } else {
                parent.clone()
            };
            fs::write(directory.join("case-probe"), b"").unwrap();
            let insensitive = directory.join("CASE-PROBE").exists();
            fs::remove_file(directory.join("case-probe")).unwrap();
            let before = tree_snapshot(tmp.path());
            match backend
                .transfer_file_names_collide(
                    "default",
                    "attachments/cert/proof.txt",
                    "attachments/cert/PROOF.txt",
                )
                .await
            {
                Ok(collides) => assert_eq!(collides, insensitive),
                Err(BackendError::Unsupported(message)) if !cfg!(target_os = "macos") => assert!(
                    message.contains("cannot establish destination filesystem case semantics")
                ),
                other => panic!("unexpected identity result: {other:?}"),
            }
            let owner = "s".repeat(220);
            assert!(!backend
                .transfer_file_names_collide(
                    "default",
                    &format!("attachments/{owner}/proof.txt"),
                    &format!("attachments/{owner}/PROOF.txt")
                )
                .await
                .unwrap());
            assert!(backend
                .transfer_file_names_collide(
                    "default",
                    "attachments/cert/proof.txt",
                    "attachments/cert/proof.txt"
                )
                .await
                .unwrap());
            assert_eq!(tree_snapshot(tmp.path()), before);
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(".", parent.join("files")).unwrap();
            let before = tree_snapshot(tmp.path());
            assert!(backend
                .transfer_file_names_collide(
                    "default",
                    "attachments/cert/proof.txt",
                    "attachments/cert/PROOF.txt"
                )
                .await
                .is_err());
            assert_eq!(tree_snapshot(tmp.path()), before);
        }
    }

    fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, current: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let mut entries = fs::read_dir(current)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    snapshot.insert(relative.clone(), b"<directory>".to_vec());
                    walk(root, &path, snapshot);
                } else if kind.is_symlink() {
                    snapshot.insert(
                        relative,
                        fs::read_link(&path)
                            .unwrap()
                            .as_os_str()
                            .as_encoded_bytes()
                            .to_vec(),
                    );
                } else {
                    snapshot.insert(relative, fs::read(path).unwrap());
                }
            }
        }

        let mut snapshot = BTreeMap::new();
        walk(root, root, &mut snapshot);
        snapshot
    }

    fn snapshot_request(generation: &str) -> FileUploadRequest {
        FileUploadRequest {
            name: "snapshot.bin".into(),
            content: generation.as_bytes().to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::from([("generation".into(), generation.into())]),
            tags: HashMap::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn snapshots_remain_coherent_during_concurrent_replacement() {
        let (backend, _tmp) = test_file_backend();
        backend
            .upload_file("default", snapshot_request("initial"), None)
            .await
            .unwrap();
        let backend = std::sync::Arc::new(backend);
        let writer = backend.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let other_barrier = barrier.clone();
        let task = tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();
            other_barrier.wait();
            for i in 0..32 {
                runtime
                    .block_on(writer.upload_file("default", snapshot_request(&i.to_string()), None))
                    .unwrap();
            }
        });
        // Run on a blocking worker since local store locking is synchronous.
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Handle::current();
            barrier.wait();
            for _ in 0..32 {
                let snapshot = runtime
                    .block_on(backend.download_file_snapshot("default", "snapshot.bin", None))
                    .unwrap();
                assert_eq!(snapshot.content, snapshot.metadata["generation"].as_bytes());
            }
        })
        .await
        .unwrap();
        task.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_retains_directory_generation_between_metadata_and_body() {
        let (backend, tmp) = test_file_backend();
        backend
            .upload_file("default", snapshot_request("original"), None)
            .await
            .unwrap();
        let external = tmp.path().join("outside");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"untouched").unwrap();
        let before = tree_snapshot(&external);
        install_file_swap(
            tmp.path(),
            "snapshot-after-metadata",
            &external,
            &tmp.path().join("parked-files"),
        );
        let snapshot = backend
            .download_file_snapshot("default", "snapshot.bin", None)
            .await
            .unwrap();
        assert_eq!(snapshot.content, b"original");
        assert_eq!(snapshot.metadata["generation"], "original");
        assert_eq!(tree_snapshot(&external), before);
        assert!(backend
            .download_file_snapshot("../../outside", "snapshot.bin", None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn snapshot_refuses_missing_and_corrupt_metadata_even_when_body_exists() {
        let (backend, tmp) = test_file_backend();
        backend
            .upload_file("default", snapshot_request("original"), None)
            .await
            .unwrap();
        let metadata = tmp.path().join("vaults/default/files").join(format!(
            "{}.meta.json",
            storage_stem("snapshot.bin").unwrap()
        ));
        fs::write(&metadata, b"invalid json").unwrap();
        assert!(backend
            .download_file_snapshot("default", "snapshot.bin", None)
            .await
            .is_err());
        fs::remove_file(metadata).unwrap();
        assert!(matches!(
            backend
                .download_file_snapshot("default", "snapshot.bin", None)
                .await,
            Err(BackendError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn upload_and_download_file() {
        let (backend, _tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "cert.pem".into(),
            content: b"-----BEGIN CERTIFICATE-----\nMIIBxTCCA...".to_vec(),
            content_type: Some("application/x-pem-file".into()),
            groups: vec!["infra".into()],
            metadata: HashMap::new(),
            tags: HashMap::from([("env".into(), "prod".into())]),
        };

        let info = backend.upload_file("default", request, None).await.unwrap();
        assert_eq!(info.name, "cert.pem");
        assert_eq!(info.content_type, "application/x-pem-file");
        assert!(info.size > 0);

        // Download and verify
        let bytes = backend
            .download_file("default", "cert.pem", None)
            .await
            .unwrap();
        assert_eq!(bytes, b"-----BEGIN CERTIFICATE-----\nMIIBxTCCA...");
    }

    #[tokio::test]
    async fn rejects_traversal_vault_name_for_file_writes() {
        let (backend, tmp) = test_file_backend();

        let result = backend
            .upload_file(
                "../../outside",
                FileUploadRequest {
                    name: "escape.txt".into(),
                    content: b"content".to_vec(),
                    content_type: None,
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                },
                None,
            )
            .await;

        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert!(!tmp.path().join("outside").exists());
    }

    #[tokio::test]
    async fn rejects_traversal_vault_name_for_file_reads() {
        let (backend, _tmp) = test_file_backend();

        for result in [
            backend
                .download_file("../../outside", "any.txt", None)
                .await
                .map(|_| ()),
            backend
                .list_files(
                    "../../outside",
                    FileListRequest {
                        prefix: None,
                        groups: None,
                        limit: None,
                        delimiter: None,
                    },
                )
                .await
                .map(|_| ()),
            backend.delete_file("../../outside", "any.txt").await,
            backend
                .get_file_info("../../outside", "any.txt")
                .await
                .map(|_| ()),
        ] {
            assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_file_entrypoint_rejects_symlinked_vault_without_touching_external_tree() {
        use std::os::unix::fs::symlink;

        let (backend, tmp) = test_file_backend();
        let external = tmp.path().join("external-vault");
        fs::create_dir_all(external.join("files")).unwrap();
        fs::write(external.join("files/existing.age"), b"external ciphertext").unwrap();
        fs::write(
            external.join("files/existing.meta.json"),
            br#"{"name":"existing","size":19}"#,
        )
        .unwrap();
        symlink(&external, tmp.path().join("vaults/linked")).unwrap();
        let before = tree_snapshot(&external);

        let list_request = || FileListRequest {
            prefix: None,
            groups: None,
            limit: None,
            delimiter: None,
        };
        let upload_request = || upload_request("new.txt", b"must not escape", "marker");
        let results = [
            backend
                .get_file_info("linked", "existing")
                .await
                .map(|_| ()),
            backend
                .download_file("linked", "existing", None)
                .await
                .map(|_| ()),
            backend
                .list_files("linked", list_request())
                .await
                .map(|_| ()),
            backend.delete_file("linked", "existing").await,
            backend
                .upload_file("linked", upload_request(), None)
                .await
                .map(|_| ()),
            backend
                .upload_file_if_absent("linked", upload_request(), None)
                .await
                .map(|_| ()),
        ];
        for result in results {
            assert!(
                matches!(result, Err(BackendError::Internal(_))),
                "unsafe chain must fail closed: {result:?}"
            );
            assert_eq!(
                tree_snapshot(&external),
                before,
                "external tree changed after rejected operation"
            );
        }
    }

    #[tokio::test]
    async fn fresh_store_and_vault_links_are_durable_before_file_mutation() {
        let _event_guard = file_event_test_lock().lock().await;
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();
        let store = tmp.path().join("fresh-store");
        let backend = LocalFileBackend::new(store.clone(), identity, recipients);
        take_file_events();

        backend
            .upload_file(
                "fresh",
                upload_request("durable.txt", b"bytes", "marker"),
                None,
            )
            .await
            .unwrap();

        let events: Vec<_> = take_file_events()
            .into_iter()
            .filter(|(_, path)| path.starts_with(&store) || path == tmp.path())
            .collect();
        let active = events
            .iter()
            .position(|(kind, _)| kind == "active-rename")
            .expect("active rename event");
        for required in [
            tmp.path().to_path_buf(),
            store.clone(),
            store.join("vaults"),
            store.join("vaults/fresh"),
        ] {
            assert!(
                events[..active]
                    .iter()
                    .any(|(kind, path)| kind == "sync-dir" && path == &required),
                "{} must be synced before file mutation: {events:?}",
                required.display()
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [
                store.clone(),
                store.join("vaults"),
                store.join("vaults/fresh"),
            ] {
                assert_eq!(
                    fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
    }

    #[tokio::test]
    async fn operations_target_the_requested_vault_not_default() {
        let (backend, tmp) = test_file_backend();

        let upload = |vault: &'static str, content: &'static [u8]| {
            let backend = &backend;
            async move {
                backend
                    .upload_file(
                        vault,
                        FileUploadRequest {
                            name: "config.txt".into(),
                            content: content.to_vec(),
                            content_type: None,
                            groups: Vec::new(),
                            metadata: HashMap::new(),
                            tags: HashMap::new(),
                        },
                        None,
                    )
                    .await
                    .unwrap()
            }
        };

        // Same file name uploaded to two non-default vaults with different content.
        upload("dev", b"dev content").await;
        upload("prod", b"prod content").await;

        // Each vault stores its files under its own directory; the default
        // vault's files dir (pre-created by the test helper) stays empty.
        assert!(tmp.path().join("vaults/dev/files").exists());
        assert!(tmp.path().join("vaults/prod/files").exists());
        let mut default_entries = fs::read_dir(tmp.path().join("vaults/default/files")).unwrap();
        assert!(default_entries.next().is_none());

        // Downloads resolve per vault, not to the default vault.
        let dev = backend
            .download_file("dev", "config.txt", None)
            .await
            .unwrap();
        let prod = backend
            .download_file("prod", "config.txt", None)
            .await
            .unwrap();
        assert_eq!(dev, b"dev content");
        assert_eq!(prod, b"prod content");

        // Listing and metadata are scoped to the requested vault.
        let list_req = || FileListRequest {
            prefix: None,
            groups: None,
            limit: None,
            delimiter: None,
        };
        assert_eq!(
            backend.list_files("dev", list_req()).await.unwrap().len(),
            1
        );
        assert!(backend
            .list_files("default", list_req())
            .await
            .unwrap()
            .is_empty());
        backend.get_file_info("prod", "config.txt").await.unwrap();
        let missing = backend.get_file_info("default", "config.txt").await;
        assert!(matches!(missing, Err(BackendError::NotFound { .. })));

        // Deleting in one vault leaves the other vault untouched.
        backend.delete_file("dev", "config.txt").await.unwrap();
        let gone = backend.download_file("dev", "config.txt", None).await;
        assert!(matches!(gone, Err(BackendError::NotFound { .. })));
        let still_there = backend
            .download_file("prod", "config.txt", None)
            .await
            .unwrap();
        assert_eq!(still_there, b"prod content");
    }

    #[tokio::test]
    async fn list_files_with_filters() {
        let (backend, _tmp) = test_file_backend();

        // Upload several files
        for (name, groups) in [
            ("configs/app.yaml", vec!["prod"]),
            ("configs/db.yaml", vec!["prod", "db"]),
            ("scripts/deploy.sh", vec!["ops"]),
        ] {
            let request = FileUploadRequest {
                name: name.into(),
                content: b"content".to_vec(),
                content_type: None,
                groups: groups.into_iter().map(String::from).collect(),
                metadata: HashMap::new(),
                tags: HashMap::new(),
            };
            backend.upload_file("default", request, None).await.unwrap();
        }

        // List all
        let all = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: None,
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(all.len(), 3);

        // Filter by prefix
        let configs = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: Some("configs/".into()),
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(configs.len(), 2);

        // Filter by group
        let db_files = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: None,
                    groups: Some(vec!["db".into()]),
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(db_files.len(), 1);
        assert_eq!(db_files[0].name, "configs/db.yaml");

        // Limit
        let limited = backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: None,
                    groups: None,
                    limit: Some(2),
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[tokio::test]
    async fn delete_file() {
        let (backend, _tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "to-delete.txt".into(),
            content: b"delete me".to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        backend.upload_file("default", request, None).await.unwrap();

        // File exists
        let info = backend
            .get_file_info("default", "to-delete.txt")
            .await
            .unwrap();
        assert_eq!(info.name, "to-delete.txt");

        // Delete
        backend
            .delete_file("default", "to-delete.txt")
            .await
            .unwrap();

        // Should be gone
        let result = backend.get_file_info("default", "to-delete.txt").await;
        assert!(matches!(result, Err(BackendError::NotFound { .. })));
    }

    #[tokio::test]
    async fn get_file_info_returns_metadata() {
        let (backend, _tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "info-test.bin".into(),
            content: vec![0u8; 1024],
            content_type: Some("application/octet-stream".into()),
            groups: vec!["test".into()],
            metadata: HashMap::from([("uploaded_by".into(), "test-agent".into())]),
            tags: HashMap::from([("version".into(), "1.0".into())]),
        };
        backend.upload_file("default", request, None).await.unwrap();

        let info = backend
            .get_file_info("default", "info-test.bin")
            .await
            .unwrap();
        assert_eq!(info.name, "info-test.bin");
        assert_eq!(info.size, 1024);
        assert_eq!(info.content_type, "application/octet-stream");
        assert_eq!(info.groups, vec!["test"]);
        assert_eq!(info.metadata.get("uploaded_by").unwrap(), "test-agent");
        assert_eq!(info.tags.get("version").unwrap(), "1.0");
    }

    #[tokio::test]
    async fn concurrent_create_if_absent_preserves_exactly_one_winner() {
        let (backend, _tmp) = test_file_backend();
        let backend = std::sync::Arc::new(backend);
        let first = FileUploadRequest {
            name: "race.txt".into(),
            content: b"first".to_vec(),
            content_type: Some("text/plain".into()),
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        let second = FileUploadRequest {
            name: "race.txt".into(),
            content: b"second".to_vec(),
            content_type: Some("text/plain".into()),
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        let a = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.upload_file_if_absent("default", first, None).await })
        };
        let b = {
            let backend = backend.clone();
            tokio::spawn(
                async move { backend.upload_file_if_absent("default", second, None).await },
            )
        };
        let results = [a.await.unwrap(), b.await.unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(BackendError::DestinationExists { .. })))
                .count(),
            1
        );
        let bytes = backend
            .download_file("default", "race.txt", None)
            .await
            .unwrap();
        assert!(bytes == b"first" || bytes == b"second");
    }

    fn upload_request(name: &str, bytes: &[u8], marker: &str) -> FileUploadRequest {
        FileUploadRequest {
            name: name.into(),
            content: bytes.to_vec(),
            content_type: Some("text/plain".into()),
            groups: vec![marker.into()],
            metadata: HashMap::from([("marker".into(), marker.into())]),
            tags: HashMap::from([("marker".into(), marker.into())]),
        }
    }

    #[cfg(unix)]
    fn restore_swapped_files(files_dir: &Path, parked: &Path) {
        fs::remove_file(files_dir).unwrap();
        fs::rename(parked, files_dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_remains_anchored_when_files_path_is_swapped_after_lock() {
        let (backend, tmp) = test_file_backend();
        backend
            .upload_file(
                "default",
                upload_request("anchored.txt", b"original", "old"),
                None,
            )
            .await
            .unwrap();
        let files_dir = tmp.path().join("vaults/default/files");
        let parked = tmp.path().join("parked-files");
        let external = tmp.path().join("external-files");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"external").unwrap();
        let external_before = tree_snapshot(&external);
        install_file_swap(
            &backend.store_path,
            "before-download-read",
            &external,
            &parked,
        );

        let bytes = backend
            .download_file("default", "anchored.txt", None)
            .await
            .unwrap();
        assert_eq!(bytes, b"original");
        assert_eq!(tree_snapshot(&external), external_before);
        restore_swapped_files(&files_dir, &parked);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activation_remains_in_one_anchored_generation_when_files_path_is_swapped() {
        let (backend, tmp) = test_file_backend();
        backend
            .upload_file(
                "default",
                upload_request("anchored.txt", b"old", "old"),
                None,
            )
            .await
            .unwrap();
        let files_dir = tmp.path().join("vaults/default/files");
        let parked = tmp.path().join("parked-files");
        let external = tmp.path().join("external-files");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"external").unwrap();
        let external_before = tree_snapshot(&external);
        install_file_swap(
            &backend.store_path,
            "before-active-rename",
            &external,
            &parked,
        );

        backend
            .upload_file(
                "default",
                upload_request("anchored.txt", b"new", "new"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(tree_snapshot(&external), external_before);
        assert!(fs::read_dir(parked.join(".transactions"))
            .unwrap()
            .next()
            .is_none());
        restore_swapped_files(&files_dir, &parked);
        assert_eq!(
            backend
                .download_file("default", "anchored.txt", None)
                .await
                .unwrap(),
            b"new"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_remains_anchored_when_files_path_is_swapped() {
        let (backend, tmp) = test_file_backend();
        backend
            .upload_file(
                "default",
                upload_request("anchored.txt", b"old", "old"),
                None,
            )
            .await
            .unwrap();
        let identity = backend.identity.clone();
        let recipients = backend.recipients.clone();
        install_file_crash(&backend.store_path, 2);
        backend
            .upload_file(
                "default",
                upload_request("anchored.txt", b"partial", "partial"),
                None,
            )
            .await
            .expect_err("injected crash");
        drop(backend);

        let files_dir = tmp.path().join("vaults/default/files");
        let parked = tmp.path().join("parked-files");
        let external = tmp.path().join("external-files");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"external").unwrap();
        let external_before = tree_snapshot(&external);
        let restarted = LocalFileBackend::new(tmp.path().to_path_buf(), identity, recipients);
        install_file_swap(&restarted.store_path, "before-recovery", &external, &parked);

        let info = restarted
            .get_file_info("default", "anchored.txt")
            .await
            .unwrap();
        assert_eq!(info.metadata.get("marker").map(String::as_str), Some("old"));
        assert_eq!(tree_snapshot(&external), external_before);
        assert!(fs::read_dir(parked.join(".transactions"))
            .unwrap()
            .next()
            .is_none());
        restore_swapped_files(&files_dir, &parked);
        assert_eq!(
            restarted
                .download_file("default", "anchored.txt", None)
                .await
                .unwrap(),
            b"old"
        );
    }

    #[tokio::test]
    async fn conditional_file_delete_recovers_every_boundary() {
        for stage in 10..=14 {
            let (backend, tmp) = test_file_backend();
            let info = backend
                .upload_file(
                    "default",
                    upload_request("atomic.txt", b"original", "old"),
                    None,
                )
                .await
                .unwrap();
            assert!(backend
                .delete_file_if_etag("default", "atomic.txt", "wrong-generation")
                .await
                .is_err());
            let identity = backend.identity.clone();
            let recipients = backend.recipients.clone();
            install_file_crash(&backend.store_path, stage);
            assert!(backend
                .delete_file_if_etag("default", "atomic.txt", &info.etag)
                .await
                .is_err());
            drop(backend);
            let restarted = LocalFileBackend::new(tmp.path().to_path_buf(), identity, recipients);
            let current = restarted.get_file_info("default", "atomic.txt").await;
            if stage == 14 {
                assert!(matches!(current, Err(BackendError::NotFound { .. })));
            } else {
                assert_eq!(current.unwrap().etag, info.etag);
                assert_eq!(
                    restarted
                        .download_file("default", "atomic.txt", None)
                        .await
                        .unwrap(),
                    b"original"
                );
                restarted
                    .delete_file_if_etag("default", "atomic.txt", &info.etag)
                    .await
                    .unwrap();
            }
            assert!(matches!(
                restarted.get_file_info("default", "atomic.txt").await,
                Err(BackendError::NotFound { .. })
            ));
        }
    }

    #[tokio::test]
    async fn transfer_location_missing_files_is_read_only() {
        let (_backend, tmp) = test_file_backend();
        fs::remove_dir(tmp.path().join("vaults/default/files")).unwrap();
        fs::create_dir_all(tmp.path().join("vaults/default/secrets")).unwrap();
        let location = transfer_location(tmp.path(), "default").unwrap();
        assert!(location.files.starts_with("local-pending-files:"));
        assert!(!tmp.path().join("vaults/default/files").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transfer_location_binds_aliases_and_replaced_directories() {
        let (backend, tmp) = test_file_backend();
        backend
            .upload_file(
                "default",
                upload_request("atomic.txt", b"original", "old"),
                None,
            )
            .await
            .unwrap();
        let vault = tmp.path().join("vaults/default");
        fs::create_dir_all(vault.join("secrets")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let original = transfer_location(tmp.path(), "default").unwrap();
        assert_eq!(
            fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777,
            0o755,
            "namespace discovery must not repair directory modes"
        );
        let alias_root = TempDir::new().unwrap();
        let alias = alias_root.path().join("alias");
        std::os::unix::fs::symlink(tmp.path(), &alias).unwrap();
        assert!(transfer_location(&alias, "default").is_err());
        assert_eq!(
            original,
            transfer_location(&fs::canonicalize(tmp.path()).unwrap(), "default").unwrap()
        );
        fs::rename(vault.join("files"), vault.join("files-old")).unwrap();
        fs::create_dir(vault.join("files")).unwrap();
        let changed = transfer_location(tmp.path(), "default").unwrap();
        assert_ne!(original.files, changed.files);
        assert_eq!(original.secrets, changed.secrets);
        fs::rename(vault.join("secrets"), vault.join("secrets-old")).unwrap();
        fs::create_dir(vault.join("secrets")).unwrap();
        let changed = transfer_location(tmp.path(), "default").unwrap();
        assert_ne!(original.secrets, changed.secrets);
        assert_ne!(original.keys, changed.keys);
    }

    #[tokio::test]
    async fn restart_recovers_replace_crashes_without_mixing_bytes_and_metadata() {
        for stage in 0..=4 {
            let (backend, tmp) = test_file_backend();
            backend
                .upload_file(
                    "default",
                    upload_request("atomic.txt", b"old-bytes", "old"),
                    None,
                )
                .await
                .unwrap();
            let identity = backend.identity.clone();
            let recipients = backend.recipients.clone();
            install_file_crash(&backend.store_path, stage);
            let error = backend
                .upload_file(
                    "default",
                    upload_request("atomic.txt", b"new-bytes", "new"),
                    None,
                )
                .await
                .expect_err("injected crash must interrupt upload");
            assert!(error.to_string().contains("simulated crash"));
            drop(backend);

            let restarted = LocalFileBackend::new(tmp.path().to_path_buf(), identity, recipients);
            let info = restarted
                .get_file_info("default", "atomic.txt")
                .await
                .unwrap();
            let bytes = restarted
                .download_file("default", "atomic.txt", None)
                .await
                .unwrap();
            let expected = if stage < 4 { "old" } else { "new" };
            assert_eq!(
                info.metadata.get("marker").map(String::as_str),
                Some(expected)
            );
            assert_eq!(info.tags.get("marker").map(String::as_str), Some(expected));
            assert_eq!(info.groups, vec![expected]);
            assert_eq!(
                bytes,
                if stage < 4 {
                    b"old-bytes".as_slice()
                } else {
                    b"new-bytes".as_slice()
                },
                "stage {stage}"
            );
        }
    }

    #[tokio::test]
    async fn create_only_crash_recovery_removes_partial_destination_and_retry_succeeds() {
        let (backend, tmp) = test_file_backend();
        let identity = backend.identity.clone();
        let recipients = backend.recipients.clone();
        install_file_crash(&backend.store_path, 2);
        backend
            .upload_file_if_absent(
                "default",
                upload_request("new.txt", b"partial", "partial"),
                None,
            )
            .await
            .expect_err("injected crash");
        drop(backend);

        let restarted = LocalFileBackend::new(tmp.path().to_path_buf(), identity, recipients);
        restarted
            .upload_file_if_absent(
                "default",
                upload_request("new.txt", b"retry", "retry"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            restarted
                .download_file("default", "new.txt", None)
                .await
                .unwrap(),
            b"retry"
        );
    }

    #[tokio::test]
    async fn every_file_entrypoint_recovers_before_observing_or_mutating() {
        for operation in ["info", "download", "list", "delete", "upload"] {
            let (backend, tmp) = test_file_backend();
            backend
                .upload_file("default", upload_request("entry.txt", b"old", "old"), None)
                .await
                .unwrap();
            let identity = backend.identity.clone();
            let recipients = backend.recipients.clone();
            install_file_crash(&backend.store_path, 2);
            backend
                .upload_file(
                    "default",
                    upload_request("entry.txt", b"partial", "partial"),
                    None,
                )
                .await
                .expect_err("injected crash");
            drop(backend);
            let restarted = LocalFileBackend::new(tmp.path().to_path_buf(), identity, recipients);

            match operation {
                "info" => {
                    let info = restarted
                        .get_file_info("default", "entry.txt")
                        .await
                        .unwrap();
                    assert_eq!(info.metadata["marker"], "old");
                }
                "download" => assert_eq!(
                    restarted
                        .download_file("default", "entry.txt", None)
                        .await
                        .unwrap(),
                    b"old"
                ),
                "list" => {
                    let files = restarted
                        .list_files(
                            "default",
                            FileListRequest {
                                prefix: None,
                                groups: None,
                                limit: None,
                                delimiter: None,
                            },
                        )
                        .await
                        .unwrap();
                    assert_eq!(files[0].metadata["marker"], "old");
                }
                "delete" => {
                    restarted.delete_file("default", "entry.txt").await.unwrap();
                    assert!(matches!(
                        restarted.get_file_info("default", "entry.txt").await,
                        Err(BackendError::NotFound { .. })
                    ));
                }
                "upload" => {
                    restarted
                        .upload_file(
                            "default",
                            upload_request("entry.txt", b"retry", "retry"),
                            None,
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        restarted
                            .download_file("default", "entry.txt", None)
                            .await
                            .unwrap(),
                        b"retry"
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn upload_rejects_symlinked_transaction_root_without_writing_outside() {
        use std::os::unix::fs::symlink;

        let (backend, tmp) = test_file_backend();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(
            &outside,
            tmp.path()
                .join("vaults/default/files")
                .join(".transactions"),
        )
        .unwrap();

        let error = backend
            .upload_file(
                "default",
                upload_request("safe.txt", b"secret", "marker"),
                None,
            )
            .await
            .expect_err("symlink transaction root must fail closed");
        assert!(error.to_string().contains("unsafe"));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        assert!(!file_age_path(&backend.store_path, "default", "safe.txt")
            .unwrap()
            .exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staged_transaction_artifacts_are_private_and_restart_cleans_them() {
        use std::os::unix::fs::PermissionsExt;

        let (backend, tmp) = test_file_backend();
        let identity = backend.identity.clone();
        let recipients = backend.recipients.clone();
        install_file_crash(&backend.store_path, 0);
        backend
            .upload_file(
                "default",
                upload_request("private.txt", b"private", "private"),
                None,
            )
            .await
            .expect_err("injected crash");
        let paths =
            FileTransactionPaths::new(&backend.store_path, "default", "private.txt").unwrap();
        assert_eq!(
            fs::metadata(&paths.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for path in [&paths.staged_age, &paths.staged_meta] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(backend);
        let restarted = LocalFileBackend::new(tmp.path().to_path_buf(), identity, recipients);
        assert!(matches!(
            restarted.get_file_info("default", "private.txt").await,
            Err(BackendError::NotFound { .. })
        ));
        assert!(!paths.dir.exists());
    }

    #[tokio::test]
    async fn local_name_validation_honors_logical_and_encoded_component_boundaries() {
        let (backend, tmp) = test_file_backend();
        let ascii_255 = "a".repeat(255);
        let ascii_256 = "a".repeat(256);
        backend.validate_file_name(&ascii_255).unwrap();
        assert!(matches!(
            backend.validate_file_name(&ascii_256),
            Err(BackendError::InvalidArgument(_))
        ));

        let multibyte_254 = "é".repeat(127);
        let multibyte_256 = "é".repeat(128);
        backend.validate_file_name(&multibyte_254).unwrap();
        assert!(matches!(
            backend.validate_file_name(&multibyte_256),
            Err(BackendError::InvalidArgument(_))
        ));
        let stem = storage_stem(&multibyte_254).unwrap();
        assert!(
            stem.len() + ".meta.json".len() <= PLATFORM_SAFE_NAME_MAX,
            "percent expansion must switch to a fixed safe stem"
        );

        backend
            .upload_file(
                "default",
                upload_request(&ascii_255, b"boundary", "marker"),
                None,
            )
            .await
            .unwrap();
        let downloaded = backend
            .download_file("default", &ascii_255, None)
            .await
            .unwrap();
        assert_eq!(downloaded, b"boundary");
        let meta_path = file_meta_path(tmp.path(), "default", &ascii_255).unwrap();
        assert!(meta_path.exists());
        assert!(meta_path.file_name().unwrap().as_encoded_bytes().len() <= PLATFORM_SAFE_NAME_MAX);
    }

    #[tokio::test]
    async fn invalid_local_name_fails_before_creating_transaction_artifacts() {
        let (backend, tmp) = test_file_backend();
        let error = backend
            .upload_file(
                "default",
                upload_request(&"x".repeat(256), b"bytes", "marker"),
                None,
            )
            .await
            .expect_err("overlong logical key must fail");
        assert!(matches!(error, BackendError::InvalidArgument(_)));
        assert!(!tmp
            .path()
            .join("vaults/default/files/.transactions")
            .exists());
    }

    #[tokio::test]
    async fn fresh_directory_links_are_durable_before_first_active_rename() {
        let _event_guard = file_event_test_lock().lock().await;
        let (backend, tmp) = test_file_backend();
        let fdir = tmp.path().join("vaults/default/files");
        fs::remove_dir(&fdir).unwrap();
        take_file_events();

        backend
            .upload_file(
                "default",
                upload_request("durable.txt", b"bytes", "marker"),
                None,
            )
            .await
            .unwrap();
        let events: Vec<_> = take_file_events()
            .into_iter()
            .filter(|(_, path)| path.starts_with(tmp.path()))
            .collect();
        let active = events
            .iter()
            .position(|(kind, _)| kind == "active-rename")
            .expect("active rename event");
        let transaction_root = fdir.join(".transactions");
        for required in [
            tmp.path().join("vaults/default"),
            fdir.clone(),
            transaction_root,
        ] {
            assert!(
                events[..active]
                    .iter()
                    .any(|(kind, path)| kind == "sync-dir" && path == &required),
                "{} must be synced before active mutation: {events:?}",
                required.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_transaction_directories_are_repaired_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let (backend, _tmp) = test_file_backend();
        let paths =
            FileTransactionPaths::new(&backend.store_path, "default", "repair.txt").unwrap();
        ensure_private_real_directory(paths.dir.parent().unwrap()).unwrap();
        ensure_private_real_directory(&paths.dir).unwrap();
        fs::set_permissions(
            paths.dir.parent().unwrap(),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_real_directory(paths.dir.parent().unwrap()).unwrap();
        ensure_private_real_directory(&paths.dir).unwrap();
        assert_eq!(
            fs::metadata(paths.dir.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&paths.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn download_nonexistent_file_returns_not_found() {
        let (backend, _tmp) = test_file_backend();

        let result = backend
            .download_file("default", "nonexistent.txt", None)
            .await;
        assert!(matches!(result, Err(BackendError::NotFound { .. })));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn meta_json_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let (backend, tmp) = test_file_backend();

        let request = FileUploadRequest {
            name: "perm-test.bin".into(),
            content: b"secret data".to_vec(),
            content_type: None,
            groups: Vec::new(),
            metadata: HashMap::new(),
            tags: HashMap::new(),
        };
        backend.upload_file("default", request, None).await.unwrap();

        let files_dir = tmp.path().join("vaults").join("default").join("files");
        let meta_path = fs::read_dir(&files_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with(".meta.json"))
            .expect("meta.json not found after upload");

        let mode = fs::metadata(&meta_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "meta.json mode should be 0600, got {mode:o}");
    }

    #[tokio::test]
    async fn waiter_refreshes_transactions_created_after_its_pre_lock_snapshot() {
        let (backend, _tmp) = test_file_backend();
        let chain_a = ensure_writable_local_file_chain(&backend.store_path, "default").unwrap();
        let chain_b = existing_local_file_chain(&backend.store_path, "default")
            .unwrap()
            .unwrap();
        assert!(chain_a.files.open_dir(".transactions").unwrap().is_none());
        assert!(chain_b.files.open_dir(".transactions").unwrap().is_none());

        let lock_a = lock_local_file_chain(&backend.store_path, "default", &chain_a).unwrap();
        install_file_crash(&backend.store_path, 2);
        backend
            .upload_transaction_locked(
                &chain_a,
                upload_request("partial.txt", b"partial", "partial"),
                false,
            )
            .expect_err("first waiter should stop after half activation");
        drop(lock_a);

        let lock_b = lock_local_file_chain(&backend.store_path, "default", &chain_b).unwrap();
        backend.recover_all_locked(&chain_b).unwrap();
        assert!(!chain_b
            .files
            .file_exists(&format!("{}.age", storage_stem("partial.txt").unwrap()))
            .unwrap());
        assert!(chain_b
            .files
            .open_dir(".transactions")
            .unwrap()
            .unwrap()
            .entry_names()
            .unwrap()
            .is_empty());
        drop(lock_b);

        assert!(matches!(
            backend.download_file("default", "partial.txt", None).await,
            Err(BackendError::NotFound { .. })
        ));
        assert!(backend
            .list_files(
                "default",
                FileListRequest {
                    prefix: None,
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap()
            .iter()
            .all(|file| file.name != "partial.txt"));
        assert!(matches!(
            backend.delete_file("default", "partial.txt").await,
            Err(BackendError::NotFound { .. })
        ));
        backend
            .upload_file(
                "default",
                upload_request("different.txt", b"complete", "complete"),
                None,
            )
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_store_rejects_user_controlled_intermediate_symlink() {
        let tmp = TempDir::new().unwrap();
        let external = tmp.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"untouched").unwrap();
        let link = tmp.path().join("user-link");
        std::os::unix::fs::symlink(&external, &link).unwrap();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();
        let before = tree_snapshot(&external);
        let backend = LocalFileBackend::new(link.join("store"), identity, recipients);

        let result = backend
            .upload_file(
                "default",
                upload_request("proof.txt", b"proof", "proof"),
                None,
            )
            .await;
        assert!(matches!(result, Err(BackendError::Internal(message))
            if message.contains("configured-store symlink")));
        assert_eq!(tree_snapshot(&external), before);
    }

    #[cfg(unix)]
    #[test]
    fn retained_parent_symlink_inspection_rejects_an_inode_swap() {
        let tmp = TempDir::new().unwrap();
        let first = tmp.path().join("first");
        let replacement = tmp.path().join("replacement");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(first.join("sentinel"), b"first").unwrap();
        fs::write(replacement.join("sentinel"), b"replacement").unwrap();
        let first_before = tree_snapshot(&first);
        let replacement_before = tree_snapshot(&replacement);
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let parent = AnchoredDir::open_path(tmp.path()).unwrap();
        install_configured_link_swap(&link, &replacement);

        let error = parent
            .read_stable_symlink("link")
            .expect_err("a replaced symlink inode must fail closed");
        assert!(matches!(error, BackendError::Internal(message)
                if message.contains("changed during inspection")));
        assert_eq!(tree_snapshot(&first), first_before);
        assert_eq!(tree_snapshot(&replacement), replacement_before);
    }

    #[tokio::test]
    async fn large_file_replacement_streams_ciphertext_and_backups_in_bounded_chunks() {
        let _guard = file_event_test_lock().lock().await;
        let (backend, _tmp) = test_file_backend();
        let old = vec![0x31; 4 * 1024 * 1024];
        let new = vec![0xa7; 4 * 1024 * 1024];
        backend
            .upload_file(
                "default",
                FileUploadRequest {
                    name: "large.bin".into(),
                    content: old,
                    content_type: None,
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                },
                None,
            )
            .await
            .unwrap();

        reset_transfer_stats();
        backend
            .upload_file(
                "default",
                FileUploadRequest {
                    name: "large.bin".into(),
                    content: new.clone(),
                    content_type: None,
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            backend
                .download_file("default", "large.bin", None)
                .await
                .unwrap(),
            new
        );
        let stats = take_transfer_stats();
        assert!(stats.max_chunk > 0);
        assert!(stats.max_chunk <= 64 * 1024);
        assert!(
            stats.full_reads.iter().all(|name| !name.ends_with(".age")),
            "ciphertext must not be materialized by read_file: {:?}",
            stats.full_reads
        );
    }

    #[tokio::test]
    async fn large_download_transfers_the_zeroizing_plaintext_allocation_without_cloning() {
        let _guard = file_event_test_lock().lock().await;
        let (backend, _tmp) = test_file_backend();
        let bytes = vec![0x5c; 100 * 1024 * 1024];
        backend
            .upload_file(
                "default",
                FileUploadRequest {
                    name: "hundred-megabytes.bin".into(),
                    content: bytes,
                    content_type: None,
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                },
                None,
            )
            .await
            .unwrap();

        let downloaded = backend
            .download_file("default", "hundred-megabytes.bin", None)
            .await
            .unwrap();
        let decrypted = crypto::take_decrypted_allocation().expect("decrypted allocation");
        assert_eq!(downloaded.len(), 100 * 1024 * 1024);
        assert_eq!(downloaded.as_ptr() as usize, decrypted.0);
        assert_eq!(downloaded.capacity(), decrypted.1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attachment_validation_uses_retained_vault_generation_after_path_swap() {
        let (backend, tmp) = test_file_backend();
        let vault = tmp.path().join("vaults/default");
        let secrets = vault.join("secrets");
        fs::create_dir(&secrets).unwrap();
        let now = Utc::now();
        let metadata = super::super::secrets::SecretMeta {
            name: "source".into(),
            original_name: "source".into(),
            content_type: "text/plain".into(),
            enabled: true,
            created_at: now,
            updated_at: now,
            expires_on: None,
            not_before: None,
            tags: HashMap::new(),
            groups: Vec::new(),
            note: None,
            folder: None,
            version: "v1".into(),
            revision: "retained".into(),
        };
        fs::write(
            secrets.join("source.meta.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();

        let parked = tmp.path().join("parked-vault");
        let external = tmp.path().join("external-vault");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("sentinel"), b"untouched").unwrap();
        let before = tree_snapshot(&external);
        install_file_swap(
            &backend.store_path,
            "before-attachment-validation",
            &external,
            &parked,
        );

        backend
            .upload_file(
                "default",
                upload_request("attachments/source/proof.txt", b"proof", "proof"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(tree_snapshot(&external), before);
        fs::remove_file(&vault).unwrap();
        fs::rename(&parked, &vault).unwrap();
        assert_eq!(
            backend
                .download_file("default", "attachments/source/proof.txt", None)
                .await
                .unwrap(),
            b"proof"
        );
    }
}
