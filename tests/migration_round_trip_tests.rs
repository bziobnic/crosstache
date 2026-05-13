//! Cross-backend migration round-trip tests.
//!
//! Tests Local<->AWS today (LocalStack-gated). Azure<->AWS deferred to live tests.
//!
//! These tests only run when both `AWS_INTEGRATION_TESTS` and `AWS_ENDPOINT_URL`
//! env vars are set (i.e. a LocalStack instance is available).

#![cfg(feature = "aws")]

use crosstache::backend::aws::AwsBackend;
use crosstache::backend::local::LocalBackend;
use crosstache::backend::Backend;
use crosstache::config::settings::AwsConfig;
use crosstache::config::settings::LocalConfig;
use crosstache::secret::manager::SecretRequest;
use tempfile::TempDir;
use zeroize::Zeroizing;

/// Returns `true` when the LocalStack integration environment is NOT configured,
/// meaning the test should silently skip.
fn skip_unless_enabled() -> bool {
    std::env::var("AWS_INTEGRATION_TESTS").is_err() || std::env::var("AWS_ENDPOINT_URL").is_err()
}

/// Extract groups from a `SecretProperties.tags` map (mirrors `SecretInfo::extract_groups`).
fn groups_from_tags(tags: &std::collections::HashMap<String, String>) -> Vec<String> {
    tags.get("groups")
        .map(|g| {
            g.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn local_to_aws_round_trip() {
    if skip_unless_enabled() {
        return;
    }

    let tmp = TempDir::new().unwrap();

    // ---- set up local backend ----
    let local_cfg = LocalConfig {
        store_path: Some(tmp.path().join("store").to_string_lossy().to_string()),
        key_file: Some(tmp.path().join("key.txt").to_string_lossy().to_string()),
        default_vault: Some("test-vault".into()),
    };
    let local = LocalBackend::new(Some(&local_cfg)).unwrap();

    // ---- set up AWS backend (LocalStack) ----
    let aws_cfg = AwsConfig {
        region: Some("us-east-1".into()),
        endpoint_url: Some(std::env::var("AWS_ENDPOINT_URL").unwrap()),
        ..Default::default()
    };
    let aws = AwsBackend::new(&aws_cfg, None, None).await.unwrap();

    let vault = format!("xv-rt-{}", uuid::Uuid::new_v4());

    // ---- write 3 secrets to local ----
    for (n, v) in [("a", "1"), ("b", "2"), ("c", "3")] {
        let request = SecretRequest {
            name: n.into(),
            value: Zeroizing::new(v.into()),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: Some(vec!["roundtrip".into()]),
            note: None,
            folder: None,
        };
        local.secrets().set_secret(&vault, request).await.unwrap();
    }

    // ---- read from local, write to AWS ----
    for n in ["a", "b", "c"] {
        let props = local.secrets().get_secret(&vault, n, true).await.unwrap();
        let groups_vec = groups_from_tags(&props.tags);
        let request = SecretRequest {
            name: props.name.clone(),
            value: Zeroizing::new(
                props
                    .value
                    .as_ref()
                    .map(|v| v.as_str().to_string())
                    .unwrap_or_default(),
            ),
            content_type: None,
            enabled: None,
            expires_on: None,
            not_before: None,
            tags: None,
            groups: if groups_vec.is_empty() {
                None
            } else {
                Some(groups_vec)
            },
            note: None,
            folder: None,
        };
        aws.secrets().set_secret(&vault, request).await.unwrap();
    }

    // ---- verify on AWS side ----
    for (n, expected) in [("a", "1"), ("b", "2"), ("c", "3")] {
        let got = aws.secrets().get_secret(&vault, n, true).await.unwrap();
        assert_eq!(
            got.value.as_ref().map(|v| v.as_str().to_string()),
            Some(expected.to_string()),
            "value mismatch for secret {n}"
        );
        let got_groups = groups_from_tags(&got.tags);
        assert_eq!(
            got_groups,
            vec!["roundtrip"],
            "groups mismatch for secret {n}"
        );
    }

    // ---- cleanup AWS side ----
    for n in ["a", "b", "c"] {
        aws.secrets().purge_secret(&vault, n).await.unwrap();
    }
}
