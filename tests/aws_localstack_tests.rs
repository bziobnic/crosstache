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
use crosstache::secret::manager::SecretRequest;
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
    // Groups are stored in tags as "xv:groups"
    let groups_tag = got.tags.get("xv:groups").map(|s| s.as_str()).unwrap_or("");
    assert_eq!(groups_tag, "test");

    backend
        .secrets()
        .purge_secret(&vault, "round-trip-test")
        .await
        .unwrap();
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
