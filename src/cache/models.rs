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

/// On-disk filename for the secrets-list cache entry, versioned in the name
/// itself so a schema change to the cached payload (`SecretSummary`) makes
/// old entries simply miss instead of deserializing into wrong/incomplete
/// data.
///
/// Bumped v1 -> v2 for the record-types plan (Task 10): `SecretSummary`
/// gained a `tags` field with `#[serde(default)]`, so a pre-existing v1
/// cache entry written before that change would deserialize successfully
/// but with an EMPTY `tags` map — silently hiding every typed secret's
/// `xv-type`/`f.*` tags from `ls --type` and the `ls` TYPE column until the
/// entry's TTL expired (Bugbot review: "a cache hit hides typed secrets").
/// Renaming the file (rather than trying to detect "empty tags" as stale)
/// is deliberate: legitimately untagged secrets exist, so an empty map is
/// not itself evidence of staleness. Bump this suffix again on any future
/// change to `SecretSummary`'s shape.
///
/// Bumped v2 -> v3 for the multi-vault workspaces plan (Phase B, Task 7):
/// `CacheKey::SecretsList` gained a `backend` field so union `ls` caches one
/// entry per `(backend, vault)` pair instead of `vault` alone — two
/// workspace entries that happen to share a vault NAME on different
/// backends (e.g. `local-a`'s "default" and `local-b`'s "default") would
/// otherwise silently collide on the same cache file. The `v3` directory
/// layout also nests under a `backend` component (see `to_path` below), so a
/// pre-existing `v2`-era entry (no backend component in its path) simply
/// misses rather than being read by the wrong backend's listing.
pub(crate) const SECRETS_LIST_FILENAME: &str = "secrets-list-v3.json";

/// Identifies a cache entry and determines its file path.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
pub enum CacheKey {
    SecretsList {
        backend: String,
        vault_name: String,
    },
    VaultList,
    FileList {
        backend: String,
        vault_name: String,
        recursive: bool,
    },
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
            CacheKey::SecretsList {
                backend,
                vault_name,
            } => {
                // Nested under a `backend` component (new in v3) so two
                // workspace entries sharing a vault NAME on different
                // backends never collide on the same cache file.
                let backend_dir = match validate_cache_vault_name(backend) {
                    Ok(()) => backend.clone(),
                    Err(_) => safe_fallback_dir(backend),
                };
                let vault_dir = match validate_cache_vault_name(vault_name) {
                    Ok(()) => vault_name.clone(),
                    Err(_) => safe_fallback_dir(vault_name),
                };
                let candidate = cache_dir
                    .join(&backend_dir)
                    .join(&vault_dir)
                    .join(SECRETS_LIST_FILENAME);
                if ensure_cache_child_path(cache_dir, &candidate) {
                    candidate
                } else {
                    cache_dir
                        .join(safe_fallback_dir(backend))
                        .join(safe_fallback_dir(vault_name))
                        .join(SECRETS_LIST_FILENAME)
                }
            }
            CacheKey::VaultList => cache_dir.join("vaults-list.json"),
            CacheKey::FileList {
                backend,
                vault_name,
                recursive,
            } => {
                let filename = if *recursive {
                    "files-list-recursive.json"
                } else {
                    "files-list.json"
                };
                // Nested under `backend` (like SecretsList) so two workspace
                // entries sharing a vault NAME on different backends never
                // collide on one cache file.
                let backend_dir = match validate_cache_vault_name(backend) {
                    Ok(()) => backend.clone(),
                    Err(_) => safe_fallback_dir(backend),
                };
                let dir_name = match validate_cache_vault_name(vault_name) {
                    Ok(()) => vault_name.clone(),
                    Err(_) => safe_fallback_dir(vault_name),
                };
                let candidate = cache_dir.join(&backend_dir).join(&dir_name).join(filename);
                if ensure_cache_child_path(cache_dir, &candidate) {
                    candidate
                } else {
                    cache_dir
                        .join(safe_fallback_dir(backend))
                        .join(safe_fallback_dir(vault_name))
                        .join(filename)
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
            CacheKey::SecretsList { vault_name, .. } | CacheKey::FileList { vault_name, .. } => {
                Some(vault_name)
            }
            CacheKey::VaultList => None,
        }
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheKey::SecretsList {
                backend,
                vault_name,
            } => write!(f, "secrets:{backend}:{vault_name}"),
            CacheKey::VaultList => write!(f, "vaults"),
            CacheKey::FileList {
                backend,
                vault_name,
                recursive,
            } => {
                if *recursive {
                    write!(f, "files-recursive:{backend}:{vault_name}")
                } else {
                    write!(f, "files:{backend}:{vault_name}")
                }
            }
        }
    }
}

/// Parse the `<backend>:<vault>` tail of a `files:`/`files-recursive:` cache
/// key, mirroring the `secrets:<backend>:<vault>` (v3) shape.
fn parse_backend_vault(kind: &str, rest: &str) -> std::result::Result<(String, String), String> {
    let (backend, vault_name) = rest.split_once(':').ok_or_else(|| {
        format!("Invalid cache key: '{kind}:{rest}'. Expected '{kind}:<backend>:<vault>'")
    })?;
    if backend.is_empty() || vault_name.is_empty() {
        return Err(format!(
            "Invalid cache key: '{kind}:{rest}'. Expected '{kind}:<backend>:<vault>'"
        ));
    }
    validate_cache_vault_name(backend)
        .map_err(|reason| format!("Invalid backend name '{backend}' in cache key: {reason}"))?;
    validate_cache_vault_name(vault_name)
        .map_err(|reason| format!("Invalid vault name '{vault_name}' in cache key: {reason}"))?;
    Ok((backend.to_string(), vault_name.to_string()))
}

impl std::str::FromStr for CacheKey {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s == "vaults" {
            return Ok(CacheKey::VaultList);
        }
        if let Some(rest) = s.strip_prefix("secrets:") {
            if rest.is_empty() {
                return Err("Missing vault name after 'secrets:'".to_string());
            }
            // `secrets:<backend>:<vault>` (v3). A pre-v3 key with no `:`
            // separator (`secrets:<vault>`) is rejected rather than guessed
            // at — every producer of this string (`CacheKey::to_string`,
            // `xv cache clear <key>`) writes the v3 shape.
            let (backend, vault_name) = rest.split_once(':').ok_or_else(|| {
                format!("Invalid cache key: 'secrets:{rest}'. Expected 'secrets:<backend>:<vault>'")
            })?;
            if backend.is_empty() || vault_name.is_empty() {
                return Err(format!(
                    "Invalid cache key: 'secrets:{rest}'. Expected 'secrets:<backend>:<vault>'"
                ));
            }
            validate_cache_vault_name(backend).map_err(|reason| {
                format!("Invalid backend name '{backend}' in cache key: {reason}")
            })?;
            validate_cache_vault_name(vault_name).map_err(|reason| {
                format!("Invalid vault name '{vault_name}' in cache key: {reason}")
            })?;
            return Ok(CacheKey::SecretsList {
                backend: backend.to_string(),
                vault_name: vault_name.to_string(),
            });
        }
        if let Some(rest) = s.strip_prefix("files-recursive:") {
            if rest.is_empty() {
                return Err("Missing backend/vault after 'files-recursive:'".to_string());
            }
            let (backend, vault_name) = parse_backend_vault("files-recursive", rest)?;
            return Ok(CacheKey::FileList {
                backend,
                vault_name,
                recursive: true,
            });
        }
        if let Some(rest) = s.strip_prefix("files:") {
            if rest.is_empty() {
                return Err("Missing backend/vault after 'files:'".to_string());
            }
            let (backend, vault_name) = parse_backend_vault("files", rest)?;
            return Ok(CacheKey::FileList {
                backend,
                vault_name,
                recursive: false,
            });
        }
        Err(format!(
            "Invalid cache key: '{s}'. Expected 'secrets:<backend>:<vault>', 'vaults', 'files:<backend>:<vault>', or 'files-recursive:<backend>:<vault>'"
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
                backend: "azure".to_string(),
                vault_name: "myvault".to_string()
            }
            .to_string(),
            "secrets:azure:myvault"
        );
        assert_eq!(CacheKey::VaultList.to_string(), "vaults");
        assert_eq!(
            CacheKey::FileList {
                backend: "azure".to_string(),
                vault_name: "myvault".to_string(),
                recursive: false,
            }
            .to_string(),
            "files:azure:myvault"
        );
        assert_eq!(
            CacheKey::FileList {
                backend: "azure".to_string(),
                vault_name: "myvault".to_string(),
                recursive: true,
            }
            .to_string(),
            "files-recursive:azure:myvault"
        );
    }

    #[test]
    fn test_cache_key_from_str() {
        let key: CacheKey = "secrets:azure:myvault".parse().unwrap();
        assert!(
            matches!(key, CacheKey::SecretsList { backend, vault_name } if backend == "azure" && vault_name == "myvault")
        );

        let key: CacheKey = "vaults".parse().unwrap();
        assert!(matches!(key, CacheKey::VaultList));

        let key: CacheKey = "files:azure:myvault".parse().unwrap();
        assert!(
            matches!(key, CacheKey::FileList { backend, vault_name, recursive } if backend == "azure" && vault_name == "myvault" && !recursive)
        );

        let key: CacheKey = "files-recursive:azure:myvault".parse().unwrap();
        assert!(
            matches!(key, CacheKey::FileList { backend, vault_name, recursive } if backend == "azure" && vault_name == "myvault" && recursive)
        );
    }

    #[test]
    fn test_cache_key_from_str_invalid() {
        assert!("invalid".parse::<CacheKey>().is_err());
        assert!("secrets:".parse::<CacheKey>().is_err());
        assert!("unknown:vault".parse::<CacheKey>().is_err());
        assert!("secrets::myvault".parse::<CacheKey>().is_err());
        assert!("secrets:azure:".parse::<CacheKey>().is_err());
    }

    /// v2-era single-segment `secrets:<vault>` keys (pre-Phase-B, no
    /// `backend` component) must fail to parse rather than being guessed at
    /// — the v2 -> v3 cache-schema bump's whole point is that pre-existing
    /// entries miss cleanly instead of being misread. `xv cache clear` is
    /// the only producer of this string form and always writes the v3
    /// `secrets:<backend>:<vault>` shape.
    #[test]
    fn test_cache_key_from_str_rejects_pre_workspace_v2_shape() {
        let err = "secrets:myvault".parse::<CacheKey>().unwrap_err();
        assert!(err.contains("backend"), "{err}");
    }

    #[test]
    fn test_cache_key_from_str_rejects_traversal_vault_names() {
        // Path traversal
        assert!("secrets:../outside".parse::<CacheKey>().is_err());
        assert!("secrets:../../etc".parse::<CacheKey>().is_err());
        assert!("files:azure:../outside".parse::<CacheKey>().is_err());
        assert!("files-recursive:azure:../../etc"
            .parse::<CacheKey>()
            .is_err());

        // Separators (in the vault segment, backend valid)
        assert!("secrets:azure:foo/bar".parse::<CacheKey>().is_err());
        assert!("secrets:azure:foo\\bar".parse::<CacheKey>().is_err());
        // Separators (in the backend segment)
        assert!("secrets:foo/bar:vault".parse::<CacheKey>().is_err());

        // Special names
        assert!("secrets:azure:.".parse::<CacheKey>().is_err());
        assert!("secrets:azure:..".parse::<CacheKey>().is_err());

        // Absolute
        assert!("secrets:azure:/absolute".parse::<CacheKey>().is_err());

        // Control characters
        assert!("secrets:azure:bad\nname".parse::<CacheKey>().is_err());
        assert!("secrets:azure:bad\x00name".parse::<CacheKey>().is_err());

        // Leading dash
        assert!("secrets:azure:-badname".parse::<CacheKey>().is_err());
    }

    #[test]
    fn test_cache_key_to_path() {
        let base = PathBuf::from("/cache");

        let key = CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "myvault".to_string(),
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/azure/myvault/secrets-list-v3.json")
        );

        let key = CacheKey::VaultList;
        assert_eq!(key.to_path(&base), PathBuf::from("/cache/vaults-list.json"));

        let key = CacheKey::FileList {
            backend: "azure".to_string(),
            vault_name: "myvault".to_string(),
            recursive: false,
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/azure/myvault/files-list.json")
        );

        let key = CacheKey::FileList {
            backend: "azure".to_string(),
            vault_name: "myvault".to_string(),
            recursive: true,
        };
        assert_eq!(
            key.to_path(&base),
            PathBuf::from("/cache/azure/myvault/files-list-recursive.json")
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
                backend: "azure".to_string(),
                vault_name: bad_name.to_string(),
            };
            let path = key.to_path(&base);
            assert!(
                path.starts_with(&base),
                "to_path escaped cache_dir for vault name {bad_name:?}: {}",
                path.display()
            );

            // Adversarial backend segment too.
            let key = CacheKey::SecretsList {
                backend: bad_name.to_string(),
                vault_name: "myvault".to_string(),
            };
            let path = key.to_path(&base);
            assert!(
                path.starts_with(&base),
                "to_path escaped cache_dir for backend name {bad_name:?}: {}",
                path.display()
            );

            let key = CacheKey::FileList {
                backend: "azure".to_string(),
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
