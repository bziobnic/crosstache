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
use crate::secret::manager::{
    DeletedSecretSummary, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
};

use super::{crypto, opaque, paths};

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
///
/// This is the **legacy** (reversible) filename scheme. It is still used as the
/// on-disk stem when `opaque_filenames` is off, and as the stem an opaque store
/// migrates *away from* (see [`opaque`]).
fn encode_name(name: &str) -> String {
    // Percent-encode everything except unreserved characters per RFC 3986.
    url::form_urlencoded::byte_serialize(name.as_bytes()).collect()
}

/// Decode a URL-encoded (legacy) filename back to the original secret name.
///
/// Used by the migration path to recover a name from a legacy stem when the
/// `.meta.json` is unavailable, and by tests.
fn decode_name(encoded: &str) -> String {
    url::form_urlencoded::parse(encoded.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect()
}

fn secrets_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::secrets_dir(store_path, vault)
}

/// Active value-file path for a given filename `stem` (already encoded/hashed).
fn age_path(store_path: &Path, vault: &str, stem: &str) -> Result<PathBuf, BackendError> {
    Ok(secrets_dir(store_path, vault)?.join(format!("{stem}.age")))
}

/// Active metadata-file path for a given filename `stem`.
fn meta_path(store_path: &Path, vault: &str, stem: &str) -> Result<PathBuf, BackendError> {
    Ok(secrets_dir(store_path, vault)?.join(format!("{stem}.meta.json")))
}

/// Version-archive directory for a given filename `stem`.
fn versions_dir(store_path: &Path, vault: &str, stem: &str) -> Result<PathBuf, BackendError> {
    Ok(secrets_dir(store_path, vault)?.join(".versions").join(stem))
}

/// Whether a version-archive directory exists and contains at least one entry.
///
/// An empty directory (e.g. left by an interrupted `merge_versions_dir`) must
/// not win over a populated legacy archive dir during read resolution.
fn versions_dir_nonempty(vdir: &Path) -> bool {
    fs::read_dir(vdir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn trash_base_dir(store_path: &Path, vault: &str) -> Result<PathBuf, BackendError> {
    paths::trash_base_dir(store_path, vault)
}

/// Directory for a single trash entry, suffixed with the deletion timestamp so
/// repeated delete/recreate/delete cycles never collide. `@` cannot appear in a
/// percent-encoded name or a base32 opaque stem, so the suffix is unambiguous.
fn trash_entry_dir(
    store_path: &Path,
    vault: &str,
    stem: &str,
    deleted_at_millis: u128,
) -> Result<PathBuf, BackendError> {
    Ok(trash_base_dir(store_path, vault)?.join(format!("{stem}@{deleted_at_millis}")))
}

/// All trash entries for a given filename `stem`, as `(deleted_at_millis, dir)`
/// pairs.
///
/// Handles both naming formats: legacy un-suffixed dirs (`<stem>`, written by
/// older versions, treated as timestamp 0) and suffixed dirs (`<stem>@<millis>`).
fn trash_entries_for_stem(
    store_path: &Path,
    vault: &str,
    stem: &str,
) -> Result<Vec<(u128, PathBuf)>, BackendError> {
    let tbase = trash_base_dir(store_path, vault)?;
    let mut entries = Vec::new();
    if !tbase.exists() {
        return Ok(entries);
    }

    let prefix = format!("{stem}@");
    let dir_entries =
        fs::read_dir(&tbase).map_err(|e| BackendError::Internal(format!("read trash dir: {e}")))?;
    for entry in dir_entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname == stem {
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

/// Locate the inner `.age` and `.meta.json` files inside a trash entry dir,
/// regardless of the stem they are named with (legacy or opaque). `.deleted.json`
/// is ignored.
fn trash_inner_files(tdir: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut age_file = None;
    let mut meta_file = None;
    if let Ok(entries) = fs::read_dir(tdir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname == ".deleted.json" {
                continue;
            }
            if fname.ends_with(".meta.json") {
                meta_file = Some(entry.path());
            } else if fname.ends_with(".age") {
                age_file = Some(entry.path());
            }
        }
    }
    (age_file, meta_file)
}

/// Move every entry from a legacy version-archive dir into the opaque one, then
/// remove the (now-empty) legacy dir. Files already present at the destination
/// (same `v<N>.*` label = identical archived content) are left in place and the
/// legacy duplicate dropped, so this is safe to re-run.
fn merge_versions_dir(legacy_vdir: &Path, opaque_vdir: &Path) -> Result<(), BackendError> {
    let entries = fs::read_dir(legacy_vdir)
        .map_err(|e| BackendError::Internal(format!("read legacy versions: {e}")))?;
    let mut created_opaque = false;
    for entry in entries.flatten() {
        if !created_opaque {
            fs::create_dir_all(opaque_vdir)
                .map_err(|e| BackendError::Internal(format!("mkdir versions: {e}")))?;
            created_opaque = true;
        }
        let src = entry.path();
        let Some(fname) = src.file_name() else {
            continue;
        };
        let dest = opaque_vdir.join(fname);
        if dest.exists() {
            if src.is_dir() {
                fs::remove_dir_all(&src).ok();
            } else {
                fs::remove_file(&src).ok();
            }
        } else {
            fs::rename(&src, &dest).map_err(|e| {
                BackendError::Internal(format!(
                    "merge version {} -> {}: {e}",
                    src.display(),
                    dest.display()
                ))
            })?;
        }
    }
    fs::remove_dir_all(legacy_vdir)
        .map_err(|e| BackendError::Internal(format!("remove legacy versions dir: {e}")))?;
    Ok(())
}

/// Rename a legacy-named trash entry to the opaque scheme: rename inner files to
/// `<opaque_stem>.{age,meta.json}`, strip plaintext `original_name` from
/// `.deleted.json`, then rename the dir itself to `<opaque_stem>@<millis>`.
/// Skips (leaves untouched) if the opaque target dir already exists, to avoid
/// clobbering a distinct entry.
fn migrate_trash_dir(
    tdir: &Path,
    legacy_stem: &str,
    opaque_stem: &str,
    millis: u128,
) -> Result<(), BackendError> {
    let target = tdir.with_file_name(format!("{opaque_stem}@{millis}"));
    if target == tdir {
        return Ok(());
    }
    if target.exists() {
        return Ok(());
    }

    // Rename inner age/meta files to the opaque stem.
    let (age_file, meta_file) = trash_inner_files(tdir);
    for (src, ext) in [(age_file, "age"), (meta_file, "meta.json")] {
        let Some(src) = src else { continue };
        let dest = tdir.join(format!("{opaque_stem}.{ext}"));
        if src != dest {
            fs::rename(&src, &dest).map_err(|e| {
                BackendError::Internal(format!("rename trash inner {}: {e}", src.display()))
            })?;
        }
    }
    let _ = legacy_stem; // inner files located by extension, not by stem name.

    // Strip plaintext original_name from .deleted.json (keep deleted_at).
    let deleted = tdir.join(".deleted.json");
    if deleted.exists() {
        if let Ok(raw) = fs::read(&deleted) {
            if let Ok(mut val) = serde_json::from_slice::<serde_json::Value>(&raw) {
                if let Some(obj) = val.as_object_mut() {
                    if obj.remove("original_name").is_some() {
                        let bytes = serde_json::to_vec_pretty(&val).map_err(|e| {
                            BackendError::Internal(format!("serialize deleted meta: {e}"))
                        })?;
                        write_private(&deleted, &bytes).map_err(|e| {
                            BackendError::Internal(format!("rewrite deleted meta: {e}"))
                        })?;
                    }
                }
            }
        }
    }

    fs::rename(tdir, &target).map_err(|e| {
        BackendError::Internal(format!(
            "rename trash dir {} -> {}: {e}",
            tdir.display(),
            target.display()
        ))
    })?;
    Ok(())
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

/// Read and deserialize secret metadata from `path`.
///
/// Auto-detects whether the file holds age ciphertext (encrypted metadata) or
/// plaintext JSON by inspecting the age magic header, so a store may contain a
/// mix of both during/after migration. `identity` is only used when the file
/// is encrypted.
fn read_meta(path: &Path, identity: &age::x25519::Identity) -> Result<SecretMeta, BackendError> {
    let raw = fs::read(path)
        .map_err(|e| BackendError::Internal(format!("read meta {}: {e}", path.display())))?;

    if crypto::is_age_encrypted(&raw) {
        let plaintext = crypto::decrypt_bytes(&raw, identity)
            .map_err(|e| BackendError::Internal(format!("decrypt meta {}: {e}", path.display())))?;
        serde_json::from_slice(&plaintext)
            .map_err(|e| BackendError::Internal(format!("parse meta {}: {e}", path.display())))
    } else {
        serde_json::from_slice(&raw)
            .map_err(|e| BackendError::Internal(format!("parse meta {}: {e}", path.display())))
    }
}

/// How metadata should be written: which recipients to encrypt to, and whether
/// to encrypt at all. Bundled so write paths don't sprout extra positional args.
#[derive(Clone, Copy)]
struct MetaCrypto<'a> {
    recipients: &'a [age::x25519::Recipient],
    encrypt: bool,
}

/// Serialize and write secret metadata to `path`.
///
/// When `crypto_opts.encrypt` is true, the JSON is age-encrypted to the given
/// recipients before being written; otherwise it is written as plaintext JSON.
/// Either way the file is created with private (0600) permissions.
fn write_meta(path: &Path, meta: &SecretMeta, crypto_opts: MetaCrypto) -> Result<(), BackendError> {
    let json = serde_json::to_vec_pretty(meta)
        .map_err(|e| BackendError::Internal(format!("serialize meta: {e}")))?;

    let bytes = if crypto_opts.encrypt {
        crypto::encrypt_bytes(&json, crypto_opts.recipients)
            .map_err(|e| BackendError::Internal(format!("encrypt meta: {e}")))?
    } else {
        json
    };

    write_private(path, &bytes)
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
    stem: &str,
    version: &str,
    age_bytes: &[u8],
    meta: &SecretMeta,
    crypto_opts: MetaCrypto,
) -> Result<(), BackendError> {
    let vdir = versions_dir(store_path, vault, stem)?;
    fs::create_dir_all(&vdir)
        .map_err(|e| BackendError::Internal(format!("mkdir versions: {e}")))?;

    let age_dest = vdir.join(format!("{version}.age"));
    let meta_dest = vdir.join(format!("{version}.meta.json"));

    write_private(&age_dest, age_bytes)
        .map_err(|e| BackendError::Internal(format!("archive age: {e}")))?;
    write_meta(&meta_dest, meta, crypto_opts)
        .map_err(|e| BackendError::Internal(format!("archive meta: {e}")))?;

    Ok(())
}

/// Determine the next version number by scanning `.versions/<stem>/`.
fn next_version(store_path: &Path, vault: &str, stem: &str) -> Result<u32, BackendError> {
    let vdir = versions_dir(store_path, vault, stem)?;
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

/// Archive the current secret to `.versions/<stem>/v<N>.*`.
fn archive_current(store_path: &Path, vault: &str, stem: &str) -> Result<u32, BackendError> {
    let ap = age_path(store_path, vault, stem)?;
    let mp = meta_path(store_path, vault, stem)?;

    if !ap.exists() && !mp.exists() {
        return Ok(1);
    }

    let ver = next_version(store_path, vault, stem)?;
    let vdir = versions_dir(store_path, vault, stem)?;
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

/// Summary of an `xv local migrate` run (or dry-run plan).
#[derive(Debug, Default)]
pub struct MigrationReport {
    /// Active legacy secrets renamed to opaque stems.
    pub migrated: usize,
    /// Orphan opaque stems whose missing index entry was rebuilt from metadata.
    pub recovered: usize,
    /// Legacy trash entries renamed to opaque stems.
    pub trash_migrated: usize,
    /// Human-readable lines describing each planned/performed action.
    pub plan: Vec<String>,
}

impl MigrationReport {
    /// Total number of on-disk changes (would-be changes for a dry run).
    pub fn total(&self) -> usize {
        self.migrated + self.recovered + self.trash_migrated
    }
}

/// File-backed secret operations using age encryption.
pub struct LocalSecretBackend {
    store_path: PathBuf,
    identity: age::x25519::Identity,
    recipients: Vec<age::x25519::Recipient>,
    encrypt_metadata: bool,
    /// Whether on-disk filenames are opaque keyed-hash stems (see [`opaque`]).
    opaque_filenames: bool,
    /// HMAC key for opaque stems, derived from `identity`. `None` when
    /// `opaque_filenames` is off, so the legacy path never computes it.
    index_key: Option<[u8; 32]>,
}

impl LocalSecretBackend {
    /// Construct a backend, choosing whether new metadata writes are encrypted
    /// and whether on-disk filenames are opaque.
    pub fn with_options(
        store_path: PathBuf,
        identity: age::x25519::Identity,
        recipients: Vec<age::x25519::Recipient>,
        encrypt_metadata: bool,
        opaque_filenames: bool,
    ) -> Self {
        let index_key = if opaque_filenames {
            Some(opaque::derive_index_key(&identity))
        } else {
            None
        };
        Self {
            store_path,
            identity,
            recipients,
            encrypt_metadata,
            opaque_filenames,
            index_key,
        }
    }

    // -----------------------------------------------------------------------
    // Opaque-filename helpers
    //
    // When `opaque_filenames` is off, `active_stem` == `encode_name`, the index
    // is never touched, and `ensure_opaque_layout`/reconciliation are no-ops —
    // so behavior is byte-for-byte identical to the legacy layout.
    // -----------------------------------------------------------------------

    /// The preferred on-disk stem for `name`: the opaque keyed-hash stem when
    /// `opaque_filenames` is on, else the legacy URL-encoded stem.
    fn active_stem(&self, name: &str) -> String {
        match self.index_key.as_ref() {
            Some(key) => opaque::opaque_stem(key, name),
            None => encode_name(name),
        }
    }

    /// Resolve the stem of an existing active secret, honoring the read-only
    /// back-compat fallback to the legacy stem. Returns the stem whose
    /// `.meta.json` exists; otherwise the preferred (active) stem (caller checks
    /// existence). Never creates or renames anything.
    fn resolve_active_stem(&self, vault: &str, name: &str) -> Result<String, BackendError> {
        let primary = self.active_stem(name);
        if meta_path(&self.store_path, vault, &primary)?.exists() {
            return Ok(primary);
        }
        if self.opaque_filenames {
            let legacy = encode_name(name);
            if meta_path(&self.store_path, vault, &legacy)?.exists() {
                return Ok(legacy);
            }
        }
        Ok(primary)
    }

    /// Locate the version-archive directory for `name`, preferring the active
    /// stem but falling back to the legacy stem (read-only) so version reads
    /// work on a not-yet-fully-migrated store.
    ///
    /// An empty active-stem directory (e.g. after `merge_versions_dir` created
    /// the destination then failed before moving entries) must not hide a
    /// populated legacy archive directory.
    fn resolve_versions_dir(&self, vault: &str, name: &str) -> Result<PathBuf, BackendError> {
        let active = self.active_stem(name);
        let dir = versions_dir(&self.store_path, vault, &active)?;
        if self.opaque_filenames {
            let legacy_dir = versions_dir(&self.store_path, vault, &encode_name(name))?;
            let active_has = versions_dir_nonempty(&dir);
            let legacy_has = versions_dir_nonempty(&legacy_dir);
            if active_has {
                return Ok(dir);
            }
            if legacy_has {
                return Ok(legacy_dir);
            }
        } else if dir.exists() {
            return Ok(dir);
        }
        Ok(dir)
    }

    /// All trash entries for `name`, covering opaque and (when opaque is on)
    /// legacy stems. `(deleted_at_millis, dir)` pairs.
    fn trash_entries_for(
        &self,
        vault: &str,
        name: &str,
    ) -> Result<Vec<(u128, PathBuf)>, BackendError> {
        let mut entries = trash_entries_for_stem(&self.store_path, vault, &self.active_stem(name))?;
        if self.opaque_filenames {
            entries.extend(trash_entries_for_stem(
                &self.store_path,
                vault,
                &encode_name(name),
            )?);
        }
        Ok(entries)
    }

    /// Bring a single secret's on-disk layout up to the opaque scheme and keep
    /// the encrypted index consistent. Idempotent; a no-op when
    /// `opaque_filenames` is off. **The caller must already hold the vault
    /// `fs2` lock.**
    ///
    /// Steps (all keyed on `name`, no metadata decryption required):
    /// 1. Move any legacy active pair to the opaque stem (or drop it if the
    ///    opaque pair already exists).
    /// 2. Set the index entry `{opaque_stem → name}` when an active pair exists,
    ///    or remove it when none does (e.g. just soft-deleted).
    /// 3. Merge a legacy `.versions/<encode_name>/` dir into the opaque one.
    /// 4. Rename legacy trash dirs (`<encode_name>@*`, unsuffixed) to the opaque
    ///    stem and strip plaintext `original_name` from their `.deleted.json`.
    fn ensure_opaque_layout(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        let Some(key) = self.index_key.as_ref() else {
            return Ok(());
        };
        let opaque_stem = opaque::opaque_stem(key, name);
        let legacy_stem = encode_name(name);
        let store = &self.store_path;

        // 1. Active pair: legacy -> opaque (or remove legacy dup).
        if legacy_stem != opaque_stem {
            for ext in ["age", "meta.json"] {
                let legacy = secrets_dir(store, vault)?.join(format!("{legacy_stem}.{ext}"));
                if !legacy.exists() {
                    continue;
                }
                let opaque_path = secrets_dir(store, vault)?.join(format!("{opaque_stem}.{ext}"));
                if opaque_path.exists() {
                    fs::remove_file(&legacy).map_err(|e| {
                        BackendError::Internal(format!("remove legacy {}: {e}", legacy.display()))
                    })?;
                } else {
                    fs::rename(&legacy, &opaque_path).map_err(|e| {
                        BackendError::Internal(format!(
                            "migrate {} -> {}: {e}",
                            legacy.display(),
                            opaque_path.display()
                        ))
                    })?;
                }
            }
        }

        // 3. Version archive: merge legacy dir into the opaque dir.
        if legacy_stem != opaque_stem {
            let legacy_vdir = versions_dir(store, vault, &legacy_stem)?;
            if legacy_vdir.exists() {
                let opaque_vdir = versions_dir(store, vault, &opaque_stem)?;
                merge_versions_dir(&legacy_vdir, &opaque_vdir)?;
            }
        }

        // 4. Trash: rename legacy-named entries to the opaque stem.
        if legacy_stem != opaque_stem {
            for (millis, tdir) in trash_entries_for_stem(store, vault, &legacy_stem)? {
                migrate_trash_dir(&tdir, &legacy_stem, &opaque_stem, millis)?;
            }
        }

        // 2. Index entry follows the active pair's presence.
        let sdir = secrets_dir(store, vault)?;
        let mut index = opaque::load_index(&sdir, &self.identity)?;
        let active_exists = meta_path(store, vault, &opaque_stem)?.exists();
        let changed = if active_exists {
            let entry = index.get(&opaque_stem);
            if entry.map(|e| e.name.as_str()) != Some(name) {
                index.insert(
                    opaque_stem.clone(),
                    opaque::IndexEntry {
                        name: name.to_string(),
                        v: 1,
                    },
                );
                true
            } else {
                false
            }
        } else {
            index.remove(&opaque_stem).is_some()
        };
        if changed {
            opaque::save_index(&sdir, &index, &self.recipients)?;
        }
        Ok(())
    }

    /// Re-encrypt every plaintext secret `.meta.json` under the store as age
    /// ciphertext, in place. Covers active secrets (`secrets/`), archived
    /// versions (`secrets/.versions/`), and trashed secrets (`.trash/`).
    /// Already-encrypted metadata is skipped, so this is idempotent and safe to
    /// re-run.
    ///
    /// File-storage metadata (`files/*.meta.json`, which holds `FileInfo`, not
    /// `SecretMeta`) is deliberately left untouched — the file backend reads it
    /// as plaintext JSON and is not part of the `encrypt_metadata` contract.
    ///
    /// Each vault is processed while holding its `.lock` (the same lock taken by
    /// `set_secret`/`update_secret`/etc.), so a concurrent `xv` mutation cannot
    /// interleave a write between this migration's read and its atomic rename.
    /// If a vault's lock cannot be acquired (e.g. another process holds it), the
    /// migration fails loudly rather than silently leaving that vault plaintext.
    ///
    /// Returns `(converted, skipped)` counts. When `dry_run` is true, nothing
    /// is written and `converted` reflects what *would* be converted.
    pub fn reencrypt_all_metadata(&self, dry_run: bool) -> Result<(usize, usize), BackendError> {
        let vaults_root = paths::vaults_dir(&self.store_path);
        let mut converted = 0usize;
        let mut skipped = 0usize;
        if !vaults_root.exists() {
            return Ok((0, 0));
        }

        let vault_dirs = fs::read_dir(&vaults_root)
            .map_err(|e| BackendError::Internal(format!("read vaults dir: {e}")))?;
        for vault_entry in vault_dirs.flatten() {
            let vault_dir = vault_entry.path();
            // Only directories that look like vaults (have a .vault.json).
            if !vault_dir.join(".vault.json").exists() {
                continue;
            }
            // Serialize against concurrent mutations of this vault. Fail loudly
            // on lock errors so the caller never reports success while leaving a
            // vault's metadata plaintext. The lock is held until `_lock` drops
            // at the end of this iteration.
            let _lock = lock_vault(&vault_dir).map_err(|e| {
                BackendError::Internal(format!(
                    "could not lock vault {} for metadata migration (another xv process may be \
                     running): {e}",
                    vault_dir.display()
                ))
            })?;

            // Walk only the secret-metadata subtrees: active + archived secrets
            // live under secrets/ (which contains .versions/), trashed secrets
            // under .trash/. files/ is intentionally excluded (FileInfo JSON).
            let roots = [vault_dir.join("secrets"), vault_dir.join(".trash")];
            let mut stack: Vec<PathBuf> = roots.into_iter().filter(|p| p.exists()).collect();
            while let Some(dir) = stack.pop() {
                let entries = match fs::read_dir(&dir) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let is_meta = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.ends_with(".meta.json"))
                        .unwrap_or(false);
                    if !is_meta {
                        continue;
                    }
                    // Skip symlinks defensively.
                    if fs::symlink_metadata(&path)
                        .map(|m| m.is_symlink())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let raw = fs::read(&path).map_err(|e| {
                        BackendError::Internal(format!("read meta {}: {e}", path.display()))
                    })?;
                    if crypto::is_age_encrypted(&raw) {
                        skipped += 1;
                        continue;
                    }
                    // Validate it parses before rewriting, so we never clobber a
                    // genuinely corrupt file silently.
                    let _meta: SecretMeta = serde_json::from_slice(&raw).map_err(|e| {
                        BackendError::Internal(format!("parse meta {}: {e}", path.display()))
                    })?;
                    converted += 1;
                    if dry_run {
                        continue;
                    }
                    let ciphertext = crypto::encrypt_bytes(&raw, &self.recipients)?;
                    // Write via a temp file + rename for atomicity.
                    let tmp = temp_path_for(&path)?;
                    write_private(&tmp, &ciphertext).map_err(|e| {
                        BackendError::Internal(format!("write temp meta {}: {e}", tmp.display()))
                    })?;
                    fs::rename(&tmp, &path).map_err(|e| {
                        BackendError::Internal(format!("activate meta {}: {e}", path.display()))
                    })?;
                }
            }
        }
        Ok((converted, skipped))
    }

    /// Migrate every vault in the store to the opaque-filename layout.
    ///
    /// Per vault, under its `fs2` lock: rename legacy `encode_name` active +
    /// version + trash paths to opaque stems, (re)build the encrypted index,
    /// and run an idempotent metadata-based recovery pass for opaque stems
    /// missing from the index. Safe to re-run. With `dry_run`, nothing is
    /// written and the returned report's `plan` describes what *would* change.
    ///
    /// Requires `opaque_filenames` to be enabled (the CLI verifies this).
    pub fn migrate_all(&self, dry_run: bool) -> Result<MigrationReport, BackendError> {
        if !self.opaque_filenames {
            return Err(BackendError::Internal(
                "opaque_filenames is disabled; enable it under [local] before migrating".into(),
            ));
        }

        let mut report = MigrationReport::default();
        let vaults_root = paths::vaults_dir(&self.store_path);
        if !vaults_root.exists() {
            return Ok(report);
        }

        let vault_dirs = fs::read_dir(&vaults_root)
            .map_err(|e| BackendError::Internal(format!("read vaults dir: {e}")))?;
        for vault_entry in vault_dirs.flatten() {
            let vault_dir = vault_entry.path();
            if !vault_dir.join(".vault.json").exists() {
                continue;
            }
            let vault = vault_dir
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| {
                    BackendError::Internal(format!("invalid vault dir {}", vault_dir.display()))
                })?
                .to_string();

            // Same lock as runtime mutations, so a migration step can't race a
            // concurrent write. Held until `_lock` drops at iteration end.
            let _lock = lock_vault(&vault_dir).map_err(|e| {
                BackendError::Internal(format!(
                    "could not lock vault {} for migration (another xv process may be \
                     running): {e}",
                    vault_dir.display()
                ))
            })?;

            self.migrate_vault(&vault, dry_run, &mut report)?;
        }
        Ok(report)
    }

    /// Migrate a single vault. Caller must hold the vault `fs2` lock.
    fn migrate_vault(
        &self,
        vault: &str,
        dry_run: bool,
        report: &mut MigrationReport,
    ) -> Result<(), BackendError> {
        let key = self
            .index_key
            .as_ref()
            .ok_or_else(|| BackendError::Internal("opaque filenames disabled".into()))?;
        let store = &self.store_path;
        let sdir = secrets_dir(store, vault)?;
        if !sdir.exists() {
            return Ok(());
        }

        let mut index = opaque::load_index(&sdir, &self.identity)?;

        // Collect active stems once (read_dir is invalidated by renames).
        let mut active_stems: Vec<String> = Vec::new();
        for entry in fs::read_dir(&sdir)
            .map_err(|e| BackendError::Internal(format!("read secrets dir: {e}")))?
            .flatten()
        {
            let fname = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = fname.strip_suffix(".meta.json") {
                active_stems.push(stem.to_string());
            }
        }
        active_stems.sort();

        // Pass A: legacy active pairs → opaque (index entry FIRST, then rename).
        for stem in &active_stems {
            let mp = meta_path(store, vault, stem)?;
            let name = match read_meta(&mp, &self.identity) {
                Ok(m) => m.name,
                Err(_) => decode_name(stem),
            };
            let opaque_stem = opaque::opaque_stem(key, &name);
            if opaque::is_canonical_stem(key, stem, &name) {
                continue; // already at the HMAC stem (Pass B recovers a missing index)
            }
            report
                .plan
                .push(format!("secret: {stem}.* -> {opaque_stem}.* ({name:?})"));
            report.migrated += 1;
            if dry_run {
                continue;
            }
            // Index-before-rename: a crash after this still lists the secret
            // from the index; a crash before it leaves legacy files for the
            // legacy scan. Never rename away the only recoverable name source
            // before the index records the stem.
            index.insert(
                opaque_stem.clone(),
                opaque::IndexEntry {
                    name: name.clone(),
                    v: 1,
                },
            );
            opaque::save_index(&sdir, &index, &self.recipients)?;
            self.ensure_opaque_layout(vault, &name)?;
        }

        // Pass B (recovery): HMAC stems on disk but missing from the index.
        for stem in &active_stems {
            if index.contains_key(stem) {
                continue;
            }
            let mp = meta_path(store, vault, stem)?;
            if !mp.exists() {
                continue;
            }
            let name = match read_meta(&mp, &self.identity) {
                Ok(m) => m.name,
                Err(e) => {
                    eprintln!("warning: cannot recover name for stem {stem:?}: {e}");
                    continue;
                }
            };
            if !opaque::is_canonical_stem(key, stem, &name) {
                continue; // opaque-looking legacy stem; Pass A handles migration
            }
            report
                .plan
                .push(format!("recover index: {stem} ({name:?})"));
            report.recovered += 1;
            if dry_run {
                continue;
            }
            index.insert(stem.clone(), opaque::IndexEntry { name, v: 1 });
        }

        // Pass C: legacy trash dirs → opaque stems.
        let tbase = trash_base_dir(store, vault)?;
        if tbase.exists() {
            let mut trash_dirs: Vec<PathBuf> = fs::read_dir(&tbase)
                .map_err(|e| BackendError::Internal(format!("read trash dir: {e}")))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            trash_dirs.sort();
            for tdir in trash_dirs {
                let dname = tdir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let (base, millis) = match dname.rsplit_once('@') {
                    Some((b, ms)) => match ms.parse::<u128>() {
                        Ok(n) => (b.to_string(), n),
                        // Suffix is not a timestamp; treat as legacy unsuffixed stem
                        // `<base>@<garbage>` with millis 0 (same as trash_entries_for_stem).
                        Err(_) => (b.to_string(), 0),
                    },
                    None => (dname.clone(), 0),
                };
                // Recover the name: prefer inner metadata, fall back to decode.
                let (_, meta_file) = trash_inner_files(&tdir);
                let name = meta_file
                    .and_then(|p| read_meta(&p, &self.identity).ok())
                    .map(|m| m.name)
                    .unwrap_or_else(|| decode_name(&base));
                let opaque_stem = opaque::opaque_stem(key, &name);
                if opaque::is_canonical_stem(key, &base, &name) {
                    continue; // already at the HMAC stem
                }
                report.plan.push(format!(
                    "trash: {dname} -> {opaque_stem}@{millis} ({name:?})"
                ));
                report.trash_migrated += 1;
                if dry_run {
                    continue;
                }
                migrate_trash_dir(&tdir, &base, &opaque_stem, millis)?;
            }
        }

        if !dry_run {
            opaque::save_index(&sdir, &index, &self.recipients)?;
        }
        Ok(())
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

        // Where the active pair currently lives (opaque stem, or legacy via the
        // read fallback on an un-migrated store).
        let src_stem = self.resolve_active_stem(vault, name)?;
        let mp = meta_path(&self.store_path, vault, &src_stem)?;
        let ap = age_path(&self.store_path, vault, &src_stem)?;

        if !mp.exists() && !ap.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        // The trash entry is named with the active (opaque when enabled) stem so
        // no legacy name leaks into `.trash/`.
        let trash_stem = self.active_stem(name);
        let tdir = trash_entry_dir(&self.store_path, vault, &trash_stem, deleted_at_millis)?;
        if tdir.exists() {
            return Err(BackendError::Conflict(format!(
                "trash entry for '{name}' at timestamp {deleted_at_millis} already exists; \
                 refusing to overwrite previously deleted secret"
            )));
        }
        fs::create_dir_all(&tdir)
            .map_err(|e| BackendError::Internal(format!("mkdir trash: {e}")))?;

        if ap.exists() {
            let dest = tdir.join(format!("{trash_stem}.age"));
            fs::rename(&ap, &dest)
                .map_err(|e| BackendError::Internal(format!("move age to trash: {e}")))?;
        }
        if mp.exists() {
            let dest = tdir.join(format!("{trash_stem}.meta.json"));
            fs::rename(&mp, &dest)
                .map_err(|e| BackendError::Internal(format!("move meta to trash: {e}")))?;
        }

        // Write deletion metadata. With opaque filenames on, the trash dir name
        // is opaque and `.deleted.json` must NOT carry plaintext `original_name`
        // (the name is recovered from the encrypted `.meta.json`). Legacy mode
        // keeps the field for byte-for-byte back-compat.
        let deleted_meta = if self.opaque_filenames {
            serde_json::json!({ "deleted_at": Utc::now().to_rfc3339() })
        } else {
            serde_json::json!({ "deleted_at": Utc::now().to_rfc3339(), "original_name": name })
        };
        let deleted_path = tdir.join(".deleted.json");
        write_private(
            &deleted_path,
            serde_json::to_string_pretty(&deleted_meta)
                .map_err(|e| BackendError::Internal(format!("serialize deleted meta: {e}")))?
                .as_bytes(),
        )
        .map_err(|e| BackendError::Internal(format!("write deleted meta: {e}")))?;

        // Drop the active index entry and clean up any legacy layout (versions,
        // other legacy trash dirs) for this name.
        self.ensure_opaque_layout(vault, name)?;

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
        // Resolve where an existing pair lives (opaque stem, or legacy via the
        // read fallback). A brand-new secret resolves to the active stem. The
        // trailing `ensure_opaque_layout` migrates any legacy stem to opaque.
        let stem = self.resolve_active_stem(vault, &name)?;
        let ap = age_path(&store, vault, &stem)?;
        let mp = meta_path(&store, vault, &stem)?;

        // Snapshot old state for transactional replace+archive.
        let old_snapshot = if mp.exists() {
            let old_meta = read_meta(&mp, &self.identity)?;
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
        write_meta(
            &mp_tmp,
            &meta,
            MetaCrypto {
                recipients: &recipients,
                encrypt: self.encrypt_metadata,
            },
        )?;

        fs::rename(&ap_tmp, &ap)
            .map_err(|e| BackendError::Internal(format!("activate age {}: {e}", ap.display())))?;
        fs::rename(&mp_tmp, &mp)
            .map_err(|e| BackendError::Internal(format!("activate meta {}: {e}", mp.display())))?;

        // Archive old version only after replacement is durable.
        if let Some((old_meta, old_age)) = old_snapshot {
            archive_snapshot(
                &store,
                vault,
                &stem,
                &old_meta.version,
                &old_age,
                &old_meta,
                MetaCrypto {
                    recipients: &recipients,
                    encrypt: self.encrypt_metadata,
                },
            )?;
        }

        // Upgrade this secret's on-disk layout to opaque stems + refresh the
        // index entry (no-op when opaque filenames are off).
        self.ensure_opaque_layout(vault, &name)?;

        Ok(meta_to_properties(&meta, None))
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        // Resolve the on-disk stem (opaque, or legacy via the read-only
        // back-compat fallback). Reads never create or upgrade legacy files.
        let stem = self.resolve_active_stem(vault, name)?;
        let mp = meta_path(&self.store_path, vault, &stem)?;
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

        let meta = read_meta(&mp, &self.identity)?;

        let value = if include_value {
            let ap = age_path(&self.store_path, vault, &stem)?;

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
        let stem = self.resolve_active_stem(vault, name)?;
        let mp = meta_path(&self.store_path, vault, &stem)?;
        if mp.exists() {
            let meta = read_meta(&mp, &self.identity)?;
            if meta.version == version {
                return self.get_secret(vault, name, include_value).await;
            }
        }

        // Look in .versions/ (opaque, or legacy via read fallback).
        let vdir = self.resolve_versions_dir(vault, name)?;
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

        let meta = read_meta(&meta_file, &self.identity)?;

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
        let push_meta = |meta: &SecretMeta, results: &mut Vec<SecretSummary>| {
            if let Some(group) = group_filter {
                if !meta.groups.iter().any(|g| g == group) {
                    return;
                }
            }
            results.push(meta_to_summary(meta));
        };

        // Track stems already accounted for so reconciliation never
        // double-counts a secret present both in the index and on disk.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        if self.opaque_filenames {
            // Primary source: the decrypted reverse index.
            let index = opaque::load_index(&sdir, &self.identity)?;
            for stem in index.keys() {
                let mp = meta_path(&self.store_path, vault, stem)?;
                if !mp.exists() {
                    continue;
                }
                match read_meta(&mp, &self.identity) {
                    Ok(meta) => {
                        seen.insert(stem.clone());
                        push_meta(&meta, &mut results);
                    }
                    Err(e) => {
                        eprintln!("warning: indexed secret {stem:?} has corrupted metadata: {e}");
                    }
                }
            }
        }

        // Reconciliation (always for legacy mode; back-compat window for opaque
        // mode): pick up any on-disk pair not already represented in the index —
        // legacy `encode_name` pairs and orphan opaque-stem pairs alike. The
        // real name comes from the decrypted metadata, never from the filename.
        let entries = fs::read_dir(&sdir)
            .map_err(|e| BackendError::Internal(format!("read secrets dir: {e}")))?;
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            let Some(stem) = fname.strip_suffix(".meta.json") else {
                continue;
            };
            if seen.contains(stem) {
                continue;
            }

            let meta = match read_meta(&entry.path(), &self.identity) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "warning: secret {:?} exists but has corrupted metadata: {}",
                        fname, e
                    );
                    continue;
                }
            };
            seen.insert(stem.to_string());
            push_meta(&meta, &mut results);
        }

        // Sort by name for deterministic output
        results.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(results)
    }

    async fn delete_secret(&self, vault: &str, name: &str) -> Result<(), BackendError> {
        let mut deleted_at_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BackendError::Internal(format!("clock error: {e}")))?
            .as_millis();

        // Millisecond clocks can repeat during tight delete/recreate/delete
        // cycles. Keep explicit timestamp collisions fail-closed in
        // `delete_secret_at`, but make the normal API choose the next free
        // suffix so rapid successive deletes preserve every trash snapshot.
        for _ in 0..1000 {
            match self.delete_secret_at(vault, name, deleted_at_millis) {
                Err(BackendError::Conflict(_)) => deleted_at_millis += 1,
                other => return other,
            }
        }
        Err(BackendError::Conflict(format!(
            "could not allocate a unique trash entry timestamp for '{name}'"
        )))
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

        // Resolve where the active pair lives (opaque, or legacy via the read
        // fallback). The trailing `ensure_opaque_layout` migrates legacy → opaque
        // so even a metadata-only update upgrades the layout and clears the leak.
        let stem = self.resolve_active_stem(vault, name)?;
        let mp = meta_path(&self.store_path, vault, &stem)?;
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        let mut meta = read_meta(&mp, &self.identity)?;
        let now = Utc::now();

        // If value is being updated, replace active first, then archive prior snapshot.
        if let Some(ref new_value) = request.value {
            let ap = age_path(&self.store_path, vault, &stem)?;
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
                &stem,
                &old_meta.version,
                &old_age,
                &old_meta,
                MetaCrypto {
                    recipients: &self.recipients,
                    encrypt: self.encrypt_metadata,
                },
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
        meta.expires_on = request.expires_on.apply(meta.expires_on);
        meta.not_before = request.not_before.apply(meta.not_before);
        meta.note = request.note.apply(meta.note.take());
        meta.folder = request.folder.apply(meta.folder.take());

        meta.updated_at = now;
        write_meta(
            &mp,
            &meta,
            MetaCrypto {
                recipients: &self.recipients,
                encrypt: self.encrypt_metadata,
            },
        )?;

        // Upgrade legacy layout → opaque and refresh the index entry.
        self.ensure_opaque_layout(vault, name)?;

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

        // Collect archived versions (opaque, or legacy via read fallback)
        let vdir = self.resolve_versions_dir(vault, name)?;
        if vdir.exists() {
            let entries = fs::read_dir(&vdir)
                .map_err(|e| BackendError::Internal(format!("read versions dir: {e}")))?;
            for entry in entries {
                let entry = entry
                    .map_err(|e| BackendError::Internal(format!("read versions entry: {e}")))?;
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.ends_with(".meta.json") {
                    let meta = read_meta(&entry.path(), &self.identity)?;
                    versions.push(meta_to_properties(&meta, None));
                }
            }
        }

        // Add current version
        let stem = self.resolve_active_stem(vault, name)?;
        let mp = meta_path(&self.store_path, vault, &stem)?;
        if mp.exists() {
            let meta = read_meta(&mp, &self.identity)?;
            versions.push(meta_to_properties(&meta, None));
        }

        // Sort by version number
        versions.sort_by_key(|v| v.version_number.unwrap_or(0));

        Ok(versions)
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        let stem = self.resolve_active_stem(vault, name)?;
        let mp = meta_path(&self.store_path, vault, &stem)?;
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

        // Operate on the stem where this secret currently lives (opaque, or
        // legacy via the read fallback). `ensure_opaque_layout` migrates to
        // opaque afterwards.
        let stem = self.resolve_active_stem(vault, name)?;

        // Find the target version (opaque or legacy archive dir — same fallback
        // as get_secret_version / list_versions).
        let vdir = self.resolve_versions_dir(vault, name)?;
        let ver_age = vdir.join(format!("{version}.age"));
        let ver_meta = vdir.join(format!("{version}.meta.json"));

        if !ver_meta.exists() {
            return Err(BackendError::NotFound {
                name: format!("{name}@{version}"),
                suggestion: None,
            });
        }

        // Archive current as the next version
        archive_current(&self.store_path, vault, &stem)?;

        // Copy the target version files to current
        let ap = age_path(&self.store_path, vault, &stem)?;
        let mp = meta_path(&self.store_path, vault, &stem)?;

        if ver_age.exists() {
            fs::copy(&ver_age, &ap)
                .map_err(|e| BackendError::Internal(format!("restore age: {e}")))?;
        }
        fs::copy(&ver_meta, &mp)
            .map_err(|e| BackendError::Internal(format!("restore meta: {e}")))?;

        // Update the version label to the next version number
        let mut meta = read_meta(&mp, &self.identity)?;
        let next_ver = next_version(&self.store_path, vault, &stem)?;
        meta.version = format!("v{next_ver}");
        meta.updated_at = Utc::now();
        write_meta(
            &mp,
            &meta,
            MetaCrypto {
                recipients: &self.recipients,
                encrypt: self.encrypt_metadata,
            },
        )?;

        // Upgrade legacy layout → opaque and refresh the index entry.
        self.ensure_opaque_layout(vault, name)?;

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
        // choice is deterministic regardless of read_dir order. Covers both
        // opaque and legacy trash stems.
        let mut entries = self.trash_entries_for(vault, name)?;
        entries.sort();
        let Some((_, tdir)) = entries.pop() else {
            return Err(BackendError::NotFound {
                name: format!("{name} (deleted)"),
                suggestion: Some("Secret is not in the trash".into()),
            });
        };

        // Inner files may be named with an opaque or legacy stem; locate by
        // extension rather than recomputing a stem.
        let (trash_age, trash_meta) = trash_inner_files(&tdir);
        let Some(trash_meta) = trash_meta.filter(|p| p.exists()) else {
            return Err(BackendError::NotFound {
                name: format!("{name} (deleted)"),
                suggestion: Some("Trash metadata not found".into()),
            });
        };

        // Move files back to secrets/ at the active (opaque when enabled) stem.
        let sdir = secrets_dir(&self.store_path, vault)?;
        fs::create_dir_all(&sdir)
            .map_err(|e| BackendError::Internal(format!("mkdir secrets: {e}")))?;

        let target_stem = self.active_stem(name);
        let ap = age_path(&self.store_path, vault, &target_stem)?;
        let mp = meta_path(&self.store_path, vault, &target_stem)?;

        // If an active secret with this name exists (delete → recreate →
        // restore), archive it to .versions/ instead of silently destroying
        // it, mirroring the rollback path.
        let existing_stem = self.resolve_active_stem(vault, name)?;
        let existing_ap = age_path(&self.store_path, vault, &existing_stem)?;
        let existing_mp = meta_path(&self.store_path, vault, &existing_stem)?;
        let had_active = existing_ap.exists() || existing_mp.exists();
        if had_active {
            archive_current(&self.store_path, vault, &existing_stem)?;
        }

        if let Some(trash_age) = trash_age.filter(|p| p.exists()) {
            fs::rename(&trash_age, &ap)
                .map_err(|e| BackendError::Internal(format!("restore age from trash: {e}")))?;
        }
        fs::rename(&trash_meta, &mp)
            .map_err(|e| BackendError::Internal(format!("restore meta from trash: {e}")))?;

        // Remove the trash entry
        fs::remove_dir_all(&tdir)
            .map_err(|e| BackendError::Internal(format!("remove trash dir: {e}")))?;

        let mut meta = read_meta(&mp, &self.identity)?;
        if had_active {
            // Relabel so the restored secret doesn't reuse a version label
            // already taken by the archived active secret.
            let next_ver = next_version(&self.store_path, vault, &target_stem)?;
            meta.version = format!("v{next_ver}");
            meta.updated_at = Utc::now();
            write_meta(
                &mp,
                &meta,
                MetaCrypto {
                    recipients: &self.recipients,
                    encrypt: self.encrypt_metadata,
                },
            )?;
        }

        // Re-add the active index entry and upgrade any remaining legacy layout.
        self.ensure_opaque_layout(vault, name)?;

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

        // Permanently remove every trash entry for this name (opaque + legacy
        // stems, suffixed and unsuffixed).
        for (_, tdir) in self.trash_entries_for(vault, name)? {
            fs::remove_dir_all(&tdir)
                .map_err(|e| BackendError::Internal(format!("purge trash: {e}")))?;
        }

        // Also remove any .versions/ for that secret, under both stems.
        let mut version_stems = vec![self.active_stem(name)];
        if self.opaque_filenames {
            version_stems.push(encode_name(name));
        }
        for stem in version_stems {
            let vdir = versions_dir(&self.store_path, vault, &stem)?;
            if vdir.exists() {
                fs::remove_dir_all(&vdir)
                    .map_err(|e| BackendError::Internal(format!("purge versions: {e}")))?;
            }
        }

        Ok(())
    }

    async fn list_deleted_secrets(
        &self,
        vault: &str,
    ) -> Result<Vec<DeletedSecretSummary>, BackendError> {
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

            // Trash entry dirs are named `{stem}@{deleted_at_millis}` (see
            // `trash_entry_dir`); legacy unsuffixed dirs simply yield None.
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let deleted_on = dir_name
                .rsplit('@')
                .next()
                .and_then(|ms| ms.parse::<i64>().ok())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|dt| dt.to_string());

            // Look for .meta.json files in this trash entry
            let dir_path = entry.path();
            if let Ok(inner_entries) = fs::read_dir(&dir_path) {
                for inner in inner_entries.flatten() {
                    let fname = inner.file_name().to_string_lossy().to_string();
                    if fname.ends_with(".meta.json") {
                        if let Ok(meta) = read_meta(&inner.path(), &self.identity) {
                            results.push(DeletedSecretSummary {
                                name: meta.name.clone(),
                                original_name: meta.original_name.clone(),
                                deleted_on: deleted_on.clone(),
                                // Local trash persists until an explicit
                                // `xv purge` — no schedule to report.
                                scheduled_purge_on: None,
                            });
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
    use crate::secret::manager::FieldUpdate;
    use tempfile::TempDir;

    /// Create a test backend with a temp dir and return it along with the temp dir.
    /// Metadata encryption is off (matches the default).
    fn test_backend() -> (LocalSecretBackend, TempDir) {
        test_backend_opts(false)
    }

    /// Like [`test_backend`] but lets the caller opt into metadata encryption.
    fn test_backend_opts(encrypt_metadata: bool) -> (LocalSecretBackend, TempDir) {
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

        let backend =
            LocalSecretBackend::with_options(store, identity, recipients, encrypt_metadata, false);
        (backend, tmp)
    }

    /// Create a test backend with opaque filenames enabled (metadata encryption
    /// off unless `encrypt_metadata` is set), returning it with its temp dir.
    fn test_backend_opaque() -> (LocalSecretBackend, TempDir) {
        test_backend_opaque_opts(false)
    }

    fn test_backend_opaque_opts(encrypt_metadata: bool) -> (LocalSecretBackend, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        let vault_dir = store.join("vaults").join("default");
        fs::create_dir_all(vault_dir.join("secrets")).unwrap();
        fs::write(
            vault_dir.join(".vault.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "name": "default", "created_at": Utc::now().to_rfc3339(), "tags": {}
            }))
            .unwrap(),
        )
        .unwrap();

        let backend =
            LocalSecretBackend::with_options(store, identity, recipients, encrypt_metadata, true);
        (backend, tmp)
    }

    /// Re-open an existing store with opaque filenames enabled, reusing its key
    /// files. Used to migrate a legacy-layout store created by
    /// [`test_backend_opts`].
    fn reopen_opaque(tmp: &TempDir, encrypt_metadata: bool) -> LocalSecretBackend {
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let identity = crypto::load_identity(&key_path).unwrap();
        let recipients = crypto::load_recipients(&recipients_path).unwrap();
        LocalSecretBackend::with_options(store, identity, recipients, encrypt_metadata, true)
    }

    /// Re-open an existing store (created by [`test_backend_opts`]) reusing its
    /// key files, optionally toggling metadata encryption. Used to simulate a
    /// user flipping `encrypt_metadata` on and re-running against the same data.
    fn test_backend_reopen(tmp: &TempDir, encrypt_metadata: bool) -> LocalSecretBackend {
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let identity = crypto::load_identity(&key_path).unwrap();
        let recipients = crypto::load_recipients(&recipients_path).unwrap();
        LocalSecretBackend::with_options(store, identity, recipients, encrypt_metadata, false)
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

    /// Path to the active `.meta.json` for a secret in the default vault.
    fn meta_file_path(tmp: &TempDir, name: &str) -> std::path::PathBuf {
        let enc = encode_name(name);
        tmp.path()
            .join("vaults")
            .join("default")
            .join("secrets")
            .join(format!("{enc}.meta.json"))
    }

    #[tokio::test]
    async fn encrypted_metadata_roundtrips_and_is_age_on_disk() {
        let (backend, tmp) = test_backend_opts(true);

        let props = backend
            .set_secret("default", make_request("api-key", "s3cr3t"))
            .await
            .unwrap();
        assert_eq!(props.name, "api-key");
        assert_eq!(
            props.tags.get("note").map(String::as_str),
            Some("test note")
        );

        // On disk, the meta file must be age ciphertext, not readable JSON.
        let raw = fs::read(meta_file_path(&tmp, "api-key")).unwrap();
        assert!(
            crypto::is_age_encrypted(&raw),
            "meta file should be age-encrypted"
        );
        assert!(
            serde_json::from_slice::<SecretMeta>(&raw).is_err(),
            "encrypted meta must not parse as plaintext JSON"
        );
        // The sensitive note must not appear in cleartext anywhere in the file.
        assert!(
            !raw.windows(9).any(|w| w == b"test note"),
            "note leaked in cleartext"
        );

        // Reading it back through the backend transparently decrypts.
        let got = backend
            .get_secret("default", "api-key", true)
            .await
            .unwrap();
        assert_eq!(
            got.value.as_deref().map(|z| z.to_string()),
            Some("s3cr3t".into())
        );
        assert_eq!(got.tags.get("note").map(String::as_str), Some("test note"));
    }

    #[tokio::test]
    async fn plaintext_metadata_is_json_on_disk_by_default() {
        let (backend, tmp) = test_backend_opts(false);

        backend
            .set_secret("default", make_request("api-key", "s3cr3t"))
            .await
            .unwrap();

        let raw = fs::read(meta_file_path(&tmp, "api-key")).unwrap();
        assert!(!crypto::is_age_encrypted(&raw));
        // Plaintext mode must parse straight back as JSON.
        let meta: SecretMeta = serde_json::from_slice(&raw).unwrap();
        assert_eq!(meta.name, "api-key");
    }

    #[tokio::test]
    async fn mixed_mode_store_reads_both_plaintext_and_encrypted() {
        // Write one secret in plaintext mode, then reopen the SAME store with
        // encryption on and confirm the old plaintext secret is still readable
        // and that a newly written secret is encrypted.
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let key_path = tmp.path().join("key.txt");
        let recipients_path = tmp.path().join("recipients.txt");
        let (identity, recipients) = generate_keypair(&key_path, &recipients_path).unwrap();

        let vault_dir = store.join("vaults").join("default");
        fs::create_dir_all(vault_dir.join("secrets")).unwrap();
        fs::write(
            vault_dir.join(".vault.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "name": "default", "created_at": Utc::now().to_rfc3339(), "tags": {}
            }))
            .unwrap(),
        )
        .unwrap();

        let plain = LocalSecretBackend::with_options(
            store.clone(),
            identity.clone(),
            recipients.clone(),
            false,
            false,
        );
        plain
            .set_secret("default", make_request("old-plain", "v1"))
            .await
            .unwrap();

        let enc =
            LocalSecretBackend::with_options(store.clone(), identity, recipients, true, false);
        enc.set_secret("default", make_request("new-enc", "v2"))
            .await
            .unwrap();

        // Both readable through the encryption-on backend.
        let a = enc.get_secret("default", "old-plain", true).await.unwrap();
        let b = enc.get_secret("default", "new-enc", true).await.unwrap();
        assert_eq!(a.value.as_deref().map(|z| z.to_string()), Some("v1".into()));
        assert_eq!(b.value.as_deref().map(|z| z.to_string()), Some("v2".into()));

        // list_secrets sees both regardless of meta encoding.
        let listed = enc.list_secrets("default", None).await.unwrap();
        let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"old-plain") && names.contains(&"new-enc"));
    }

    #[tokio::test]
    async fn reencrypt_all_metadata_migrates_plaintext_and_is_idempotent() {
        // Seed a plaintext store with two secrets (one updated, so it has an
        // archived version), then migrate with the same key and verify every
        // meta file becomes age ciphertext while values still decrypt.
        let (plain, tmp) = test_backend_opts(false);
        plain
            .set_secret("default", make_request("a", "1"))
            .await
            .unwrap();
        plain
            .set_secret("default", make_request("b", "2"))
            .await
            .unwrap();
        // Second write to "a" archives v1 under .versions/.
        plain
            .set_secret("default", make_request("a", "1b"))
            .await
            .unwrap();

        // Simulate a file upload: files/<x>.meta.json holds FileInfo JSON, not
        // SecretMeta. The migration must NOT touch it (the file backend reads
        // it as plaintext), and must not abort trying to parse it as SecretMeta.
        let files_dir = tmp.path().join("vaults").join("default").join("files");
        fs::create_dir_all(&files_dir).unwrap();
        let file_meta = files_dir.join("upload.meta.json");
        fs::write(
            &file_meta,
            br#"{"name":"upload","size":3,"not_a_secret":true}"#,
        )
        .unwrap();

        // Every secret meta file is plaintext right now (under secrets/ + .trash/).
        let metas: Vec<std::path::PathBuf> = {
            let mut v = Vec::new();
            let mut stack = vec![
                tmp.path().join("vaults").join("default").join("secrets"),
                tmp.path().join("vaults").join("default").join(".trash"),
            ];
            while let Some(d) = stack.pop() {
                if !d.exists() {
                    continue;
                }
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.to_string_lossy().ends_with(".meta.json") {
                        v.push(p);
                    }
                }
            }
            v
        };
        assert!(metas.len() >= 3, "expected active + archived meta files");
        for m in &metas {
            assert!(!crypto::is_age_encrypted(&fs::read(m).unwrap()));
        }

        // Re-open with encryption enabled and run the migration.
        let enc = test_backend_reopen(&tmp, true);

        // Dry run reports work but changes nothing.
        let (would, already0) = enc.reencrypt_all_metadata(true).unwrap();
        assert_eq!(would, metas.len());
        assert_eq!(already0, 0);
        for m in &metas {
            assert!(!crypto::is_age_encrypted(&fs::read(m).unwrap()));
        }

        // Real run converts everything.
        let (converted, already1) = enc.reencrypt_all_metadata(false).unwrap();
        assert_eq!(converted, metas.len());
        assert_eq!(already1, 0);
        for m in &metas {
            assert!(crypto::is_age_encrypted(&fs::read(m).unwrap()));
        }

        // Idempotent: a second run converts nothing and skips all.
        let (converted2, already2) = enc.reencrypt_all_metadata(false).unwrap();
        assert_eq!(converted2, 0);
        assert_eq!(already2, metas.len());

        // Values and metadata still resolve correctly post-migration.
        let a = enc.get_secret("default", "a", true).await.unwrap();
        assert_eq!(a.value.as_deref().map(|z| z.to_string()), Some("1b".into()));
        let listed = enc.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.len(), 2);

        // The file-storage meta (FileInfo JSON under files/) was left untouched:
        // still plaintext, still parses, and the migration didn't abort on it.
        let file_raw = fs::read(&file_meta).unwrap();
        assert!(
            !crypto::is_age_encrypted(&file_raw),
            "files/*.meta.json must not be encrypted by the secret migration"
        );
        assert_eq!(
            file_raw,
            br#"{"name":"upload","size":3,"not_a_secret":true}"#
        );
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
    async fn deleted_listing_carries_deletion_date_but_no_purge_schedule() {
        let (backend, _tmp) = test_backend();
        backend
            .set_secret("default", make_request("dated", "v1"))
            .await
            .unwrap();
        backend.delete_secret("default", "dated").await.unwrap();

        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].name, "dated");
        assert!(
            deleted[0].deleted_on.is_some(),
            "deleted_on must come from the trash dir's @millis suffix"
        );
        assert!(
            deleted[0].scheduled_purge_on.is_none(),
            "local trash never auto-purges"
        );
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
    async fn list_versions_surfaces_corrupt_archived_metadata() {
        let (backend, tmp) = test_backend();
        backend
            .set_secret("default", make_request("corrupt-ver", "v1"))
            .await
            .unwrap();
        backend
            .set_secret("default", make_request("corrupt-ver", "v2"))
            .await
            .unwrap();

        let enc = encode_name("corrupt-ver");
        let archived_meta = tmp
            .path()
            .join("vaults")
            .join("default")
            .join("secrets")
            .join(".versions")
            .join(enc)
            .join("v1.meta.json");
        fs::write(&archived_meta, b"not json").unwrap();

        let err = backend
            .list_versions("default", "corrupt-ver")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("parse meta"),
            "corrupt archived metadata should be surfaced, got: {err}"
        );
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
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: Some(HashMap::from([("new_tag".into(), "new_val".into())])),
            groups: Some(vec!["added-group".into()]),
            note: FieldUpdate::Set("updated note".into()),
            folder: FieldUpdate::Unchanged,
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
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: None,
            groups: None,
            note: FieldUpdate::Unchanged,
            folder: FieldUpdate::Unchanged,
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

    /// An update request that changes nothing — base for tri-state tests.
    fn unchanged_update(name: &str) -> SecretUpdateRequest {
        SecretUpdateRequest {
            name: name.into(),
            new_name: None,
            value: None,
            content_type: None,
            enabled: None,
            expires_on: FieldUpdate::Unchanged,
            not_before: FieldUpdate::Unchanged,
            tags: None,
            groups: None,
            note: FieldUpdate::Unchanged,
            folder: FieldUpdate::Unchanged,
            replace_tags: false,
            replace_groups: false,
        }
    }

    #[tokio::test]
    async fn tristate_expires_set_survives_unrelated_update_then_clears() {
        let (backend, _tmp) = test_backend();
        backend
            .set_secret("default", make_request("tri-exp", "v"))
            .await
            .unwrap();

        let exp: DateTime<Utc> = "2030-01-02T03:04:05Z".parse().unwrap();
        let mut update = unchanged_update("tri-exp");
        update.expires_on = FieldUpdate::Set(exp);
        let props = backend
            .update_secret("default", "tri-exp", update)
            .await
            .unwrap();
        assert_eq!(props.expires_on, Some(exp));

        // Unrelated update must leave expiry untouched
        let mut update = unchanged_update("tri-exp");
        update.note = FieldUpdate::Set("something else".into());
        let props = backend
            .update_secret("default", "tri-exp", update)
            .await
            .unwrap();
        assert_eq!(props.expires_on, Some(exp));

        // Clear removes it
        let mut update = unchanged_update("tri-exp");
        update.expires_on = FieldUpdate::Clear;
        let props = backend
            .update_secret("default", "tri-exp", update)
            .await
            .unwrap();
        assert_eq!(props.expires_on, None);

        let got = backend
            .get_secret("default", "tri-exp", false)
            .await
            .unwrap();
        assert_eq!(got.expires_on, None);
    }

    #[tokio::test]
    async fn tristate_not_before_set_survives_unrelated_update_then_clears() {
        let (backend, _tmp) = test_backend();
        backend
            .set_secret("default", make_request("tri-nbf", "v"))
            .await
            .unwrap();

        let nbf: DateTime<Utc> = "2029-06-07T08:09:10Z".parse().unwrap();
        let mut update = unchanged_update("tri-nbf");
        update.not_before = FieldUpdate::Set(nbf);
        let props = backend
            .update_secret("default", "tri-nbf", update)
            .await
            .unwrap();
        assert_eq!(props.not_before, Some(nbf));

        let mut update = unchanged_update("tri-nbf");
        update.folder = FieldUpdate::Set("elsewhere".into());
        let props = backend
            .update_secret("default", "tri-nbf", update)
            .await
            .unwrap();
        assert_eq!(props.not_before, Some(nbf));

        let mut update = unchanged_update("tri-nbf");
        update.not_before = FieldUpdate::Clear;
        let props = backend
            .update_secret("default", "tri-nbf", update)
            .await
            .unwrap();
        assert_eq!(props.not_before, None);

        let got = backend
            .get_secret("default", "tri-nbf", false)
            .await
            .unwrap();
        assert_eq!(got.not_before, None);
    }

    #[tokio::test]
    async fn tristate_note_set_survives_unrelated_update_then_clears() {
        let (backend, _tmp) = test_backend();
        backend
            .set_secret("default", make_request("tri-note", "v"))
            .await
            .unwrap();

        let mut update = unchanged_update("tri-note");
        update.note = FieldUpdate::Set("important note".into());
        let props = backend
            .update_secret("default", "tri-note", update)
            .await
            .unwrap();
        assert_eq!(
            props.tags.get("note").map(String::as_str),
            Some("important note")
        );

        // Unrelated update must leave the note untouched
        let mut update = unchanged_update("tri-note");
        update.folder = FieldUpdate::Set("new/folder".into());
        let props = backend
            .update_secret("default", "tri-note", update)
            .await
            .unwrap();
        assert_eq!(
            props.tags.get("note").map(String::as_str),
            Some("important note")
        );

        // Clear removes it
        let mut update = unchanged_update("tri-note");
        update.note = FieldUpdate::Clear;
        let props = backend
            .update_secret("default", "tri-note", update)
            .await
            .unwrap();
        assert_eq!(props.tags.get("note"), None);

        let got = backend
            .get_secret("default", "tri-note", false)
            .await
            .unwrap();
        assert_eq!(got.tags.get("note"), None);
    }

    #[tokio::test]
    async fn tristate_folder_set_survives_unrelated_update_then_clears() {
        let (backend, _tmp) = test_backend();
        backend
            .set_secret("default", make_request("tri-folder", "v"))
            .await
            .unwrap();

        let mut update = unchanged_update("tri-folder");
        update.folder = FieldUpdate::Set("app/database".into());
        let props = backend
            .update_secret("default", "tri-folder", update)
            .await
            .unwrap();
        assert_eq!(
            props.tags.get("folder").map(String::as_str),
            Some("app/database")
        );

        // Unrelated update must leave the folder untouched
        let mut update = unchanged_update("tri-folder");
        update.note = FieldUpdate::Set("touch note".into());
        let props = backend
            .update_secret("default", "tri-folder", update)
            .await
            .unwrap();
        assert_eq!(
            props.tags.get("folder").map(String::as_str),
            Some("app/database")
        );

        // Clear removes it
        let mut update = unchanged_update("tri-folder");
        update.folder = FieldUpdate::Clear;
        let props = backend
            .update_secret("default", "tri-folder", update)
            .await
            .unwrap();
        assert_eq!(props.tags.get("folder"), None);

        let got = backend
            .get_secret("default", "tri-folder", false)
            .await
            .unwrap();
        assert_eq!(got.tags.get("folder"), None);
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

    // -----------------------------------------------------------------------
    // Opaque-filename tests (design test plan:
    // docs/plans/2026-06-19-local-secret-filename-opaquing.md)
    // -----------------------------------------------------------------------

    /// Path to a vault's `secrets/` dir in a test store.
    fn secrets_path(tmp: &TempDir, vault: &str) -> std::path::PathBuf {
        tmp.path().join("vaults").join(vault).join("secrets")
    }

    /// Path to a vault's `.trash/` dir in a test store.
    fn trash_path(tmp: &TempDir, vault: &str) -> std::path::PathBuf {
        tmp.path().join("vaults").join(vault).join(".trash")
    }

    /// Every file/dir entry name (non-recursive-aware: collects names at all
    /// depths) under `root`. Used to assert no secret name leaks into a listing.
    fn all_entry_names(root: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    names.push(entry.file_name().to_string_lossy().to_string());
                    if entry.path().is_dir() {
                        stack.push(entry.path());
                    }
                }
            }
        }
        names
    }

    /// Assert no entry name under `root` contains `name` or its percent-encoding
    /// as a substring.
    fn assert_no_name_leak(root: &Path, name: &str) {
        let enc = encode_name(name);
        for entry in all_entry_names(root) {
            assert!(
                !entry.contains(name),
                "raw name {name:?} leaked in entry {entry:?}"
            );
            assert!(
                !entry.contains(&enc),
                "percent-encoded name {enc:?} leaked in entry {entry:?}"
            );
        }
    }

    #[tokio::test]
    async fn opaque_off_is_byte_for_byte_legacy_layout() {
        let (backend, tmp) = test_backend(); // opaque off
        backend
            .set_secret("default", make_request("DB-PASSWORD", "v"))
            .await
            .unwrap();

        // Legacy filename is the (here unchanged) URL-encoding of the name.
        let enc = encode_name("DB-PASSWORD");
        assert!(secrets_path(&tmp, "default")
            .join(format!("{enc}.meta.json"))
            .exists());
        // No encrypted index is written in legacy mode.
        assert!(!secrets_path(&tmp, "default")
            .join(opaque::INDEX_FILE)
            .exists());
    }

    #[tokio::test]
    async fn opaque_roundtrip_and_index_maps_stem_to_name() {
        let (backend, tmp) = test_backend_opaque();
        backend
            .set_secret("default", make_request("DB-PASSWORD", "s3cret"))
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        // Listing shows only a 26-char opaque stem + the index, no name.
        let mut stems: Vec<String> = fs::read_dir(&sdir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        stems.sort();
        assert!(stems.contains(&opaque::INDEX_FILE.to_string()));
        let stem = backend.active_stem("DB-PASSWORD");
        assert!(opaque::is_opaque_stem(&stem));
        assert!(sdir.join(format!("{stem}.age")).exists());
        assert!(sdir.join(format!("{stem}.meta.json")).exists());
        assert_no_name_leak(&sdir, "DB-PASSWORD");

        // The encrypted index maps the stem back to the real name.
        let index = opaque::load_index(&sdir, &backend.identity).unwrap();
        assert_eq!(
            index.get(&stem).map(|e| e.name.as_str()),
            Some("DB-PASSWORD")
        );

        // get + list still return the real name and value.
        let got = backend
            .get_secret("default", "DB-PASSWORD", true)
            .await
            .unwrap();
        assert_eq!(&*got.value.unwrap(), "s3cret");
        let listed = backend.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "DB-PASSWORD");
    }

    #[tokio::test]
    async fn opaque_listing_has_no_name_substring_property() {
        let (backend, tmp) = test_backend_opaque();
        // A spread of awkward names: percent-y, unicode (NFC), spaces, slashes.
        let names = [
            "DB-PASSWORD",
            "api/key:prod",
            "spaced secret name",
            "weird%2Dlooking",
            "café-token",
            "Ω-omega-secret",
            "emoji-🔑-key",
        ];
        for name in names {
            backend
                .set_secret("default", make_request(name, "value"))
                .await
                .unwrap();
        }
        // Delete one so .trash/ is exercised too.
        backend
            .delete_secret("default", "café-token")
            .await
            .unwrap();

        let vault_dir = tmp.path().join("vaults").join("default");
        for name in names {
            assert_no_name_leak(&vault_dir, name);
        }
    }

    #[tokio::test]
    async fn opaque_wrong_key_yields_different_stem() {
        let (backend, _tmp) = test_backend_opaque();
        let stem = backend.active_stem("aws-key");
        // A stem computed under a different identity must not match.
        let other = opaque::derive_index_key(&age::x25519::Identity::generate());
        assert_ne!(stem, opaque::opaque_stem(&other, "aws-key"));
    }

    #[tokio::test]
    async fn unicode_nfc_nfd_map_to_distinct_stems() {
        let (backend, tmp) = test_backend_opaque();
        let nfc = "caf\u{e9}"; // é as one code point
        let nfd = "cafe\u{301}"; // e + combining acute
        assert_ne!(nfc, nfd);

        backend
            .set_secret("default", make_request(nfc, "one"))
            .await
            .unwrap();
        backend
            .set_secret("default", make_request(nfd, "two"))
            .await
            .unwrap();

        let stem_nfc = backend.active_stem(nfc);
        let stem_nfd = backend.active_stem(nfd);
        assert_ne!(stem_nfc, stem_nfd, "NFC and NFD must not collide");

        // Both readable by their exact byte-identity names.
        assert_eq!(
            &*backend
                .get_secret("default", nfc, true)
                .await
                .unwrap()
                .value
                .unwrap(),
            "one"
        );
        assert_eq!(
            &*backend
                .get_secret("default", nfd, true)
                .await
                .unwrap()
                .value
                .unwrap(),
            "two"
        );
        let listed = backend.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.len(), 2);

        let sdir = secrets_path(&tmp, "default");
        let index = opaque::load_index(&sdir, &backend.identity).unwrap();
        assert_eq!(index.len(), 2);
    }

    #[tokio::test]
    async fn migration_old_to_new_idempotent_with_back_compat_read() {
        // Build a legacy-layout store with a couple secrets (one versioned).
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("alpha", "a1"))
            .await
            .unwrap();
        legacy
            .set_secret("default", make_request("alpha", "a2"))
            .await
            .unwrap(); // archives v1
        legacy
            .set_secret("default", make_request("bravo", "b1"))
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        // Legacy filenames present.
        assert!(sdir.join("alpha.meta.json").exists());
        assert!(sdir.join("bravo.meta.json").exists());

        // Re-open with opaque enabled: back-compat read works *before* migration.
        let opaque_backend = reopen_opaque(&tmp, false);
        let pre = opaque_backend
            .get_secret("default", "alpha", true)
            .await
            .unwrap();
        assert_eq!(&*pre.value.unwrap(), "a2");

        // Migrate.
        let report = opaque_backend.migrate_all(false).unwrap();
        assert!(report.migrated >= 2);

        // No legacy filenames remain; opaque stems + index do.
        assert!(!sdir.join("alpha.meta.json").exists());
        assert!(!sdir.join("bravo.meta.json").exists());
        assert!(sdir.join(opaque::INDEX_FILE).exists());
        let alpha_stem = opaque_backend.active_stem("alpha");
        assert!(sdir.join(format!("{alpha_stem}.meta.json")).exists());
        assert!(sdir.join(format!("{alpha_stem}.age")).exists());
        // Version archive moved under the opaque stem.
        assert!(sdir.join(".versions").join(&alpha_stem).exists());

        // Values + listing intact.
        assert_eq!(
            &*opaque_backend
                .get_secret("default", "alpha", true)
                .await
                .unwrap()
                .value
                .unwrap(),
            "a2"
        );
        let listed = opaque_backend.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.len(), 2);

        // Re-running migrate is a no-op.
        let again = opaque_backend.migrate_all(false).unwrap();
        assert_eq!(again.total(), 0, "re-running migrate must be a no-op");

        // Dry-run also reports nothing now.
        let dry = opaque_backend.migrate_all(true).unwrap();
        assert_eq!(dry.total(), 0);
    }

    #[tokio::test]
    async fn migration_opaque_looking_legacy_name_is_renamed() {
        // A 26-char secret name using only a-z matches is_opaque_stem but is
        // not the HMAC stem; migrate must rename it, not treat it as done.
        let legacy_name = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(legacy_name.len(), opaque::STEM_LEN);
        assert!(opaque::is_opaque_stem(legacy_name));

        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request(legacy_name, "secret"))
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        assert!(sdir.join(format!("{legacy_name}.meta.json")).exists());

        let opaque_backend = reopen_opaque(&tmp, false);
        let expected_stem = opaque_backend.active_stem(legacy_name);
        assert_ne!(
            expected_stem, legacy_name,
            "HMAC stem must differ from the legacy on-disk stem"
        );

        let report = opaque_backend.migrate_all(false).unwrap();
        assert_eq!(
            report.migrated, 1,
            "must migrate the opaque-looking legacy stem"
        );
        assert_eq!(
            report.recovered, 0,
            "index entry comes from Pass A, not recovery"
        );

        assert!(
            !sdir.join(format!("{legacy_name}.meta.json")).exists(),
            "legacy filename must be removed"
        );
        assert!(sdir.join(format!("{expected_stem}.meta.json")).exists());
        assert_no_name_leak(&sdir, legacy_name);

        let index = opaque::load_index(&sdir, &opaque_backend.identity).unwrap();
        assert_eq!(
            index.get(&expected_stem).map(|e| e.name.as_str()),
            Some(legacy_name)
        );
        assert_eq!(
            &*opaque_backend
                .get_secret("default", legacy_name, true)
                .await
                .unwrap()
                .value
                .unwrap(),
            "secret"
        );
    }

    #[tokio::test]
    async fn migration_dry_run_touches_nothing() {
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("dry-secret", "v"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let report = opaque_backend.migrate_all(true).unwrap();
        assert_eq!(report.migrated, 1);
        assert!(!report.plan.is_empty());

        // Disk unchanged: legacy file still there, no index, no opaque stem.
        let sdir = secrets_path(&tmp, "default");
        assert!(sdir.join("dry-secret.meta.json").exists());
        assert!(!sdir.join(opaque::INDEX_FILE).exists());
    }

    #[tokio::test]
    async fn rollback_finds_versions_in_legacy_archive_dir() {
        // Active pair on the opaque stem but archives still under the legacy
        // `.versions/<encode_name>/` dir (e.g. ensure_opaque_layout moved active
        // files then failed during merge_versions_dir). Reads use
        // resolve_versions_dir; rollback must too.
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("split-ver", "v1-value"))
            .await
            .unwrap();
        legacy
            .set_secret("default", make_request("split-ver", "v2-value"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let sdir = secrets_path(&tmp, "default");
        let opaque_stem = opaque_backend.active_stem("split-ver");
        let legacy_stem = encode_name("split-ver");

        // Simulate partial migration: active on opaque stem, v1 still archived
        // under the legacy versions dir only.
        for ext in ["age", "meta.json"] {
            fs::rename(
                sdir.join(format!("{legacy_stem}.{ext}")),
                sdir.join(format!("{opaque_stem}.{ext}")),
            )
            .unwrap();
        }
        assert!(
            sdir.join(".versions").join(&legacy_stem).exists(),
            "v1 archive must remain under legacy stem"
        );
        assert!(
            !sdir.join(".versions").join(&opaque_stem).exists(),
            "opaque versions dir must not exist yet"
        );

        // Reads succeed via legacy fallback.
        let versions = opaque_backend
            .list_versions("default", "split-ver")
            .await
            .unwrap();
        assert_eq!(versions.len(), 2);
        let v1 = opaque_backend
            .get_secret_version("default", "split-ver", "v1", true)
            .await
            .unwrap();
        assert_eq!(&*v1.value.unwrap(), "v1-value");

        // Rollback must restore v1, not report not found.
        opaque_backend
            .rollback("default", "split-ver", "v1")
            .await
            .unwrap();
        let current = opaque_backend
            .get_secret("default", "split-ver", true)
            .await
            .unwrap();
        assert_eq!(&*current.value.unwrap(), "v1-value");

        // ensure_opaque_layout at end of rollback merges legacy archives forward.
        assert!(
            sdir.join(".versions").join(&opaque_stem).exists(),
            "rollback should merge archives under opaque stem"
        );
    }

    #[tokio::test]
    async fn resolve_versions_dir_falls_back_when_opaque_versions_dir_empty() {
        // merge_versions_dir used to mkdir the opaque archive dir before moving
        // legacy entries; an interrupted merge left an empty opaque dir that
        // hid populated legacy archives from list/get/rollback.
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("split-empty", "v1-value"))
            .await
            .unwrap();
        legacy
            .set_secret("default", make_request("split-empty", "v2-value"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let sdir = secrets_path(&tmp, "default");
        let opaque_stem = opaque_backend.active_stem("split-empty");
        let legacy_stem = encode_name("split-empty");

        for ext in ["age", "meta.json"] {
            fs::rename(
                sdir.join(format!("{legacy_stem}.{ext}")),
                sdir.join(format!("{opaque_stem}.{ext}")),
            )
            .unwrap();
        }
        assert!(sdir.join(".versions").join(&legacy_stem).exists());

        // Simulate interrupted merge_versions_dir: empty opaque dir, legacy still populated.
        fs::create_dir_all(sdir.join(".versions").join(&opaque_stem)).unwrap();

        let versions = opaque_backend
            .list_versions("default", "split-empty")
            .await
            .unwrap();
        assert_eq!(versions.len(), 2, "must list archived v1 plus current v2");

        let v1 = opaque_backend
            .get_secret_version("default", "split-empty", "v1", true)
            .await
            .unwrap();
        assert_eq!(&*v1.value.unwrap(), "v1-value");

        opaque_backend
            .rollback("default", "split-empty", "v1")
            .await
            .unwrap();
        let current = opaque_backend
            .get_secret("default", "split-empty", true)
            .await
            .unwrap();
        assert_eq!(&*current.value.unwrap(), "v1-value");
    }

    #[tokio::test]
    async fn migration_interrupt_after_index_before_rename_still_lists() {
        // Simulate a crash that wrote the index entry but had not yet renamed
        // the legacy files: list_secrets must still include the secret exactly
        // once, and get must still read it (legacy fallback).
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("halfway", "hv"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let sdir = secrets_path(&tmp, "default");
        let stem = opaque_backend.active_stem("halfway");

        // Hand-write an index entry for the opaque stem while the legacy files
        // remain in place (index-before-rename, rename not yet done).
        let mut index = opaque::Index::new();
        index.insert(
            stem.clone(),
            opaque::IndexEntry {
                name: "halfway".into(),
                v: 1,
            },
        );
        opaque::save_index(&sdir, &index, &opaque_backend.recipients).unwrap();
        assert!(sdir.join("halfway.meta.json").exists());
        assert!(!sdir.join(format!("{stem}.meta.json")).exists());

        // Listed exactly once (no double-count of index + legacy reconciliation).
        let listed = opaque_backend.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.iter().filter(|s| s.name == "halfway").count(), 1);
        // Still readable via the legacy fallback.
        assert_eq!(
            &*opaque_backend
                .get_secret("default", "halfway", true)
                .await
                .unwrap()
                .value
                .unwrap(),
            "hv"
        );
    }

    #[tokio::test]
    async fn migration_crash_recovery_rebuilds_index_from_metadata() {
        // Opaque store with secrets, then delete the index to simulate a
        // rename-before-index crash: list still works (orphan scan) and migrate
        // rebuilds the index from metadata.
        let (backend, tmp) = test_backend_opaque();
        backend
            .set_secret("default", make_request("recover-me", "rv"))
            .await
            .unwrap();
        backend
            .set_secret("default", make_request("also-me", "av"))
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        fs::remove_file(sdir.join(opaque::INDEX_FILE)).unwrap();

        // Listing still finds both via the orphan opaque scan.
        let listed = backend.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.len(), 2);

        // migrate rebuilds the index from metadata.
        let report = backend.migrate_all(false).unwrap();
        assert!(report.recovered >= 2);
        let index = opaque::load_index(&sdir, &backend.identity).unwrap();
        assert_eq!(index.len(), 2);
        // Listing matches get for both.
        for name in ["recover-me", "also-me"] {
            assert!(backend.secret_exists("default", name).await.unwrap());
        }
    }

    #[tokio::test]
    async fn upgrade_on_write_set_removes_legacy_pair() {
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("upg-set", "old"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        opaque_backend
            .set_secret("default", make_request("upg-set", "new"))
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        assert!(
            !sdir.join("upg-set.meta.json").exists(),
            "legacy pair removed"
        );
        let stem = opaque_backend.active_stem("upg-set");
        assert!(sdir.join(format!("{stem}.meta.json")).exists());
        assert_no_name_leak(&sdir, "upg-set");
        let index = opaque::load_index(&sdir, &opaque_backend.identity).unwrap();
        assert_eq!(index.get(&stem).map(|e| e.name.as_str()), Some("upg-set"));
    }

    #[tokio::test]
    async fn upgrade_on_write_metadata_only_update_removes_legacy_pair() {
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("upg-meta", "v"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let mut update = unchanged_update("upg-meta");
        update.note = FieldUpdate::Set("metadata-only".into());
        opaque_backend
            .update_secret("default", "upg-meta", update)
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        assert!(
            !sdir.join("upg-meta.meta.json").exists(),
            "metadata-only update must still upgrade the layout"
        );
        let stem = opaque_backend.active_stem("upg-meta");
        assert!(sdir.join(format!("{stem}.meta.json")).exists());
        assert_no_name_leak(&sdir, "upg-meta");
    }

    #[tokio::test]
    async fn upgrade_on_write_value_update_archives_under_opaque() {
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("upg-val", "v1"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let mut update = unchanged_update("upg-val");
        update.value = Some(Zeroizing::new("v2".into()));
        opaque_backend
            .update_secret("default", "upg-val", update)
            .await
            .unwrap();

        let sdir = secrets_path(&tmp, "default");
        let stem = opaque_backend.active_stem("upg-val");
        assert!(!sdir.join("upg-val.meta.json").exists());
        // Prior snapshot archived under the opaque version dir; legacy gone.
        assert!(!sdir.join(".versions").join("upg-val").exists());
        assert!(sdir.join(".versions").join(&stem).exists());
        let versions = opaque_backend
            .list_versions("default", "upg-val")
            .await
            .unwrap();
        assert_eq!(versions.len(), 2);
    }

    #[tokio::test]
    async fn upgrade_on_write_rollback_and_restore_opaque_only() {
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("rbr", "v1"))
            .await
            .unwrap();
        legacy
            .set_secret("default", make_request("rbr", "v2"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        opaque_backend
            .rollback("default", "rbr", "v1")
            .await
            .unwrap();
        let after = opaque_backend
            .get_secret("default", "rbr", true)
            .await
            .unwrap();
        assert_eq!(&*after.value.unwrap(), "v1");

        let sdir = secrets_path(&tmp, "default");
        assert!(!sdir.join("rbr.meta.json").exists());
        assert_no_name_leak(&sdir, "rbr");
    }

    #[tokio::test]
    async fn restore_readds_index_entry() {
        let (backend, tmp) = test_backend_opaque();
        backend
            .set_secret("default", make_request("restore-idx", "v"))
            .await
            .unwrap();
        let sdir = secrets_path(&tmp, "default");
        let stem = backend.active_stem("restore-idx");

        backend
            .delete_secret("default", "restore-idx")
            .await
            .unwrap();
        let index = opaque::load_index(&sdir, &backend.identity).unwrap();
        assert!(!index.contains_key(&stem), "soft-delete drops the entry");

        backend
            .restore_secret("default", "restore-idx")
            .await
            .unwrap();
        let index = opaque::load_index(&sdir, &backend.identity).unwrap();
        assert_eq!(
            index.get(&stem).map(|e| e.name.as_str()),
            Some("restore-idx"),
            "restore must re-add the index entry"
        );
        let listed = backend.list_secrets("default", None).await.unwrap();
        assert!(listed.iter().any(|s| s.name == "restore-idx"));
    }

    #[tokio::test]
    async fn delete_of_unmigrated_secret_leaves_no_legacy_files() {
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("del-legacy", "v"))
            .await
            .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        opaque_backend
            .delete_secret("default", "del-legacy")
            .await
            .unwrap();

        let enc = encode_name("del-legacy");
        let sdir = secrets_path(&tmp, "default");
        assert!(!sdir.join(format!("{enc}.age")).exists());
        assert!(!sdir.join(format!("{enc}.meta.json")).exists());
        assert_no_name_leak(&trash_path(&tmp, "default"), "del-legacy");
        // Still recoverable.
        let deleted = opaque_backend
            .list_deleted_secrets("default")
            .await
            .unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].name, "del-legacy");
    }

    #[tokio::test]
    async fn soft_delete_trash_has_no_name_substring() {
        let (backend, tmp) = test_backend_opaque();
        backend
            .set_secret("default", make_request("trash-secret", "v"))
            .await
            .unwrap();
        backend
            .delete_secret("default", "trash-secret")
            .await
            .unwrap();

        assert_no_name_leak(&trash_path(&tmp, "default"), "trash-secret");
        // Name still recoverable from encrypted metadata.
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].name, "trash-secret");
    }

    #[tokio::test]
    async fn deleted_json_has_no_plaintext_name_when_opaque() {
        let (backend, tmp) = test_backend_opaque();
        backend
            .set_secret("default", make_request("no-name-json", "v"))
            .await
            .unwrap();
        backend
            .delete_secret("default", "no-name-json")
            .await
            .unwrap();

        // Find the .deleted.json under .trash/ and confirm it lacks original_name.
        let tbase = trash_path(&tmp, "default");
        let entry = fs::read_dir(&tbase).unwrap().flatten().next().unwrap();
        let deleted_json = entry.path().join(".deleted.json");
        let raw = fs::read(&deleted_json).unwrap();
        let val: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(val.get("original_name").is_none());
        assert!(val.get("deleted_at").is_some());
    }

    #[tokio::test]
    async fn migration_trash_dir_with_invalid_millis_suffix_uses_stem_before_at() {
        // Pass C must recover the secret name from the stem before `@` when the
        // suffix is not a valid u128 and inner metadata is unavailable.
        let (legacy, tmp) = test_backend_opts(false);
        let name = "trash-at";
        legacy
            .set_secret("default", make_request(name, "v"))
            .await
            .unwrap();
        legacy.delete_secret("default", name).await.unwrap();

        let enc = encode_name(name);
        let tbase = trash_path(&tmp, "default");
        let trash_dir = fs::read_dir(&tbase)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(&enc))
            .unwrap()
            .path();
        // Corrupt the suffix so it is not a valid u128 timestamp.
        let bad_name = format!("{enc}@not-a-timestamp");
        fs::rename(&trash_dir, tbase.join(&bad_name)).unwrap();
        // Force decode_name fallback by corrupting inner metadata.
        write_private(
            tbase
                .join(&bad_name)
                .join(format!("{enc}.meta.json"))
                .as_path(),
            b"not-valid-meta",
        )
        .unwrap();

        let opaque_backend = reopen_opaque(&tmp, false);
        let expected_stem = opaque_backend.active_stem(name);
        let report = opaque_backend.migrate_all(false).unwrap();
        assert_eq!(report.trash_migrated, 1);
        assert!(
            report
                .plan
                .iter()
                .any(|line| { line.contains(&expected_stem) && line.contains("\"trash-at\"") }),
            "plan must migrate using the stem before @, not the full dir name: {:?}",
            report.plan
        );
        assert!(
            fs::read_dir(&tbase)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(&expected_stem)),
            "trash dir must be renamed to the opaque stem for {name:?}"
        );
    }

    #[tokio::test]
    async fn migration_with_preexisting_trash() {
        // Legacy store with a soft-deleted secret (legacy trash dir + plaintext
        // original_name), then migrate.
        let (legacy, tmp) = test_backend_opts(false);
        legacy
            .set_secret("default", make_request("trashed-legacy", "v"))
            .await
            .unwrap();
        legacy
            .delete_secret("default", "trashed-legacy")
            .await
            .unwrap();

        // Legacy trash dir carries the percent name + original_name.
        let enc = encode_name("trashed-legacy");
        let tbase = trash_path(&tmp, "default");
        let legacy_dir = fs::read_dir(&tbase)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(&enc))
            .unwrap()
            .path();
        let dj: serde_json::Value =
            serde_json::from_slice(&fs::read(legacy_dir.join(".deleted.json")).unwrap()).unwrap();
        assert_eq!(
            dj.get("original_name").and_then(|v| v.as_str()),
            Some("trashed-legacy")
        );

        let opaque_backend = reopen_opaque(&tmp, false);
        opaque_backend.migrate_all(false).unwrap();

        // Trash dir renamed to an opaque stem; no name substring; no
        // original_name in .deleted.json.
        assert_no_name_leak(&tbase, "trashed-legacy");
        for entry in fs::read_dir(&tbase).unwrap().flatten() {
            let dj_path = entry.path().join(".deleted.json");
            if dj_path.exists() {
                let v: serde_json::Value =
                    serde_json::from_slice(&fs::read(&dj_path).unwrap()).unwrap();
                assert!(v.get("original_name").is_none());
            }
        }
        // Still recoverable by name.
        let deleted = opaque_backend
            .list_deleted_secrets("default")
            .await
            .unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].name, "trashed-legacy");
        opaque_backend
            .restore_secret("default", "trashed-legacy")
            .await
            .unwrap();
        assert_eq!(
            &*opaque_backend
                .get_secret("default", "trashed-legacy", true)
                .await
                .unwrap()
                .value
                .unwrap(),
            "v"
        );
    }

    #[tokio::test]
    async fn concurrent_set_stays_consistent_under_lock() {
        let (_backend, tmp) = test_backend_opaque();
        let b1 = reopen_opaque(&tmp, false);
        let b2 = reopen_opaque(&tmp, false);

        let (r1, r2) = tokio::join!(
            b1.set_secret("default", make_request("conc-one", "1")),
            b2.set_secret("default", make_request("conc-two", "2")),
        );
        r1.unwrap();
        r2.unwrap();

        let sdir = secrets_path(&tmp, "default");
        let index = opaque::load_index(&sdir, &b1.identity).unwrap();
        assert_eq!(index.len(), 2, "both secrets indexed");
        let listed = b1.list_secrets("default", None).await.unwrap();
        assert_eq!(listed.len(), 2);
    }

    /// Shared rename assertions, run against both store layouts.
    async fn assert_rename_roundtrip(backend: &LocalSecretBackend) {
        let mut req = make_request("old-name", "v1");
        req.groups = Some(vec!["team".to_string()]);
        req.note = Some("keep".to_string());
        req.folder = Some("proj".to_string());
        backend.set_secret("default", req).await.unwrap();

        let created = backend
            .rename_secret("default", "old-name", "new-name")
            .await
            .unwrap();
        assert_eq!(created.name, "new-name");

        let got = backend
            .get_secret("default", "new-name", true)
            .await
            .unwrap();
        assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("v1"));
        assert_eq!(got.tags.get("groups").map(String::as_str), Some("team"));
        assert_eq!(got.tags.get("note").map(String::as_str), Some("keep"));
        assert_eq!(got.tags.get("folder").map(String::as_str), Some("proj"));
        assert_eq!(got.original_name, "new-name");

        // Old name is out of the active set and waiting in trash.
        assert!(matches!(
            backend.get_secret("default", "old-name", false).await,
            Err(BackendError::NotFound { .. })
        ));
        let deleted = backend.list_deleted_secrets("default").await.unwrap();
        assert!(
            deleted.iter().any(|d| d.name == "old-name"),
            "old name must land in trash: {deleted:?}"
        );

        // Version history does not carry over — the new name starts fresh.
        let versions = backend.list_versions("default", "new-name").await.unwrap();
        assert_eq!(versions.len(), 1, "{versions:?}");
    }

    #[tokio::test]
    async fn rename_moves_secret_in_plaintext_store() {
        let (backend, _tmp) = test_backend();
        assert_rename_roundtrip(&backend).await;
    }

    #[tokio::test]
    async fn rename_moves_secret_in_opaque_store_without_leaking_names() {
        let (backend, tmp) = test_backend_opaque();
        assert_rename_roundtrip(&backend).await;

        // Opaque property holds after a rename: no on-disk filename under the
        // secrets dir contains either the old or the new secret name.
        let sdir = tmp.path().join("vaults").join("default").join("secrets");
        for entry in fs::read_dir(&sdir).unwrap().flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            assert!(
                !fname.contains("new-name") && !fname.contains("old-name"),
                "leaky stem after rename: {fname}"
            );
        }
    }
}
