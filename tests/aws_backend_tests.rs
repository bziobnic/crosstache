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
