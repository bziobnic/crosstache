//! Integration tests for the cache module.
//!
//! These tests verify CacheManager behavior without requiring Azure credentials.

use crosstache::cache::{CacheKey, CacheManager};
use tempfile::TempDir;

#[test]
fn test_cache_roundtrip_secrets_list() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "test-vault".to_string(),
    };
    let data = vec![
        serde_json::json!({"name": "secret1", "updated_on": "2026-03-19"}),
        serde_json::json!({"name": "secret2", "updated_on": "2026-03-18"}),
    ];
    mgr.set(&key, &data);
    let cached: Option<Vec<serde_json::Value>> = mgr.get(&key);
    assert!(cached.is_some());
    assert_eq!(cached.unwrap().len(), 2);
}

#[test]
fn test_cache_roundtrip_vault_list() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::VaultList;
    let data = vec![serde_json::json!({"name": "vault1", "location": "eastus"})];
    mgr.set(&key, &data);
    let cached: Option<Vec<serde_json::Value>> = mgr.get(&key);
    assert!(cached.is_some());
    assert_eq!(cached.unwrap().len(), 1);
}

#[test]
fn test_cache_no_cache_flag_behavior() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "test-vault".to_string(),
    };
    mgr.set(&key, &vec!["data".to_string()]);
    let path = key.to_path(dir.path());
    assert!(path.exists());
}

#[test]
fn test_cache_disabled_behavior() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), false, 900);
    let key = CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "test-vault".to_string(),
    };
    mgr.set(&key, &vec!["data".to_string()]);
    let path = key.to_path(dir.path());
    assert!(!path.exists());
    let result: Option<Vec<String>> = mgr.get(&key);
    assert!(result.is_none());
}

#[test]
fn test_cache_clear_specific_vault() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    mgr.set(
        &CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "vault-a".to_string(),
        },
        &vec!["a".to_string()],
    );
    mgr.set(
        &CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "vault-b".to_string(),
        },
        &vec!["b".to_string()],
    );
    mgr.clear(Some("vault-a"));
    let a: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "vault-a".to_string(),
    });
    let b: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "vault-b".to_string(),
    });
    assert!(a.is_none());
    assert!(b.is_some());
}

#[test]
fn test_cache_clear_all() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    mgr.set(
        &CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "vault-a".to_string(),
        },
        &vec!["a".to_string()],
    );
    mgr.set(&CacheKey::VaultList, &vec!["v".to_string()]);
    mgr.clear(None);
    let a: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "vault-a".to_string(),
    });
    let v: Option<Vec<String>> = mgr.get(&CacheKey::VaultList);
    assert!(a.is_none());
    assert!(v.is_none());
}

#[test]
fn test_cache_invalidation() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let key = CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "v1".to_string(),
    };
    mgr.set(&key, &vec!["data".to_string()]);
    assert!(mgr.get::<Vec<String>>(&key).is_some());
    mgr.invalidate(&key);
    assert!(mgr.get::<Vec<String>>(&key).is_none());
}

#[test]
fn test_cache_invalidate_vault_removes_all_entries() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    mgr.set(
        &CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "v1".to_string(),
        },
        &vec!["s".to_string()],
    );
    mgr.set(
        &CacheKey::FileList {
            backend: "azure".to_string(),
            vault_name: "v1".to_string(),
            recursive: false,
        },
        &vec!["f".to_string()],
    );
    mgr.invalidate_vault("v1");
    let s: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "v1".to_string(),
    });
    let f: Option<Vec<String>> = mgr.get(&CacheKey::FileList {
        backend: "azure".to_string(),
        vault_name: "v1".to_string(),
        recursive: false,
    });
    assert!(s.is_none());
    assert!(f.is_none());
}

#[test]
fn test_cache_status() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 900);
    let status = mgr.status();
    assert_eq!(status.entry_count, 0);
    assert_eq!(status.total_size_bytes, 0);
    mgr.set(&CacheKey::VaultList, &vec!["v".to_string()]);
    mgr.set(
        &CacheKey::SecretsList {
            backend: "azure".to_string(),
            vault_name: "v1".to_string(),
        },
        &vec!["s".to_string()],
    );
    let status = mgr.status();
    assert_eq!(status.entry_count, 2);
    assert!(status.total_size_bytes > 0);
    assert!(status.enabled);
    assert_eq!(status.ttl_secs, 900);
}

#[test]
fn test_cache_ttl_zero_expires_immediately() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), true, 0);
    let key = CacheKey::VaultList;
    mgr.set(&key, &vec!["data".to_string()]);
    // TTL=0 means age >= ttl_secs is always true, so entry is expired
    let result: Option<Vec<String>> = mgr.get(&key);
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// v5 hardening — private modes and no-follow I/O (Task 1)
// ---------------------------------------------------------------------------

/// After a `set`, the entry file is mode 0600 and every directory from the
/// cache root down to the entry's parent is 0700.
#[cfg(unix)]
#[test]
fn test_cache_set_creates_private_dirs_and_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let mgr = CacheManager::new(root.clone(), true, 900);
    let key = CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "vault-x".to_string(),
    };
    mgr.set(&key, &vec!["s1".to_string()]);

    let entry = key.to_path(&root);
    assert!(entry.exists(), "entry file should have been written");

    let file_mode = std::fs::metadata(&entry).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        file_mode, 0o600,
        "entry file must be 0600, got {file_mode:o}"
    );

    let mut d = entry.parent().unwrap().to_path_buf();
    loop {
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o700,
            "dir {} must be 0700, got {mode:o}",
            d.display()
        );
        if d == root {
            break;
        }
        d = d.parent().unwrap().to_path_buf();
    }
}

/// A symlink pre-planted at the final entry path must not be written through:
/// the outside target file must never come into existence.
#[cfg(unix)]
#[test]
fn test_cache_set_refuses_symlinked_entry_path() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let mgr = CacheManager::new(root.clone(), true, 900);
    let key = CacheKey::SecretsList {
        backend: "azure".to_string(),
        vault_name: "vault-y".to_string(),
    };

    let entry = key.to_path(&root);
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    let outside = dir.path().join("outside-secret.txt");
    std::os::unix::fs::symlink(&outside, &entry).unwrap();

    mgr.set(&key, &vec!["should-not-be-written".to_string()]);

    assert!(
        !outside.exists(),
        "set() must not write through a symlinked entry path to an outside file"
    );
}

// ---------------------------------------------------------------------------
// v5 hardening — tightening pre-existing loose cache trees (Task 2)
// ---------------------------------------------------------------------------

/// A cache tree created before v5 (dirs 0755, files 0644) is tightened to
/// 0700/0600 on first construction of a manager over it.
#[cfg(unix)]
#[test]
fn test_cache_tightens_preexisting_loose_tree_on_first_use() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let root = dir.path().join("cacheroot");
    let backend_dir = root.join("azure");
    let vault_dir = backend_dir.join("v1");
    std::fs::create_dir_all(&vault_dir).unwrap();

    let set = |p: &std::path::Path, m: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    };
    let json = vault_dir.join("secrets-list-v5.json");
    std::fs::write(&json, b"{}").unwrap();
    let lock = vault_dir.join("secrets-list-v5.lock");
    std::fs::write(&lock, b"").unwrap();
    // Loosen everything to simulate a pre-v5 world-readable tree.
    set(&json, 0o644);
    set(&lock, 0o644);
    set(&vault_dir, 0o755);
    set(&backend_dir, 0o755);
    set(&root, 0o755);

    // Construction triggers the run-once tighten walk for this root.
    let _mgr = CacheManager::new(root.clone(), true, 900);

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700, "root dir must be tightened to 0700");
    assert_eq!(mode(&backend_dir), 0o700, "backend dir must be tightened");
    assert_eq!(mode(&vault_dir), 0o700, "vault dir must be tightened");
    assert_eq!(mode(&json), 0o600, ".json entry must be tightened to 0600");
    assert_eq!(mode(&lock), 0o600, ".lock file must be tightened to 0600");
}

// ---------------------------------------------------------------------------
// v5 hardening — corruption quarantine + status count (Task 5)
// ---------------------------------------------------------------------------

/// Garbage JSON at an entry path: `get` returns None, the file is renamed to
/// `<name>.corrupt`, and `status` counts it as quarantined.
#[test]
fn test_corrupt_entry_is_quarantined_and_counted_in_status() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let mgr = CacheManager::new(root.clone(), true, 900);
    let key = CacheKey::VaultList;

    let path = key.to_path(&root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not valid json at all {{{{").unwrap();

    let result: Option<Vec<String>> = mgr.get(&key);
    assert!(result.is_none(), "corrupt entry must be a miss");
    assert!(
        !path.exists(),
        "corrupt entry must be renamed aside, not left in place"
    );

    let file_name = path.file_name().unwrap().to_str().unwrap();
    let corrupt = path.with_file_name(format!("{file_name}.corrupt"));
    assert!(
        corrupt.exists(),
        "corrupt entry must be quarantined to a .corrupt file"
    );

    let status = mgr.status();
    assert_eq!(
        status.corrupt_count, 1,
        "status must count the quarantined file"
    );
    assert_eq!(status.corrupt_entries.len(), 1);
}
