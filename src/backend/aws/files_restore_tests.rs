use super::*;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use std::sync::Mutex;

#[derive(Debug)]
struct RestoreTransport {
    tag_status: u16,
    puts: Mutex<usize>,
}
#[derive(Clone, Debug)]
struct TestClient(Arc<RestoreTransport>);
impl HttpConnector for TestClient {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let status = if request.method() == "PUT" {
            *self.0.puts.lock().unwrap() += 1;
            assert_eq!(
                request.headers().get("x-amz-meta-uploaded_at"),
                Some("original-time")
            );
            assert_eq!(
                request.headers().get("x-amz-meta-uploaded_by"),
                Some("original-owner")
            );
            assert_eq!(request.headers().get("x-amz-meta-groups"), Some("one, two"));
            assert_eq!(request.headers().get("x-amz-meta-custom"), Some("kept"));
            assert_eq!(request.headers().get("x-amz-tagging"), Some("env=prod"));
            assert_eq!(
                request.headers().get("content-type"),
                Some("application/x-ciphertext")
            );
            assert_eq!(request.body().bytes().unwrap(), b"ciphertext");
            200
        } else if request.method() == "HEAD" {
            200
        } else {
            assert_eq!(request.method(), "GET");
            self.0.tag_status
        };
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
fn backend(status: u16) -> (AwsFileBackend, Arc<RestoreTransport>) {
    let transport = Arc::new(RestoreTransport {
        tag_status: status,
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
#[tokio::test]
async fn s3_restore_info_propagates_every_tag_failure() {
    for status in [403, 500] {
        let (backend, transport) = backend(status);
        let error = backend
            .get_file_restore_info("prod", "file")
            .await
            .unwrap_err();
        assert!(!matches!(error, BackendError::Unsupported(_)));
        assert!(backend.get_file_info("prod", "file").await.is_ok());
        assert_eq!(*transport.puts.lock().unwrap(), 0);
    }
}
#[tokio::test]
async fn s3_restore_upload_preserves_metadata_tags_and_bytes() {
    let (backend, transport) = backend(200);
    let metadata = HashMap::from([
        ("uploaded_at".into(), "original-time".into()),
        ("uploaded_by".into(), "original-owner".into()),
        ("groups".into(), "one, two".into()),
        ("custom".into(), "kept".into()),
    ]);
    let info = backend
        .restore_file(
            "prod",
            FileUploadRequest {
                name: "file".into(),
                content: b"ciphertext".to_vec(),
                content_type: Some("application/x-ciphertext".into()),
                groups: vec!["one".into(), "two".into()],
                metadata: metadata.clone(),
                tags: HashMap::from([("env".into(), "prod".into())]),
            },
        )
        .await
        .unwrap();
    assert_eq!(info.metadata, metadata);
    assert_eq!(*transport.puts.lock().unwrap(), 1);
}
