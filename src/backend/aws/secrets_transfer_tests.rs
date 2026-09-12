use super::*;
use crate::secret::domain::SecretValue;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use serde_json::json;
use std::sync::Mutex;

#[derive(Debug)]
struct State {
    reads: usize,
    values: usize,
    drift: bool,
}
#[derive(Clone, Debug)]
struct Transport(Arc<Mutex<State>>);
impl HttpConnector for Transport {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let mut state = self.0.lock().unwrap();
        let operation = request.headers().get("x-amz-target").unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(request.body().bytes().unwrap()).unwrap();
        let output = if operation.ends_with("DescribeSecret") {
            state.reads += 1;
            json!({"ARN":"arn:aws:secretsmanager:us-east-1:123456789012:secret:prod/source-abcdef", "Name":"prod/source", "Description":"note", "VersionIdsToStages":{"version-one":["AWSCURRENT"]}, "Tags":[{"Key":"xv:folder","Value": if state.drift && state.reads > 1 { "changed" } else { "original" }}]})
        } else {
            assert!(operation.ends_with("GetSecretValue"));
            state.values += 1;
            if !body["VersionId"].is_null() {
                assert_eq!(body["VersionId"], "version-one");
            }
            json!({"VersionId":"version-one", "SecretString":"value", "VersionStages":["AWSCURRENT"]})
        };
        let mut response = HttpResponse::new(
            StatusCode::try_from(200).unwrap(),
            output.to_string().into(),
        );
        response
            .headers_mut()
            .insert("content-type", "application/x-amz-json-1.1");
        HttpConnectorFuture::ready(Ok(response))
    }
}
impl HttpClient for Transport {
    fn http_connector(
        &self,
        _: &HttpConnectorSettings,
        _: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}
fn backend_with_state(drift: bool) -> (AwsSecretBackend, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State {
        reads: 0,
        values: 0,
        drift,
    }));
    let config = aws_sdk_secretsmanager::Config::builder()
        .behavior_version(aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(aws_sdk_secretsmanager::config::Region::new("us-east-1"))
        .credentials_provider(aws_sdk_secretsmanager::config::Credentials::new(
            "test", "test", None, None, "test",
        ))
        .http_client(Transport(state.clone()))
        .build();
    (
        AwsSecretBackend::new(Arc::new(SecretsManagerClient::from_conf(config))),
        state,
    )
}

fn backend(drift: bool) -> AwsSecretBackend {
    backend_with_state(drift).0
}

/// The metadata getter must never reach for a value: one `DescribeSecret`,
/// zero `GetSecretValue`.
#[tokio::test]
async fn metadata_getter_issues_describe_only() {
    let (backend, state) = backend_with_state(false);
    let metadata = backend.get_secret_metadata("prod", "source").await.unwrap();
    assert_eq!(metadata.name, "source");
    let state = state.lock().unwrap();
    assert_eq!(state.reads, 1, "expected exactly one DescribeSecret");
    assert_eq!(state.values, 0, "metadata path fetched a value");
}

/// The value getter still makes both calls (concurrently, via `tokio::join!`).
#[tokio::test]
async fn value_getter_issues_describe_and_get_value() {
    let (backend, state) = backend_with_state(false);
    let secret = backend.get_secret("prod", "source").await.unwrap();
    assert_eq!(secret.value.expose_secret(), "value");
    let state = state.lock().unwrap();
    assert_eq!(state.reads, 1, "expected exactly one DescribeSecret");
    assert_eq!(state.values, 1, "expected exactly one GetSecretValue");
}
#[tokio::test]
async fn aws_transfer_snapshot_rechecks_folder_metadata_without_claiming_cas() {
    let stable = backend(false);
    assert!(stable.supports_atomic_create());
    assert!(!stable.supports_conditional_delete());
    let snapshot = stable
        .get_transfer_snapshot("prod", "source", SnapshotValue::Include)
        .await
        .unwrap();
    assert_eq!(
        snapshot.value.as_ref().map(SecretValue::expose_secret),
        Some("value")
    );
    assert_eq!(snapshot.metadata.tags["folder"], "original");
    assert!(!snapshot.revision.is_empty());
    assert!(backend(true)
        .get_transfer_snapshot("prod", "source", SnapshotValue::Include)
        .await
        .is_err());
}

#[tokio::test]
async fn aws_transfer_metadata_preflight_refuses_lossy_fields_and_invalid_names() {
    let backend = backend(false);
    let request = SecretRequest {
        name: "destination".into(),
        value: SecretValue::new("value"),
        content_type: None,
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    };
    backend
        .validate_transfer_metadata("prod", &request)
        .await
        .unwrap();
    let mut cases = Vec::new();
    let mut changed = request.clone();
    changed.enabled = Some(false);
    cases.push(changed);
    let mut changed = request.clone();
    changed.not_before = Some(chrono::Utc::now());
    cases.push(changed);
    let mut changed = request.clone();
    changed.name = "bad name".into();
    cases.push(changed);
    let mut changed = request.clone();
    changed.tags = Some(HashMap::from([(
        "xv:unknown".into(),
        "would-be-dropped".into(),
    )]));
    cases.push(changed);
    let mut changed = request.clone();
    changed.note = Some(String::new());
    cases.push(changed);
    let mut changed = request.clone();
    changed.groups = Some(vec!["team+ops".into()]);
    cases.push(changed);
    let mut changed = request.clone();
    changed.folder = Some(String::new());
    cases.push(changed);
    for request in cases {
        assert!(
            backend
                .validate_transfer_metadata("prod", &request)
                .await
                .is_err(),
            "accepted lossy metadata"
        );
    }
    assert!(backend
        .validate_transfer_metadata(&"v".repeat(510), &request)
        .await
        .is_err());
}

#[tokio::test]
async fn aws_transfer_metadata_preflight_checks_provider_limits() {
    let backend = backend(false);
    let base = SecretRequest {
        name: "destination".into(),
        value: SecretValue::new("value"),
        content_type: None,
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    };
    let mut cases = Vec::new();
    let mut request = base.clone();
    request.value = SecretValue::new("x".repeat(65537));
    cases.push(request);
    let mut request = base.clone();
    request.value = SecretValue::new(String::new());
    cases.push(request);
    let mut request = base.clone();
    request.note = Some("x".repeat(2049));
    cases.push(request);
    let mut request = base.clone();
    request.name = "x".repeat(257);
    cases.push(request);
    let mut request = base.clone();
    request.folder = Some("x".repeat(257));
    cases.push(request);
    let mut request = base.clone();
    request.content_type = Some("x".repeat(257));
    cases.push(request);
    let mut request = base.clone();
    request.groups = Some(vec!["x".repeat(128), "y".repeat(128)]);
    cases.push(request);
    let mut request = base.clone();
    request.tags = Some(HashMap::from([("x".repeat(129), "value".into())]));
    cases.push(request);
    for request in cases {
        assert!(
            backend
                .validate_transfer_metadata("prod", &request)
                .await
                .is_err(),
            "accepted provider-bound violation"
        );
    }
    let mut valid = base;
    valid.value = SecretValue::new("x".repeat(65536));
    valid.note = Some("x".repeat(2048));
    valid.folder = Some("x".repeat(256));
    backend
        .validate_transfer_metadata("prod", &valid)
        .await
        .unwrap();
}

#[tokio::test]
async fn aws_transfer_metadata_preflight_rejects_invalid_generated_tags_before_io() {
    let backend = backend(false);
    let base = SecretRequest {
        name: "destination".into(),
        value: SecretValue::new("value"),
        content_type: None,
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    };
    let mut cases = Vec::new();
    for (key, value) in [
        ("key", "a,b"),
        ("aws:reserved", "value"),
        ("AWS:reserved", "value"),
        ("key!", "value"),
    ] {
        let mut request = base.clone();
        request.tags = Some(HashMap::from([(key.into(), value.into())]));
        cases.push(request);
    }
    let mut request = base.clone();
    request.groups = Some(vec!["team!".into()]);
    cases.push(request);
    let mut request = base.clone();
    request.content_type = Some("text/plain;charset=utf8".into());
    cases.push(request);
    let mut request = base.clone();
    request.tags = Some(
        (0..48)
            .map(|i| (format!("key{i}"), "value".into()))
            .collect(),
    );
    request.content_type = Some("text/plain".into());
    request.expires_on = Some(chrono::Utc::now());
    cases.push(request);
    for request in cases {
        assert!(
            backend
                .validate_transfer_metadata("prod", &request)
                .await
                .is_err(),
            "accepted invalid generated AWS tags"
        );
    }
    let mut valid = base;
    valid.tags = Some(
        (0..49)
            .map(|i| (format!("key{i}"), "value".into()))
            .collect(),
    );
    backend
        .validate_transfer_metadata("prod", &valid)
        .await
        .unwrap();
}

/// The AWS adapter's "provider returned no value" branch. `DescribeSecret` and
/// `GetSecretValue` are two calls: the metadata getters use the first alone,
/// while the value getters combine both through `secret_from_value`. A
/// `GetSecretValue` response with no `SecretString` (an AWS secret stored as
/// `SecretBinary` reaches exactly this shape) must be a hard error rather than
/// a `Secret` carrying an empty value.
///
/// The same `describe` drives `metadata_from_describe` in the same test, so the
/// assertion is specifically about the *value* half: if the split regressed and
/// the metadata getter routed through `secret_from_value`, metadata reads of a
/// binary secret would start failing too.
#[test]
fn aws_value_read_refuses_a_describe_plus_value_pair_with_no_secret_string() {
    use aws_sdk_secretsmanager::types::Tag;

    let backend = backend(false);
    let describe = DescribeSecretOutput::builder()
        .arn("arn:aws:secretsmanager:us-east-1:123456789012:secret:prod/binary-abcdef")
        .name("prod/binary")
        .description("a note")
        .tags(Tag::builder().key("xv:folder").value("infra").build())
        .build();

    // Metadata half: succeeds on this input, value-free by construction.
    let metadata = backend.metadata_from_describe(&describe, "binary");
    assert_eq!(metadata.name, "binary");
    assert_eq!(
        metadata.tags.get("folder").map(String::as_str),
        Some("infra")
    );
    assert_eq!(
        metadata.tags.get("note").map(String::as_str),
        Some("a note")
    );

    // Value half: no SecretString, so no Secret.
    let value = GetSecretValueOutput::builder()
        .version_id("version-one")
        .version_stages("AWSCURRENT")
        .build();
    let error = backend
        .secret_from_value(&describe, &value, "binary")
        .unwrap_err();
    assert!(
        matches!(&error, BackendError::Internal(message)
            if message.contains("provider returned no value") && message.contains("binary")),
        "{error:?}"
    );

    // Companion positive case: with a SecretString, the value getter pairs it
    // with the version actually returned by GetSecretValue.
    let value = GetSecretValueOutput::builder()
        .version_id("version-one")
        .version_stages("AWSCURRENT")
        .secret_string("s3cr3t")
        .build();
    let secret = backend
        .secret_from_value(&describe, &value, "binary")
        .unwrap();
    assert_eq!(secret.value.expose_secret(), "s3cr3t");
    assert_eq!(secret.metadata.version, "version-one");
    assert_eq!(secret.metadata.name, metadata.name);
}
