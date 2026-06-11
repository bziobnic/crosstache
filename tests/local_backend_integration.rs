//! Integration tests for the local age-encrypted file backend.
//!
//! These tests exercise the full `LocalBackend` through the backend trait
//! interfaces, verifying end-to-end secret lifecycle, versioning,
//! soft-delete, file storage, and error handling.

use std::collections::HashMap;

use crosstache::backend::error::BackendError;
use crosstache::backend::Backend;
use crosstache::config::settings::LocalConfig;
use crosstache::secret::manager::SecretRequest;
use crosstache::vault::models::VaultCreateRequest;
use tempfile::TempDir;
use zeroize::Zeroizing;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a `LocalBackend` rooted in a fresh temp directory.
fn make_backend(tmp: &TempDir) -> crosstache::backend::local::LocalBackend {
    let cfg = LocalConfig {
        store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
        key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
        default_vault: Some("default".into()),
    };
    crosstache::backend::local::LocalBackend::new(Some(&cfg)).expect("create backend")
}

fn secret_req(name: &str, value: &str) -> SecretRequest {
    SecretRequest {
        name: name.to_string(),
        value: Zeroizing::new(value.to_string()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    }
}

fn vault_req(name: &str) -> VaultCreateRequest {
    VaultCreateRequest {
        name: name.to_string(),
        location: "local".to_string(),
        resource_group: String::new(),
        subscription_id: String::new(),
        sku: None,
        enabled_for_deployment: None,
        enabled_for_disk_encryption: None,
        enabled_for_template_deployment: None,
        soft_delete_retention_in_days: None,
        purge_protection: None,
        tags: None,
        access_policies: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_secret_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);

    // Create vault (default already exists, use it)
    let vaults = backend.vaults().unwrap();
    let vault_list = vaults.list_vaults().await.unwrap();
    assert_eq!(vault_list.len(), 1);
    assert_eq!(vault_list[0].name, "default");

    // Set secret
    let secrets = backend.secrets();
    let props = secrets
        .set_secret("default", secret_req("DB_PASSWORD", "hunter2"))
        .await
        .unwrap();
    assert_eq!(props.name, "DB_PASSWORD");
    assert_eq!(props.version, "v1");

    // List secrets
    let list = secrets.list_secrets("default", None).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "DB_PASSWORD");

    // Get secret (with value)
    let got = secrets
        .get_secret("default", "DB_PASSWORD", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "hunter2");

    // Update secret (new value via set_secret)
    let updated = secrets
        .set_secret("default", secret_req("DB_PASSWORD", "new-password"))
        .await
        .unwrap();
    assert_eq!(updated.version, "v2");

    // Get updated value
    let got = secrets
        .get_secret("default", "DB_PASSWORD", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "new-password");

    // Delete secret
    secrets
        .delete_secret("default", "DB_PASSWORD")
        .await
        .unwrap();

    // Verify gone from active list
    let list = secrets.list_secrets("default", None).await.unwrap();
    assert!(list.is_empty());

    // Get should return NotFound
    let err = secrets.get_secret("default", "DB_PASSWORD", false).await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));
}

#[tokio::test]
async fn test_version_history_and_rollback() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let secrets = backend.secrets();

    // Set secret v1
    secrets
        .set_secret("default", secret_req("API_KEY", "v1-value"))
        .await
        .unwrap();

    // Update to v2
    secrets
        .set_secret("default", secret_req("API_KEY", "v2-value"))
        .await
        .unwrap();

    // Update to v3
    secrets
        .set_secret("default", secret_req("API_KEY", "v3-value"))
        .await
        .unwrap();

    // List versions — should have 3
    let versions = secrets.list_versions("default", "API_KEY").await.unwrap();
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].version_number, Some(1));
    assert_eq!(versions[1].version_number, Some(2));
    assert_eq!(versions[2].version_number, Some(3));

    // Rollback to v1
    let rolled = secrets.rollback("default", "API_KEY", "v1").await.unwrap();
    assert!(rolled.version.starts_with('v'));

    // Get current value — should be v1's content
    let got = secrets
        .get_secret("default", "API_KEY", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "v1-value");
}

#[tokio::test]
async fn test_soft_delete_restore_purge() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let secrets = backend.secrets();

    // Set secret
    secrets
        .set_secret("default", secret_req("TEMP_KEY", "secret-value"))
        .await
        .unwrap();

    // Delete (soft)
    secrets.delete_secret("default", "TEMP_KEY").await.unwrap();

    // Should appear in deleted list
    let deleted = secrets.list_deleted_secrets("default").await.unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].name, "TEMP_KEY");

    // Get should return NotFound
    let err = secrets.get_secret("default", "TEMP_KEY", false).await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));

    // Restore
    let restored = secrets.restore_secret("default", "TEMP_KEY").await.unwrap();
    assert_eq!(restored.name, "TEMP_KEY");

    // Get should work now
    let got = secrets
        .get_secret("default", "TEMP_KEY", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "secret-value");

    // Delete again
    secrets.delete_secret("default", "TEMP_KEY").await.unwrap();

    // Purge permanently
    secrets.purge_secret("default", "TEMP_KEY").await.unwrap();

    // Deleted list should be empty
    let deleted = secrets.list_deleted_secrets("default").await.unwrap();
    assert!(deleted.is_empty());
}

#[cfg(feature = "file-ops")]
#[tokio::test]
async fn test_file_storage_roundtrip() {
    use crosstache::blob::models::{FileListRequest, FileUploadRequest};

    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let files = backend.files().expect("file-ops feature should be enabled");

    let content = b"-----BEGIN RSA PRIVATE KEY-----\ntest content\n-----END RSA PRIVATE KEY-----";

    // Upload file
    let upload_req = FileUploadRequest {
        name: "certs/server.pem".into(),
        content: content.to_vec(),
        content_type: Some("application/x-pem-file".into()),
        groups: vec!["infra".into()],
        metadata: HashMap::from([("env".into(), "prod".into())]),
        tags: HashMap::from([("purpose".into(), "tls".into())]),
    };
    let info = files
        .upload_file("default", upload_req, None)
        .await
        .unwrap();
    assert_eq!(info.name, "certs/server.pem");
    assert_eq!(info.size, content.len() as u64);

    // List files
    let list = files
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
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "certs/server.pem");

    // Download and verify content matches
    let downloaded = files
        .download_file("default", "certs/server.pem", None)
        .await
        .unwrap();
    assert_eq!(downloaded, content);

    // Get file info
    let file_info = files
        .get_file_info("default", "certs/server.pem")
        .await
        .unwrap();
    assert_eq!(file_info.name, "certs/server.pem");
    assert_eq!(file_info.content_type, "application/x-pem-file");

    // Delete
    files
        .delete_file("default", "certs/server.pem")
        .await
        .unwrap();

    // Verify gone
    let list = files
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
    assert!(list.is_empty());

    // Download should fail
    let err = files
        .download_file("default", "certs/server.pem", None)
        .await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));
}

/// Regression test for ROADMAP P2: file operations used to be pinned to the
/// vault captured at backend construction (the default vault), so every
/// upload landed in `default` regardless of the selected vault.
#[cfg(feature = "file-ops")]
#[tokio::test]
async fn test_file_operations_target_selected_vault() {
    use crosstache::blob::models::{FileListRequest, FileUploadRequest};

    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let vaults = backend.vaults().expect("local backend has vaults");
    let files = backend.files().expect("file-ops feature should be enabled");

    vaults.create_vault(vault_req("dev")).await.unwrap();
    vaults.create_vault(vault_req("prod")).await.unwrap();

    let upload_req = |content: &[u8]| FileUploadRequest {
        name: "app.env".into(),
        content: content.to_vec(),
        content_type: None,
        groups: Vec::new(),
        metadata: HashMap::new(),
        tags: HashMap::new(),
    };
    files
        .upload_file("dev", upload_req(b"DEV=1"), None)
        .await
        .unwrap();
    files
        .upload_file("prod", upload_req(b"PROD=1"), None)
        .await
        .unwrap();

    // Nothing leaked into the default vault.
    assert!(!tmp.path().join("store/vaults/default/files").exists());
    let list_req = || FileListRequest {
        prefix: None,
        groups: None,
        limit: None,
        delimiter: None,
    };
    assert!(files
        .list_files("default", list_req())
        .await
        .unwrap()
        .is_empty());

    // Each vault sees exactly its own copy with its own content.
    let dev_list = files.list_files("dev", list_req()).await.unwrap();
    assert_eq!(dev_list.len(), 1);
    assert_eq!(
        files.download_file("dev", "app.env", None).await.unwrap(),
        b"DEV=1"
    );
    assert_eq!(
        files.download_file("prod", "app.env", None).await.unwrap(),
        b"PROD=1"
    );

    // Deleting from one vault does not touch the other.
    files.delete_file("dev", "app.env").await.unwrap();
    let err = files.get_file_info("dev", "app.env").await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));
    assert_eq!(
        files.download_file("prod", "app.env", None).await.unwrap(),
        b"PROD=1"
    );
}

#[tokio::test]
async fn test_special_characters_in_names() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let secrets = backend.secrets();

    // Secret with slashes and @ symbols
    secrets
        .set_secret("default", secret_req("my/secret@test", "slash-value"))
        .await
        .unwrap();
    let got = secrets
        .get_secret("default", "my/secret@test", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "slash-value");
    assert_eq!(got.name, "my/secret@test");

    // Secret with spaces
    secrets
        .set_secret("default", secret_req("has spaces", "space-value"))
        .await
        .unwrap();
    let got = secrets
        .get_secret("default", "has spaces", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "space-value");
    assert_eq!(got.name, "has spaces");

    // Secret with unicode/emoji
    secrets
        .set_secret("default", secret_req("key-🔑", "emoji-value"))
        .await
        .unwrap();
    let got = secrets.get_secret("default", "key-🔑", true).await.unwrap();
    assert_eq!(&*got.value.unwrap(), "emoji-value");

    // List should show all three
    let list = secrets.list_secrets("default", None).await.unwrap();
    assert_eq!(list.len(), 3);
}

#[tokio::test]
async fn test_multiple_vaults() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let vaults = backend.vaults().unwrap();
    let secrets = backend.secrets();

    // Create additional vaults
    vaults.create_vault(vault_req("prod")).await.unwrap();
    vaults.create_vault(vault_req("staging")).await.unwrap();

    // Set secret in prod
    secrets
        .set_secret("prod", secret_req("DB_URL", "prod-db.example.com"))
        .await
        .unwrap();

    // Set secret in staging
    secrets
        .set_secret("staging", secret_req("DB_URL", "staging-db.example.com"))
        .await
        .unwrap();

    // List prod — should only see prod's secrets
    let prod_secrets = secrets.list_secrets("prod", None).await.unwrap();
    assert_eq!(prod_secrets.len(), 1);
    assert_eq!(prod_secrets[0].name, "DB_URL");

    // List staging — should only see staging's secrets
    let staging_secrets = secrets.list_secrets("staging", None).await.unwrap();
    assert_eq!(staging_secrets.len(), 1);
    assert_eq!(staging_secrets[0].name, "DB_URL");

    // Values should be different (vault isolation)
    let prod_val = secrets.get_secret("prod", "DB_URL", true).await.unwrap();
    assert_eq!(&*prod_val.value.unwrap(), "prod-db.example.com");

    let staging_val = secrets.get_secret("staging", "DB_URL", true).await.unwrap();
    assert_eq!(&*staging_val.value.unwrap(), "staging-db.example.com");

    // Default vault should be empty (no secrets set there)
    let default_secrets = secrets.list_secrets("default", None).await.unwrap();
    assert!(default_secrets.is_empty());

    // List vaults — should have 3
    let vault_list = vaults.list_vaults().await.unwrap();
    assert_eq!(vault_list.len(), 3);
}

#[tokio::test]
async fn test_error_cases() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let secrets = backend.secrets();
    let vaults = backend.vaults().unwrap();

    // Get non-existent secret → NotFound
    let err = secrets.get_secret("default", "nonexistent", false).await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));

    // Delete non-existent secret → NotFound
    let err = secrets.delete_secret("default", "nonexistent").await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));

    // Set secret in non-existent vault → VaultNotFound
    let err = secrets
        .set_secret("no-such-vault", secret_req("key", "val"))
        .await;
    assert!(matches!(err, Err(BackendError::VaultNotFound { .. })));

    // Delete non-existent vault → VaultNotFound
    let err = vaults.delete_vault("no-such-vault").await;
    assert!(matches!(err, Err(BackendError::VaultNotFound { .. })));

    // Create duplicate vault → Conflict
    vaults.create_vault(vault_req("dup-test")).await.unwrap();
    let err = vaults.create_vault(vault_req("dup-test")).await;
    assert!(matches!(err, Err(BackendError::Conflict(_))));

    // Rollback to non-existent version → NotFound
    secrets
        .set_secret("default", secret_req("rb-err", "val"))
        .await
        .unwrap();
    let err = secrets.rollback("default", "rb-err", "v99").await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));

    // Restore non-deleted secret → NotFound
    let err = secrets.restore_secret("default", "rb-err").await;
    assert!(matches!(err, Err(BackendError::NotFound { .. })));
}

#[tokio::test]
async fn test_backend_metadata() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);

    assert_eq!(backend.name(), "local");
    assert_eq!(backend.kind(), crosstache::backend::BackendKind::Local);

    let caps = backend.capabilities();
    assert!(caps.has_vaults);
    assert!(caps.has_versioning);
    assert!(caps.has_soft_delete);
    assert!(caps.has_groups);
    assert!(caps.has_notes);
    assert!(!caps.has_rbac);
    assert!(!caps.has_audit);
}

#[tokio::test]
async fn test_health_check() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);

    backend.health_check().await.unwrap();
}

#[tokio::test]
async fn test_empty_and_large_values() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let secrets = backend.secrets();

    // Empty value
    secrets
        .set_secret("default", secret_req("empty-secret", ""))
        .await
        .unwrap();
    let got = secrets
        .get_secret("default", "empty-secret", true)
        .await
        .unwrap();
    assert_eq!(&*got.value.unwrap(), "");

    // Large value (64KB)
    let large = "x".repeat(65536);
    secrets
        .set_secret("default", secret_req("large-secret", &large))
        .await
        .unwrap();
    let got = secrets
        .get_secret("default", "large-secret", true)
        .await
        .unwrap();
    assert_eq!(got.value.unwrap().len(), 65536);
}

#[tokio::test]
async fn test_secret_with_metadata() {
    let tmp = TempDir::new().unwrap();
    let backend = make_backend(&tmp);
    let secrets = backend.secrets();

    let req = SecretRequest {
        name: "tagged-secret".into(),
        value: Zeroizing::new("val".into()),
        content_type: Some("application/json".into()),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: Some(HashMap::from([
            ("env".into(), "prod".into()),
            ("team".into(), "platform".into()),
        ])),
        groups: Some(vec!["databases".into(), "critical".into()]),
        note: Some("Production database credential".into()),
        folder: Some("infra/db".into()),
    };

    let props = secrets.set_secret("default", req).await.unwrap();
    assert_eq!(props.content_type, "application/json");
    assert!(props.enabled);

    // List with group filter
    let filtered = secrets
        .list_secrets("default", Some("databases"))
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "tagged-secret");

    // Non-matching group filter
    let empty = secrets
        .list_secrets("default", Some("nonexistent-group"))
        .await
        .unwrap();
    assert!(empty.is_empty());
}
