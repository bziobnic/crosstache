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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::error::BackendError;
use crate::backend::file::FileBackend;
use crate::blob::models::{FileInfo, FileListRequest, FileUploadRequest};
use crate::utils::helpers::{create_private_dir, write_private};
use crate::utils::progress::ProgressReporter;

use super::{crypto, paths};

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// URL-encode a file name for safe use as a filename component.
fn encode_name(name: &str) -> String {
    url::form_urlencoded::byte_serialize(name.as_bytes()).collect()
}

const PLATFORM_SAFE_NAME_MAX: usize = 255;
const LONGEST_ACTIVE_SUFFIX_BYTES: usize = ".meta.json".len();

fn validate_logical_file_name(name: &str) -> Result<(), BackendError> {
    if name.is_empty() || name.len() > PLATFORM_SAFE_NAME_MAX {
        return Err(BackendError::InvalidArgument(
            "local file key must contain 1 to 255 UTF-8 bytes".into(),
        ));
    }
    Ok(())
}

fn storage_stem(name: &str) -> Result<String, BackendError> {
    validate_logical_file_name(name)?;
    let encoded = encode_name(name);
    if encoded.len() + LONGEST_ACTIVE_SUFFIX_BYTES <= PLATFORM_SAFE_NAME_MAX {
        return Ok(encoded);
    }
    let digest = Sha256::digest(name.as_bytes());
    Ok(format!("h-{digest:x}"))
}

fn files_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::files_dir(store_path, vault)
}

fn file_age_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = storage_stem(name)?;
    Ok(files_dir(store_path, vault)?.join(format!("{enc}.age")))
}

fn file_meta_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = storage_stem(name)?;
    Ok(files_dir(store_path, vault)?.join(format!("{enc}.meta.json")))
}

// ---------------------------------------------------------------------------
// Metadata persisted alongside each file
// ---------------------------------------------------------------------------

fn read_file_meta(path: &Path) -> Result<FileInfo, BackendError> {
    let data = fs::read_to_string(path)
        .map_err(|e| BackendError::Internal(format!("read file meta {}: {e}", path.display())))?;
    serde_json::from_str(&data)
        .map_err(|e| BackendError::Internal(format!("parse file meta {}: {e}", path.display())))
}

fn write_file_meta(path: &Path, info: &FileInfo) -> Result<(), BackendError> {
    let json = serde_json::to_string_pretty(info)
        .map_err(|e| BackendError::Internal(format!("serialize file meta: {e}")))?;
    write_private(path, json.as_bytes())
        .map_err(|e| BackendError::Internal(format!("write file meta {}: {e}", path.display())))
}

fn sync_file(path: &Path) -> Result<(), BackendError> {
    fs::OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| BackendError::Internal(format!("sync file {}: {e}", path.display())))
}

fn sync_directory(path: &Path) -> Result<(), BackendError> {
    #[cfg(unix)]
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| BackendError::Internal(format!("sync directory {}: {e}", path.display())))?;
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(test)]
    tests::record_file_event("sync-dir", path);
    Ok(())
}

fn ensure_private_real_directory(path: &Path) -> Result<(), BackendError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(BackendError::Internal(format!(
                    "refusing unsafe file transaction directory {}",
                    path.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};

                // SAFETY: `geteuid` takes no arguments and has no preconditions.
                if metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(BackendError::Internal(
                        "file transaction directory is not owned by the current user".into(),
                    ));
                }
                if metadata.permissions().mode() & 0o777 != 0o700 {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(
                        |error| {
                            BackendError::Internal(format!(
                                "repair file transaction directory permissions: {error}"
                            ))
                        },
                    )?;
                    sync_directory(path)?;
                    if let Some(parent) = path.parent() {
                        sync_directory(parent)?;
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                BackendError::Internal("file transaction directory has no parent".into())
            })?;
            ensure_existing_real_directory(parent)?
                .then_some(())
                .ok_or_else(|| {
                    BackendError::Internal("file transaction parent does not exist".into())
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(path).map_err(|error| {
                    BackendError::Internal(format!("create file transaction directory: {error}"))
                })?;
            }
            #[cfg(not(unix))]
            fs::create_dir(path).map_err(|error| {
                BackendError::Internal(format!("create file transaction directory: {error}"))
            })?;
            sync_directory(path)?;
            sync_directory(parent)?;
        }
        Err(error) => {
            return Err(BackendError::Internal(format!(
                "inspect file transaction directory: {error}"
            )))
        }
    }
    Ok(())
}

fn ensure_existing_real_directory(path: &Path) -> Result<bool, BackendError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(BackendError::Internal(format!(
                "inspect local file directory: {error}"
            )))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(BackendError::Internal(
            "refusing unsafe local file directory".into(),
        ));
    }
    Ok(true)
}

fn ensure_regular_no_symlink(path: &Path) -> Result<(), BackendError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| BackendError::Internal(format!("inspect file transaction artifact: {e}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackendError::Internal(
            "refusing unsafe file transaction artifact".into(),
        ));
    }
    Ok(())
}

fn durable_private_copy(source: &Path, destination: &Path) -> Result<(), BackendError> {
    ensure_regular_no_symlink(source)?;
    let bytes = fs::read(source)
        .map_err(|e| BackendError::Internal(format!("read file transaction backup: {e}")))?;
    write_private(destination, bytes)
        .map_err(|e| BackendError::Internal(format!("write file transaction backup: {e}")))?;
    sync_file(destination)
}

#[derive(Debug, Serialize, Deserialize)]
struct FileUploadJournal {
    version: u8,
    had_age: bool,
    had_meta: bool,
}

struct FileTransactionPaths {
    dir: PathBuf,
    staged_age: PathBuf,
    staged_meta: PathBuf,
    old_age: PathBuf,
    old_meta: PathBuf,
    journal_temp: PathBuf,
    journal: PathBuf,
    active_age: PathBuf,
    active_meta: PathBuf,
}

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
            old_age: dir.join("old.age"),
            old_meta: dir.join("old.meta.json"),
            journal_temp: dir.join("journal.tmp"),
            journal: dir.join("journal.json"),
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

    #[cfg(test)]
    fn file_crash(&self, stage: u8) -> Result<(), BackendError> {
        tests::run_file_crash_hook(&self.store_path, stage)
    }

    #[cfg(not(test))]
    fn file_crash(&self, _stage: u8) -> Result<(), BackendError> {
        Ok(())
    }

    fn remove_transaction_dir(paths: &FileTransactionPaths) -> Result<(), BackendError> {
        if !paths.dir.exists() {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(&paths.dir).map_err(|e| {
            BackendError::Internal(format!("inspect file transaction directory: {e}"))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(BackendError::Internal(
                "refusing unsafe file transaction cleanup".into(),
            ));
        }
        for entry in fs::read_dir(&paths.dir)
            .map_err(|e| BackendError::Internal(format!("read file transaction: {e}")))?
        {
            let entry =
                entry.map_err(|e| BackendError::Internal(format!("read file transaction: {e}")))?;
            let metadata = entry.metadata().map_err(|e| {
                BackendError::Internal(format!("inspect file transaction artifact: {e}"))
            })?;
            if !metadata.is_file() || entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                return Err(BackendError::Internal(
                    "refusing unsafe file transaction artifact".into(),
                ));
            }
            fs::remove_file(entry.path()).map_err(|e| {
                BackendError::Internal(format!("remove file transaction artifact: {e}"))
            })?;
        }
        fs::remove_dir(&paths.dir)
            .map_err(|e| BackendError::Internal(format!("remove file transaction: {e}")))?;
        if let Some(root) = paths.dir.parent() {
            sync_directory(root)?;
        }
        Ok(())
    }

    fn restore_path(backup: &Path, active: &Path, existed: bool) -> Result<(), BackendError> {
        if existed {
            let temp = active.with_extension(format!(
                "{}.recovering",
                active
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("tmp")
            ));
            durable_private_copy(backup, &temp)?;
            fs::rename(&temp, active)
                .map_err(|e| BackendError::Internal(format!("restore local file: {e}")))?;
        } else if active.exists() {
            fs::remove_file(active)
                .map_err(|e| BackendError::Internal(format!("remove partial local file: {e}")))?;
        }
        Ok(())
    }

    fn recover_transaction_locked(&self, paths: &FileTransactionPaths) -> Result<(), BackendError> {
        if !paths.dir.exists() {
            return Ok(());
        }
        ensure_private_real_directory(&paths.dir)?;
        if !paths.journal.exists() {
            return Self::remove_transaction_dir(paths);
        }
        ensure_regular_no_symlink(&paths.journal)?;
        let journal: FileUploadJournal = serde_json::from_slice(
            &fs::read(&paths.journal)
                .map_err(|e| BackendError::Internal(format!("read file upload journal: {e}")))?,
        )
        .map_err(|e| BackendError::Internal(format!("parse file upload journal: {e}")))?;
        if journal.version != 1
            || (journal.had_age && !paths.old_age.exists())
            || (journal.had_meta && !paths.old_meta.exists())
        {
            return Err(BackendError::Internal(
                "invalid local file upload journal".into(),
            ));
        }
        if journal.had_age {
            ensure_regular_no_symlink(&paths.old_age)?;
        }
        if journal.had_meta {
            ensure_regular_no_symlink(&paths.old_meta)?;
        }
        Self::restore_path(&paths.old_age, &paths.active_age, journal.had_age)?;
        Self::restore_path(&paths.old_meta, &paths.active_meta, journal.had_meta)?;
        sync_directory(
            paths
                .active_age
                .parent()
                .ok_or_else(|| BackendError::Internal("file path has no parent".into()))?,
        )?;
        fs::remove_file(&paths.journal)
            .map_err(|e| BackendError::Internal(format!("remove file upload journal: {e}")))?;
        sync_directory(&paths.dir)?;
        Self::remove_transaction_dir(paths)
    }

    fn recover_all_locked(&self, vault: &str) -> Result<(), BackendError> {
        let fdir = files_dir(&self.store_path, vault)?;
        if !ensure_existing_real_directory(&fdir)? {
            return Ok(());
        }
        let root = fdir.join(".transactions");
        if !root.exists() {
            return Ok(());
        }
        ensure_private_real_directory(&root)?;
        let mut stems = Vec::new();
        for entry in fs::read_dir(&root)
            .map_err(|e| BackendError::Internal(format!("read file transactions: {e}")))?
        {
            let entry =
                entry.map_err(|e| BackendError::Internal(format!("read file transaction: {e}")))?;
            let kind = entry.file_type().map_err(|e| {
                BackendError::Internal(format!("inspect file transaction entry: {e}"))
            })?;
            if kind.is_symlink() || !kind.is_dir() {
                return Err(BackendError::Internal(
                    "refusing unsafe file transaction entry".into(),
                ));
            }
            stems.push(entry.file_name().to_string_lossy().into_owned());
        }
        stems.sort();
        for stem in stems {
            let paths = FileTransactionPaths::from_stem(&fdir, &stem)?;
            self.recover_transaction_locked(&paths)?;
        }
        Ok(())
    }

    fn upload_transaction_locked(
        &self,
        vault: &str,
        request: FileUploadRequest,
        create_only: bool,
    ) -> Result<FileInfo, BackendError> {
        let fdir = files_dir(&self.store_path, vault)?;
        ensure_private_real_directory(&fdir)?;

        self.recover_all_locked(vault)?;
        let original_size = request.content.len() as u64;
        let paths = FileTransactionPaths::new(&self.store_path, vault, &request.name)?;
        let had_age = paths.active_age.exists();
        let had_meta = paths.active_meta.exists();
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
        let transaction_root = paths
            .dir
            .parent()
            .ok_or_else(|| BackendError::Internal("transaction path has no parent".into()))?;
        ensure_private_real_directory(transaction_root)?;
        if paths.dir.exists() {
            self.recover_transaction_locked(&paths)?;
        }
        ensure_private_real_directory(&paths.dir)?;

        let staged = (|| {
            crypto::encrypt_to_file(&paths.staged_age, &request.content, &self.recipients)?;
            sync_file(&paths.staged_age)?;
            write_file_meta(&paths.staged_meta, &info)?;
            sync_file(&paths.staged_meta)?;
            if had_age {
                durable_private_copy(&paths.active_age, &paths.old_age)?;
                durable_private_copy(&paths.active_meta, &paths.old_meta)?;
            }
            sync_directory(&paths.dir)
        })();
        if let Err(error) = staged {
            let _ = Self::remove_transaction_dir(&paths);
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
            write_private(&paths.journal_temp, journal_bytes)
                .map_err(|e| BackendError::Internal(format!("write file upload journal: {e}")))?;
            sync_file(&paths.journal_temp)?;
            fs::rename(&paths.journal_temp, &paths.journal)
                .map_err(|e| BackendError::Internal(format!("publish file upload journal: {e}")))?;
            sync_directory(&paths.dir)
        })();
        if let Err(error) = published {
            if paths.journal.exists() {
                self.recover_transaction_locked(&paths)?;
            } else {
                Self::remove_transaction_dir(&paths)?;
            }
            return Err(error);
        }
        self.file_crash(1)?;

        let activated = (|| {
            #[cfg(test)]
            tests::record_file_event("active-rename", &paths.active_age);
            fs::rename(&paths.staged_age, &paths.active_age)
                .map_err(|e| BackendError::Internal(format!("activate file ciphertext: {e}")))?;
            sync_directory(&fdir)?;
            Ok::<(), BackendError>(())
        })();
        if let Err(error) = activated {
            self.recover_transaction_locked(&paths)?;
            return Err(error);
        }
        self.file_crash(2)?;

        let activated = (|| {
            #[cfg(test)]
            tests::record_file_event("active-rename", &paths.active_meta);
            fs::rename(&paths.staged_meta, &paths.active_meta)
                .map_err(|e| BackendError::Internal(format!("activate file metadata: {e}")))?;
            sync_directory(&fdir)?;
            Ok::<(), BackendError>(())
        })();
        if let Err(error) = activated {
            self.recover_transaction_locked(&paths)?;
            return Err(error);
        }
        self.file_crash(3)?;

        fs::remove_file(&paths.journal)
            .map_err(|e| BackendError::Internal(format!("commit file upload: {e}")))?;
        sync_directory(&paths.dir)?;
        sync_directory(&fdir)?;
        self.file_crash(4)?;
        Self::remove_transaction_dir(&paths)?;
        Ok(info)
    }

    fn upload_with_policy(
        &self,
        vault: &str,
        request: FileUploadRequest,
        create_only: bool,
    ) -> Result<FileInfo, BackendError> {
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.exists() {
            create_private_dir(&vault_dir)
                .map_err(|e| BackendError::Internal(format!("mkdir vault files root: {e}")))?;
        }
        let _lock = super::secrets::lock_vault(&vault_dir)?;
        self.recover_all_locked(vault)?;
        if let Some(secret_name) = Self::attachment_secret_name(&request.name) {
            if !super::secrets::active_secret_exists_by_metadata_locked(
                &self.store_path,
                &self.identity,
                vault,
                secret_name,
            )? {
                return Err(BackendError::NotFound {
                    name: secret_name.to_string(),
                    suggestion: None,
                });
            }
        }
        self.upload_transaction_locked(vault, request, create_only)
    }
}

#[async_trait]
impl FileBackend for LocalFileBackend {
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
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }
        let _lock = super::secrets::lock_vault(&vault_dir)?;
        self.recover_all_locked(vault)?;
        let ap = file_age_path(&self.store_path, vault, name)?;
        if !ap.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        crypto::decrypt_bytes_from_file(&ap, &self.identity)
    }

    async fn list_files(
        &self,
        vault: &str,
        request: FileListRequest,
    ) -> Result<Vec<FileInfo>, BackendError> {
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.exists() {
            return Ok(Vec::new());
        }
        let _lock = super::secrets::lock_vault(&vault_dir)?;
        self.recover_all_locked(vault)?;
        let fdir = files_dir(&self.store_path, vault)?;
        if !fdir.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let entries = fs::read_dir(&fdir)
            .map_err(|e| BackendError::Internal(format!("read files dir: {e}")))?;

        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !fname.ends_with(".meta.json") {
                continue;
            }

            let info = match read_file_meta(&entry.path()) {
                Ok(i) => i,
                Err(_) => continue,
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

    async fn delete_file(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        self.validate_file_name(name)?;
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }
        let _lock = super::secrets::lock_vault(&vault_dir)?;
        self.recover_all_locked(vault)?;
        let ap = file_age_path(&self.store_path, vault, name)?;
        let mp = file_meta_path(&self.store_path, vault, name)?;

        if !ap.exists() && !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        if ap.exists() {
            fs::remove_file(&ap)
                .map_err(|e| BackendError::Internal(format!("remove file age: {e}")))?;
        }
        if mp.exists() {
            fs::remove_file(&mp)
                .map_err(|e| BackendError::Internal(format!("remove file meta: {e}")))?;
        }

        Ok(())
    }

    async fn get_file_info(&self, vault: &str, name: &str) -> Result<FileInfo, BackendError> {
        self.validate_file_name(name)?;
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }
        let _lock = super::secrets::lock_vault(&vault_dir)?;
        self.recover_all_locked(vault)?;
        let mp = file_meta_path(&self.store_path, vault, name)?;
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        read_file_meta(&mp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::local::crypto::generate_keypair;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn file_crash_hooks() -> &'static Mutex<HashMap<PathBuf, u8>> {
        static HOOKS: OnceLock<Mutex<HashMap<PathBuf, u8>>> = OnceLock::new();
        HOOKS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn file_events() -> &'static Mutex<Vec<(String, PathBuf)>> {
        static EVENTS: OnceLock<Mutex<Vec<(String, PathBuf)>>> = OnceLock::new();
        EVENTS.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub(super) fn record_file_event(kind: &str, path: &Path) {
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
        let events = take_file_events();
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
}
