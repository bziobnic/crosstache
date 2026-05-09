//! Cache data models
//!
//! Data structures for cache entries, keys, and status reporting.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};

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
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheEntryType {
    SecretsList,
    VaultList,
    FileList,
}

/// Identifies a cache entry and determines its file path.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
pub enum CacheKey {
    SecretsList { vault_name: String },
    VaultList,
    FileList { vault_name: String, recursive: bool },
}

/// Validate that a vault name is a single safe filesystem component.
///
/// Mirrors `validate_vault_name` in `backend::local::paths` but returns
/// `Result<(), String>` so the cache module stays independent of the backend
/// error types.
pub(crate) fn validate_cache_vault_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("vault name cannot be empty".to_string());
    }
    if name == "." || name == ".." {
        return Err("vault name must not be '.' or '..'".to_string());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("vault name must not contain path separators".to_string());
    }
    if name.starts_with('-') {
        return Err("vault name must not start with '-'".to_string());
    }
    if name.chars().any(|ch| ch.is_control()) {
        return Err("vault name must not contain control characters".to_string());
    }
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == name => Ok(()),
        _ => Err(
            "vault name must be a single path component without separators or prefixes".to_string(),
        ),
    }
}

/// Defence-in-depth: verify that `candidate` is a child of `base`.
fn ensure_cache_child_path(base: &Path, candidate: &Path) -> bool {
    candidate.starts_with(base)
}

/// Compute a safe fallback directory name for an invalid vault name.
///
/// The result is always a single safe component that cannot escape the cache
/// directory.
fn safe_fallback_dir(vault_name: &str) -> String {
    let hash = vault_name
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
    format!("_invalid_{hash:x}")
}

impl CacheKey {
    /// Resolve this key to its file path under the given cache directory.
    ///
    /// Vault names are validated before being used as path components.
    /// Invalid names are replaced with a deterministic safe fallback so the
    /// cache continues to function without escaping the cache root.
    pub fn to_path(&self, cache_dir: &Path) -> PathBuf {
        match self {
            CacheKey::SecretsList { vault_name } => {
                let dir_name = match validate_cache_vault_name(vault_name) {
                    Ok(()) => vault_name.clone(),
                    Err(_) => safe_fallback_dir(vault_name),
                };
                let candidate = cache_dir.join(&dir_name).join("secrets-list.json");
                if ensure_cache_child_path(cache_dir, &candidate) {
                    candidate
                } else {
                    cache_dir
                        .join(safe_fallback_dir(vault_name))
                        .join("secrets-list.json")
                }
            }
            CacheKey::VaultList => cache_dir.join("vaults-list.json"),
            CacheKey::FileList {
                vault_name,
                recursive,
            } => {
                let filename = if *recursive {
                    "files-list-recursive.json"
                } else {
                    "files-list.json"
                };
                let dir_name = match validate_cache_vault_name(vault_name) {
                    Ok(()) => vault_name.clone(),
                    Err(_) => safe_fallback_dir(vault_name),
                };
                let candidate = cache_dir.join(&dir_name).join(filename);
                if ensure_cache_child_path(cache_dir, &candidate) {
                    candidate
                } else {
                    cache_dir.join(safe_fallback_dir(vault_name)).join(filename)
                }
            }
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
            CacheKey::SecretsList { vault_name } | CacheKey::FileList { vault_name, .. } => {
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
            CacheKey::FileList {
                vault_name,
                recursive,
            } => {
                if *recursive {
                    write!(f, "files-recursive:{vault_name}")
                } else {
                    write!(f, "files:{vault_name}")
                }
            }
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
            validate_cache_vault_name(vault_name).map_err(|reason| {
                format!("Invalid vault name '{vault_name}' in cache key: {reason}")
            })?;
            return Ok(CacheKey::SecretsList {
                vault_name: vault_name.to_string(),
            });
        }
        if let Some(vault_name) = s.strip_prefix("files-recursive:") {
            if vault_name.is_empty() {
                return Err("Missing vault name after 'files-recursive:'".to_string());
            }
            validate_cache_vault_name(vault_name).map_err(|reason| {
                format!("Invalid vault name '{vault_name}' in cache key: {reason}")
            })?;
            return Ok(CacheKey::FileList {
                vault_name: vault_name.to_string(),
                recursive: true,
            });
        }
        if let Some(vault_name) = s.strip_prefix("files:") {
            if vault_name.is_empty() {
                return Err("Missing vault name after 'files:'".to_string());
            }
            validate_cache_vault_name(vault_name).map_err(|reason| {
                format!("Invalid vault name '{vault_name}' in cache key: {reason}")
            })?;
            return Ok(CacheKey::FileList {
                vault_name: vault_name.to_string(),
                recursive: false,
            });
        }
        Err(format!(
            "Invalid cache key: '{s}'. Expected 'secrets:<vault>', 'vaults', 'files:<vault>', or 'files-recursive:<vault>'"
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
    #[allow(dead_code)]
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
                vault_name: "myvault".to_string(),
                recursive: false,
            }
            .to_string(),
            "files:myvault"
        );
        assert_eq!(
            CacheKey::FileList {
                vault_name: "myvault".to_string(),
                recursive: true,
            }
            .to_string(),
            "files-recursive:myvault"
        );
    }

    #[test]
    fn test_cache_key_from_str() {
        let key: CacheKey = "secrets:myvault".parse().unwrap();
        assert!(matches!(key, CacheKey::SecretsList { vault_name } if vault_name == "myvault"));

        let key: CacheKey = "vaults".parse().unwrap();
        assert!(matches!(key, CacheKey::VaultList));

        let key: CacheKey = "files:myvault".parse().unwrap();
        assert!(
            matches!(key, CacheKey::FileList { vault_name, recursive } if vault_name == "myvault" && !recursive)
        );

        let key: CacheKey = "files-recursive:myvault".parse().unwrap();
        assert!(
            matches!(key, CacheKey::FileList { vault_name, recursive } if vault_name == "myvault" && recursive)
        );
    }

    #[test]
    fn test_cache_key_from_str_invalid() {
        assert!("invalid".parse::<CacheKey>().is_err());
        assert!("secrets:".parse::<CacheKey>().is_err());
        assert!("unknown:vault".parse::<CacheKey>().is_err());
    }

    #[test]
    fn test_cache_key_from_str_rejects_traversal_vault_names() {
        // Path traversal
        assert!("secrets:../outside".parse::<CacheKey>().is_err());
        assert!("secrets:../../etc".parse::<CacheKey>().is_err());
        assert!("files:../outside".parse::<CacheKey>().is_err());
        assert!("files-recursive:../../etc".parse::<CacheKey>().is_err());

        // Separators
        assert!("secrets:foo/bar".parse::<CacheKey>().is_err());
        assert!("secrets:foo\\bar".parse::<CacheKey>().is_err());

        // Special names
        assert!("secrets:.".parse::<CacheKey>().is_err());
        assert!("secrets:..".parse::<CacheKey>().is_err());

        // Absolute
        assert!("secrets:/absolute".parse::<CacheKey>().is_err());

        // Control characters
        assert!("secrets:bad\nname".parse::<CacheKey>().is_err());
        assert!("secrets:bad\x00name".parse::<CacheKey>().is_err());

        // Leading dash
        assert!("secrets:-badname".parse::<CacheKey>().is_err());
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
            recursive: false,
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/myvault/files-list.json")
        );

        let key = CacheKey::FileList {
            vault_name: "myvault".to_string(),
            recursive: true,
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/myvault/files-list-recursive.json")
        );
    }

    #[test]
    fn test_to_path_never_escapes_cache_dir_for_adversarial_names() {
        let base = PathBuf::from("/cache");

        // These vault names are invalid and should be replaced with a safe
        // fallback. Critically, the result must always be under /cache/.
        let adversarial = [
            "../..",
            "../../etc",
            "foo/bar",
            "/absolute",
            "\\absolute",
            "..",
            ".",
            "bad\nname",
            "bad\x00name",
            "-leading",
        ];

        for bad_name in adversarial {
            let key = CacheKey::SecretsList {
                vault_name: bad_name.to_string(),
            };
            let path = key.to_path(&base);
            assert!(
                path.starts_with(&base),
                "to_path escaped cache_dir for vault name {bad_name:?}: {}",
                path.display()
            );

            let key = CacheKey::FileList {
                vault_name: bad_name.to_string(),
                recursive: true,
            };
            let path = key.to_path(&base);
            assert!(
                path.starts_with(&base),
                "to_path escaped cache_dir for vault name {bad_name:?}: {}",
                path.display()
            );
        }
    }

    #[test]
    fn test_validate_cache_vault_name_accepts_valid() {
        for name in ["default", "work-secrets", "team_1", "Vault123"] {
            assert!(
                validate_cache_vault_name(name).is_ok(),
                "should accept {name:?}"
            );
        }
    }

    #[test]
    fn test_validate_cache_vault_name_rejects_invalid() {
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "../../outside",
            "outside/child",
            "/absolute",
            "\\absolute",
            "parent\\child",
            "bad\nname",
            "-leading",
        ] {
            assert!(
                validate_cache_vault_name(name).is_err(),
                "should reject {name:?}"
            );
        }
    }
}
