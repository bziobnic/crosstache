use super::*;
use azure_core::{headers::Headers, HttpClient, Method, Request, Response, StatusCode};
use std::sync::Mutex;
#[derive(Debug)]
struct RestoreTransport {
    tag_status: StatusCode,
    puts: Mutex<usize>,
}
#[async_trait::async_trait]
impl HttpClient for RestoreTransport {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<Response> {
        let mut headers = Headers::new();
        for (key, value) in [
            ("x-ms-request-id", "00000000-0000-0000-0000-000000000001"),
            ("date", "Sat, 05 Sep 2026 12:00:00 GMT"),
            ("last-modified", "Sat, 05 Sep 2026 12:00:00 GMT"),
            ("x-ms-creation-time", "Sat, 05 Sep 2026 12:00:00 GMT"),
            ("x-ms-blob-type", "BlockBlob"),
            ("x-ms-server-encrypted", "true"),
            ("x-ms-request-server-encrypted", "true"),
            ("etag", "\"generation-a\""),
            ("content-length", "10"),
        ] {
            headers.insert(key, value);
        }
        let status = if *request.method() == Method::Put {
            *self.puts.lock().unwrap() += 1;
            for (key, value) in [
                ("x-ms-meta-uploaded_at", "original-time"),
                ("x-ms-meta-uploaded_by", "original-owner"),
                ("x-ms-meta-groups", "one, two"),
                ("x-ms-meta-custom", "kept"),
                ("x-ms-tags", "env=prod"),
                ("x-ms-blob-content-type", "application/x-ciphertext"),
            ] {
                assert_eq!(
                    request
                        .headers()
                        .get_str(&azure_core::headers::HeaderName::from(key))?,
                    value
                );
            }
            match request.body() {
                azure_core::Body::Bytes(bytes) => assert_eq!(bytes.as_ref(), b"ciphertext"),
                _ => panic!("expected bytes"),
            }
            StatusCode::Created
        } else if *request.method() == Method::Head {
            StatusCode::Ok
        } else {
            assert_eq!(*request.method(), Method::Get);
            self.tag_status
        };
        Ok(Response::new(
            status,
            headers,
            Box::pin(futures::stream::iter([Ok(Vec::<u8>::new().into())])),
        ))
    }
}
fn client(status: StatusCode) -> (BlobClient, Arc<RestoreTransport>) {
    let transport = Arc::new(RestoreTransport {
        tag_status: status,
        puts: Mutex::new(0),
    });
    let client = ClientBuilder::emulator()
        .transport(azure_core::TransportOptions::new(transport.clone()))
        .retry(azure_core::RetryOptions::none())
        .blob_client("container", "file");
    (client, transport)
}
#[tokio::test]
async fn azure_restore_info_propagates_every_tag_failure() {
    for status in [StatusCode::Forbidden, StatusCode::InternalServerError] {
        let (client, transport) = client(status);
        assert!(get_file_info_from_client(&client, "file", true)
            .await
            .is_err());
        assert!(get_file_info_from_client(&client, "file", false)
            .await
            .is_ok());
        assert_eq!(*transport.puts.lock().unwrap(), 0);
    }
}
#[tokio::test]
async fn azure_restore_upload_preserves_metadata_tags_and_bytes() {
    let (client, transport) = client(StatusCode::Ok);
    let metadata = HashMap::from([
        ("uploaded_at".into(), "original-time".into()),
        ("uploaded_by".into(), "original-owner".into()),
        ("groups".into(), "one, two".into()),
        ("custom".into(), "kept".into()),
    ]);
    let info = upload_file_to_client(
        &client,
        FileUploadRequest {
            name: "file".into(),
            content: b"ciphertext".to_vec(),
            content_type: Some("application/x-ciphertext".into()),
            groups: vec!["one".into(), "two".into()],
            metadata: metadata.clone(),
            tags: HashMap::from([("env".into(), "prod".into())]),
        },
        &crate::utils::progress::NoopReporter,
        true,
    )
    .await
    .unwrap();
    assert_eq!(info.metadata, metadata);
    assert_eq!(*transport.puts.lock().unwrap(), 1);
}
