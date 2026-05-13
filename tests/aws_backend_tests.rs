//! Hermetic AWS backend tests using aws-smithy-mocks-experimental.
//!
//! Each test builds a mock SecretsManager client by stubbing per-operation
//! responses, then exercises the `AwsSecretBackend` / `AwsVaultBackend`
//! against it. No AWS credentials, no network — fully deterministic.

#![cfg(feature = "aws")]
#![allow(deprecated)] // aws-smithy-mocks-experimental is deprecated but still functional

use aws_sdk_secretsmanager::Client;
use aws_smithy_mocks_experimental::{mock, mock_client, RuleMode};
use crosstache::backend::aws::{secrets::AwsSecretBackend, vaults::AwsVaultBackend};
use std::sync::Arc;

/// Build an `AwsSecretBackend` directly around a mock client.
pub fn aws_secret_backend(client: Client) -> AwsSecretBackend {
    AwsSecretBackend::new(Arc::new(client))
}

/// Build an `AwsVaultBackend` directly around a mock client.
pub fn aws_vault_backend(client: Client) -> AwsVaultBackend {
    AwsVaultBackend::new(Arc::new(client))
}

#[tokio::test]
async fn smoke_health_check_with_empty_list() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;

    let rule = mock!(Client::list_secrets)
        .then_output(|| ListSecretsOutput::builder().build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);
    backend.health_check().await.expect("health check should pass");
}

#[tokio::test]
async fn set_secret_create_writes_to_aws() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretOutput;
    use crosstache::backend::SecretBackend;
    use crosstache::secret::manager::SecretRequest;
    use zeroize::Zeroizing;

    let rule = mock!(Client::create_secret)
        .match_requests(|req| req.name() == Some("myproj-kv/db-password"))
        .then_output(|| {
            CreateSecretOutput::builder()
                .name("myproj-kv/db-password")
                .version_id("v1")
                .build()
        });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let request = SecretRequest {
        name: "db-password".to_string(),
        value: Zeroizing::new("super-secret".to_string()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    };

    let result = backend
        .set_secret("myproj-kv", request)
        .await
        .expect("set_secret should succeed");

    assert_eq!(result.name, "db-password");
    assert_eq!(result.version, "v1");
}

#[tokio::test]
async fn get_secret_no_value_returns_metadata_only() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/api-key"))
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/api-key")
                .description("API key for service")
                .tags(
                    Tag::builder()
                        .key("xv:original_name")
                        .value("api-key")
                        .build(),
                )
                .build()
        });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let result = backend
        .get_secret("myproj-kv", "api-key", false)
        .await
        .expect("get_secret should succeed");

    assert_eq!(result.name, "api-key");
    assert!(result.value.is_none(), "value should be absent when include_value=false");
}

#[tokio::test]
async fn get_secret_with_value_includes_value() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::SecretBackend;

    let describe_rule = mock!(Client::describe_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/db-password"))
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/db-password")
                .tags(
                    Tag::builder()
                        .key("xv:original_name")
                        .value("db-password")
                        .build(),
                )
                .build()
        });

    let value_rule = mock!(Client::get_secret_value)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/db-password"))
        .then_output(|| {
            GetSecretValueOutput::builder()
                .name("myproj-kv/db-password")
                .version_id("v1")
                .secret_string("super-secret-value")
                .build()
        });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&describe_rule, &value_rule]
    );
    let backend = aws_secret_backend(client);

    let result = backend
        .get_secret("myproj-kv", "db-password", true)
        .await
        .expect("get_secret with value should succeed");

    assert_eq!(result.name, "db-password");
    let value = result.value.expect("value should be present when include_value=true");
    assert_eq!(value.as_str(), "super-secret-value");
}

#[tokio::test]
async fn get_secret_not_found_maps_to_backend_not_found() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::SecretBackend;
    use crosstache::backend::error::BackendError;

    let rule = mock!(Client::describe_secret)
        .then_error(|| {
            DescribeSecretError::ResourceNotFoundException(
                ResourceNotFoundException::builder()
                    .message("Secret not found")
                    .build(),
            )
        });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let result = backend
        .get_secret("myproj-kv", "missing-secret", false)
        .await;

    assert!(
        matches!(result, Err(BackendError::NotFound { .. })),
        "expected NotFound error, got: {result:?}"
    );
}

#[tokio::test]
async fn list_secrets_paginates_and_filters_marker() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::SecretBackend;

    // Page 1: marker + one real secret, with next_token
    let page1 = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/.xv-vault").build())
            .secret_list(SecretListEntry::builder().name("myproj-kv/db-password").build())
            .next_token("tok1")
            .build()
    });
    // Page 2: one more secret, no next_token
    let page2 = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/api-key").build())
            .build()
    });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&page1, &page2]);
    let backend = aws_secret_backend(client);

    let secrets = backend.list_secrets("myproj-kv", None).await.unwrap();
    let names: Vec<String> = secrets.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"db-password".to_string()));
    assert!(names.contains(&"api-key".to_string()));
    assert!(!names.contains(&".xv-vault".to_string()), "marker should be excluded");
}

#[tokio::test]
async fn delete_secret_uses_recovery_window() {
    use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::delete_secret)
        .match_requests(|req| {
            req.secret_id() == Some("myproj-kv/db-password")
                && req.recovery_window_in_days() == Some(30)
                && req.force_delete_without_recovery() != Some(true)
        })
        .then_output(|| DeleteSecretOutput::builder().build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);
    backend.delete_secret("myproj-kv", "db-password").await.unwrap();
}

#[tokio::test]
async fn purge_secret_forces_immediate_delete() {
    use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::delete_secret)
        .match_requests(|req| req.force_delete_without_recovery() == Some(true))
        .then_output(|| DeleteSecretOutput::builder().build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);
    backend.purge_secret("myproj-kv", "db-password").await.unwrap();
}

#[tokio::test]
async fn secret_exists_true_when_describe_succeeds() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret)
        .then_output(|| DescribeSecretOutput::builder().name("myproj-kv/db").build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    assert!(backend
        .secret_exists("myproj-kv", "db")
        .await
        .unwrap());
}

#[tokio::test]
async fn secret_exists_false_on_not_found() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret).then_error(|| {
        DescribeSecretError::ResourceNotFoundException(
            ResourceNotFoundException::builder()
                .message("not found")
                .build(),
        )
    });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    assert!(
        !backend
            .secret_exists("myproj-kv", "missing")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn set_secret_update_path_when_already_exists() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretError;
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::put_secret_value::PutSecretValueOutput;
    use aws_sdk_secretsmanager::operation::tag_resource::TagResourceOutput;
    use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceOutput;
    use aws_sdk_secretsmanager::operation::update_secret::UpdateSecretOutput;
    use aws_sdk_secretsmanager::types::error::ResourceExistsException;
    use crosstache::backend::SecretBackend;
    use crosstache::secret::manager::SecretRequest;
    use zeroize::Zeroizing;

    // create_secret returns ResourceExistsException — triggers update path.
    let create_err = mock!(Client::create_secret).then_error(|| {
        CreateSecretError::ResourceExistsException(
            ResourceExistsException::builder()
                .message("already exists")
                .build(),
        )
    });
    // put_secret_value — new version.
    let put_value = mock!(Client::put_secret_value)
        .then_output(|| PutSecretValueOutput::builder().version_id("v2").build());
    // update_secret — description update (note is Some).
    let update_secret_mock =
        mock!(Client::update_secret).then_output(|| UpdateSecretOutput::builder().build());
    // describe_secret — fetch existing tags (empty).
    let describe =
        mock!(Client::describe_secret).then_output(|| DescribeSecretOutput::builder().build());
    // untag_resource — no-op because describe returned no tags; not called when key list is empty.
    // tag_resource — apply new tags.
    let tag = mock!(Client::tag_resource).then_output(|| TagResourceOutput::builder().build());

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&create_err, &put_value, &update_secret_mock, &describe, &tag]
    );
    let backend = aws_secret_backend(client);

    let request = SecretRequest {
        name: "db-password".to_string(),
        value: Zeroizing::new("new-secret-value".to_string()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: Some("updated note".to_string()),
        folder: None,
    };

    let result = backend
        .set_secret("myproj-kv", request)
        .await
        .expect("set_secret update path should succeed");

    assert_eq!(result.version, "v2");
    assert_eq!(result.name, "db-password");
}

#[tokio::test]
async fn list_versions_returns_history() {
    use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsOutput;
    use aws_sdk_secretsmanager::types::SecretVersionsListEntry;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::list_secret_version_ids).then_output(|| {
        ListSecretVersionIdsOutput::builder()
            .versions(
                SecretVersionsListEntry::builder()
                    .version_id("v1")
                    .build()
            )
            .versions(
                SecretVersionsListEntry::builder()
                    .version_id("v2")
                    .build()
            )
            .build()
    });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let versions = backend
        .list_versions("myproj-kv", "db-password")
        .await
        .unwrap();

    assert_eq!(versions.len(), 2);
    let ids: Vec<String> = versions.iter().map(|v| v.version.clone()).collect();
    assert!(ids.contains(&"v1".to_string()));
    assert!(ids.contains(&"v2".to_string()));
}
