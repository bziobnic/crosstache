//! Exercise custody through the real AWS SDK serializer, transport and error mapper.

use super::{AttachmentKeyStore, BackendError, RawAttachmentKeyStore};
use crate::backend::aws::secrets::AwsSecretBackend;
use crate::secret::attachment_key::{
    retained_record_name, AttachmentKeyId, KEY_RECORD_CONTENT_TYPE,
};
use crate::secret::domain::SecretRequest;
use crate::secret::domain::SecretValue;
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

const VAULT: &str = "attachment-test";

#[derive(Debug, Default)]
struct ServiceState {
    records: HashMap<String, (String, String)>,
    operations: Vec<String>,
    list_pages: Vec<Value>,
    deny_describe: bool,
    describe_current: Option<String>,
    retirement_tags: HashMap<String, String>,
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
                "ListSecrets" => {
                    assert_eq!(body["Filters"][0]["Values"][0], format!("{VAULT}/"));
                    let page = match body.get("NextToken").and_then(Value::as_str) {
                        None => 0,
                        Some("page-2") => 1,
                        other => panic!("unexpected continuation token: {other:?}"),
                    };
                    response(
                        200,
                        state
                            .list_pages
                            .get(page)
                            .expect("configured list page")
                            .clone(),
                    )
                }
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
                "DescribeSecret" => {
                    if state.deny_describe {
                        response(
                            400,
                            json!({"__type": "AccessDeniedException", "Message": "metadata denied"}),
                        )
                    } else {
                        let name = body["SecretId"].as_str().unwrap();
                        let (version, _) = state.records.get(name).expect("committed key");
                        let current = state.describe_current.as_ref().unwrap_or(version);
                        let mut tags = vec![
                            json!({"Key": "xv:content_type", "Value": KEY_RECORD_CONTENT_TYPE}),
                            json!({"Key": "xv:original_name", "Value": name.strip_prefix(&format!("{VAULT}/")).unwrap()}),
                            json!({"Key": "owner", "Value": "custody"}),
                        ];
                        if let Some(value) = state.retirement_tags.get(name) {
                            tags.push(json!({"Key": crate::secret::attachment_key::KEY_RETIRED_TAG, "Value": value}));
                        }
                        response(
                            200,
                            json!({"Name": name, "Description": "retained note",
                            "Tags": tags,
                            "VersionIdsToStages": {current: ["AWSCURRENT"]}}),
                        )
                    }
                }
                "GetSecretValue" => {
                    let name = body["SecretId"].as_str().unwrap();
                    let (version, value) = state.records.get(name).expect("committed key");
                    if body.get("VersionId").is_some() {
                        assert_eq!(body["VersionId"], *version, "must read exact version");
                    }
                    assert!(body.get("VersionStage").is_none());
                    response(
                        200,
                        json!({"Name": name, "VersionId": version, "SecretString": value, "VersionStages": ["AWSPREVIOUS"]}),
                    )
                }
                "TagResource" => {
                    let name = body["SecretId"].as_str().unwrap().to_owned();
                    assert_eq!(
                        body["Tags"],
                        json!([{"Key": crate::secret::attachment_key::KEY_RETIRED_TAG, "Value": "true"}])
                    );
                    state.retirement_tags.insert(name, "true".into());
                    response(200, json!({}))
                }
                // Retirement is tag-only; value/description writes and removals
                // remain forbidden in this custody seam.
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
        value: SecretValue::new(identity.to_string().expose_secret().to_owned()),
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
    state.lock().unwrap().describe_current = Some("newer-current-version".into());
    let read = keys
        .get_secret_version(VAULT, &request.name, &committed.version, true)
        .await
        .unwrap();
    assert_eq!(read.version, committed.version);
    assert_eq!(read.content_type, KEY_RECORD_CONTENT_TYPE);
    assert!(read.enabled);
    assert_eq!(read.tags["owner"], "custody");
    assert_eq!(read.tags["note"], "retained note");
    assert_eq!(read.tags["aws:stages"], "AWSPREVIOUS");
    assert_eq!(
        read.value.unwrap().expose_secret(),
        request.value.expose_secret()
    );
    assert_eq!(
        state.lock().unwrap().operations,
        ["CreateSecret", "DescribeSecret", "GetSecretValue"]
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
    assert_eq!(
        read.value.unwrap().expose_secret(),
        request.value.expose_secret()
    );
    assert_eq!(winner.version, format!("{:032}", 1));
    let state = state.lock().unwrap();
    assert_eq!(state.records.len(), 1);
    assert_eq!(
        state.operations,
        [
            "CreateSecret",
            "CreateSecret",
            "DescribeSecret",
            "GetSecretValue"
        ]
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
        assert_eq!(
            read.value.unwrap().expose_secret(),
            request.value.expose_secret()
        );
    }
    let state = state.lock().unwrap();
    assert_eq!(state.records.len(), 2);
    assert_eq!(
        state.operations,
        [
            "CreateSecret",
            "CreateSecret",
            "DescribeSecret",
            "GetSecretValue",
            "DescribeSecret",
            "GetSecretValue"
        ]
    );
}

#[tokio::test]
async fn aws_retained_listing_preserves_content_type_and_generic_custody_hiding() {
    use crate::backend::guard::GuardedSecretBackend;
    use crate::backend::SecretBackend;

    let (backend, state) = backend(1);
    let first = format!("xv-attachment-key-ak1-{}", "a".repeat(64));
    let last = format!("xv-attachment-key-ak1-{}", "f".repeat(64));
    let collision = format!("xv-attachment-key-ak1-{}", "c".repeat(64));
    let entry = |name: &str, content_type: Option<&str>| {
        let tags: Vec<Value> = content_type
            .into_iter()
            .map(|ct| json!({"Key": "xv:content_type", "Value": ct}))
            .collect();
        json!({"Name": format!("{VAULT}/{name}"), "Tags": tags})
    };
    state.lock().unwrap().list_pages = vec![
        json!({"SecretList": [
            entry(&last, Some(KEY_RECORD_CONTENT_TYPE)),
            entry(&collision, None),
            entry("ordinary", Some("text/plain")),
        ], "NextToken": "page-2"}),
        json!({"SecretList": [
            entry(&first, Some(KEY_RECORD_CONTENT_TYPE)),
            entry("xv-attachment-key", Some(KEY_RECORD_CONTENT_TYPE)),
            entry("xv-attachment-key-notes", Some(KEY_RECORD_CONTENT_TYPE)),
            {"Name": format!("another-vault/{first}"),
             "Tags": [{"Key": "xv:content_type", "Value": KEY_RECORD_CONTENT_TYPE}]}
        ]}),
    ];

    let keys = RawAttachmentKeyStore::new(&backend)
        .list_retained_keys(VAULT)
        .await
        .unwrap();
    assert_eq!(
        keys.iter().map(|key| key.name.as_str()).collect::<Vec<_>>(),
        vec![first.as_str(), last.as_str()],
        "AWS tags must identify both retained records"
    );
    assert_eq!(keys[0].key_id, format!("ak1-{}", "a".repeat(64)));

    let raw = backend.list_secrets(VAULT, None).await.unwrap();
    assert_eq!(
        raw.iter()
            .find(|s| s.name == "ordinary")
            .unwrap()
            .content_type,
        "text/plain"
    );
    assert_eq!(
        raw.iter()
            .find(|s| s.name == collision)
            .unwrap()
            .content_type,
        ""
    );
    let visible = GuardedSecretBackend::new(&backend)
        .list_secrets(VAULT, None)
        .await
        .unwrap();
    let names: Vec<&str> = visible.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&collision.as_str()),
        "unmarked strict-name collisions stay visible"
    );
    assert!(names.contains(&"ordinary"));
    assert!(
        names.contains(&"xv-attachment-key-notes"),
        "broad prefix remains ordinary"
    );
    assert_eq!(
        names.len(),
        3,
        "generic lists hide only pointer and marked retained records"
    );
    assert_eq!(
        state.lock().unwrap().operations,
        vec!["ListSecrets"; 6],
        "all three reads must paginate without fetching values or extra metadata"
    );
}

#[tokio::test]
async fn aws_current_value_version_comes_from_value_response_not_describe() {
    use crate::backend::SecretBackend;
    let (backend, state) = backend(1);
    let request = key_request();
    let committed = RawAttachmentKeyStore::new(&backend)
        .commit_retained_key(VAULT, request.clone())
        .await
        .unwrap();
    state.lock().unwrap().describe_current = Some("different-described-version".into());
    let read = backend
        .get_secret(VAULT, &request.name, true)
        .await
        .unwrap();
    assert_eq!(read.version, committed.version);
    assert_eq!(
        read.value.unwrap().expose_secret(),
        request.value.expose_secret()
    );
    assert_eq!(read.content_type, KEY_RECORD_CONTENT_TYPE);
    assert_eq!(read.tags["aws:stages"], "AWSPREVIOUS");
}

#[tokio::test]
async fn aws_exact_value_read_fails_if_custody_metadata_cannot_be_read() {
    let (backend, state) = backend(1);
    let keys = RawAttachmentKeyStore::new(&backend);
    let request = key_request();
    let committed = keys
        .commit_retained_key(VAULT, request.clone())
        .await
        .unwrap();
    state.lock().unwrap().deny_describe = true;
    assert!(keys
        .get_secret_version(VAULT, &request.name, &committed.version, true)
        .await
        .is_err());
    assert_eq!(
        state.lock().unwrap().operations,
        ["CreateSecret", "DescribeSecret"]
    );
}

#[tokio::test]
async fn retirement_aws_transport_only_adds_one_tag_without_value_or_version_writes() {
    use crate::secret::attachment_key::{
        AttachmentKeyRef, KeySlot, SecretVersion, KEY_RETIRED_TAG,
    };
    let (backend, state) = backend(1);
    let keys = RawAttachmentKeyStore::new(&backend);
    let request = key_request();
    let committed = keys
        .commit_retained_key(VAULT, request.clone())
        .await
        .unwrap();
    let id = AttachmentKeyId::parse(
        request
            .name
            .strip_prefix(crate::secret::attachment_key::RETAINED_RECORD_PREFIX)
            .unwrap(),
    )
    .unwrap();
    let reference = AttachmentKeyRef {
        key_id: id,
        slot: KeySlot::Retained,
        provider_version: SecretVersion::new(committed.version.clone()),
    };
    let before = keys.get_secret(VAULT, &request.name, true).await.unwrap();
    state.lock().unwrap().operations.clear();
    let after = keys.mark_retired(VAULT, &reference).await.unwrap();
    assert_eq!(after.version, committed.version);
    assert_eq!(after.value, before.value);
    assert!(after.enabled);
    assert_eq!(after.tags[KEY_RETIRED_TAG], "true");
    for (key, value) in before.tags {
        assert_eq!(after.tags.get(&key), Some(&value));
    }
    let original = keys
        .get_secret_version(VAULT, &request.name, &committed.version, true)
        .await
        .unwrap();
    assert_eq!(
        original.value.unwrap().expose_secret(),
        request.value.expose_secret()
    );
    keys.mark_retired(VAULT, &reference).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.records.len(), 1);
    assert_eq!(
        state
            .operations
            .iter()
            .filter(|op| op.as_str() == "TagResource")
            .count(),
        1
    );
    assert!(state
        .operations
        .iter()
        .all(|op| ["DescribeSecret", "GetSecretValue", "TagResource"].contains(&op.as_str())));
}
