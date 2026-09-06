use super::*;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use std::sync::Mutex;

#[derive(Debug)]
struct ConditionalTransport {
    put_status: u16,
    puts: Mutex<usize>,
}
#[derive(Clone, Debug)]
struct TestClient(Arc<ConditionalTransport>);
impl HttpConnector for TestClient {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        assert_eq!(request.method(), "PUT");
        *self.0.puts.lock().unwrap() += 1;
        assert_eq!(request.headers().get("if-none-match"), Some("*"));
        assert_eq!(
            request.headers().get("x-amz-meta-uploaded_at"),
            Some("original-time")
        );
        assert_eq!(request.headers().get("x-amz-meta-custom"), Some("kept"));
        assert_eq!(request.headers().get("x-amz-tagging"), Some("env=prod"));
        assert_eq!(
            request.headers().get("content-type"),
            Some("application/x-ciphertext")
        );
        assert_eq!(request.body().bytes().unwrap(), b"ciphertext");
        let status = self.0.put_status;
        let mut response = HttpResponse::new(
            StatusCode::try_from(status).unwrap(),
            Vec::<u8>::new().into(),
        );
        response.headers_mut().insert("content-length", "10");
        response.headers_mut().insert("etag", "etag");
        HttpConnectorFuture::ready(Ok(response))
    }
}
impl HttpClient for TestClient {
    fn http_connector(
        &self,
        _: &HttpConnectorSettings,
        _: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}
fn backend(status: u16) -> (AwsFileBackend, Arc<ConditionalTransport>) {
    let transport = Arc::new(ConditionalTransport {
        put_status: status,
        puts: Mutex::new(0),
    });
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .credentials_provider(aws_sdk_s3::config::Credentials::new(
            "test", "test", None, None, "test",
        ))
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(1))
        .http_client(TestClient(transport.clone()))
        .build();
    (
        AwsFileBackend::new(S3Client::from_conf(config), "bucket".into()),
        transport,
    )
}
fn request() -> FileUploadRequest {
    FileUploadRequest {
        name: "file".into(),
        content: b"ciphertext".to_vec(),
        content_type: Some("application/x-ciphertext".into()),
        groups: vec!["one".into()],
        metadata: HashMap::from([
            ("uploaded_at".into(), "original-time".into()),
            ("custom".into(), "kept".into()),
        ]),
        tags: HashMap::from([("env".into(), "prod".into())]),
    }
}
#[tokio::test]
async fn s3_conditional_create_preserves_request_and_sends_condition() {
    let (backend, transport) = backend(200);
    assert!(backend.supports_atomic_create());
    let expected = request();
    let info = backend
        .upload_file_if_absent("prod", request(), None)
        .await
        .unwrap();
    assert_eq!(info.metadata, expected.metadata);
    assert_eq!(info.tags, expected.tags);
    assert_eq!(info.groups, expected.groups);
    assert_eq!(*transport.puts.lock().unwrap(), 1);
}
#[tokio::test]
async fn s3_conditional_create_never_retries_unconditionally_after_errors() {
    for status in [409, 412, 403, 500] {
        let (backend, transport) = backend(status);
        let error = backend
            .upload_file_if_absent("prod", request(), None)
            .await
            .unwrap_err();
        if matches!(status, 409 | 412) {
            assert!(matches!(error, BackendError::Conflict(_)));
        } else {
            assert!(!matches!(
                error,
                BackendError::Conflict(_) | BackendError::Unsupported(_)
            ));
        }
        assert_eq!(*transport.puts.lock().unwrap(), 1);
    }
}

#[test]
fn s3_transfer_namespace_uses_bucket_and_prefix() {
    let (backend, _) = backend(200);
    let first = backend.transfer_namespace("prod").unwrap();
    assert_ne!(first, backend.transfer_namespace("other").unwrap());
    let mut other = backend;
    other.bucket = "other-bucket".into();
    assert_ne!(first, other.transfer_namespace("prod").unwrap());
}

#[test]
fn s3_transfer_namespace_normalizes_standard_endpoint_and_pins_custom_origin() {
    let (backend, _) = backend(200);
    let default = backend.transfer_namespace("prod").unwrap();
    let explicit =
        backend.with_service_endpoint(Some("https://s3.us-east-1.amazonaws.com/".into()));
    assert_eq!(default, explicit.transfer_namespace("prod").unwrap());
    let custom = explicit.with_service_endpoint(Some("https://s3.example.test/".into()));
    assert_ne!(default, custom.transfer_namespace("prod").unwrap());
}

#[test]
fn s3_transfer_namespace_refuses_access_point_aliases_with_unknown_bucket_identity() {
    let (mut backend, _) = backend(200);
    for bucket in [
        "arn:aws:s3:us-east-1:123456789012:accesspoint/example",
        "example-access-point-s3alias",
        "example.mrap",
    ] {
        backend.bucket = bucket.into();
        assert!(
            backend.transfer_namespace("prod").is_err(),
            "accepted alias {bucket}"
        );
    }
}
