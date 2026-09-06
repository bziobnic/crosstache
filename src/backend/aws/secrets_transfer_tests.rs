use super::*;
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
            assert_eq!(body["VersionId"], "version-one");
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
fn backend(drift: bool) -> AwsSecretBackend {
    let config = aws_sdk_secretsmanager::Config::builder()
        .behavior_version(aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(aws_sdk_secretsmanager::config::Region::new("us-east-1"))
        .credentials_provider(aws_sdk_secretsmanager::config::Credentials::new(
            "test", "test", None, None, "test",
        ))
        .http_client(Transport(Arc::new(Mutex::new(State { reads: 0, drift }))))
        .build();
    AwsSecretBackend::new(Arc::new(SecretsManagerClient::from_conf(config)))
}
#[tokio::test]
async fn aws_transfer_snapshot_rechecks_folder_metadata_without_claiming_cas() {
    let stable = backend(false);
    assert!(stable.supports_atomic_create());
    assert!(!stable.supports_conditional_delete());
    let snapshot = stable
        .get_transfer_snapshot("prod", "source", true)
        .await
        .unwrap();
    assert_eq!(
        snapshot.properties.value.as_deref().map(|v| v.as_str()),
        Some("value")
    );
    assert_eq!(snapshot.properties.tags["folder"], "original");
    assert!(!snapshot.revision.is_empty());
    assert!(backend(true)
        .get_transfer_snapshot("prod", "source", true)
        .await
        .is_err());
}

#[tokio::test]
async fn aws_transfer_metadata_preflight_refuses_lossy_fields_and_invalid_names() {
    use zeroize::Zeroizing;
    let backend = backend(false);
    let request = SecretRequest {
        name: "destination".into(),
        value: Zeroizing::new("value".into()),
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
        value: zeroize::Zeroizing::new("value".into()),
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
    request.value = zeroize::Zeroizing::new("x".repeat(65537));
    cases.push(request);
    let mut request = base.clone();
    request.value = zeroize::Zeroizing::new(String::new());
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
    valid.value = zeroize::Zeroizing::new("x".repeat(65536));
    valid.note = Some("x".repeat(2048));
    valid.folder = Some("x".repeat(256));
    backend
        .validate_transfer_metadata("prod", &valid)
        .await
        .unwrap();
}
