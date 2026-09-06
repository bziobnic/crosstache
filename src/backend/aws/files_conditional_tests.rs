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
    persisted_metadata: Mutex<HashMap<String, String>>,
}
#[derive(Clone, Debug)]
struct TestClient(Arc<ConditionalTransport>);
impl HttpConnector for TestClient {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        if request.method() == "HEAD" {
            let mut response =
                HttpResponse::new(StatusCode::try_from(200).unwrap(), Vec::<u8>::new().into());
            response.headers_mut().insert("content-length", "10");
            response
                .headers_mut()
                .insert("content-type", "application/x-ciphertext");
            for (key, value) in self.0.persisted_metadata.lock().unwrap().iter() {
                response
                    .headers_mut()
                    .insert(format!("x-amz-meta-{key}"), value.clone());
            }
            return HttpConnectorFuture::ready(Ok(response));
        }
        if request.method() == "GET" {
            let body =
                b"<Tagging><TagSet><Tag><Key>env</Key><Value>prod</Value></Tag></TagSet></Tagging>"
                    .to_vec();
            return HttpConnectorFuture::ready(Ok(HttpResponse::new(
                StatusCode::try_from(200).unwrap(),
                body.into(),
            )));
        }
        assert_eq!(request.method(), "PUT");
        *self.0.puts.lock().unwrap() += 1;
        *self.0.persisted_metadata.lock().unwrap() = ["groups", "custom", "uploaded_at"]
            .into_iter()
            .filter_map(|key| {
                request
                    .headers()
                    .get(format!("x-amz-meta-{key}"))
                    .map(|value| (key.to_string(), value.to_string()))
            })
            .collect();
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
        persisted_metadata: Mutex::new(HashMap::new()),
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

#[test]
fn s3_complete_file_preflight_rejects_tags_and_qualified_key_without_requests() {
    use crate::backend::file::FileTransferRequest;
    let (backend, transport) = backend(200);
    let mut upload = request();
    upload.metadata = backend
        .prepare_transfer_metadata(&upload.groups, &upload.metadata)
        .unwrap();
    let validate = |vault: &str, upload: &FileUploadRequest| {
        backend.validate_transfer_request(&FileTransferRequest {
            vault,
            name: &upload.name,
            content_type: upload.content_type.as_deref(),
            groups: &upload.groups,
            tags: &upload.tags,
            metadata: &upload.metadata,
            size: upload.content.len() as u64,
        })
    };
    assert!(validate("a", &upload).is_ok());
    upload.tags = (0..11)
        .map(|i| (format!("tag-{i}"), "value".into()))
        .collect();
    assert!(
        validate("a", &upload).is_err(),
        "11 object tags must fail pure preflight"
    );
    upload.tags.clear();
    upload.name = format!("attachments/s/{}", "x".repeat(990));
    assert_eq!(validated_key("a", &upload.name).unwrap().len(), 1012);
    assert!(validate("a", &upload).is_ok());
    assert!(
        validate("destination-vault", &upload).is_err(),
        "full destination key is 1028 bytes"
    );
    assert_eq!(*transport.puts.lock().unwrap(), 0);
}

#[test]
fn s3_complete_file_preflight_enforces_tag_encoding_and_headers() {
    use crate::backend::file::FileTransferRequest;
    let (backend, transport) = backend(200);
    let upload = request();
    let metadata = backend
        .prepare_transfer_metadata(&upload.groups, &upload.metadata)
        .unwrap();
    for tags in [
        HashMap::from([("".into(), "value".into())]),
        HashMap::from([("x".repeat(129), "value".into())]),
        HashMap::from([("label".into(), "v".repeat(257))]),
        HashMap::from([("label".into(), "bad\u{0000}value".into())]),
        HashMap::from([("aws:reserved".into(), "value".into())]),
        (0..10)
            .map(|i| (format!("tag-{i}"), "界".repeat(256)))
            .collect(),
    ] {
        let result = backend.validate_transfer_request(&FileTransferRequest {
            vault: "a",
            name: &upload.name,
            content_type: upload.content_type.as_deref(),
            groups: &upload.groups,
            tags: &tags,
            metadata: &metadata,
            size: 10,
        });
        assert!(
            result.is_err(),
            "invalid tags must be refused without an upload"
        );
    }
    let result = backend.validate_transfer_request(&FileTransferRequest {
        vault: "a",
        name: &upload.name,
        content_type: Some("text/plain\r\nx-injected: true"),
        groups: &upload.groups,
        tags: &upload.tags,
        metadata: &metadata,
        size: 10,
    });
    assert!(result.is_err());
    assert!(backend
        .validate_transfer_request(&FileTransferRequest {
            vault: "a",
            name: &upload.name,
            content_type: upload.content_type.as_deref(),
            groups: &upload.groups,
            tags: &upload.tags,
            metadata: &metadata,
            size: u64::MAX,
        })
        .is_err());
    let valid = HashMap::from([("café".into(), "a+b / c".into())]);
    assert_eq!(
        encode_tagging(&valid).unwrap().as_deref(),
        Some("caf%C3%A9=a%2Bb+%2F+c")
    );
    assert_eq!(*transport.puts.lock().unwrap(), 0);
}
#[tokio::test]
async fn s3_conditional_create_preserves_request_and_sends_condition() {
    let (backend, transport) = backend(200);
    assert!(backend.supports_atomic_create());
    let mut expected = request();
    expected.metadata.insert("groups".into(), "one".into());
    let info = backend
        .upload_file_if_absent("prod", request(), None)
        .await
        .unwrap();
    let readback = backend.get_file_info("prod", "file").await.unwrap();
    assert_eq!(readback.groups, expected.groups);
    assert_eq!(readback.metadata, expected.metadata);
    assert_eq!(readback.tags, expected.tags);
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
fn s3_transfer_namespace_service_aliases_share_physical_identity() {
    for endpoint in [
        "https://s3.dualstack.us-east-1.amazonaws.com/",
        "https://s3-fips.us-east-1.amazonaws.com/",
        "https://s3-fips.dualstack.us-east-1.amazonaws.com/",
        "http://s3.us-east-1.amazonaws.com/",
    ] {
        let (backend, _) = backend(200);
        let expected = backend.transfer_namespace("prod").unwrap();
        assert_eq!(
            expected,
            backend
                .with_service_endpoint(Some(endpoint.into()))
                .transfer_namespace("prod")
                .unwrap(),
            "{endpoint}"
        );
    }
    let (backend, _) = backend(200);
    assert!(backend
        .with_service_endpoint(Some("https://unproven.s3.amazonaws.com/".into()))
        .transfer_namespace("prod")
        .is_err());
}

#[test]
fn s3_transfer_metadata_preflight_rejects_loss_and_header_limits() {
    let (backend, _) = backend(200);
    assert_eq!(
        backend
            .prepare_transfer_metadata(&["infra".into()], &HashMap::new())
            .unwrap()
            .get("groups")
            .map(String::as_str),
        Some("infra")
    );
    for (groups, metadata) in [
        (vec!["a,b".into()], HashMap::new()),
        (vec![" a".into()], HashMap::new()),
        (
            vec!["infra".into()],
            HashMap::from([("groups".into(), "other".into())]),
        ),
        (vec![], HashMap::from([("UPPER".into(), "value".into())])),
        (vec![], HashMap::from([("custom".into(), "x".repeat(2048))])),
        (
            vec![],
            HashMap::from([("custom".into(), "not\nroundtrip".into())]),
        ),
    ] {
        assert!(backend
            .prepare_transfer_metadata(&groups, &metadata)
            .is_err());
    }
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
