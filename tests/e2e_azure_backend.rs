//! End-to-end **authenticated** tests for the Azure Key Vault backend.
//!
//! These tests exercise `AzureBackend` against the **real Azure Key Vault
//! API** using the credentials already configured for the `az` CLI
//! (`DefaultAzureCredential` chain — env vars, az CLI login, managed
//! identity, …).
//!
//! Requirements:
//! - A working Azure identity (`az account show` succeeds)
//! - A reachable test Key Vault — default `heythere`, override with
//!   `XV_E2E_AZURE_VAULT` or the `DEFAULT_VAULT` env var
//! - The caller must have get/list/set/delete/recover permissions on it
//! - Internet connection
//!
//! Run with:
//!   cargo test --test e2e_azure_backend -- --ignored --nocapture --test-threads=1
//!
//! Safety:
//! - Every secret name is uniquely timestamped (`xv-e2e-az-<tag>-<ts>`), so
//!   runs never collide with each other or with unrelated secrets.
//! - The `heythere` vault has **purge protection enabled**, so soft-deleted
//!   secrets cannot be purged — cleanup is therefore best-effort soft-delete
//!   only. Unique names guarantee no name reuse within the recovery window.
//! - Tests operate only on their own secrets; they never create, delete, or
//!   mutate the Key Vault itself.

use crosstache::auth::provider::DefaultAzureCredentialProvider;
use crosstache::backend::azure::AzureBackend;
use crosstache::backend::Backend;
use crosstache::config::settings::Config;
use crosstache::secret::manager::{FieldUpdate, SecretRequest, SecretUpdateRequest};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// The test vault. `heythere` is the configured safe-for-testing vault.
fn test_vault() -> String {
    std::env::var("XV_E2E_AZURE_VAULT")
        .or_else(|_| std::env::var("DEFAULT_VAULT"))
        .unwrap_or_else(|_| "heythere".to_string())
}

/// Load the real xv config (subscription/tenant come from `~/.config/xv/xv.conf`).
async fn load_config() -> Config {
    Config::load()
        .await
        .expect("failed to load xv config — run `xv init` or check ~/.config/xv/xv.conf")
}

/// Build an `AzureBackend` from the default Azure credential chain.
async fn azure_backend() -> AzureBackend {
    let config = load_config().await;
    let provider = DefaultAzureCredentialProvider::with_credential_priority(
        config.azure_credential_priority.clone(),
    )
    .expect("failed to build Azure credential provider — is the az CLI authenticated?");
    AzureBackend::new(&config, Arc::new(provider)).expect("failed to construct AzureBackend")
}

/// Unique secret name for one test run.
fn unique_name(tag: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("xv-e2e-az-{tag}-{ts}")
}

fn make_request(name: &str, value: &str) -> SecretRequest {
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

/// Best-effort cleanup. The vault has purge protection, so we can only
/// soft-delete; unique timestamped names prevent any reuse collision.
async fn cleanup(backend: &AzureBackend, vault: &str, names: &[&str]) {
    for name in names {
        let _ = backend.secrets().delete_secret(vault, name).await;
    }
}

#[tokio::test]
#[ignore]
async fn e2e_azure_health_check() {
    let backend = azure_backend().await;
    backend
        .health_check()
        .await
        .expect("Azure health check (token acquisition) should succeed");
}

#[tokio::test]
#[ignore]
async fn e2e_azure_secret_full_lifecycle() {
    let backend = azure_backend().await;
    let vault = test_vault();
    let secret = unique_name("lc");

    // --- SET (create) ---
    let v1_value = "secret-value-v1";
    let r1 = backend
        .secrets()
        .set_secret(&vault, make_request(&secret, v1_value))
        .await
        .expect("set_secret (create) should succeed");
    assert_eq!(r1.name, secret);
    let v1_version = r1.version.clone();
    assert!(!v1_version.is_empty(), "create should return a version id");

    // --- GET (with value) ---
    let got = backend
        .secrets()
        .get_secret(&vault, &secret, true)
        .await
        .expect("get_secret with value should succeed");
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str()),
        Some(v1_value),
        "round-tripped value must match"
    );

    // --- GET (metadata only) ---
    let meta = backend
        .secrets()
        .get_secret(&vault, &secret, false)
        .await
        .expect("get_secret metadata-only should succeed");
    assert_eq!(meta.name, secret);
    assert!(
        meta.value.is_none(),
        "value must be absent when include_value=false"
    );

    // --- EXISTS ---
    assert!(
        backend
            .secrets()
            .secret_exists(&vault, &secret)
            .await
            .expect("secret_exists should succeed"),
        "secret should exist after creation"
    );

    // --- LIST ---
    let listed = backend
        .secrets()
        .list_secrets(&vault, None)
        .await
        .expect("list_secrets should succeed");
    assert!(
        listed.iter().any(|s| s.name == secret),
        "list should include our freshly created secret"
    );

    // --- SET (update → new version) ---
    let v2_value = "secret-value-v2";
    let r2 = backend
        .secrets()
        .set_secret(&vault, make_request(&secret, v2_value))
        .await
        .expect("set_secret (update) should succeed");
    assert_ne!(
        r2.version, v1_version,
        "update should produce a new version id"
    );
    let got2 = backend
        .secrets()
        .get_secret(&vault, &secret, true)
        .await
        .expect("get after update should succeed");
    assert_eq!(got2.value.as_ref().map(|v| v.as_str()), Some(v2_value));

    // --- GET SPECIFIC VERSION (v1 still readable by id) ---
    // `version` is the bare Key Vault version segment, which is exactly
    // what get_secret_version expects.
    let v1_again = backend
        .secrets()
        .get_secret_version(&vault, &secret, &v1_version, true)
        .await
        .expect("get_secret_version for v1 should succeed");
    assert_eq!(
        v1_again.value.as_ref().map(|v| v.as_str()),
        Some(v1_value),
        "the original version should still serve its original value"
    );

    // --- LIST VERSIONS ---
    let versions = backend
        .secrets()
        .list_versions(&vault, &secret)
        .await
        .expect("list_versions should succeed");
    assert!(
        versions.len() >= 2,
        "expected at least 2 versions after one update, got {}",
        versions.len()
    );

    // --- UPDATE METADATA (note + groups) ---
    let update = SecretUpdateRequest {
        name: secret.clone(),
        value: None,
        content_type: None,
        enabled: None,
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: None,
        groups: Some(vec!["e2e".to_string(), "prod".to_string()]),
        note: FieldUpdate::Set("updated by e2e test".to_string()),
        folder: FieldUpdate::Unchanged,
        replace_tags: false,
        replace_groups: true,
    };
    backend
        .secrets()
        .update_secret(&vault, &secret, update)
        .await
        .expect("update_secret should succeed");
    let after_meta = backend
        .secrets()
        .get_secret(&vault, &secret, false)
        .await
        .expect("get_secret after metadata update should succeed");
    assert_eq!(
        after_meta.tags.get("note").map(String::as_str),
        Some("updated by e2e test"),
        "note should be persisted after update_secret, tags: {:?}",
        after_meta.tags
    );

    // --- ROLLBACK (re-apply v1's value as a new current version) ---
    backend
        .secrets()
        .rollback(&vault, &secret, &v1_version)
        .await
        .expect("rollback to v1 should succeed");
    let rolled = backend
        .secrets()
        .get_secret(&vault, &secret, true)
        .await
        .expect("get after rollback should succeed");
    assert_eq!(
        rolled.value.as_ref().map(|v| v.as_str()),
        Some(v1_value),
        "rollback should restore the v1 value"
    );

    // --- DELETE (soft) ---
    backend
        .secrets()
        .delete_secret(&vault, &secret)
        .await
        .expect("delete_secret should succeed");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let exists_after_delete = backend
        .secrets()
        .secret_exists(&vault, &secret)
        .await
        .expect("secret_exists after delete should not error");
    assert!(
        !exists_after_delete,
        "soft-deleted secret should not be reported as existing"
    );

    // --- RESTORE ---
    backend
        .secrets()
        .restore_secret(&vault, &secret)
        .await
        .expect("restore_secret should succeed");
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let restored = backend
        .secrets()
        .get_secret(&vault, &secret, true)
        .await
        .expect("get after restore should succeed");
    assert!(
        restored.value.is_some(),
        "restored secret should be readable again"
    );

    // --- CLEANUP (best-effort soft delete; purge blocked by purge protection) ---
    cleanup(&backend, &vault, &[&secret]).await;
}

#[tokio::test]
#[ignore]
async fn e2e_azure_rename_roundtrip() {
    let backend = azure_backend().await;
    let vault = test_vault();
    let source = unique_name("rn-src");
    let dest = unique_name("rn-dst");

    let mut req = make_request(&source, "rename-me");
    req.groups = Some(vec!["e2e".to_string()]);
    req.note = Some("rename e2e".to_string());
    backend
        .secrets()
        .set_secret(&vault, req)
        .await
        .expect("create rename source");

    let created = backend
        .secrets()
        .rename_secret(&vault, &source, &dest)
        .await
        .expect("rename_secret should succeed");
    assert_eq!(created.name, dest);

    let got = backend
        .secrets()
        .get_secret(&vault, &dest, true)
        .await
        .expect("get renamed secret");
    assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("rename-me"));
    assert_eq!(got.tags.get("note").map(String::as_str), Some("rename e2e"));
    assert_eq!(got.tags.get("groups").map(String::as_str), Some("e2e"));

    // The old name must be soft-deleted (GET returns 404 once the delete
    // lands; Key Vault applies it promptly after DELETE returns).
    let exists = backend
        .secrets()
        .secret_exists(&vault, &source)
        .await
        .expect("exists check on the old name");
    assert!(
        !exists,
        "source '{source}' should be soft-deleted after rename"
    );

    // Cleanup: soft-delete the destination. The vault has purge protection,
    // so purging is blocked by policy — soft delete + unique names is the
    // documented harness contract. `source` is already soft-deleted.
    cleanup(&backend, &vault, &[&dest]).await;
}

/// mv semantics = folder tag update, then rename. Exercise the exact
/// two-call sequence `execute_secret_mv` performs (`xv mv db/<src>
/// app/<dst>`) against real Azure Key Vault.
#[tokio::test]
#[ignore]
async fn e2e_azure_mv_sequence_roundtrip() {
    let backend = azure_backend().await;
    let vault = test_vault();
    let source = unique_name("mv-src");
    let dest = unique_name("mv-dst");

    // 1. Create the source secret already tagged with folder "db".
    let mut req = make_request(&source, "mv-me");
    req.folder = Some("db".to_string());
    backend
        .secrets()
        .set_secret(&vault, req)
        .await
        .expect("create mv source");

    // 2. Folder update — what mv does first.
    let update = SecretUpdateRequest {
        name: source.clone(),
        value: None,
        content_type: None,
        enabled: None,
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: None,
        groups: None,
        note: FieldUpdate::Unchanged,
        folder: FieldUpdate::Set("app".to_string()),
        replace_tags: false,
        replace_groups: false,
    };
    backend
        .secrets()
        .update_secret(&vault, &source, update)
        .await
        .expect("folder update");

    // 3. Rename — what mv does second.
    let renamed = backend
        .secrets()
        .rename_secret(&vault, &source, &dest)
        .await
        .expect("rename_secret should succeed");
    assert_eq!(renamed.name, dest);

    // 4. Verify: value + new folder tag on dest, source soft-deleted.
    let got = backend
        .secrets()
        .get_secret(&vault, &dest, true)
        .await
        .expect("get moved secret");
    assert_eq!(got.value.as_ref().map(|v| v.as_str()), Some("mv-me"));
    assert_eq!(got.tags.get("folder").map(String::as_str), Some("app"));

    let exists = backend
        .secrets()
        .secret_exists(&vault, &source)
        .await
        .expect("exists check on the old name");
    assert!(!exists, "source '{source}' should be soft-deleted after mv");

    // Cleanup: soft-delete the destination (best-effort; matches the file's
    // other tests). `source` is already soft-deleted.
    cleanup(&backend, &vault, &[&dest]).await;
}

#[tokio::test]
#[ignore]
async fn e2e_azure_bulk_set_and_get() {
    let backend = azure_backend().await;
    let vault = test_vault();

    let a = unique_name("bulk-a");
    let b = unique_name("bulk-b");
    let c = unique_name("bulk-c");
    let entries = [
        (a.as_str(), "alpha-val"),
        (b.as_str(), "beta-val"),
        (c.as_str(), "gamma-val"),
    ];

    for (name, value) in entries {
        backend
            .secrets()
            .set_secret(&vault, make_request(name, value))
            .await
            .unwrap_or_else(|e| panic!("set_secret {name} failed: {e:?}"));
    }

    for (name, value) in entries {
        let got = backend
            .secrets()
            .get_secret(&vault, name, true)
            .await
            .unwrap_or_else(|e| panic!("get_secret {name} failed: {e:?}"));
        assert_eq!(
            got.value.as_ref().map(|v| v.as_str()),
            Some(value),
            "value mismatch for {name}"
        );
    }

    // All three should be visible in a single list call.
    let listed = backend
        .secrets()
        .list_secrets(&vault, None)
        .await
        .expect("list_secrets should succeed");
    for (name, _) in entries {
        assert!(
            listed.iter().any(|s| s.name == name),
            "list should include {name}"
        );
    }

    cleanup(&backend, &vault, &[&a, &b, &c]).await;
}

#[tokio::test]
#[ignore]
async fn e2e_azure_get_missing_secret_is_not_found() {
    use crosstache::backend::error::BackendError;

    let backend = azure_backend().await;
    let vault = test_vault();
    let missing = unique_name("missing");

    let result = backend.secrets().get_secret(&vault, &missing, false).await;
    assert!(
        matches!(result, Err(BackendError::NotFound { .. })),
        "getting a non-existent secret should map to BackendError::NotFound, got: {result:?}"
    );

    // secret_exists should agree.
    assert!(
        !backend
            .secrets()
            .secret_exists(&vault, &missing)
            .await
            .expect("secret_exists should succeed for a missing secret"),
        "secret_exists should be false for a name that was never created"
    );
}
