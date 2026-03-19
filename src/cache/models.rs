//! Cache data models
//!
//! Data structures for cache entries, keys, and status reporting.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// A cached response stored on disk as JSON.
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: DeserializeOwned"))]
pub struct CacheEntry<T: Serialize + DeserializeOwned> {
    pub created_at: DateTime<Utc>,
    pub ttl_secs: u64,
    pub vault_name: Option<String>,
    pub entry_type: CacheEntryType,
    pub data: T,
}

/// The type of listing operation that produced this cache entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheEntryType {
    SecretsList,
    VaultList,
    FileList,
}

/// Identifies a cache entry and determines its file path.
#[derive(Debug, Clone)]
pub enum CacheKey {
    SecretsList { vault_name: String },
    VaultList,
    FileList { vault_name: String },
}

impl CacheKey {
    /// Resolve this key to its file path under the given cache directory.
    pub fn to_path(&self, cache_dir: &PathBuf) -> PathBuf {
        match self {
            CacheKey::SecretsList { vault_name } => {
                cache_dir.join(vault_name).join("secrets-list.json")
            }
            CacheKey::VaultList => cache_dir.join("vaults-list.json"),
            CacheKey::FileList { vault_name } => cache_dir.join(vault_name).join("files-list.json"),
        }
    }

    /// Return the CacheEntryType for this key.
    pub fn entry_type(&self) -> CacheEntryType {
        match self {
            CacheKey::SecretsList { .. } => CacheEntryType::SecretsList,
            CacheKey::VaultList => CacheEntryType::VaultList,
            CacheKey::FileList { .. } => CacheEntryType::FileList,
        }
    }

    /// Return the vault name if this key is vault-scoped.
    pub fn vault_name(&self) -> Option<&str> {
        match self {
            CacheKey::SecretsList { vault_name } | CacheKey::FileList { vault_name } => {
                Some(vault_name)
            }
            CacheKey::VaultList => None,
        }
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheKey::SecretsList { vault_name } => write!(f, "secrets:{vault_name}"),
            CacheKey::VaultList => write!(f, "vaults"),
            CacheKey::FileList { vault_name } => write!(f, "files:{vault_name}"),
        }
    }
}

impl std::str::FromStr for CacheKey {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "vaults" {
            return Ok(CacheKey::VaultList);
        }
        if let Some(vault_name) = s.strip_prefix("secrets:") {
            if vault_name.is_empty() {
                return Err("Missing vault name after 'secrets:'".to_string());
            }
            return Ok(CacheKey::SecretsList {
                vault_name: vault_name.to_string(),
            });
        }
        if let Some(vault_name) = s.strip_prefix("files:") {
            if vault_name.is_empty() {
                return Err("Missing vault name after 'files:'".to_string());
            }
            return Ok(CacheKey::FileList {
                vault_name: vault_name.to_string(),
            });
        }
        Err(format!(
            "Invalid cache key: '{s}'. Expected 'secrets:<vault>', 'vaults', or 'files:<vault>'"
        ))
    }
}

/// Summary of the cache state, returned by `xv cache status`.
#[derive(Debug)]
pub struct CacheStatus {
    pub cache_dir: PathBuf,
    pub enabled: bool,
    pub ttl_secs: u64,
    pub entry_count: usize,
    pub total_size_bytes: u64,
    pub entries: Vec<CacheEntryInfo>,
}

/// Metadata about a single cache entry file.
#[derive(Debug)]
pub struct CacheEntryInfo {
    pub key: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub is_stale: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_display() {
        assert_eq!(
            CacheKey::SecretsList {
                vault_name: "myvault".to_string()
            }
            .to_string(),
            "secrets:myvault"
        );
        assert_eq!(CacheKey::VaultList.to_string(), "vaults");
        assert_eq!(
            CacheKey::FileList {
                vault_name: "myvault".to_string()
            }
            .to_string(),
            "files:myvault"
        );
    }

    #[test]
    fn test_cache_key_from_str() {
        let key: CacheKey = "secrets:myvault".parse().unwrap();
        assert!(matches!(key, CacheKey::SecretsList { vault_name } if vault_name == "myvault"));

        let key: CacheKey = "vaults".parse().unwrap();
        assert!(matches!(key, CacheKey::VaultList));

        let key: CacheKey = "files:myvault".parse().unwrap();
        assert!(matches!(key, CacheKey::FileList { vault_name } if vault_name == "myvault"));
    }

    #[test]
    fn test_cache_key_from_str_invalid() {
        assert!("invalid".parse::<CacheKey>().is_err());
        assert!("secrets:".parse::<CacheKey>().is_err());
        assert!("unknown:vault".parse::<CacheKey>().is_err());
    }

    #[test]
    fn test_cache_key_to_path() {
        let base = PathBuf::from("/cache");

        let key = CacheKey::SecretsList {
            vault_name: "myvault".to_string(),
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/myvault/secrets-list.json")
        );

        let key = CacheKey::VaultList;
        assert_eq!(key.to_path(&base), PathBuf::from("/cache/vaults-list.json"));

        let key = CacheKey::FileList {
            vault_name: "myvault".to_string(),
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/myvault/files-list.json")
        );
    }
}
