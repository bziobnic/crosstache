use super::*;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct SnapshotTransport {
    length: Option<i64>,
    body: Vec<u8>,
    calls: AtomicUsize,
}
#[derive(Clone, Debug)]
struct TestClient(Arc<SnapshotTransport>);

impl HttpConnector for TestClient {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        assert_eq!(
            request.method(),
            "GET",
            "snapshot must not use separate HEAD or tagging calls"
        );
        assert!(url::Url::parse(request.uri())
            .unwrap()
            .path()
            .ends_with("/prod/files/cert.pem"));
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let mut response = HttpResponse::new(
            StatusCode::try_from(200).unwrap(),
            self.0.body.clone().into(),
        );
        if let Some(length) = self.0.length {
            response
                .headers_mut()
                .insert("content-length", length.to_string());
        }
        response
            .headers_mut()
            .insert("x-amz-meta-xv_key_version", "generation-a");
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
fn backend(length: Option<i64>, body: &[u8]) -> (AwsFileBackend, Arc<SnapshotTransport>) {
    let transport = Arc::new(SnapshotTransport {
        length,
        body: body.to_vec(),
        calls: AtomicUsize::new(0),
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
async fn s3_snapshot_uses_body_and_metadata_from_one_get() {
    for body in [b"ciphertext-a".as_slice(), b""] {
        let (backend, transport) = backend(Some(body.len() as i64), body);
        let snapshot = backend
            .download_file_snapshot("prod", "cert.pem", None)
            .await
            .unwrap();
        assert_eq!(snapshot.content, body);
        assert_eq!(snapshot.metadata["xv_key_version"], "generation-a");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn s3_snapshot_refuses_missing_invalid_oversized_or_truncated_lengths() {
    for length in [
        None,
        Some(-1),
        Some(2),
        Some(4),
        Some(MAX_DOWNLOAD_SIZE_BYTES as i64 + 1),
    ] {
        let (backend, _) = backend(length, b"abc");
        assert!(
            backend
                .download_file_snapshot("prod", "cert.pem", None)
                .await
                .is_err(),
            "accepted length {length:?}"
        );
    }
}
