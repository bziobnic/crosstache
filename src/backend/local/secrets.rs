//! Local secret backend — file-based encrypted secret CRUD.
//!
//! Each secret is stored as two files inside
//! `<store>/vaults/<vault>/secrets/`:
//!
//! - `<encoded_name>.age`       — age-encrypted secret value
//! - `<encoded_name>.meta.json` — plaintext metadata
//!
//! Versions are archived under `.versions/<encoded_name>/v<N>.*`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

use crate::utils::helpers::{create_private_dir, write_private};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::secret::manager::{SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest};

use super::{crypto, paths};

// ---------------------------------------------------------------------------
// Metadata persisted alongside each secret
// ---------------------------------------------------------------------------

/// On-disk metadata for a secret (`.meta.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMeta {
    pub name: String,
    pub original_name: String,
    pub content_type: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_on: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<DateTime<Utc>>,
    #[serde(default)]
    pub tags: HashMap<String, String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Current version label, e.g. `"v1"`.
    pub version: String,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// URL-encode a secret name for safe use as a filename component.
fn encode_name(name: &str) -> String {
    // Percent-encode everything except unreserved characters per RFC 3986.
    url::form_urlencoded::byte_serialize(name.as_bytes()).collect()
}

/// Decode a URL-encoded filename back to the original secret name.
#[cfg(test)]
fn decode_name(encoded: &str) -> String {
    url::form_urlencoded::parse(encoded.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect()
}

fn secrets_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::secrets_dir(store_path, vault)
}

fn age_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = encode_name(name);
    Ok(secrets_dir(store_path, vault)?.join(format!("{enc}.age")))
}

fn meta_path(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = encode_name(name);
    Ok(secrets_dir(store_path, vault)?.join(format!("{enc}.meta.json")))
}

fn versions_dir(store_path: &Path, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
    let enc = encode_name(name);
    Ok(secrets_dir(store_path, vault)?.join(".versions").join(enc))
}

fn trash_base_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::trash_base_dir(store_path, vault)
}

/// Directory for a single trash entry, suffixed with the deletion timestamp so
/// repeated delete/recreate/delete cycles never collide. `@` cannot appear in a
/// percent-encoded name (it encodes as `%40`), so the suffix is unambiguous.
fn trash_entry_dir(
    store_path: &Path,
    vault: &str,
    name: &str,
    deleted_at_millis: u128,
) -> Result<PathBuf, BackendError> {
    let enc = encode_name(name);
    Ok(trash_base_dir(store_path, vault)?.join(format!("{enc}@{deleted_at_millis}")))
}

/// All trash entries for a secret, as `(deleted_at_millis, dir)` pairs.
///
/// Handles both naming formats: legacy un-suffixed dirs (`<enc>`, written by
/// older versions, treated as timestamp 0) and suffixed dirs (`<enc>@<millis>`).
fn trash_entries_for(
    store_path: &Path,
    vault: &str,
    name: &str,
) -> Result<Vec<(u128, PathBuf)>, BackendError> {
    let tbase = trash_base_dir(store_path, vault)?;
    let mut entries = Vec::new();
    if !tbase.exists() {
        return Ok(entries);
    }

    let enc = encode_name(name);
    let prefix = format!("{enc}@");
    let dir_entries =
        fs::read_dir(&tbase).map_err(|e| BackendError::Internal(format!("read trash dir: {e}")))?;
    for entry in dir_entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname == enc {
            entries.push((0, entry.path()));
        } else if let Some(ts) = fname
            .strip_prefix(&prefix)
            .and_then(|rest| rest.parse::<u128>().ok())
        {
            entries.push((ts, entry.path()));
        }
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn meta_to_properties(meta: &SecretMeta, value: Option<Zeroizing<String>>) -> SecretProperties {
    let version_num = meta
        .version
        .strip_prefix('v')
        .and_then(|s| s.parse::<u32>().ok());
    let mut tags = meta.tags.clone();
    if !meta.groups.is_empty() {
        tags.insert("groups".to_string(), meta.groups.join(","));
    }
    if let Some(note) = meta.note.as_ref().filter(|n| !n.is_empty()) {
        tags.insert("note".to_string(), note.clone());
    }
    if let Some(folder) = meta.folder.as_ref().filter(|f| !f.is_empty()) {
        tags.insert("folder".to_string(), folder.clone());
    }

    SecretProperties {
        name: meta.name.clone(),
        original_name: meta.original_name.clone(),
        value,
        version: meta.version.clone(),
        version_number: version_num,
        created_timestamp: meta.created_at.timestamp(),
        created_on: meta.created_at.format("%Y-%m-%d %H:%M").to_string(),
        updated_on: meta.updated_at.format("%Y-%m-%d %H:%M").to_string(),
        enabled: meta.enabled,
        expires_on: meta.expires_on,
        not_before: meta.not_before,
        tags,
        content_type: meta.content_type.clone(),
        recovery_level: None,
    }
}

fn meta_to_summary(meta: &SecretMeta) -> SecretSummary {
    let groups_str = if meta.groups.is_empty() {
        None
    } else {
        Some(meta.groups.join(", "))
    };

    SecretSummary {
        name: meta.name.clone(),
        original_name: meta.original_name.clone(),
        note: meta.note.clone(),
        folder: meta.folder.clone(),
        groups: groups_str,
        updated_on: meta.updated_at.format("%Y-%m-%d %H:%M").to_string(),
        enabled: meta.enabled,
        content_type: meta.content_type.clone(),
    }
}

fn read_meta(path: &Path) -> Result<SecretMeta, BackendError> {
    let data = fs::read_to_string(path)
        .map_err(|e| BackendError::Internal(format!("read meta {}: {e}", path.display())))?;
    serde_json::from_str(&data)
        .map_err(|e| BackendError::Internal(format!("parse meta {}: {e}", path.display())))
}

fn write_meta(path: &Path, meta: &SecretMeta) -> Result<(), BackendError> {
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| BackendError::Internal(format!("serialize meta: {e}")))?;
    write_private(path, json.as_bytes())
        .map_err(|e| BackendError::Internal(format!("write meta {}: {e}", path.display())))
}

fn temp_path_for(path: &Path) -> Result<PathBuf, BackendError> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| BackendError::Internal(format!("clock error: {e}")))?
        .as_nanos();
    let pid = std::process::id();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| BackendError::Internal(format!("invalid file name: {}", path.display())))?;
    Ok(path.with_file_name(format!(".{name}.tmp.{pid}.{ts}")))
}

fn archive_snapshot(
    store_path: &Path,
    vault: &str,
    name: &str,
    version: &str,
    age_bytes: &[u8],
    meta: &SecretMeta,
) -> Result<(), BackendError> {
    let vdir = versions_dir(store_path, vault, name)?;
    fs::create_dir_all(&vdir)
        .map_err(|e| BackendError::Internal(format!("mkdir versions: {e}")))?;

    let age_dest = vdir.join(format!("{version}.age"));
    let meta_dest = vdir.join(format!("{version}.meta.json"));

    write_private(&age_dest, age_bytes)
        .map_err(|e| BackendError::Internal(format!("archive age: {e}")))?;
    write_meta(&meta_dest, meta)
        .map_err(|e| BackendError::Internal(format!("archive meta: {e}")))?;

    Ok(())
}

/// Determine the next version number by scanning `.versions/<name>/`.
fn next_version(store_path: &Path, vault: &str, name: &str) -> Result<u32, BackendError> {
    let vdir = versions_dir(store_path, vault, name)?;
    if !vdir.exists() {
        return Ok(1);
    }
    let mut max: u32 = 0;
    if let Ok(entries) = fs::read_dir(&vdir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if let Some(rest) = fname.strip_prefix('v') {
                if let Some(num_str) = rest.split('.').next() {
                    if let Ok(n) = num_str.parse::<u32>() {
                        max = max.max(n);
                    } else {
                        eprintln!(
                            "warning: ignoring non-numeric entry {:?} in versions directory",
                            fname
                        );
                    }
                }
            } else {
                eprintln!(
                    "warning: ignoring non-numeric entry {:?} in versions directory",
                    fname
                );
            }
        }
    }
    Ok(max + 1)
}

/// Archive the current secret to `.versions/<name>/v<N>.*`.
fn archive_current(store_path: &Path, vault: &str, name: &str) -> Result<u32, BackendError> {
    let ap = age_path(store_path, vault, name)?;
    let mp = meta_path(store_path, vault, name)?;

    if !ap.exists() && !mp.exists() {
        return Ok(1);
    }

    let ver = next_version(store_path, vault, name)?;
    let vdir = versions_dir(store_path, vault, name)?;
    fs::create_dir_all(&vdir)
        .map_err(|e| BackendError::Internal(format!("mkdir versions: {e}")))?;

    if ap.exists() {
        let dest = vdir.join(format!("v{ver}.age"));
        fs::rename(&ap, &dest).map_err(|e| BackendError::Internal(format!("archive age: {e}")))?;
    }
    if mp.exists() {
        let dest = vdir.join(format!("v{ver}.meta.json"));
        fs::rename(&mp, &dest).map_err(|e| BackendError::Internal(format!("archive meta: {e}")))?;
    }

    Ok(ver)
}

/// Acquire an exclusive file lock on the vault directory to prevent concurrent mutations.
fn lock_vault(vault_dir: &Path) -> Result<fs::File, BackendError> {
    let lock_path = vault_dir.join(".lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| BackendError::Internal(format!("open lock: {e}")))?;
    lock_file
        .lock_exclusive()
        .map_err(|e| BackendError::Internal(format!("vault locked by another process: {e}")))?;
    Ok(lock_file)
}

// ---------------------------------------------------------------------------
// LocalSecretBackend
// ---------------------------------------------------------------------------

/// File-backed secret operations using age encryption.
pub struct LocalSecretBackend {
    store_path: PathBuf,
    identity: age::x25519::Identity,
    recipients: Vec<age::x25519::Recipient>,
}

impl LocalSecretBackend {
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

    /// Soft-delete `name` into a trash entry stamped with `deleted_at_millis`.
    ///
    /// Rejects with [`BackendError::Conflict`] if an entry with the same name
    /// and timestamp already exists, rather than overwriting trashed material.
    fn delete_secret_at(
        &self,
        vault: &str,
        name: &str,
        deleted_at_millis: u128,
    ) -> Result<(), BackendError> {
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.join(".vault.json").exists() {
            return Err(BackendError::VaultNotFound {
                name: vault.to_string(),
                suggestion: None,
            });
        }
        let _lock = lock_vault(&vault_dir)?;

        let mp = meta_path(&self.store_path, vault, name)?;
        let ap = age_path(&self.store_path, vault, name)?;

        if !mp.exists() && !ap.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        // Soft delete: move files to .trash/{encoded_name}@{deleted_at_millis}/
        let tdir = trash_entry_dir(&self.store_path, vault, name, deleted_at_millis)?;
        if tdir.exists() {
            return Err(BackendError::Conflict(format!(
                "trash entry for '{name}' at timestamp {deleted_at_millis} already exists; \
                 refusing to overwrite previously deleted secret"
            )));
        }
        fs::create_dir_all(&tdir)
            .map_err(|e| BackendError::Internal(format!("mkdir trash: {e}")))?;

        let enc = encode_name(name);
        if ap.exists() {
            let dest = tdir.join(format!("{enc}.age"));
            fs::rename(&ap, &dest)
                .map_err(|e| BackendError::Internal(format!("move age to trash: {e}")))?;
        }
        if mp.exists() {
            let dest = tdir.join(format!("{enc}.meta.json"));
            fs::rename(&mp, &dest)
                .map_err(|e| BackendError::Internal(format!("move meta to trash: {e}")))?;
        }

        // Write deletion metadata
        let deleted_meta = serde_json::json!({
            "deleted_at": Utc::now().to_rfc3339(),
            "original_name": name,
        });
        let deleted_path = tdir.join(".deleted.json");
        write_private(
            &deleted_path,
            serde_json::to_string_pretty(&deleted_meta)
                .map_err(|e| BackendError::Internal(format!("serialize deleted meta: {e}")))?
                .as_bytes(),
        )
        .map_err(|e| BackendError::Internal(format!("write deleted meta: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl SecretBackend for LocalSecretBackend {
    async fn set_secret(
        &self,
        vault: &str,
        request: SecretRequest,
    ) -> Result<SecretProperties, BackendError> {
        let store = self.store_path.clone();
        let identity = self.identity.clone();
        let recipients = self.recipients.clone();

        // Validate vault exists before attempting to lock.
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        let vault_json = vault_dir.join(".vault.json");
        if !vault_json.exists() {
            return Err(BackendError::VaultNotFound {
                name: vault.to_string(),
                suggestion: None,
            });
        }

        // Acquire exclusive vault lock for the duration of the mutation.
        let _lock = lock_vault(&vault_dir)?;

        let sdir = secrets_dir(&store, vault)?;
        create_private_dir(&sdir)
            .map_err(|e| BackendError::Internal(format!("mkdir secrets: {e}")))?;

        let name = request.name.clone();
        let ap = age_path(&store, vault, &name)?;
        let mp = meta_path(&store, vault, &name)?;

        // Snapshot old state for transactional replace+archive.
        let old_snapshot = if mp.exists() {
            let old_meta = read_meta(&mp)?;
            let old_age = fs::read(&ap).map_err(|e| {
                BackendError::Internal(format!("read existing age {}: {e}", ap.display()))
            })?;
            Some((old_meta, old_age))
        } else {
            None
        };

        let version = if let Some((old_meta, _)) = old_snapshot.as_ref() {
            let old_num: u32 = old_meta
                .version
                .strip_prefix('v')
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            format!("v{}", old_num + 1)
        } else {
            "v1".to_string()
        };

        let now = Utc::now();
        let meta = SecretMeta {
            name: name.clone(),
            original_name: name.clone(),
            content_type: request.content_type.clone().unwrap_or_default(),
            enabled: request.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
            expires_on: request.expires_on,
            not_before: request.not_before,
            tags: request.tags.clone().unwrap_or_default(),
            groups: request.groups.clone().unwrap_or_default(),
            note: request.note.clone(),
            folder: request.folder.clone(),
            version: version.clone(),
        };

        // Encrypt + write to temp files first, then atomically replace active.
        let _identity = identity;
        let ap_tmp = temp_path_for(&ap)?;
        let mp_tmp = temp_path_for(&mp)?;

        crypto::encrypt_to_file(&ap_tmp, request.value.as_bytes(), &recipients)?;
        write_meta(&mp_tmp, &meta)?;

        fs::rename(&ap_tmp, &ap)
            .map_err(|e| BackendError::Internal(format!("activate age {}: {e}", ap.display())))?;
        fs::rename(&mp_tmp, &mp)
            .map_err(|e| BackendError::Internal(format!("activate meta {}: {e}", mp.display())))?;

        // Archive old version only after replacement is durable.
        if let Some((old_meta, old_age)) = old_snapshot {
            archive_snapshot(&store, vault, &name, &old_meta.version, &old_age, &old_meta)?;
        }

        Ok(meta_to_properties(&meta, None))
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        let mp = meta_path(&self.store_path, vault, name)?;
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        // Reject symlinks to prevent attackers from redirecting reads to
        // arbitrary files on the filesystem.
        if fs::symlink_metadata(&mp)
            .map(|m| m.is_symlink())
            .unwrap_or(false)
        {
            return Err(BackendError::Internal(format!(
                "refusing to read metadata file: {} is a symlink",
                mp.display()
            )));
        }

        let meta = read_meta(&mp)?;

        let value = if include_value {
            let ap = age_path(&self.store_path, vault, name)?;

            if fs::symlink_metadata(&ap)
                .map(|m| m.is_symlink())
                .unwrap_or(false)
            {
                return Err(BackendError::Internal(format!(
                    "refusing to decrypt secret file: {} is a symlink",
                    ap.display()
                )));
            }

            Some(crypto::decrypt_from_file(&ap, &self.identity)?)
        } else {
            None
        };

        Ok(meta_to_properties(&meta, value))
    }

    async fn get_secret_version(
        &self,
        vault: &str,
        name: &str,
        version: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        // First check if this is the current version.
        let mp = meta_path(&self.store_path, vault, name)?;
        if mp.exists() {
            let meta = read_meta(&mp)?;
            if meta.version == version {
                return self.get_secret(vault, name, include_value).await;
            }
        }

        // Look in .versions/
        let vdir = versions_dir(&self.store_path, vault, name)?;
        let meta_file = vdir.join(format!("{version}.meta.json"));
        if !meta_file.exists() {
            return Err(BackendError::NotFound {
                name: format!("{name}@{version}"),
                suggestion: None,
            });
        }

        if fs::symlink_metadata(&meta_file)
            .map(|m| m.is_symlink())
            .unwrap_or(false)
        {
            return Err(BackendError::Internal(format!(
                "refusing to read metadata file: {} is a symlink",
                meta_file.display()
            )));
        }

        let meta = read_meta(&meta_file)?;

        let value = if include_value {
            let age_file = vdir.join(format!("{version}.age"));

            if fs::symlink_metadata(&age_file)
                .map(|m| m.is_symlink())
                .unwrap_or(false)
            {
                return Err(BackendError::Internal(format!(
                    "refusing to decrypt secret file: {} is a symlink",
                    age_file.display()
                )));
            }

            Some(crypto::decrypt_from_file(&age_file, &self.identity)?)
        } else {
            None
        };

        Ok(meta_to_properties(&meta, value))
    }

    async fn list_secrets(
        &self,
        vault: &str,
        group_filter: Option<&str>,
    ) -> Result<Vec<SecretSummary>, BackendError> {
        let sdir = secrets_dir(&self.store_path, vault)?;
        if !sdir.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let entries = fs::read_dir(&sdir)
            .map_err(|e| BackendError::Internal(format!("read secrets dir: {e}")))?;

        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !fname.ends_with(".meta.json") {
                continue;
            }

            let meta = match read_meta(&entry.path()) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "warning: secret {:?} exists but has corrupted metadata: {}",
                        fname, e
                    );
                    continue;
                }
            };

            // Apply group filter
            if let Some(group) = group_filter {
                if !meta.groups.iter().any(|g| g == group) {
                    continue;
                }
            }

            results.push(meta_to_summary(&meta));
        }

        // Sort by name for deterministic output
        results.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(results)
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        let deleted_at_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BackendError::Internal(format!("clock error: {e}")))?
            .as_millis();
        self.delete_secret_at(vault, name, deleted_at_millis)
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        // Acquire exclusive vault lock — update_secret may call archive_current/next_version.
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        let _lock = lock_vault(&vault_dir)?;

        let mp = meta_path(&self.store_path, vault, name)?;
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        let mut meta = read_meta(&mp)?;
        let now = Utc::now();

        // If value is being updated, replace active first, then archive prior snapshot.
        if let Some(ref new_value) = request.value {
            let ap = age_path(&self.store_path, vault, name)?;
            let old_age = fs::read(&ap).map_err(|e| {
                BackendError::Internal(format!("read existing age {}: {e}", ap.display()))
            })?;
            let old_meta = meta.clone();

            let old_num: u32 = meta
                .version
                .strip_prefix('v')
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            meta.version = format!("v{}", old_num + 1);

            let ap_tmp = temp_path_for(&ap)?;
            crypto::encrypt_to_file(&ap_tmp, new_value.as_bytes(), &self.recipients)?;
            fs::rename(&ap_tmp, &ap).map_err(|e| {
                BackendError::Internal(format!("activate age {}: {e}", ap.display()))
            })?;

            archive_snapshot(
                &self.store_path,
                vault,
                name,
                &old_meta.version,
                &old_age,
                &old_meta,
            )?;
        }

        // Merge or replace tags
        if let Some(new_tags) = &request.tags {
            if request.replace_tags {
                meta.tags = new_tags.clone();
            } else {
                for (k, v) in new_tags {
                    meta.tags.insert(k.clone(), v.clone());
                }
            }
        }

        // Merge or replace groups
        if let Some(new_groups) = &request.groups {
            if request.replace_groups {
                meta.groups = new_groups.clone();
            } else {
                for g in new_groups {
                    if !meta.groups.contains(g) {
                        meta.groups.push(g.clone());
                    }
                }
            }
        }

        if let Some(ct) = &request.content_type {
            meta.content_type = ct.clone();
        }
        if let Some(enabled) = request.enabled {
            meta.enabled = enabled;
        }
        if request.expires_on.is_some() {
            meta.expires_on = request.expires_on;
        }
        if request.not_before.is_some() {
            meta.not_before = request.not_before;
        }
        if request.note.is_some() {
            meta.note = request.note.clone();
        }
        if request.folder.is_some() {
            meta.folder = request.folder.clone();
        }

        meta.updated_at = now;
        write_meta(&mp, &meta)?;

        Ok(meta_to_properties(&meta, None))
    }

    // -----------------------------------------------------------------------
    // Optional operations
    // -----------------------------------------------------------------------

    async fn list_versions(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<SecretProperties>, BackendError> {
        let mut versions = Vec::new();

        // Collect archived versions
        let vdir = versions_dir(&self.store_path, vault, name)?;
        if vdir.exists() {
            if let Ok(entries) = fs::read_dir(&vdir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if fname.ends_with(".meta.json") {
                        if let Ok(meta) = read_meta(&entry.path()) {
                            versions.push(meta_to_properties(&meta, None));
                        }
                    }
                }
            }
        }

        // Add current version
        let mp = meta_path(&self.store_path, vault, name)?;
        if mp.exists() {
            let meta = read_meta(&mp)?;
            versions.push(meta_to_properties(&meta, None));
        }

        // Sort by version number
        versions.sort_by_key(|v| v.version_number.unwrap_or(0));

        Ok(versions)
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        let mp = meta_path(&self.store_path, vault, name)?;
        Ok(mp.exists())
    }

    async fn rollback(
        &self,
        vault: &str,
        name: &str,
        version: &str,
    ) -> Result<SecretProperties, BackendError> {
        // Acquire exclusive vault lock — rollback calls archive_current/next_version.
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        let _lock = lock_vault(&vault_dir)?;

        // Find the target version in .versions/
        let vdir = versions_dir(&self.store_path, vault, name)?;
        let ver_age = vdir.join(format!("{version}.age"));
        let ver_meta = vdir.join(format!("{version}.meta.json"));

        if !ver_meta.exists() {
            return Err(BackendError::NotFound {
                name: format!("{name}@{version}"),
                suggestion: None,
            });
        }

        // Archive current as the next version
        archive_current(&self.store_path, vault, name)?;

        // Copy the target version files to current
        let ap = age_path(&self.store_path, vault, name)?;
        let mp = meta_path(&self.store_path, vault, name)?;

        if ver_age.exists() {
            fs::copy(&ver_age, &ap)
                .map_err(|e| BackendError::Internal(format!("restore age: {e}")))?;
        }
        fs::copy(&ver_meta, &mp)
            .map_err(|e| BackendError::Internal(format!("restore meta: {e}")))?;

        // Update the version label to the next version number
        let mut meta = read_meta(&mp)?;
        let next_ver = next_version(&self.store_path, vault, name)?;
        meta.version = format!("v{next_ver}");
        meta.updated_at = Utc::now();
        write_meta(&mp, &meta)?;

        Ok(meta_to_properties(&meta, None))
    }

    async fn restore_secret(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<SecretProperties, BackendError> {
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.join(".vault.json").exists() {
            return Err(BackendError::VaultNotFound {
                name: vault.to_string(),
                suggestion: None,
            });
        }
        let _lock = lock_vault(&vault_dir)?;

        // Restore the most recent trash entry for this name (legacy un-suffixed
        // entries sort as oldest). Ties on timestamp break by path so the
        // choice is deterministic regardless of read_dir order.
        let mut entries = trash_entries_for(&self.store_path, vault, name)?;
        entries.sort();
        let Some((_, tdir)) = entries.pop() else {
            return Err(BackendError::NotFound {
                name: format!("{name} (deleted)"),
                suggestion: Some("Secret is not in the trash".into()),
            });
        };

        let enc = encode_name(name);
        let trash_age = tdir.join(format!("{enc}.age"));
        let trash_meta = tdir.join(format!("{enc}.meta.json"));

        if !trash_meta.exists() {
            return Err(BackendError::NotFound {
                name: format!("{name} (deleted)"),
                suggestion: Some("Trash metadata not found".into()),
            });
        }

        // Move files back to secrets/
        let sdir = secrets_dir(&self.store_path, vault)?;
        fs::create_dir_all(&sdir)
            .map_err(|e| BackendError::Internal(format!("mkdir secrets: {e}")))?;

        let ap = age_path(&self.store_path, vault, name)?;
        let mp = meta_path(&self.store_path, vault, name)?;

        // If an active secret with this name exists (delete → recreate →
        // restore), archive it to .versions/ instead of silently destroying
        // it, mirroring the rollback path.
        let had_active = ap.exists() || mp.exists();
        if had_active {
            archive_current(&self.store_path, vault, name)?;
        }

        if trash_age.exists() {
            fs::rename(&trash_age, &ap)
                .map_err(|e| BackendError::Internal(format!("restore age from trash: {e}")))?;
        }
        fs::rename(&trash_meta, &mp)
            .map_err(|e| BackendError::Internal(format!("restore meta from trash: {e}")))?;

        // Remove the trash entry
        fs::remove_dir_all(&tdir)
            .map_err(|e| BackendError::Internal(format!("remove trash dir: {e}")))?;

        let mut meta = read_meta(&mp)?;
        if had_active {
            // Relabel so the restored secret doesn't reuse a version label
            // already taken by the archived active secret.
            let next_ver = next_version(&self.store_path, vault, name)?;
            meta.version = format!("v{next_ver}");
            meta.updated_at = Utc::now();
            write_meta(&mp, &meta)?;
        }
        Ok(meta_to_properties(&meta, None))
    }

    async fn purge_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        let vault_dir = paths::vault_dir(&self.store_path, vault)?;
        if !vault_dir.join(".vault.json").exists() {
            return Err(BackendError::VaultNotFound {
                name: vault.to_string(),
                suggestion: None,
            });
        }
        let _lock = lock_vault(&vault_dir)?;

        // Permanently remove every trash entry for this name (both naming formats).
        for (_, tdir) in trash_entries_for(&self.store_path, vault, name)? {
            fs::remove_dir_all(&tdir)
                .map_err(|e| BackendError::Internal(format!("purge trash: {e}")))?;
        }

        // Also remove any .versions/ for that secret
        let vdir = versions_dir(&self.store_path, vault, name)?;
        if vdir.exists() {
            fs::remove_dir_all(&vdir)
                .map_err(|e| BackendError::Internal(format!("purge versions: {e}")))?;
        }

        Ok(())
    }

    async fn list_deleted_secrets(&self, vault: &str) -> Result<Vec<SecretSummary>, BackendError> {
        let tbase = trash_base_dir(&self.store_path, vault)?;
        if !tbase.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        let entries = fs::read_dir(&tbase)
            .map_err(|e| BackendError::Internal(format!("read trash dir: {e}")))?;

        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }

            // Look for .meta.json files in this trash entry
            let dir_path = entry.path();
            if let Ok(inner_entries) = fs::read_dir(&dir_path) {
                for inner in inner_entries.flatten() {
                    let fname = inner.file_name().to_string_lossy().to_string();
                    if fname.ends_with(".meta.json") {
                        if let Ok(meta) = read_meta(&inner.path()) {
                            results.push(meta_to_summary(&meta));
                        }
                    }
                }
            }
        }

        results.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::local::crypto::generate_keypair;
    use tempfile::TempDir;

    /// Create a test backend with a temp dir and return it along with the temp dir.
    fn test_backend() -> (LocalSecretBackend, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");

        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        // Create default vault
        let vault_dir = store.join("vaults").join("default");
        fs::create_dir_all(vault_dir.join("secrets")).unwrap();
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

        let backend = LocalSecretBackend::new(store, identity, recipients);
        (backend, tmp)
    }

    fn make_request(name: &str, value: &str) -> SecretRequest {
        SecretRequest {
            name: name.to_string(),
            value: Zeroizing::new(value.to_string()),
            content_type: Some("text/plain".into()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(HashMap::from([("env".into(), "test".into())])),
            groups: Some(vec!["db".into()]),
            note: Some("test note".into()),
            folder: Some("infra".into()),
        }
    }

    #[tokio::test]
    async fn set_and_get_secret() {
        let (backend, _tmp) = test_backend();

        let props = backend
            .set_secret("default", make_request("db-pass", "hunter2"))
            .await
            .unwrap();

        assert_eq!(props.name, "db-pass");
        assert_eq!(props.version, "v1");
        assert!(props.enabled);

        // Get without value
        let props = backend
            .get_secret("default", "db-pass", false)
            .await
            .unwrap();
        assert!(props.value.is_none());

        // Get with value
        let props = backend
            .get_secret("default", "db-pass", true)
            .await
            .unwrap();
        assert_eq!(&*props.value.unwrap(), "hunter2");
    }

    #[tokio::test]
    async fn rejects_traversal_vault_name_for_secret_writes() {
        let (backend, tmp) = test_backend();
        let outside = tmp.path().join("outside");

        let result = backend
            .set_secret("../../outside", make_request("escape", "value"))
            .await;
        assert!(matches!(result, Err(BackendError::InvalidArgument(_))));
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn set_secret_versions() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("key", "v1-value"))
            .await
            .unwrap();

        let props = backend
            .set_secret("default", make_request("key", "v2-value"))
            .await
            .unwrap();
        assert_eq!(props.version, "v2");

        // Current value
        let current = backend.get_secret("default", "key", true).await.unwrap();
        assert_eq!(&*current.value.unwrap(), "v2-value");

        // Version history
        let versions = backend.list_versions("default", "key").await.unwrap();
        assert_eq!(versions.len(), 2);
    }

    #[tokio::test]
    async fn list_secrets_with_group_filter() {
        let (backend, _tmp) = test_backend();

        let mut req1 = make_request("secret-a", "val-a");
        req1.groups = Some(vec!["alpha".into()]);

        let mut req2 = make_request("secret-b", "val-b");
        req2.groups = Some(vec!["beta".into()]);

        backend.set_secret("default", req1).await.unwrap();
        backend.set_secret("default", req2).await.unwrap();

        // All
        let all = backend.list_secrets("default", None).await.unwrap();
        assert_eq!(all.len(), 2);

        // Filter
        let alpha = backend
            .list_secrets("default", Some("alpha"))
            .await
            .unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].name, "secret-a");
    }

    #[tokio::test]
    async fn delete_secret() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("to-delete", "val"))
            .await
            .unwrap();

        assert!(backend.secret_exists("default", "to-delete").await.unwrap());

        backend.delete_secret("default", "to-delete").await.unwrap();

        // After soft-delete, secret should not exist in active secrets
        assert!(!backend.secret_exists("default", "to-delete").await.unwrap());

        // But should appear in deleted secrets list
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].name, "to-delete");
    }

    #[tokio::test]
    async fn soft_delete_and_restore() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("restore-me", "original-value"))
            .await
            .unwrap();

        // Delete
        backend
            .delete_secret("default", "restore-me")
            .await
            .unwrap();
        assert!(!backend
            .secret_exists("default", "restore-me")
            .await
            .unwrap());

        // Restore
        let restored = backend
            .restore_secret("default", "restore-me")
            .await
            .unwrap();
        assert_eq!(restored.name, "restore-me");

        // Should exist again
        assert!(backend
            .secret_exists("default", "restore-me")
            .await
            .unwrap());

        // Value should be recoverable
        let got = backend
            .get_secret("default", "restore-me", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "original-value");

        // Deleted list should be empty
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert!(deleted.is_empty());
    }

    #[tokio::test]
    async fn soft_delete_and_purge() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("purge-me", "val"))
            .await
            .unwrap();

        // Create a version first
        backend
            .set_secret("default", make_request("purge-me", "val-v2"))
            .await
            .unwrap();

        // Delete
        backend.delete_secret("default", "purge-me").await.unwrap();

        // Purge permanently
        backend.purge_secret("default", "purge-me").await.unwrap();

        // Should not be in trash anymore
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert!(deleted.is_empty());

        // Should not exist
        assert!(!backend.secret_exists("default", "purge-me").await.unwrap());
    }

    #[tokio::test]
    async fn rollback_to_previous_version() {
        let (backend, _tmp) = test_backend();

        // Create v1
        backend
            .set_secret("default", make_request("rb-test", "v1-value"))
            .await
            .unwrap();

        // Create v2
        backend
            .set_secret("default", make_request("rb-test", "v2-value"))
            .await
            .unwrap();

        // Current should be v2
        let current = backend
            .get_secret("default", "rb-test", true)
            .await
            .unwrap();
        assert_eq!(&*current.value.unwrap(), "v2-value");

        // Rollback to v1
        let rolled = backend.rollback("default", "rb-test", "v1").await.unwrap();
        assert!(rolled.version.starts_with('v'));

        // Current should now have v1's encrypted value
        let after = backend
            .get_secret("default", "rb-test", true)
            .await
            .unwrap();
        assert_eq!(&*after.value.unwrap(), "v1-value");
    }

    #[tokio::test]
    async fn list_versions_returns_all() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("ver-test", "v1"))
            .await
            .unwrap();
        backend
            .set_secret("default", make_request("ver-test", "v2"))
            .await
            .unwrap();
        backend
            .set_secret("default", make_request("ver-test", "v3"))
            .await
            .unwrap();

        let versions = backend.list_versions("default", "ver-test").await.unwrap();
        assert_eq!(versions.len(), 3);

        // Version numbers should be sequential
        assert_eq!(versions[0].version_number, Some(1));
        assert_eq!(versions[1].version_number, Some(2));
        assert_eq!(versions[2].version_number, Some(3));
    }

    #[tokio::test]
    async fn secret_exists_for_existing_and_missing() {
        let (backend, _tmp) = test_backend();

        // Missing secret
        assert!(!backend.secret_exists("default", "nope").await.unwrap());

        // Create and check
        backend
            .set_secret("default", make_request("exists-test", "val"))
            .await
            .unwrap();
        assert!(backend
            .secret_exists("default", "exists-test")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn get_nonexistent_secret_returns_not_found() {
        let (backend, _tmp) = test_backend();

        let result = backend.get_secret("default", "nope", false).await;
        assert!(matches!(result, Err(BackendError::NotFound { .. })));
    }

    #[tokio::test]
    async fn update_secret_metadata() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("upd", "initial"))
            .await
            .unwrap();

        let update = SecretUpdateRequest {
            name: "upd".into(),
            new_name: None,
            value: None,
            content_type: Some("application/json".into()),
            enabled: Some(false),
            expires_on: None,
            not_before: None,
            tags: Some(HashMap::from([("new_tag".into(), "new_val".into())])),
            groups: Some(vec!["added-group".into()]),
            note: Some("updated note".into()),
            folder: None,
            replace_tags: false,
            replace_groups: false,
        };

        let props = backend
            .update_secret("default", "upd", update)
            .await
            .unwrap();
        assert!(!props.enabled);
        assert_eq!(props.content_type, "application/json");
        // Tags should be merged
        assert!(props.tags.contains_key("env"));
        assert!(props.tags.contains_key("new_tag"));
    }

    #[tokio::test]
    async fn update_secret_with_new_value_creates_version() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("versioned", "old"))
            .await
            .unwrap();

        let update = SecretUpdateRequest {
            name: "versioned".into(),
            new_name: None,
            value: Some(Zeroizing::new("new".into())),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
            replace_tags: false,
            replace_groups: false,
        };

        let props = backend
            .update_secret("default", "versioned", update)
            .await
            .unwrap();
        assert_eq!(props.version, "v2");

        let got = backend
            .get_secret("default", "versioned", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "new");
    }

    #[tokio::test]
    async fn special_chars_in_secret_name() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("my/secret:key", "val"))
            .await
            .unwrap();

        let got = backend
            .get_secret("default", "my/secret:key", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "val");
        assert_eq!(got.name, "my/secret:key");
    }

    #[tokio::test]
    async fn delete_recreate_delete_preserves_both_trash_snapshots() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("cycle", "first-value"))
            .await
            .unwrap();
        backend.delete_secret("default", "cycle").await.unwrap();

        backend
            .set_secret("default", make_request("cycle", "second-value"))
            .await
            .unwrap();
        backend.delete_secret("default", "cycle").await.unwrap();

        // Both deleted versions must exist in the trash.
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(deleted.iter().all(|s| s.name == "cycle"));

        // Recover restores the most recent snapshot.
        backend.restore_secret("default", "cycle").await.unwrap();
        let got = backend.get_secret("default", "cycle", true).await.unwrap();
        assert_eq!(&*got.value.unwrap(), "second-value");

        // The older snapshot is still in the trash.
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 1);

        // Delete again and recover twice: most recent first, then the oldest.
        backend.delete_secret("default", "cycle").await.unwrap();
        backend.restore_secret("default", "cycle").await.unwrap();
        let got = backend.get_secret("default", "cycle", true).await.unwrap();
        assert_eq!(&*got.value.unwrap(), "second-value");

        backend.restore_secret("default", "cycle").await.unwrap();
        let got = backend.get_secret("default", "cycle", true).await.unwrap();
        assert_eq!(&*got.value.unwrap(), "first-value");

        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert!(deleted.is_empty());
    }

    #[tokio::test]
    async fn trash_collision_same_name_and_timestamp_rejected() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("collide", "first"))
            .await
            .unwrap();
        backend
            .delete_secret_at("default", "collide", 1_234_567)
            .unwrap();

        backend
            .set_secret("default", make_request("collide", "second"))
            .await
            .unwrap();
        let result = backend.delete_secret_at("default", "collide", 1_234_567);
        assert!(matches!(result, Err(BackendError::Conflict(_))));

        // The rejected delete must not have touched the active secret...
        let got = backend
            .get_secret("default", "collide", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "second");

        // ...nor the previously trashed snapshot, which is still recoverable.
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 1);

        backend.delete_secret("default", "collide").await.unwrap();
        backend.restore_secret("default", "collide").await.unwrap();
        let got = backend
            .get_secret("default", "collide", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "second");

        backend.restore_secret("default", "collide").await.unwrap();
        let got = backend
            .get_secret("default", "collide", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "first");
    }

    #[tokio::test]
    async fn legacy_unsuffixed_trash_entry_still_recoverable() {
        let (backend, tmp) = test_backend();

        // Trash a secret, then rename its entry to the legacy un-suffixed
        // format written by older versions.
        backend
            .set_secret("default", make_request("legacy", "legacy-value"))
            .await
            .unwrap();
        backend.delete_secret("default", "legacy").await.unwrap();

        let tbase = trash_base_dir(tmp.path(), "default").unwrap();
        let suffixed = fs::read_dir(&tbase)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("legacy@"))
            .unwrap()
            .path();
        fs::rename(&suffixed, tbase.join("legacy")).unwrap();

        // Legacy entry shows up in the deleted list.
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].name, "legacy");

        // A newer suffixed entry takes precedence on recover.
        backend
            .set_secret("default", make_request("legacy", "newer-value"))
            .await
            .unwrap();
        backend.delete_secret("default", "legacy").await.unwrap();

        backend.restore_secret("default", "legacy").await.unwrap();
        let got = backend.get_secret("default", "legacy", true).await.unwrap();
        assert_eq!(&*got.value.unwrap(), "newer-value");

        // The legacy entry is restored last.
        backend.delete_secret("default", "legacy").await.unwrap();
        backend.purge_secret("default", "legacy").await.unwrap();
        // Recreate the legacy-only situation: purge removed everything, so
        // rebuild a legacy entry and confirm restore handles it directly.
        backend
            .set_secret("default", make_request("legacy", "legacy-value"))
            .await
            .unwrap();
        backend.delete_secret("default", "legacy").await.unwrap();
        let suffixed = fs::read_dir(&tbase)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("legacy@"))
            .unwrap()
            .path();
        fs::rename(&suffixed, tbase.join("legacy")).unwrap();

        backend.restore_secret("default", "legacy").await.unwrap();
        let got = backend.get_secret("default", "legacy", true).await.unwrap();
        assert_eq!(&*got.value.unwrap(), "legacy-value");
    }

    #[tokio::test]
    async fn restore_over_active_secret_archives_instead_of_clobbering() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("overwrite", "old-value"))
            .await
            .unwrap();
        backend.delete_secret("default", "overwrite").await.unwrap();

        // Recreate while the old snapshot sits in the trash.
        backend
            .set_secret("default", make_request("overwrite", "live-value"))
            .await
            .unwrap();

        // Restore brings back the trashed snapshot...
        let restored = backend
            .restore_secret("default", "overwrite")
            .await
            .unwrap();
        let got = backend
            .get_secret("default", "overwrite", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "old-value");

        // ...and the live secret is archived as a version, not destroyed.
        let archived = backend
            .get_secret_version("default", "overwrite", "v1", true)
            .await
            .unwrap();
        assert_eq!(&*archived.value.unwrap(), "live-value");
        assert_ne!(restored.version, "v1");
    }

    #[tokio::test]
    async fn purge_removes_all_trash_snapshots() {
        let (backend, _tmp) = test_backend();

        backend
            .set_secret("default", make_request("purge-all", "v1"))
            .await
            .unwrap();
        backend.delete_secret("default", "purge-all").await.unwrap();
        backend
            .set_secret("default", make_request("purge-all", "v2"))
            .await
            .unwrap();
        backend.delete_secret("default", "purge-all").await.unwrap();

        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 2);

        backend.purge_secret("default", "purge-all").await.unwrap();
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert!(deleted.is_empty());
    }

    #[test]
    fn encode_decode_roundtrip() {
        let names = vec![
            "simple",
            "my/secret",
            "key:with:colons",
            "spaced name",
            "emoji-🔑",
            "path/to/deep/secret",
        ];
        for name in names {
            let encoded = encode_name(name);
            let decoded = decode_name(&encoded);
            assert_eq!(decoded, name, "roundtrip failed for: {name}");
        }
    }
}
