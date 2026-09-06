//! Exercise custody through the real AWS SDK serializer, transport and error mapper.

use super::{AttachmentKeyStore, BackendError, RawAttachmentKeyStore};
use crate::backend::aws::secrets::AwsSecretBackend;
use crate::secret::attachment_key::{
    retained_record_name, AttachmentKeyId, KEY_RECORD_CONTENT_TYPE,
};
use crate::secret::manager::SecretRequest;
use age::secrecy::ExposeSecret;
use aws_sdk_secretsmanager::{config::Region, Client, Config};
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_runtime_api::http::StatusCode;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Barrier;
use zeroize::Zeroizing;

const VAULT: &str = "attachment-test";

#[derive(Debug, Default)]
struct ServiceState {
    records: HashMap<String, (String, String)>,
    operations: Vec<String>,
}

#[derive(Clone, Debug)]
struct RetainedKeyTransport {
    state: Arc<Mutex<ServiceState>>,
    create_barrier: Arc<Barrier>,
}

fn response(status: u16, body: Value) -> HttpResponse {
    let mut response = HttpResponse::new(
        StatusCode::try_from(status).unwrap(),
        body.to_string().into(),
    );
    response
        .headers_mut()
        .insert("content-type", "application/x-amz-json-1.1");
    response
}

impl HttpConnector for RetainedKeyTransport {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let operation = request
            .headers()
            .get("x-amz-target")
            .expect("AWS operation header")
            .rsplit('.')
            .next()
            .unwrap()
            .to_owned();
        let body: Value = serde_json::from_slice(request.body().bytes().unwrap()).unwrap();
        let transport = self.clone();
        HttpConnectorFuture::new(async move {
            // Both initializers must reach the provider before either can commit.
            if operation == "CreateSecret" {
                transport.create_barrier.wait().await;
            }
            let mut state = transport.state.lock().unwrap();
            state.operations.push(operation.clone());
            let result = match operation.as_str() {
                "CreateSecret" => {
                    let name = body["Name"].as_str().unwrap().to_owned();
                    let value = body["SecretString"].as_str().unwrap().to_owned();
                    assert!(name.starts_with(&format!("{VAULT}/xv-attachment-key-")));
                    assert!(body["Tags"].as_array().unwrap().iter().any(|tag| {
                        tag["Key"] == "xv:content_type" && tag["Value"] == KEY_RECORD_CONTENT_TYPE
                    }));
                    if state.records.contains_key(&name) {
                        let mut conflict = response(
                            400,
                            json!({"__type": "ResourceExistsException", "Message": "already exists"}),
                        );
                        conflict
                            .headers_mut()
                            .insert("x-amzn-errortype", "ResourceExistsException");
                        conflict
                    } else {
                        let version = format!("{:032}", state.records.len() + 1);
                        state.records.insert(name.clone(), (version.clone(), value));
                        response(200, json!({"Name": name, "VersionId": version}))
                    }
                }
                "GetSecretValue" => {
                    let name = body["SecretId"].as_str().unwrap();
                    let (version, value) = state.records.get(name).expect("committed key");
                    assert_eq!(body["VersionId"], *version, "must read exact version");
                    assert!(body.get("VersionStage").is_none());
                    response(
                        200,
                        json!({"Name": name, "VersionId": version, "SecretString": value}),
                    )
                }
                // PutSecretValue, UpdateSecret, TagResource, UntagResource and
                // latest-version fallbacks are forbidden in this custody seam.
                other => panic!("retained custody made forbidden AWS call: {other}"),
            };
            Ok(result)
        })
    }
}

impl HttpClient for RetainedKeyTransport {
    fn http_connector(
        &self,
        _settings: &HttpConnectorSettings,
        _components: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}

fn backend(concurrent_creates: usize) -> (AwsSecretBackend, Arc<Mutex<ServiceState>>) {
    let state = Arc::new(Mutex::new(ServiceState::default()));
    let transport = RetainedKeyTransport {
        state: state.clone(),
        create_barrier: Arc::new(Barrier::new(concurrent_creates)),
    };
    let config = Config::builder()
        .with_test_defaults()
        .region(Region::new("us-east-1"))
        .http_client(transport)
        .build();
    (
        AwsSecretBackend::new(Arc::new(Client::from_conf(config))),
        state,
    )
}

fn key_request() -> SecretRequest {
    let identity = age::x25519::Identity::generate();
    let key_id = AttachmentKeyId::derive(&identity.to_public().to_string());
    SecretRequest {
        name: retained_record_name(&key_id),
        value: Zeroizing::new(identity.to_string().expose_secret().to_owned()),
        content_type: Some(KEY_RECORD_CONTENT_TYPE.into()),
        enabled: None,
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        // An accidental generic upsert would also attempt UpdateSecret.
        note: Some("retained attachment identity".into()),
        folder: None,
    }
}

#[tokio::test]
async fn retained_commit_returns_create_secret_version_and_reads_exact_material() {
    let (backend, state) = backend(1);
    let keys = RawAttachmentKeyStore::new(&backend);
    let request = key_request();
    let committed = keys
        .commit_retained_key(VAULT, request.clone())
        .await
        .unwrap();
    assert_eq!(committed.name, request.name);
    assert_eq!(committed.version, format!("{:032}", 1));
    let read = keys
        .get_secret_version(VAULT, &request.name, &committed.version, true)
        .await
        .unwrap();
    assert_eq!(read.version, committed.version);
    assert_eq!(read.value.unwrap().as_str(), request.value.as_str());
    assert_eq!(
        state.lock().unwrap().operations,
        ["CreateSecret", "GetSecretValue"]
    );
}

#[tokio::test]
async fn concurrent_same_name_retained_commits_conflict_without_mutating_winner() {
    let (backend, state) = backend(2);
    let keys = RawAttachmentKeyStore::new(&backend);
    let request = key_request();
    let (left, right) = tokio::join!(
        keys.commit_retained_key(VAULT, request.clone()),
        keys.commit_retained_key(VAULT, request.clone()),
    );
    let winner = match (left, right) {
        (Ok(committed), Err(BackendError::Conflict(_)))
        | (Err(BackendError::Conflict(_)), Ok(committed)) => committed,
        other => panic!("expected one commit and one Conflict, got {other:?}"),
    };
    let read = keys
        .get_secret_version(VAULT, &request.name, &winner.version, true)
        .await
        .unwrap();
    assert_eq!(read.value.unwrap().as_str(), request.value.as_str());
    assert_eq!(winner.version, format!("{:032}", 1));
    let state = state.lock().unwrap();
    assert_eq!(state.records.len(), 1);
    assert_eq!(
        state.operations,
        ["CreateSecret", "CreateSecret", "GetSecretValue"]
    );
}

#[tokio::test]
async fn concurrent_distinct_retained_keys_remain_independently_exact_version_readable() {
    let (backend, state) = backend(2);
    let keys = RawAttachmentKeyStore::new(&backend);
    let first = key_request();
    let second = key_request();
    assert_ne!(first.name, second.name);
    let (first_commit, second_commit) = tokio::join!(
        keys.commit_retained_key(VAULT, first.clone()),
        keys.commit_retained_key(VAULT, second.clone()),
    );
    let first_commit = first_commit.unwrap();
    let second_commit = second_commit.unwrap();
    assert_ne!(first_commit.version, second_commit.version);
    for (request, committed) in [(&first, first_commit), (&second, second_commit)] {
        let read = keys
            .get_secret_version(VAULT, &request.name, &committed.version, true)
            .await
            .unwrap();
        assert_eq!(read.name, request.name);
        assert_eq!(read.version, committed.version);
        assert_eq!(read.value.unwrap().as_str(), request.value.as_str());
    }
    let state = state.lock().unwrap();
    assert_eq!(state.records.len(), 2);
    assert_eq!(
        state.operations,
        [
            "CreateSecret",
            "CreateSecret",
            "GetSecretValue",
            "GetSecretValue"
        ]
    );
}
