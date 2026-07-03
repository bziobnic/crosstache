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

    let rule = mock!(Client::list_secrets).then_output(|| ListSecretsOutput::builder().build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);
    backend
        .health_check()
        .await
        .expect("health check should pass");
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
async fn set_secret_preserves_migration_idempotency_tags() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretOutput;
    use crosstache::backend::SecretBackend;
    use crosstache::secret::manager::SecretRequest;
    use std::collections::HashMap;
    use zeroize::Zeroizing;

    let rule = mock!(Client::create_secret)
        .match_requests(|req| {
            let has_migrated_from = req
                .tags()
                .iter()
                .any(|t| t.key() == Some("xv:migrated_from") && t.value() == Some("local:v1"));
            let has_migrated_at = req
                .tags()
                .iter()
                .any(|t| t.key() == Some("xv:migrated_at") && t.value() == Some("now"));
            let drops_unknown_xv = req.tags().iter().all(|t| t.key() != Some("xv:internal"));
            has_migrated_from && has_migrated_at && drops_unknown_xv
        })
        .then_output(|| CreateSecretOutput::builder().version_id("v1").build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let mut tags = HashMap::new();
    tags.insert("xv:migrated_from".to_string(), "local:v1".to_string());
    tags.insert("xv:migrated_at".to_string(), "now".to_string());
    tags.insert("xv:internal".to_string(), "drop-me".to_string());

    let request = SecretRequest {
        name: "db-password".to_string(),
        value: Zeroizing::new("super-secret".to_string()),
        content_type: None,
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: Some(tags),
        groups: None,
        note: None,
        folder: None,
    };

    backend.set_secret("myproj-kv", request).await.unwrap();
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
                .tags(
                    Tag::builder()
                        .key("xv:migrated_from")
                        .value("local:myproj-kv:v1")
                        .build(),
                )
                .tags(
                    Tag::builder()
                        .key("xv:migrated_at")
                        .value("2026-05-13T21:00:00Z")
                        .build(),
                )
                .tags(Tag::builder().key("xv:groups").value("api,prod").build())
                .tags(
                    Tag::builder()
                        .key("xv:folder")
                        .value("services/api")
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
    assert!(
        result.value.is_none(),
        "value should be absent when include_value=false"
    );
    assert_eq!(
        result.tags.get("xv:migrated_from").map(String::as_str),
        Some("local:myproj-kv:v1")
    );
    assert_eq!(
        result.tags.get("xv:migrated_at").map(String::as_str),
        Some("2026-05-13T21:00:00Z")
    );
    assert_eq!(
        result.tags.get("groups").map(String::as_str),
        Some("api,prod")
    );
    assert_eq!(
        result.tags.get("folder").map(String::as_str),
        Some("services/api")
    );
    assert_eq!(
        result.tags.get("note").map(String::as_str),
        Some("API key for service")
    );
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
    let value = result
        .value
        .expect("value should be present when include_value=true");
    assert_eq!(value.as_str(), "super-secret-value");
}

#[tokio::test]
async fn get_secret_not_found_maps_to_backend_not_found() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::error::BackendError;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret).then_error(|| {
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
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/.xv-vault")
                    .build(),
            )
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/db-password")
                    .build(),
            )
            .next_token("tok1")
            .build()
    });
    // Page 2: one more secret, no next_token
    let page2 = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/api-key").build())
            .build()
    });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&page1, &page2]
    );
    let backend = aws_secret_backend(client);

    let secrets = backend.list_secrets("myproj-kv", None).await.unwrap();
    let names: Vec<String> = secrets.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"db-password".to_string()));
    assert!(names.contains(&"api-key".to_string()));
    assert!(
        !names.contains(&".xv-vault".to_string()),
        "marker should be excluded"
    );
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
    backend
        .delete_secret("myproj-kv", "db-password")
        .await
        .unwrap();
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
    backend
        .purge_secret("myproj-kv", "db-password")
        .await
        .unwrap();
}

#[tokio::test]
async fn secret_exists_true_when_describe_succeeds() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::describe_secret)
        .then_output(|| DescribeSecretOutput::builder().name("myproj-kv/db").build());

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    assert!(backend.secret_exists("myproj-kv", "db").await.unwrap());
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

    assert!(!backend.secret_exists("myproj-kv", "missing").await.unwrap());
}

#[tokio::test]
async fn set_secret_update_path_when_already_exists() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretError;
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::put_secret_value::PutSecretValueOutput;
    use aws_sdk_secretsmanager::operation::tag_resource::TagResourceOutput;
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
        &[
            &create_err,
            &put_value,
            &update_secret_mock,
            &describe,
            &tag
        ]
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
async fn update_secret_enabled_flag_is_unsupported() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use crosstache::backend::{BackendError, SecretBackend};
    use crosstache::secret::manager::{FieldUpdate, SecretUpdateRequest};

    // AWS has no enable/disable concept; the flag must fail loudly before
    // any API call is made (no mock rules are consumed).
    let rule = mock!(Client::list_secrets).then_output(|| ListSecretsOutput::builder().build());
    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let request = SecretUpdateRequest {
        name: "db-password".to_string(),
        value: None,
        content_type: None,
        enabled: Some(false),
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: None,
        groups: None,
        note: FieldUpdate::Unchanged,
        folder: FieldUpdate::Unchanged,
        replace_tags: false,
        replace_groups: false,
    };

    let err = backend
        .update_secret("myproj-kv", "db-password", request)
        .await
        .expect_err("--enabled must not be a silent no-op on AWS");
    assert!(
        matches!(&err, BackendError::Unsupported(feature) if feature == "enable/disable secrets"),
        "expected Unsupported, got: {err:?}"
    );
}

/// Bugbot MAJOR review on the record-types edit paths (Tasks 8/9):
/// `AwsSecretBackend::update_secret` previously applied only note/tag/
/// folder/expiry deltas and never wrote a value or touched content type —
/// so `xv update --field-secret` silently discarded the new envelope on
/// AWS while reporting success, and a record's content-type marker was
/// never (re)written. This mirrors exactly what `xv update --field-secret`
/// sends: `value: Some(new_envelope)`, `content_type:
/// Some(RECORD_CONTENT_TYPE)`, and the FULL desired tag map with
/// `replace_tags: true` (per the BLOCKER fix in
/// `execute_record_field_update`, src/cli/secret_ops.rs).
#[tokio::test]
async fn update_secret_with_value_writes_value_and_content_type_tag() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::put_secret_value::PutSecretValueOutput;
    use aws_sdk_secretsmanager::operation::tag_resource::TagResourceOutput;
    use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::SecretBackend;
    use crosstache::secret::manager::{FieldUpdate, SecretUpdateRequest};
    use std::collections::HashMap;
    use zeroize::Zeroizing;

    // Step 1: the new envelope value must actually be written.
    let put_value = mock!(Client::put_secret_value)
        .match_requests(|req| {
            req.secret_id() == Some("myproj-kv/cred")
                && req.secret_string() == Some(r#"{"password":"hunter2"}"#)
        })
        .then_output(|| PutSecretValueOutput::builder().version_id("v2").build());

    // Step 2 (replace_tags diff): current AWS-side tags include a plain
    // "f.old-field" (must be dropped — absent from the new full map) and
    // an "xv:groups" bookkeeping tag (must NOT be touched by the generic
    // tags diff — it's managed by its own dedicated request field).
    let describe_for_diff = mock!(Client::describe_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/cred"))
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/cred")
                .tags(Tag::builder().key("xv:groups").value("prod").build())
                .tags(Tag::builder().key("xv-type").value("login").build())
                .tags(Tag::builder().key("f.old-field").value("stale").build())
                .build()
        });

    let untag = mock!(Client::untag_resource)
        .match_requests(|req| req.tag_keys() == ["f.old-field".to_string()])
        .then_output(|| UntagResourceOutput::builder().build());

    let tag = mock!(Client::tag_resource)
        .match_requests(|req| {
            let tags = req.tags();
            let has_content_type = tags.iter().any(|t| {
                t.key() == Some("xv:content_type") && t.value() == Some("application/vnd.xv.record")
            });
            let has_type = tags
                .iter()
                .any(|t| t.key() == Some("xv-type") && t.value() == Some("login"));
            let has_username = tags
                .iter()
                .any(|t| t.key() == Some("f.username") && t.value() == Some("bob"));
            has_content_type && has_type && has_username
        })
        .then_output(|| TagResourceOutput::builder().build());

    // Final `self.get_secret(vault, name, false)` at the end of update_secret.
    let describe_final = mock!(Client::describe_secret).then_output(|| {
        DescribeSecretOutput::builder()
            .name("myproj-kv/cred")
            .build()
    });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[
            &put_value,
            &describe_for_diff,
            &untag,
            &tag,
            &describe_final
        ]
    );
    let backend = aws_secret_backend(client);

    let mut full_tags = HashMap::new();
    full_tags.insert("xv-type".to_string(), "login".to_string());
    full_tags.insert("f.username".to_string(), "bob".to_string());

    let request = SecretUpdateRequest {
        name: "cred".to_string(),
        value: Some(Zeroizing::new(r#"{"password":"hunter2"}"#.to_string())),
        content_type: Some("application/vnd.xv.record".to_string()),
        enabled: None,
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: Some(full_tags),
        groups: None,
        note: FieldUpdate::Unchanged,
        folder: FieldUpdate::Unchanged,
        replace_tags: true,
        replace_groups: false,
    };

    backend
        .update_secret("myproj-kv", "cred", request)
        .await
        .expect("update_secret should write the value and content-type tag");
}

/// Companion to the test above: `xv update --untype` on AWS must actually
/// remove the `xv-type`/`f.*` tags (previously a no-op — Bugbot MAJOR:
/// "untype no-ops ... xv-type tag survives since absent-from-map !=
/// removed"). This exercises the metadata-only path (`value: None`), which
/// stays off the `put_secret_value` call but must still honor
/// `replace_tags` for the tag removal diff.
#[tokio::test]
async fn update_secret_untype_replace_tags_removes_dropped_keys() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::SecretBackend;
    use crosstache::secret::manager::{FieldUpdate, SecretUpdateRequest};
    use std::collections::HashMap;

    // Untyping drops xv-type and every f.* tag entirely — the caller sends
    // an EMPTY desired map (`execute_record_untype` omits them, it never
    // sends them with an empty value), so the whole plain-tag namespace on
    // the existing secret must be removed via the replace_tags diff.
    let describe_for_diff = mock!(Client::describe_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/cred"))
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/cred")
                .tags(Tag::builder().key("xv:groups").value("prod").build())
                .tags(Tag::builder().key("xv-type").value("login").build())
                .tags(Tag::builder().key("f.username").value("bob").build())
                .build()
        });

    let untag = mock!(Client::untag_resource)
        .match_requests(|req| {
            let keys = req.tag_keys();
            // xv-type and f.username (plain namespace, absent from the
            // empty desired map) must be removed, PLUS the content-type
            // tag (content_type: Some("") below); the "xv:"-prefixed
            // groups bookkeeping tag must be left alone — it isn't part
            // of the generic tags diff at all.
            keys.contains(&"xv-type".to_string())
                && keys.contains(&"f.username".to_string())
                && keys.contains(&"xv:content_type".to_string())
                && !keys.contains(&"xv:groups".to_string())
        })
        .then_output(|| UntagResourceOutput::builder().build());

    let describe_final = mock!(Client::describe_secret).then_output(|| {
        DescribeSecretOutput::builder()
            .name("myproj-kv/cred")
            .build()
    });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&describe_for_diff, &untag, &describe_final]
    );
    let backend = aws_secret_backend(client);

    let request = SecretUpdateRequest {
        name: "cred".to_string(),
        value: None,
        content_type: Some(String::new()),
        enabled: None,
        expires_on: FieldUpdate::Unchanged,
        not_before: FieldUpdate::Unchanged,
        tags: Some(HashMap::new()),
        groups: None,
        note: FieldUpdate::Unchanged,
        folder: FieldUpdate::Unchanged,
        replace_tags: true,
        replace_groups: false,
    };

    backend
        .update_secret("myproj-kv", "cred", request)
        .await
        .expect("untype's replace_tags update should succeed");
}

#[tokio::test]
async fn list_versions_returns_history() {
    use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsOutput;
    use aws_sdk_secretsmanager::types::SecretVersionsListEntry;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::list_secret_version_ids).then_output(|| {
        ListSecretVersionIdsOutput::builder()
            .versions(SecretVersionsListEntry::builder().version_id("v1").build())
            .versions(SecretVersionsListEntry::builder().version_id("v2").build())
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

#[tokio::test]
async fn rollback_moves_awscurrent_to_target_version() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsOutput;
    use aws_sdk_secretsmanager::operation::update_secret_version_stage::UpdateSecretVersionStageOutput;
    use aws_sdk_secretsmanager::types::SecretVersionsListEntry;
    use crosstache::backend::SecretBackend;

    let list = mock!(Client::list_secret_version_ids).then_output(|| {
        ListSecretVersionIdsOutput::builder()
            .versions(
                SecretVersionsListEntry::builder()
                    .version_id("v3")
                    .version_stages("AWSCURRENT".to_string())
                    .build(),
            )
            .versions(SecretVersionsListEntry::builder().version_id("v2").build())
            .build()
    });
    let update_stage = mock!(Client::update_secret_version_stage)
        .match_requests(|req| {
            req.move_to_version_id() == Some("v2")
                && req.remove_from_version_id() == Some("v3")
                && req.version_stage() == Some("AWSCURRENT")
        })
        .then_output(|| UpdateSecretVersionStageOutput::builder().build());
    // rollback calls get_secret(vault, name, false) at the end which calls describe_secret
    let describe = mock!(Client::describe_secret).then_output(|| {
        DescribeSecretOutput::builder()
            .name("myproj-kv/db-password")
            .build()
    });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&list, &update_stage, &describe]
    );
    let backend = aws_secret_backend(client);

    backend
        .rollback("myproj-kv", "db-password", "v2")
        .await
        .unwrap();
}

#[tokio::test]
async fn restore_secret_calls_aws_restore() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::restore_secret::RestoreSecretOutput;
    use crosstache::backend::SecretBackend;

    let restore = mock!(Client::restore_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/db-password"))
        .then_output(|| RestoreSecretOutput::builder().build());
    // restore_secret calls get_secret at the end which calls describe_secret
    let describe = mock!(Client::describe_secret).then_output(|| {
        DescribeSecretOutput::builder()
            .name("myproj-kv/db-password")
            .build()
    });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&restore, &describe]
    );
    let backend = aws_secret_backend(client);

    let result = backend
        .restore_secret("myproj-kv", "db-password")
        .await
        .unwrap();
    assert_eq!(result.name, "db-password");
}

#[tokio::test]
async fn list_deleted_secrets_filters_to_deleted_only() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::primitives::DateTime;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("myproj-kv/active").build())
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/deleted-one")
                    .deleted_date(DateTime::from_secs(1_700_000_000))
                    .build(),
            )
            .build()
    });
    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let deleted = backend.list_deleted_secrets("myproj-kv").await.unwrap();
    let names: Vec<String> = deleted.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, vec!["deleted-one".to_string()]);
}

#[tokio::test]
async fn list_deleted_secrets_paginates_all_pages() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::primitives::DateTime;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::SecretBackend;

    let first_page = mock!(Client::list_secrets)
        .match_requests(|req| req.next_token().is_none())
        .then_output(|| {
            ListSecretsOutput::builder()
                .secret_list(
                    SecretListEntry::builder()
                        .name("myproj-kv/deleted-one")
                        .deleted_date(DateTime::from_secs(1_700_000_000))
                        .build(),
                )
                .next_token("page-2")
                .build()
        });
    let second_page = mock!(Client::list_secrets)
        .match_requests(|req| req.next_token() == Some("page-2"))
        .then_output(|| {
            ListSecretsOutput::builder()
                .secret_list(
                    SecretListEntry::builder()
                        .name("myproj-kv/deleted-two")
                        .deleted_date(DateTime::from_secs(1_700_000_001))
                        .build(),
                )
                .build()
        });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&first_page, &second_page]
    );
    let backend = aws_secret_backend(client);

    let deleted = backend.list_deleted_secrets("myproj-kv").await.unwrap();
    let names: Vec<String> = deleted.iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        vec!["deleted-one".to_string(), "deleted-two".to_string()]
    );
}

#[tokio::test]
async fn create_vault_writes_marker_secret() {
    use aws_sdk_secretsmanager::operation::create_secret::CreateSecretOutput;
    use crosstache::backend::VaultBackend;
    use crosstache::vault::models::VaultCreateRequest;

    let rule = mock!(Client::create_secret)
        .match_requests(|req| req.name() == Some("myproj-kv/.xv-vault"))
        .then_output(|| {
            CreateSecretOutput::builder()
                .name("myproj-kv/.xv-vault")
                .build()
        });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_vault_backend(client);

    let request = VaultCreateRequest {
        name: "myproj-kv".to_string(),
        location: "eastus".to_string(),
        resource_group: "my-rg".to_string(),
        subscription_id: "sub-123".to_string(),
        sku: None,
        enabled_for_deployment: None,
        enabled_for_disk_encryption: None,
        enabled_for_template_deployment: None,
        soft_delete_retention_in_days: None,
        purge_protection: None,
        tags: None,
        access_policies: None,
    };

    let result = backend.create_vault(request).await.unwrap();
    assert_eq!(result.name, "myproj-kv");
}

#[tokio::test]
async fn get_vault_returns_vault_not_found_when_marker_missing() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::error::BackendError;
    use crosstache::backend::VaultBackend;

    let rule = mock!(Client::describe_secret).then_error(|| {
        DescribeSecretError::ResourceNotFoundException(
            ResourceNotFoundException::builder()
                .message("not found")
                .build(),
        )
    });
    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_vault_backend(client);

    let err = backend.get_vault("missing-vault").await.unwrap_err();
    assert!(
        matches!(err, BackendError::VaultNotFound { .. }),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn get_vault_returns_vault_properties() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::VaultBackend;

    let rule = mock!(Client::describe_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/.xv-vault"))
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/.xv-vault")
                .tags(
                    Tag::builder()
                        .key("xv:vault_name")
                        .value("myproj-kv")
                        .build(),
                )
                .tags(
                    Tag::builder()
                        .key("xv:created_at")
                        .value("2026-05-13T10:00:00+00:00")
                        .build(),
                )
                .build()
        });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_vault_backend(client);

    let vault = backend.get_vault("myproj-kv").await.unwrap();
    assert_eq!(vault.name, "myproj-kv");
    assert_eq!(vault.id, "vault-myproj-kv");
}

#[tokio::test]
async fn list_vaults_finds_all_markers() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::VaultBackend;

    let rule = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/.xv-vault")
                    .build(),
            )
            .secret_list(
                SecretListEntry::builder()
                    .name("staging-kv/.xv-vault")
                    .build(),
            )
            .build()
    });
    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_vault_backend(client);

    let vaults = backend.list_vaults().await.unwrap();
    let names: Vec<String> = vaults.iter().map(|v| v.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"myproj-kv".to_string()));
    assert!(names.contains(&"staging-kv".to_string()));
}

#[tokio::test]
async fn list_vaults_paginates() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::VaultBackend;

    let page1 = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("vault1/.xv-vault").build())
            .next_token("tok1")
            .build()
    });
    let page2 = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(SecretListEntry::builder().name("vault2/.xv-vault").build())
            .build()
    });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&page1, &page2]
    );
    let backend = aws_vault_backend(client);

    let vaults = backend.list_vaults().await.unwrap();
    let names: Vec<String> = vaults.iter().map(|v| v.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"vault1".to_string()));
    assert!(names.contains(&"vault2".to_string()));
}

#[tokio::test]
async fn delete_vault_refuses_when_secrets_exist() {
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::error::BackendError;
    use crosstache::backend::VaultBackend;

    let rule = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/.xv-vault")
                    .build(),
            )
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/db-password")
                    .build(),
            )
            .build()
    });
    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_vault_backend(client);

    let err = backend.delete_vault("myproj-kv").await.unwrap_err();
    assert!(matches!(err, BackendError::Conflict(_)), "got: {err:?}");
}

#[tokio::test]
async fn delete_vault_succeeds_when_only_marker_exists() {
    use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretOutput;
    use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsOutput;
    use aws_sdk_secretsmanager::types::SecretListEntry;
    use crosstache::backend::VaultBackend;

    let list = mock!(Client::list_secrets).then_output(|| {
        ListSecretsOutput::builder()
            .secret_list(
                SecretListEntry::builder()
                    .name("myproj-kv/.xv-vault")
                    .build(),
            )
            .build()
    });
    let delete = mock!(Client::delete_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/.xv-vault"))
        .then_output(|| DeleteSecretOutput::builder().build());

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&list, &delete]
    );
    let backend = aws_vault_backend(client);
    backend.delete_vault("myproj-kv").await.unwrap();
}

#[tokio::test]
async fn update_vault_updates_tags() {
    use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
    use aws_sdk_secretsmanager::operation::tag_resource::TagResourceOutput;
    use aws_sdk_secretsmanager::types::Tag;
    use crosstache::backend::VaultBackend;
    use crosstache::vault::models::VaultUpdateRequest;
    use std::collections::HashMap;

    let tag = mock!(Client::tag_resource)
        .match_requests(|req| {
            // Verify that tag_resource was called with the marker secret and tags
            req.secret_id() == Some("myproj-kv/.xv-vault")
        })
        .then_output(|| TagResourceOutput::builder().build());

    // update_vault calls get_vault at the end which calls describe_secret
    let describe = mock!(Client::describe_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/.xv-vault"))
        .then_output(|| {
            DescribeSecretOutput::builder()
                .name("myproj-kv/.xv-vault")
                .tags(
                    Tag::builder()
                        .key("xv:vault_name")
                        .value("myproj-kv")
                        .build(),
                )
                .tags(
                    Tag::builder()
                        .key("xv:created_at")
                        .value("2026-05-13T10:00:00+00:00")
                        .build(),
                )
                .tags(
                    Tag::builder()
                        .key("environment")
                        .value("production")
                        .build(),
                )
                .build()
        });

    let client = mock_client!(
        aws_sdk_secretsmanager,
        RuleMode::Sequential,
        &[&tag, &describe]
    );
    let backend = aws_vault_backend(client);

    let mut tags = HashMap::new();
    tags.insert("environment".to_string(), "production".to_string());

    let request = VaultUpdateRequest {
        enabled_for_deployment: None,
        enabled_for_disk_encryption: None,
        enabled_for_template_deployment: None,
        soft_delete_retention_in_days: None,
        purge_protection: None,
        tags: Some(tags),
        access_policies: None,
    };

    let result = backend.update_vault("myproj-kv", request).await.unwrap();
    assert_eq!(result.name, "myproj-kv");
    assert_eq!(result.id, "vault-myproj-kv");
}

// ---------------------------------------------------------------------------
// Native rotation (`RotateSecret`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn native_rotate_sends_rotate_secret_for_encoded_name() {
    use aws_sdk_secretsmanager::operation::rotate_secret::RotateSecretOutput;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::rotate_secret)
        .match_requests(|req| req.secret_id() == Some("myproj-kv/db-password"))
        .then_output(|| {
            RotateSecretOutput::builder()
                .name("myproj-kv/db-password")
                .version_id("v2")
                .build()
        });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    backend
        .native_rotate("myproj-kv", "db-password")
        .await
        .expect("native_rotate should accept the rotation request");
}

#[tokio::test]
async fn native_rotate_not_found_maps_to_backend_not_found() {
    use aws_sdk_secretsmanager::operation::rotate_secret::RotateSecretError;
    use aws_sdk_secretsmanager::types::error::ResourceNotFoundException;
    use crosstache::backend::error::BackendError;
    use crosstache::backend::SecretBackend;

    let rule = mock!(Client::rotate_secret).then_error(|| {
        RotateSecretError::ResourceNotFoundException(
            ResourceNotFoundException::builder()
                .message("Secret not found")
                .build(),
        )
    });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let result = backend.native_rotate("myproj-kv", "missing-secret").await;

    assert!(
        matches!(result, Err(BackendError::NotFound { ref name, .. }) if name == "missing-secret"),
        "expected NotFound error, got: {result:?}"
    );
}

#[tokio::test]
async fn native_rotate_without_lambda_explains_how_to_configure_one() {
    use aws_sdk_secretsmanager::operation::rotate_secret::RotateSecretError;
    use aws_sdk_secretsmanager::types::error::InvalidRequestException;
    use crosstache::backend::error::BackendError;
    use crosstache::backend::SecretBackend;

    // AWS reports "no rotation Lambda configured" as InvalidRequestException.
    let rule = mock!(Client::rotate_secret).then_error(|| {
        RotateSecretError::InvalidRequestException(
            InvalidRequestException::builder()
                .message("No Lambda rotation function ARN is associated with this secret")
                .build(),
        )
    });

    let client = mock_client!(aws_sdk_secretsmanager, RuleMode::Sequential, &[&rule]);
    let backend = aws_secret_backend(client);

    let result = backend.native_rotate("myproj-kv", "db-password").await;

    let Err(BackendError::InvalidArgument(msg)) = result else {
        panic!("expected InvalidArgument error, got: {result:?}");
    };
    assert!(
        msg.contains("aws secretsmanager rotate-secret"),
        "remediation hint missing: {msg}"
    );
    assert!(
        msg.contains("--secret-id myproj-kv/db-password"),
        "hint should reference the AWS secret id: {msg}"
    );
    assert!(
        msg.contains("--rotation-lambda-arn"),
        "hint should mention the rotation Lambda flag: {msg}"
    );
}
