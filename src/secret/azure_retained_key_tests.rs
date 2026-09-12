//! Hermetic coverage of the same REST transport used by AzureSecretOperations.
use super::*;
use crate::secret::domain::SecretValue;
use age::secrecy::ExposeSecret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Keep all endpoint substitution in tests. Production still constructs and
/// validates HTTPS Key Vault URLs before entering the shared HTTP helpers.
fn loopback_url(
    ops: &AzureSecretOperations,
    address: std::net::SocketAddr,
    path: &[&str],
) -> String {
    let vault = AzureVaultName::try_from("test-vault").unwrap();
    let mut url = url::Url::parse(&ops.key_vault_api_url(&vault, path).unwrap()).unwrap();
    url.set_scheme("http").unwrap();
    url.set_host(Some("127.0.0.1")).unwrap();
    url.set_port(Some(address.port())).unwrap();
    url.into()
}

async fn receive_request(stream: &mut tokio::net::TcpStream) -> (String, serde_json::Value) {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break offset + 4;
        }
        let mut chunk = [0u8; 1024];
        let count = stream.read(&mut chunk).await.unwrap();
        assert!(count > 0, "HTTP request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let mut chunk = [0u8; 1024];
        let count = stream.read(&mut chunk).await.unwrap();
        assert!(count > 0, "HTTP request ended before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    let body = if length == 0 {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
    };
    (headers.lines().next().unwrap().to_owned(), body)
}

#[tokio::test]
async fn azure_retained_key_interleaved_sets_preserve_exact_versions_over_http() {
    // If verification used latest, initializer A would read B's identity.
    // Likewise, returning the name rather than the version from the PUT id
    // would make the exact GET paths and returned version assertions fail.
    let name = "xv-attachment-key-ak1-test";
    let content_type = crate::secret::attachment_key::KEY_RECORD_CONTENT_TYPE;
    let values: Vec<String> = (0..2)
        .map(|_| {
            age::x25519::Identity::generate()
                .to_string()
                .expose_secret()
                .to_owned()
        })
        .collect();
    let tags = HashMap::from([("purpose".to_owned(), "retained-attachment-key".to_owned())]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected_values = values.clone();
    let server = tokio::spawn(async move {
        let mut versions = Vec::new();
        for step in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (line, body) = receive_request(&mut stream).await;
            let response = if let Some(expected_value) = expected_values.get(step) {
                assert_eq!(
                    line,
                    format!("PUT /secrets/{name}?api-version=7.4 HTTP/1.1")
                );
                // Do not include identity values in assertion diagnostics.
                assert!(body["value"].as_str() == Some(expected_value.as_str()));
                assert_eq!(body["contentType"], content_type);
                assert_eq!(body["tags"]["purpose"], "retained-attachment-key");
                assert_eq!(body["tags"]["original_name"], name);
                assert_eq!(body["tags"]["created_by"], "crosstache");
                assert_eq!(body["attributes"]["enabled"], true);
                let mut response = body;
                response["id"] = serde_json::json!(format!(
                    "https://test-vault.vault.azure.net/secrets/{name}/version-{}",
                    step + 1
                ));
                versions.push(response.clone());
                response
            } else {
                let index = step - 2;
                assert_eq!(
                    line,
                    format!(
                        "GET /secrets/{name}/version-{}?api-version=7.4 HTTP/1.1",
                        index + 1
                    )
                );
                versions[index].clone()
            };
            let body = serde_json::to_vec(&response).unwrap();
            let header = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        }
    });
    let ops = AzureSecretOperations::new(Arc::new(
        crate::auth::provider::DefaultAzureCredentialProvider::new().unwrap(),
    ));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let mut committed = Vec::new();
    // Both commits happen before either initializer verifies its version.
    for (index, value) in values.iter().enumerate() {
        let request = SecretRequest {
            name: name.into(),
            value: SecretValue::new(value.clone()),
            content_type: Some(content_type.into()),
            enabled: Some(true),
            expires_on: None,
            not_before: None,
            tags: Some(tags.clone()),
            groups: None,
            note: None,
            folder: None,
        };
        let (sanitized_name, prepared_tags) = ops.prepare_secret_request(&request).unwrap();
        let url = loopback_url(&ops, address, &["secrets", &sanitized_name]);
        let result = set_secret_http(
            client.put(&url),
            &url,
            &sanitized_name,
            &request,
            prepared_tags,
        )
        .await
        .unwrap();
        assert_eq!(result.version, format!("version-{}", index + 1));
        committed.push(result);
    }
    for (index, commit) in committed.iter().enumerate() {
        let url = loopback_url(&ops, address, &["secrets", name, &commit.version]);
        let (verified, verified_value) =
            get_secret_version_http(client.get(&url), &url, name, &commit.version)
                .await
                .unwrap();
        assert_eq!(verified.version, commit.version);
        assert_eq!(verified.name, name);
        assert_eq!(verified.original_name, name);
        assert_eq!(verified.content_type, content_type);
        assert_eq!(verified.tags, commit.tags);
        assert!(verified_value.unwrap().expose_secret() == values[index]);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn azure_retained_key_missing_exact_version_maps_to_backend_not_found() {
    let name = "xv-attachment-key-ak1-missing";
    let version = "missing-version";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (line, _) = receive_request(&mut stream).await;
        assert_eq!(
            line,
            format!("GET /secrets/{name}/{version}?api-version=7.4 HTTP/1.1")
        );
        let body =
            r#"{"error":{"code":"SecretNotFound","message":"Secret version was not found"}}"#;
        let header = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    });
    let ops = AzureSecretOperations::new(Arc::new(
        crate::auth::provider::DefaultAzureCredentialProvider::new().unwrap(),
    ));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let url = loopback_url(&ops, address, &["secrets", name, version]);
    let error = get_secret_version_http(client.get(&url), &url, name, version)
        .await
        .unwrap_err();
    let backend_error = crate::backend::azure::map_error(error);
    assert!(
        matches!(
            &backend_error,
            crate::backend::BackendError::NotFound { name: actual, suggestion: None } if actual == name
        ),
        "Expected missing version to preserve NotFound, got {backend_error:?}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
