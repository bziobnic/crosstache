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

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::backend::error::BackendError;
use crate::backend::secret::SecretBackend;
use crate::secret::manager::{SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest};

use super::crypto;

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
#[allow(dead_code)] // Used in tests and will be needed for list operations in future PRs.
fn decode_name(encoded: &str) -> String {
    url::form_urlencoded::parse(encoded.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect()
}

fn secrets_dir(store_path: &Path, vault: &str) -> PathBuf {
    store_path.join("vaults").join(vault).join("secrets")
}

fn age_path(store_path: &Path, vault: &str, name: &str) -> PathBuf {
    let enc = encode_name(name);
    secrets_dir(store_path, vault).join(format!("{enc}.age"))
}

fn meta_path(store_path: &Path, vault: &str, name: &str) -> PathBuf {
    let enc = encode_name(name);
    secrets_dir(store_path, vault).join(format!("{enc}.meta.json"))
}

fn versions_dir(store_path: &Path, vault: &str, name: &str) -> PathBuf {
    let enc = encode_name(name);
    secrets_dir(store_path, vault).join(".versions").join(enc)
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn meta_to_properties(meta: &SecretMeta, value: Option<Zeroizing<String>>) -> SecretProperties {
    let version_num = meta
        .version
        .strip_prefix('v')
        .and_then(|s| s.parse::<u32>().ok());

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
        tags: meta.tags.clone(),
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
    fs::write(path, json)
        .map_err(|e| BackendError::Internal(format!("write meta {}: {e}", path.display())))
}

/// Determine the next version number by scanning `.versions/<name>/`.
fn next_version(store_path: &Path, vault: &str, name: &str) -> u32 {
    let vdir = versions_dir(store_path, vault, name);
    if !vdir.exists() {
        return 1;
    }
    let mut max: u32 = 0;
    if let Ok(entries) = fs::read_dir(&vdir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if let Some(rest) = fname.strip_prefix('v') {
                if let Some(num_str) = rest.split('.').next() {
                    if let Ok(n) = num_str.parse::<u32>() {
                        max = max.max(n);
                    }
                }
            }
        }
    }
    max + 1
}

/// Archive the current secret to `.versions/<name>/v<N>.*`.
fn archive_current(store_path: &Path, vault: &str, name: &str) -> Result<u32, BackendError> {
    let ap = age_path(store_path, vault, name);
    let mp = meta_path(store_path, vault, name);

    if !ap.exists() && !mp.exists() {
        return Ok(1);
    }

    let ver = next_version(store_path, vault, name);
    let vdir = versions_dir(store_path, vault, name);
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

        // Validate vault exists
        let vault_json = store.join("vaults").join(vault).join(".vault.json");
        if !vault_json.exists() {
            return Err(BackendError::VaultNotFound {
                name: vault.to_string(),
                suggestion: None,
            });
        }

        let sdir = secrets_dir(&store, vault);
        fs::create_dir_all(&sdir)
            .map_err(|e| BackendError::Internal(format!("mkdir secrets: {e}")))?;

        let name = request.name.clone();
        let ap = age_path(&store, vault, &name);
        let mp = meta_path(&store, vault, &name);

        // If secret already exists, archive old version.
        let version = if mp.exists() {
            let old_meta = read_meta(&mp)?;
            let archived_ver = archive_current(&store, vault, &name)?;
            // new version = old version number + 1
            let _archived_ver = archived_ver; // already moved
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

        // Encrypt value and write files (blocking I/O).
        let _identity = identity;
        crypto::encrypt_to_file(&ap, request.value.as_bytes(), &recipients)?;
        write_meta(&mp, &meta)?;

        Ok(meta_to_properties(&meta, None))
    }

    async fn get_secret(
        &self,
        vault: &str,
        name: &str,
        include_value: bool,
    ) -> Result<SecretProperties, BackendError> {
        let mp = meta_path(&self.store_path, vault, name);
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        let meta = read_meta(&mp)?;

        let value = if include_value {
            let ap = age_path(&self.store_path, vault, name);
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
        let mp = meta_path(&self.store_path, vault, name);
        if mp.exists() {
            let meta = read_meta(&mp)?;
            if meta.version == version {
                return self.get_secret(vault, name, include_value).await;
            }
        }

        // Look in .versions/
        let vdir = versions_dir(&self.store_path, vault, name);
        let meta_file = vdir.join(format!("{version}.meta.json"));
        if !meta_file.exists() {
            return Err(BackendError::NotFound {
                name: format!("{name}@{version}"),
                suggestion: None,
            });
        }

        let meta = read_meta(&meta_file)?;

        let value = if include_value {
            let age_file = vdir.join(format!("{version}.age"));
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
        let sdir = secrets_dir(&self.store_path, vault);
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
                Err(_) => continue,
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
        let mp = meta_path(&self.store_path, vault, name);
        let ap = age_path(&self.store_path, vault, name);

        if !mp.exists() && !ap.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        // Remove current files
        if ap.exists() {
            fs::remove_file(&ap).map_err(|e| BackendError::Internal(format!("remove age: {e}")))?;
        }
        if mp.exists() {
            fs::remove_file(&mp)
                .map_err(|e| BackendError::Internal(format!("remove meta: {e}")))?;
        }

        // Remove version history
        let vdir = versions_dir(&self.store_path, vault, name);
        if vdir.exists() {
            fs::remove_dir_all(&vdir)
                .map_err(|e| BackendError::Internal(format!("remove versions: {e}")))?;
        }

        Ok(())
    }

    async fn update_secret(
        &self,
        vault: &str,
        name: &str,
        request: SecretUpdateRequest,
    ) -> Result<SecretProperties, BackendError> {
        let mp = meta_path(&self.store_path, vault, name);
        if !mp.exists() {
            return Err(BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            });
        }

        let mut meta = read_meta(&mp)?;
        let now = Utc::now();

        // If value is being updated, archive old and re-encrypt.
        if let Some(ref new_value) = request.value {
            archive_current(&self.store_path, vault, name)?;

            let old_num: u32 = meta
                .version
                .strip_prefix('v')
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            meta.version = format!("v{}", old_num + 1);

            let ap = age_path(&self.store_path, vault, name);
            crypto::encrypt_to_file(&ap, new_value.as_bytes(), &self.recipients)?;
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
        let vdir = versions_dir(&self.store_path, vault, name);
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
        let mp = meta_path(&self.store_path, vault, name);
        if mp.exists() {
            let meta = read_meta(&mp)?;
            versions.push(meta_to_properties(&meta, None));
        }

        // Sort by version number
        versions.sort_by_key(|v| v.version_number.unwrap_or(0));

        // Set sequential version numbers (1-based)
        for (i, v) in versions.iter_mut().enumerate() {
            v.version_number = Some(i as u32 + 1);
        }

        Ok(versions)
    }

    async fn secret_exists(&self, vault: &str, name: &str) -> Result<bool, BackendError> {
        let mp = meta_path(&self.store_path, vault, name);
        Ok(mp.exists())
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

        assert!(!backend.secret_exists("default", "to-delete").await.unwrap());
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
