//! LocalStack integration tests. Skipped silently when LocalStack is unavailable.
//!
//! Enable with: AWS_INTEGRATION_TESTS=1 (and a running LocalStack at AWS_ENDPOINT_URL).
//!
//! LocalStack docker quickstart:
//!   docker run -d --rm --name localstack -p 4566:4566 localstack/localstack
//!   export AWS_ENDPOINT_URL=http://localhost:4566
//!   export AWS_ACCESS_KEY_ID=test
//!   export AWS_SECRET_ACCESS_KEY=test
//!   export AWS_REGION=us-east-1

#![cfg(feature = "aws")]
#![allow(dead_code)]

use crosstache::backend::aws::AwsBackend;
use crosstache::backend::Backend;
use crosstache::config::settings::AwsConfig;
use crosstache::secret::manager::{FieldUpdate, SecretRequest, SecretUpdateRequest};
use zeroize::Zeroizing;

fn skip_unless_enabled() -> bool {
    if std::env::var("AWS_INTEGRATION_TESTS").is_err() {
        eprintln!("AWS_INTEGRATION_TESTS not set — skipping");
        return true;
    }
    if std::env::var("AWS_ENDPOINT_URL").is_err() {
        eprintln!("AWS_ENDPOINT_URL not set — skipping (LocalStack required)");
        return true;
    }
    false
}

async fn build_backend() -> AwsBackend {
    let cfg = AwsConfig {
        region: Some(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string())),
        endpoint_url: Some(std::env::var("AWS_ENDPOINT_URL").unwrap()),
        ..Default::default()
    };
    AwsBackend::new(&cfg, None, None).await.unwrap()
}

#[tokio::test]
async fn localstack_set_get_round_trip() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;

    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let request = SecretRequest {
        name: "round-trip-test".into(),
        value: Zeroizing::new("test-value-42".into()),
        groups: Some(vec!["test".into()]),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        note: None,
        folder: None,
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    let got = backend
        .secrets()
        .get_secret(&vault, "round-trip-test", true)
        .await
        .unwrap();
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str().to_string()),
        Some("test-value-42".to_string())
    );
    // Groups are written as the "xv:groups" resource tag, but get_secret
    // decodes that into the plain "groups" user-tag key (props_from_describe)
    // — the raw xv: key is deliberately consumed and never re-exposed.
    let groups_tag = got.tags.get("groups").map(|s| s.as_str()).unwrap_or("");
    assert_eq!(groups_tag, "test");

    backend
        .secrets()
        .purge_secret(&vault, "round-trip-test")
        .await
        .unwrap();
}

#[tokio::test]
async fn localstack_rename_round_trip() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let request = SecretRequest {
        name: "rename-src".into(),
        value: Zeroizing::new("rename-value".into()),
        groups: Some(vec!["team".into()]),
        note: Some("ride along".into()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        folder: None,
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    let created = backend
        .secrets()
        .rename_secret(&vault, "rename-src", "rename-dst")
        .await
        .unwrap();
    assert_eq!(created.name, "rename-dst");

    let got = backend
        .secrets()
        .get_secret(&vault, "rename-dst", true)
        .await
        .unwrap();
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str().to_string()),
        Some("rename-value".to_string())
    );
    // props_from_describe re-exposes xv: tags under the canonical keys.
    assert_eq!(got.tags.get("groups").map(String::as_str), Some("team"));
    assert_eq!(got.tags.get("note").map(String::as_str), Some("ride along"));

    // The old name is scheduled for deletion (30-day recovery window — the
    // same delete `xv delete` performs), so it drops out of ListSecrets.
    // NOTE: don't assert via secret_exists — DescribeSecret still returns
    // scheduled-deletion entries, which is what makes the rename-back-within-
    // the-window case a Conflict by design.
    let listed = backend.secrets().list_secrets(&vault, None).await.unwrap();
    assert!(
        !listed.iter().any(|s| s.name == "rename-src"),
        "old name still listed: {listed:?}"
    );
    assert!(listed.iter().any(|s| s.name == "rename-dst"));

    // Cleanup: force-purge both names so reruns never hit the recovery window.
    let _ = backend.secrets().purge_secret(&vault, "rename-dst").await;
    let _ = backend.secrets().purge_secret(&vault, "rename-src").await;
}

/// mv semantics = folder tag update, then rename. Exercise the exact
/// two-call sequence `execute_secret_mv` performs (`xv mv db/<src>
/// app/<dst>`) against LocalStack's Secrets Manager.
#[tokio::test]
async fn localstack_mv_sequence_roundtrip() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    // 1. Create source with folder "db".
    let request = SecretRequest {
        name: "mv-src".into(),
        value: Zeroizing::new("mv-value".into()),
        groups: None,
        note: None,
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        folder: Some("db".into()),
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    // 2. Folder update — what mv does first.
    let update = SecretUpdateRequest {
        name: "mv-src".into(),
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
        .update_secret(&vault, "mv-src", update)
        .await
        .unwrap();

    // 3. Rename — what mv does second.
    let created = backend
        .secrets()
        .rename_secret(&vault, "mv-src", "mv-dst")
        .await
        .unwrap();
    assert_eq!(created.name, "mv-dst");

    // 4. Verify: value + new folder tag on dest, old name gone from listings.
    let got = backend
        .secrets()
        .get_secret(&vault, "mv-dst", true)
        .await
        .unwrap();
    assert_eq!(
        got.value.as_ref().map(|v| v.as_str().to_string()),
        Some("mv-value".to_string())
    );
    assert_eq!(got.tags.get("folder").map(String::as_str), Some("app"));

    let listed = backend.secrets().list_secrets(&vault, None).await.unwrap();
    assert!(
        !listed.iter().any(|s| s.name == "mv-src"),
        "old name still listed: {listed:?}"
    );
    assert!(listed.iter().any(|s| s.name == "mv-dst"));

    // Cleanup: force-purge both names so reruns never hit the recovery window.
    let _ = backend.secrets().purge_secret(&vault, "mv-dst").await;
    let _ = backend.secrets().purge_secret(&vault, "mv-src").await;
}

#[tokio::test]
async fn localstack_list_paginates() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    for i in 0..5 {
        let request = SecretRequest {
            name: format!("test-{}", i),
            value: Zeroizing::new(format!("value-{}", i)),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: None,
            note: None,
            folder: None,
        };
        backend.secrets().set_secret(&vault, request).await.unwrap();
    }

    let listed = backend.secrets().list_secrets(&vault, None).await.unwrap();
    let names: Vec<String> = listed.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names.len(), 5);
    for i in 0..5 {
        assert!(names.contains(&format!("test-{}", i)));
    }

    for i in 0..5 {
        backend
            .secrets()
            .purge_secret(&vault, &format!("test-{}", i))
            .await
            .unwrap();
    }
}

/// list_secrets must expose the folder/original-name tags in its summaries,
/// the same way get_secret does via props_from_describe — otherwise
/// folder-qualified `xv mv`/`xv ls` appear folderless on AWS (Bugbot #300).
#[tokio::test]
async fn localstack_list_secrets_exposes_folder_tag() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let request = SecretRequest {
        name: "db-pass".into(),
        value: Zeroizing::new("folder-value".into()),
        groups: None,
        note: None,
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        folder: Some("db".into()),
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    let listed = backend.secrets().list_secrets(&vault, None).await.unwrap();
    let found = listed
        .iter()
        .find(|s| s.name == "db-pass")
        .unwrap_or_else(|| panic!("db-pass not found in listing: {listed:?}"));
    assert_eq!(found.folder.as_deref(), Some("db"));
    assert_eq!(found.original_name, "db-pass");

    backend
        .secrets()
        .purge_secret(&vault, "db-pass")
        .await
        .unwrap();
}

/// list_deleted_secrets must expose the xv:original_name tag in its
/// summaries the same way list_secrets does — otherwise `xv ls --deleted`
/// loses the user-facing name on AWS (issue #301 item A).
#[tokio::test]
async fn localstack_deleted_listing_exposes_original_name() {
    if skip_unless_enabled() {
        return;
    }
    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let request = SecretRequest {
        name: "Round.Trip".into(),
        value: Zeroizing::new("deleted-value".into()),
        groups: None,
        note: None,
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        folder: None,
    };
    backend.secrets().set_secret(&vault, request).await.unwrap();

    // Soft-delete (default 30-day recovery window).
    backend
        .secrets()
        .delete_secret(&vault, "Round.Trip")
        .await
        .unwrap();

    let deleted = backend
        .secrets()
        .list_deleted_secrets(&vault)
        .await
        .unwrap();
    let found = deleted
        .iter()
        .find(|s| s.name == "Round.Trip")
        .unwrap_or_else(|| panic!("Round.Trip not found in deleted listing: {deleted:?}"));
    assert_eq!(found.original_name, "Round.Trip");

    // Cleanup: force-purge so reruns never hit the recovery window.
    let _ = backend.secrets().purge_secret(&vault, "Round.Trip").await;
}

#[tokio::test]
async fn localstack_vault_marker_create_list_delete() {
    if skip_unless_enabled() {
        return;
    }
    use crosstache::vault::models::VaultCreateRequest;

    let backend = build_backend().await;
    let vault = format!("xv-test-{}", uuid::Uuid::new_v4());

    let vaults = backend.vaults().unwrap();

    vaults
        .create_vault(VaultCreateRequest {
            name: vault.clone(),
            ..Default::default()
        })
        .await
        .unwrap();

    let listed = vaults.list_vaults().await.unwrap();
    let names: Vec<String> = listed.iter().map(|v| v.name.clone()).collect();
    assert!(names.contains(&vault));

    vaults.delete_vault(&vault).await.unwrap();
}
