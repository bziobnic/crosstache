use super::*;
use azure_core::{headers::Headers, HttpClient, Method, Request, Response, StatusCode};
use std::sync::Mutex;

#[derive(Debug)]
struct SnapshotTransport {
    length: u64,
    missing: bool,
    replace_at_get: Option<usize>,
    truncate: bool,
    gets: Mutex<usize>,
}
#[async_trait::async_trait]
impl HttpClient for SnapshotTransport {
    async fn execute_request(&self, request: &Request) -> azure_core::Result<Response> {
        let mut headers = Headers::new();
        for (name, value) in [
            ("x-ms-request-id", "00000000-0000-0000-0000-000000000001"),
            ("date", "Sat, 05 Sep 2026 12:00:00 GMT"),
            ("last-modified", "Sat, 05 Sep 2026 12:00:00 GMT"),
            ("x-ms-creation-time", "Sat, 05 Sep 2026 12:00:00 GMT"),
            ("x-ms-blob-type", "BlockBlob"),
            ("x-ms-server-encrypted", "true"),
            ("etag", "\"generation-a\""),
            ("x-ms-meta-xv_key_version", "generation-a"),
        ] {
            headers.insert(name, value);
        }
        let (status, body) = if *request.method() == Method::Head {
            headers.insert("content-length", self.length.to_string());
            (
                if self.missing {
                    StatusCode::NotFound
                } else {
                    StatusCode::Ok
                },
                Vec::new(),
            )
        } else {
            assert_eq!(*request.method(), Method::Get);
            assert_eq!(
                request.headers().get_str(&azure_core::headers::IF_MATCH)?,
                "\"generation-a\""
            );
            let mut gets = self.gets.lock().unwrap();
            *gets += 1;
            if self.replace_at_get == Some(*gets) {
                (StatusCode::PreconditionFailed, Vec::new())
            } else {
                let start = (*gets as u64 - 1) * 3;
                let end = (start + 3).min(self.length);
                headers.insert(
                    "content-range",
                    format!("bytes {}-{}/{}", start, end - 1, self.length),
                );
                headers.insert("content-length", (end - start).to_string());
                let mut bytes = b"abcdef"[start as usize..end as usize].to_vec();
                if self.truncate {
                    bytes.pop();
                }
                (StatusCode::PartialContent, bytes)
            }
        };
        Ok(Response::new(
            status,
            headers,
            Box::pin(futures::stream::iter([Ok(body.into())])),
        ))
    }
}
fn client(
    length: u64,
    replace_at_get: Option<usize>,
    truncate: bool,
) -> (BlobClient, Arc<SnapshotTransport>) {
    let transport = Arc::new(SnapshotTransport {
        length,
        missing: false,
        replace_at_get,
        truncate,
        gets: Mutex::new(0),
    });
    let client = ClientBuilder::emulator()
        .transport(azure_core::TransportOptions::new(transport.clone()))
        .retry(azure_core::RetryOptions::none())
        .blob_client("container", "cert.pem");
    (client, transport)
}
#[tokio::test]
async fn azure_snapshot_pins_every_chunk_and_keeps_metadata() {
    let (client, transport) = client(6, None, false);
    let snapshot = download_snapshot_from_client(&client, 3, &crate::utils::progress::NoopReporter)
        .await
        .unwrap();
    assert_eq!(snapshot.content, b"abcdef");
    assert_eq!(snapshot.metadata["xv_key_version"], "generation-a");
    assert_eq!(*transport.gets.lock().unwrap(), 2);
}
#[tokio::test]
async fn azure_snapshot_refuses_replacement_at_first_or_later_get() {
    for when in [1, 2] {
        let (client, _) = client(6, Some(when), false);
        assert!(
            download_snapshot_from_client(&client, 3, &crate::utils::progress::NoopReporter)
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn azure_snapshot_empty_file_needs_no_get() {
    let (client, transport) = client(0, None, false);
    let snapshot = download_snapshot_from_client(&client, 3, &crate::utils::progress::NoopReporter)
        .await
        .unwrap();
    assert!(snapshot.content.is_empty());
    assert_eq!(snapshot.metadata["xv_key_version"], "generation-a");
    assert_eq!(*transport.gets.lock().unwrap(), 0);
}
#[tokio::test]
async fn azure_snapshot_refuses_oversized_and_truncated_bodies() {
    for (length, truncate) in [(MAX_DOWNLOAD_SIZE_BYTES + 1, false), (6, true)] {
        let (client, _) = client(length, None, truncate);
        assert!(
            download_snapshot_from_client(&client, 3, &crate::utils::progress::NoopReporter)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn azure_snapshot_preserves_missing_file_error() {
    let transport = Arc::new(SnapshotTransport {
        length: 0,
        missing: true,
        replace_at_get: None,
        truncate: false,
        gets: Mutex::new(0),
    });
    let client = ClientBuilder::emulator()
        .transport(azure_core::TransportOptions::new(transport))
        .retry(azure_core::RetryOptions::none())
        .blob_client("container", "cert.pem");
    let error = download_snapshot_from_client(&client, 3, &crate::utils::progress::NoopReporter)
        .await
        .err()
        .expect("missing blob must fail");
    assert!(
        matches!(error, CrosstacheError::VaultNotFound { .. }),
        "{error:?}"
    );
}
