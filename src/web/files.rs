//! Upload preflight and conflict-safe file operations for the web API.

use std::sync::Arc;

use axum::extract::{Multipart, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::backend::error::BackendError;
use crate::backend::FileBackend;
use crate::blob::models::FileUploadRequest;

use super::api::{ApiError, VaultQuery};
use super::errors::ApiErrorBody;
use super::WebState;

pub(crate) const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
pub(crate) const MAX_PREFLIGHT_BODY_BYTES: usize = 512 * 1024;
const MAX_CANDIDATES: usize = 1000;
const MAX_CLIENT_ID_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 1024;
const MAX_CONTENT_TYPE_BYTES: usize = 256;
const MAX_DESTINATION_BYTES: usize = 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadCandidate {
    client_id: String,
    name: String,
    size: u64,
    content_type: String,
    destination: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadPreflightRequest {
    files: Vec<UploadCandidate>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UploadPreflightResult {
    client_id: String,
    status: &'static str,
    existing_name: Option<String>,
    suggested_name: Option<String>,
    max_bytes: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct UploadPreflightResponse {
    results: Vec<UploadPreflightResult>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ConflictPolicy {
    Skip,
    Replace,
    Rename,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadQuery {
    alias: Option<String>,
    backend: Option<String>,
    vault: Option<String>,
    policy: Option<ConflictPolicy>,
    target: Option<String>,
}

impl UploadQuery {
    fn target_context(&self, state: &WebState) -> Result<super::ScopedWebTarget, ApiError> {
        state.scoped_target(
            self.alias.as_deref(),
            self.backend.as_deref(),
            self.vault.as_deref(),
        )
    }
}

fn files_backend(backend: &dyn crate::backend::Backend) -> Result<&dyn FileBackend, ApiError> {
    if !backend.capabilities().has_file_storage {
        return Err(super::api::validation_error(
            StatusCode::NOT_IMPLEMENTED,
            "This backend does not provide file storage.",
            None,
        ));
    }
    backend.files().ok_or_else(|| {
        super::api::validation_error(
            StatusCode::NOT_IMPLEMENTED,
            "This backend does not provide file storage.",
            None,
        )
    })
}

fn invalid(message: &'static str, field: &'static str) -> ApiError {
    super::api::validation_error(StatusCode::BAD_REQUEST, message, Some(field))
}

fn file_conflict(name: &str, suggested_name: Option<String>) -> ApiError {
    ApiError::Structured {
        status: StatusCode::CONFLICT,
        error: Box::new(ApiErrorBody::file_conflict(name, suggested_name)),
    }
}

fn destination_name(destination: &str, name: &str) -> Result<String, ApiError> {
    if name.is_empty() {
        return Err(invalid("Choose a file with a name.", "name"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid("The file name is too long.", "name"));
    }
    if destination.len() > MAX_DESTINATION_BYTES {
        return Err(invalid("The destination is too long.", "destination"));
    }
    let destination = destination.trim_end_matches('/');
    if destination.is_empty() {
        Ok(name.to_string())
    } else {
        Ok(format!("{destination}/{name}"))
    }
}

fn validate_candidate(candidate: &UploadCandidate) -> Result<String, ApiError> {
    if candidate.client_id.is_empty() || candidate.client_id.len() > MAX_CLIENT_ID_BYTES {
        return Err(invalid(
            "The upload client identifier is invalid.",
            "client_id",
        ));
    }
    if candidate.content_type.len() > MAX_CONTENT_TYPE_BYTES {
        return Err(invalid("The content type is too long.", "content_type"));
    }
    destination_name(&candidate.destination, &candidate.name)
}

fn renamed_name(name: &str, number: usize) -> String {
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    match extension {
        Some(extension) => format!("{stem} ({number}).{extension}"),
        None => format!("{stem} ({number})"),
    }
}

async fn exists(files: &dyn FileBackend, vault: &str, name: &str) -> Result<bool, ApiError> {
    match files.get_file_info(vault, name).await {
        Ok(_) => Ok(true),
        Err(BackendError::NotFound { .. }) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn suggestion(files: &dyn FileBackend, vault: &str, name: &str) -> Result<String, ApiError> {
    for number in 2..=10_001 {
        let candidate = renamed_name(name, number);
        if !exists(files, vault, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(invalid(
        "No available rename suggestion could be found.",
        "name",
    ))
}

pub(crate) async fn preflight(
    State(state): State<Arc<WebState>>,
    Query(query): Query<VaultQuery>,
    payload: Result<Json<UploadPreflightRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<UploadPreflightResponse>, ApiError> {
    let Json(body) =
        payload.map_err(|_| invalid("The upload preflight request is invalid.", "files"))?;
    if body.files.is_empty() || body.files.len() > MAX_CANDIDATES {
        return Err(invalid(
            "Provide between 1 and 1000 upload candidates.",
            "files",
        ));
    }
    let target = query.target(&state)?;
    let files = files_backend(target.backend.as_ref())?;
    let mut results = Vec::with_capacity(body.files.len());
    for candidate in body.files {
        let name = validate_candidate(&candidate)?;
        // Metadata lookup is also the backend-specific name/path validation
        // boundary. Perform it even for oversized candidates, then report size
        // as the primary actionable result without attempting any write.
        let destination_exists = exists(files, &target.context.vault, &name).await?;
        let (status, existing_name, suggested_name) = if candidate.size > MAX_UPLOAD_BYTES {
            ("too-large", None, None)
        } else if destination_exists {
            let suggested = suggestion(files, &target.context.vault, &name).await?;
            ("conflict", Some(name), Some(suggested))
        } else {
            ("ready", None, None)
        };
        results.push(UploadPreflightResult {
            client_id: candidate.client_id,
            status,
            existing_name,
            suggested_name,
            max_bytes: MAX_UPLOAD_BYTES,
        });
    }
    Ok(Json(UploadPreflightResponse { results }))
}

pub(crate) async fn upload(
    State(state): State<Arc<WebState>>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    if query.policy != Some(ConflictPolicy::Rename) && query.target.is_some() {
        return Err(invalid(
            "A rename target is only valid with the Rename policy.",
            "target",
        ));
    }
    let target_name = match query.policy {
        Some(ConflictPolicy::Rename) => Some(
            query
                .target
                .as_deref()
                .ok_or_else(|| invalid("Provide a rename target.", "target"))?
                .to_string(),
        ),
        _ => None,
    };

    while let Some(field) = multipart.next_field().await.map_err(|_| {
        super::api::validation_error(
            StatusCode::BAD_REQUEST,
            "The upload could not be read.",
            Some("file"),
        )
    })? {
        if field.name() != Some("file") {
            continue;
        }
        let original_name = field
            .file_name()
            .map(str::to_string)
            .ok_or_else(|| invalid("Choose a file with a name.", "file"))?;
        let content_type = field.content_type().map(str::to_string);
        let content = field
            .bytes()
            .await
            .map_err(|_| invalid("The upload could not be read.", "file"))?
            .to_vec();
        if content.len() as u64 > MAX_UPLOAD_BYTES {
            return Err(super::api::validation_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "The upload is too large.",
                Some("file"),
            ));
        }
        let target = query.target_context(&state)?;
        let vault = &target.context.vault;
        let files = files_backend(target.backend.as_ref())?;
        let name = target_name.as_deref().unwrap_or(&original_name);
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(invalid("The file name is invalid.", "target"));
        }

        let request = FileUploadRequest {
            name: name.to_string(),
            content,
            content_type,
            groups: Vec::new(),
            metadata: std::collections::HashMap::new(),
            tags: std::collections::HashMap::new(),
        };
        match query.policy {
            Some(ConflictPolicy::Replace) => {
                let info = files.upload_file(vault, request, None).await?;
                return Ok(Json(
                    serde_json::to_value(info).expect("FileInfo serializes"),
                ));
            }
            Some(ConflictPolicy::Skip) | None => {
                if exists(files, vault, name).await? {
                    if query.policy == Some(ConflictPolicy::Skip) {
                        return Ok(Json(json!({ "status": "skipped", "name": name })));
                    }
                    return Err(file_conflict(
                        name,
                        Some(suggestion(files, vault, name).await?),
                    ));
                }
                match files.upload_file_if_absent(vault, request, None).await {
                    Ok(info) => {
                        return Ok(Json(
                            serde_json::to_value(info).expect("FileInfo serializes"),
                        ))
                    }
                    Err(BackendError::DestinationExists { .. })
                        if query.policy == Some(ConflictPolicy::Skip) =>
                    {
                        return Ok(Json(json!({ "status": "skipped", "name": name })))
                    }
                    Err(BackendError::DestinationExists { .. }) => {
                        return Err(file_conflict(
                            name,
                            Some(suggestion(files, vault, name).await?),
                        ))
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Some(ConflictPolicy::Rename) => {
                match files.upload_file_if_absent(vault, request, None).await {
                    Ok(info) => {
                        return Ok(Json(
                            serde_json::to_value(info).expect("FileInfo serializes"),
                        ))
                    }
                    Err(BackendError::DestinationExists { .. }) => {
                        return Err(file_conflict(
                            name,
                            Some(suggestion(files, vault, name).await?),
                        ))
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Err(invalid("Choose a file to upload.", "file"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::web::{self, testutil};

    fn state_with_backends(
        primary: Arc<testutil::stub::StubBackend>,
        secondary: Arc<testutil::stub::StubBackend>,
    ) -> Arc<web::WebState> {
        let mut context = testutil::test_context(primary.as_ref(), "default", 30);
        context
            .workspace
            .entries
            .push(super::super::context::WorkspaceEntrySummary {
                alias: "secondary-workspace".into(),
                backend: "secondary".into(),
                vault: "other".into(),
                default: false,
            });
        let registry = Arc::new(crate::backend::BackendRegistry::for_test(
            "primary",
            vec![
                ("primary", primary.clone()),
                ("secondary", secondary.clone()),
            ],
        ));
        Arc::new(web::WebState::new(
            primary,
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            super::super::preferences::PreferenceStore::new(
                std::env::temp_dir()
                    .join(format!("xv-file-scope-{}", uuid::Uuid::new_v4()))
                    .join("ui.json"),
                30,
            ),
            registry,
        ))
    }

    async fn json_request(method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = web::build_router(testutil::test_state())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    async fn upload(app: axum::Router, uri: &str, name: &str, bytes: &str) -> (StatusCode, Value) {
        let body = format!(
            "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/plain\r\n\r\n{bytes}\r\n--B--\r\n"
        );
        let response = app
            .oneshot(
                Request::post(uri)
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn preflight_reports_too_large_with_stable_limit() {
        let (status, body) = json_request(
            "POST",
            "/api/files/preflight",
            json!({"files":[{
                "client_id":"1",
                "name":"report.pdf",
                "size":104857601_u64,
                "content_type":"application/pdf",
                "destination":"docs"
            }]}),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["results"][0]["client_id"], "1");
        assert_eq!(body["results"][0]["status"], "too-large");
        assert_eq!(body["results"][0]["max_bytes"], 104857600_u64);
    }

    #[test]
    fn destination_join_does_not_normalize_away_backend_significant_prefixes() {
        let joined = |destination, name| match super::destination_name(destination, name) {
            Ok(name) => name,
            Err(_) => panic!("valid destination"),
        };
        assert_eq!(joined("/docs", "report.pdf"), "/docs/report.pdf");
        assert_eq!(joined("docs/", "report.pdf"), "docs/report.pdf");
    }

    #[tokio::test]
    async fn conflict_requires_an_explicit_policy_and_never_changes_existing_bytes() {
        let app = web::build_router(testutil::test_state());
        assert_eq!(
            upload(
                app.clone(),
                "/api/files?policy=replace",
                "report.pdf",
                "old"
            )
            .await
            .0,
            StatusCode::OK
        );

        let (status, body) = upload(app.clone(), "/api/files", "report.pdf", "new").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "xv-file-conflict");

        let downloaded = app
            .oneshot(
                Request::get("/api/files/report.pdf")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&downloaded[..], b"old");
    }

    #[tokio::test]
    async fn skip_replace_and_rename_have_explicit_stable_semantics() {
        let app = web::build_router(testutil::test_state());
        upload(
            app.clone(),
            "/api/files?policy=replace",
            "report.pdf",
            "old",
        )
        .await;

        let (status, skipped) = upload(
            app.clone(),
            "/api/files?policy=skip",
            "report.pdf",
            "ignored",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(skipped["status"], "skipped");
        assert_eq!(skipped["name"], "report.pdf");

        let (status, replaced) = upload(
            app.clone(),
            "/api/files?policy=replace",
            "report.pdf",
            "new",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["name"], "report.pdf");

        let (status, renamed) = upload(
            app.clone(),
            "/api/files?policy=rename&target=report%20%282%29.pdf",
            "report.pdf",
            "renamed",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renamed["name"], "report (2).pdf");
    }

    #[tokio::test]
    async fn preflight_suggestion_is_deterministic_and_unreserved() {
        let app = web::build_router(testutil::test_state());
        upload(
            app.clone(),
            "/api/files?policy=replace",
            "docs/report.pdf",
            "old",
        )
        .await;
        let candidate = json!({"files":[{
            "client_id":"a",
            "name":"report.pdf",
            "size":3,
            "content_type":"application/pdf",
            "destination":"docs"
        }]});

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/files/preflight")
                        .header(header::HOST, "127.0.0.1:1")
                        .header(header::AUTHORIZATION, "Bearer test-token")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(candidate.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(body["results"][0]["status"], "conflict");
            assert_eq!(body["results"][0]["existing_name"], "docs/report.pdf");
            assert_eq!(body["results"][0]["suggested_name"], "docs/report (2).pdf");
        }

        // Preflight does not reserve or create its suggestion.
        let (status, _) = upload(
            app,
            "/api/files?policy=rename&target=docs%2Freport%20%282%29.pdf",
            "report.pdf",
            "new",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn preflight_is_bounded_and_rejects_unknown_fields() {
        let files: Vec<Value> = (0..=1000)
            .map(|index| {
                json!({
                    "client_id": index.to_string(),
                    "name": "a",
                    "size": 1,
                    "content_type": "text/plain",
                    "destination": ""
                })
            })
            .collect();
        let (status, body) =
            json_request("POST", "/api/files/preflight", json!({"files": files})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "xv-invalid-argument");

        let (status, body) = json_request(
            "POST",
            "/api/files/preflight",
            json!({"files":[{
                "client_id":"1", "name":"a", "size":1,
                "content_type":"text/plain", "destination":"",
                "value":"must-not-appear-in-the-response"
            }]}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!body.to_string().contains("must-not-appear-in-the-response"));
    }

    #[tokio::test]
    async fn preflight_and_upload_require_the_file_storage_capability() {
        let backend = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "primary",
            crate::backend::BackendCapabilities::default(),
        ));
        let state = state_with_backends(backend.clone(), backend);
        let app = web::build_router(state);
        let candidate = json!({"files":[{
            "client_id":"1", "name":"a", "size":1,
            "content_type":"text/plain", "destination":""
        }]});
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/files/preflight")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(candidate.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"]["code"], "xv-invalid-argument");

        let (status, _) = upload(app, "/api/files?policy=replace", "a", "bytes").await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn explicit_scope_targets_only_the_exact_alias_backend_and_vault() {
        let capabilities = crate::backend::BackendCapabilities {
            has_file_storage: true,
            ..Default::default()
        };
        let primary = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "primary",
            capabilities.clone(),
        ));
        let secondary = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "secondary",
            capabilities,
        ));
        let app = web::build_router(state_with_backends(primary.clone(), secondary.clone()));

        let (status, _) = upload(
            app.clone(),
            "/api/files?alias=secondary-workspace&backend=secondary&vault=other&policy=replace",
            "scoped.txt",
            "secondary bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!primary.files.lock().unwrap().contains_key("scoped.txt"));
        assert_eq!(
            secondary.files.lock().unwrap()["scoped.txt"].0,
            b"secondary bytes"
        );

        let candidate = json!({"files":[{
            "client_id":"scope", "name":"scoped.txt", "size":15,
            "content_type":"text/plain", "destination":""
        }]});
        let response = app
            .clone()
            .oneshot(
                Request::post(
                    "/api/files/preflight?alias=secondary-workspace&backend=secondary&vault=other",
                )
                .header(header::HOST, "127.0.0.1:1")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(candidate.to_string()))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["results"][0]["status"], "conflict");
        assert_eq!(body["results"][0]["existing_name"], "scoped.txt");

        let (status, body) = upload(
            app,
            "/api/files?alias=secondary-workspace&backend=primary&vault=other&policy=replace",
            "wrong.txt",
            "no",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["field"], "workspace");
    }

    #[tokio::test]
    async fn concurrent_rename_target_uploads_have_one_winner_without_overwrite() {
        let app = web::build_router(testutil::test_state());
        let mut tasks = Vec::new();
        for index in 0..12 {
            let app = app.clone();
            tasks.push(tokio::spawn(async move {
                upload(
                    app,
                    "/api/files?policy=rename&target=winner.txt",
                    "source.txt",
                    &format!("candidate-{index}"),
                )
                .await
            }));
        }
        let mut successes = 0;
        let mut conflicts = 0;
        for task in tasks {
            match task.await.unwrap().0 {
                StatusCode::OK => successes += 1,
                StatusCode::CONFLICT => conflicts += 1,
                status => panic!("unexpected status {status}"),
            }
        }
        assert_eq!(successes, 1);
        assert_eq!(conflicts, 11);
    }
}
