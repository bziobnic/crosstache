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
        vault_name: "test-vault".to_string(),
    };
    mgr.set(&key, &vec!["data".to_string()]);
    let path = key.to_path(&dir.path().to_path_buf());
    assert!(path.exists());
}

#[test]
fn test_cache_disabled_behavior() {
    let dir = TempDir::new().unwrap();
    let mgr = CacheManager::new(dir.path().to_path_buf(), false, 900);
    let key = CacheKey::SecretsList {
        vault_name: "test-vault".to_string(),
    };
    mgr.set(&key, &vec!["data".to_string()]);
    let path = key.to_path(&dir.path().to_path_buf());
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
            vault_name: "vault-a".to_string(),
        },
        &vec!["a".to_string()],
    );
    mgr.set(
        &CacheKey::SecretsList {
            vault_name: "vault-b".to_string(),
        },
        &vec!["b".to_string()],
    );
    mgr.clear(Some("vault-a"));
    let a: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
        vault_name: "vault-a".to_string(),
    });
    let b: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
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
            vault_name: "vault-a".to_string(),
        },
        &vec!["a".to_string()],
    );
    mgr.set(&CacheKey::VaultList, &vec!["v".to_string()]);
    mgr.clear(None);
    let a: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
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
            vault_name: "v1".to_string(),
        },
        &vec!["s".to_string()],
    );
    mgr.set(
        &CacheKey::FileList {
            vault_name: "v1".to_string(),
            recursive: false,
        },
        &vec!["f".to_string()],
    );
    mgr.invalidate_vault("v1");
    let s: Option<Vec<String>> = mgr.get(&CacheKey::SecretsList {
        vault_name: "v1".to_string(),
    });
    let f: Option<Vec<String>> = mgr.get(&CacheKey::FileList {
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
