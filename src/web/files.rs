//! Upload preflight and conflict-safe file operations for the web API.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Multipart, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
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
pub(crate) const MAX_MULTIPART_ENVELOPE_BYTES: usize = MAX_UPLOAD_BYTES as usize + 64 * 1024;
pub(crate) const MAX_PREFLIGHT_BODY_BYTES: usize = 512 * 1024;
const MAX_CANDIDATES: usize = 1000;
const MAX_CLIENT_ID_BYTES: usize = 256;
const MAX_NAME_BYTES: usize = 1024;
const MAX_CONTENT_TYPE_BYTES: usize = 256;
const MAX_DESTINATION_BYTES: usize = 1024;
const MAX_SUGGESTION_ATTEMPTS: usize = 100;
const MAX_PREFLIGHT_METADATA_LOOKUPS: usize = 2000;

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
    destination: Option<String>,
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

fn validate_upload_size(size: u64) -> Result<(), ApiError> {
    if size > MAX_UPLOAD_BYTES {
        return Err(super::api::validation_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The upload is too large.",
            Some("file"),
        ));
    }
    Ok(())
}

pub(crate) async fn enforce_upload_envelope(
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_MULTIPART_ENVELOPE_BYTES)
    {
        return Err(super::api::validation_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The upload request is too large.",
            Some("file"),
        )
        .into_response());
    }
    Ok(next.run(request).await)
}

fn file_conflict(name: &str, suggested_name: Option<String>) -> ApiError {
    ApiError::Structured {
        status: StatusCode::CONFLICT,
        error: Box::new(ApiErrorBody::file_conflict(name, suggested_name)),
    }
}

fn validate_backend_file_name(
    files: &dyn FileBackend,
    name: &str,
    field: &'static str,
) -> Result<(), ApiError> {
    match files.validate_file_name(name) {
        Ok(()) => Ok(()),
        Err(BackendError::InvalidArgument(_)) => Err(invalid(
            "The file name is not valid for this backend.",
            field,
        )),
        Err(error) => Err(error.into()),
    }
}

fn validate_generic_file_name(name: &str, field: &'static str) -> Result<(), ApiError> {
    if name.is_empty() {
        return Err(invalid("Choose a file with a name.", field));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(invalid("The file name is too long.", field));
    }
    Ok(())
}

fn destination_name(destination: &str, name: &str) -> Result<String, ApiError> {
    validate_generic_file_name(name, "name")?;
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
    validate_content_type(&candidate.content_type)?;
    destination_name(&candidate.destination, &candidate.name)
}

fn validate_content_type(content_type: &str) -> Result<(), ApiError> {
    if content_type.is_empty()
        || content_type.len() > MAX_CONTENT_TYPE_BYTES
        || content_type.parse::<mime_guess::Mime>().is_err()
    {
        return Err(invalid("Provide a valid content type.", "content_type"));
    }
    Ok(())
}

fn renamed_name_parts(name: &str) -> (&str, &str, Option<&str>) {
    let (directory, filename) = match name.rsplit_once('/') {
        Some((directory, filename)) => (directory, filename),
        None => ("", name),
    };
    let (stem, extension) = match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (filename, None),
    };
    (directory, stem, extension)
}

fn renamed_name(directory: &str, stem: &str, extension: Option<&str>, number: usize) -> String {
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    match extension {
        Some(extension) => format!("{prefix}{stem} ({number}).{extension}"),
        None => format!("{prefix}{stem} ({number})"),
    }
}

fn backend_valid_renamed_name(
    files: &dyn FileBackend,
    name: &str,
    number: usize,
) -> Result<Option<String>, ApiError> {
    let (original_directory, original_stem, extension) = renamed_name_parts(name);
    for directory in [original_directory, ""] {
        if directory.is_empty() && original_directory.is_empty() {
            continue;
        }
        for preserve_extension in [true, false] {
            let mut stem = if preserve_extension {
                original_stem.to_string()
            } else {
                name.rsplit_once('/')
                    .map_or(name, |(_, filename)| filename)
                    .to_string()
            };
            let extension = preserve_extension.then_some(extension).flatten();
            for _ in 0..=MAX_NAME_BYTES {
                let candidate = renamed_name(directory, &stem, extension, number);
                if candidate.len() <= MAX_NAME_BYTES {
                    match files.validate_file_name(&candidate) {
                        Ok(()) => return Ok(Some(candidate)),
                        Err(BackendError::InvalidArgument(_)) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                let Some((index, _)) = stem.char_indices().next_back() else {
                    break;
                };
                stem.truncate(index);
            }
        }
        if original_directory.is_empty() {
            break;
        }
    }
    if original_directory.is_empty() {
        for preserve_extension in [true, false] {
            let mut stem = if preserve_extension {
                original_stem.to_string()
            } else {
                name.to_string()
            };
            let extension = preserve_extension.then_some(extension).flatten();
            for _ in 0..=MAX_NAME_BYTES {
                let candidate = renamed_name("", &stem, extension, number);
                if candidate.len() <= MAX_NAME_BYTES {
                    match files.validate_file_name(&candidate) {
                        Ok(()) => return Ok(Some(candidate)),
                        Err(BackendError::InvalidArgument(_)) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                let Some((index, _)) = stem.char_indices().next_back() else {
                    break;
                };
                stem.truncate(index);
            }
        }
    }
    Ok(None)
}

async fn exists(files: &dyn FileBackend, vault: &str, name: &str) -> Result<bool, ApiError> {
    match files.get_file_info(vault, name).await {
        Ok(_) => Ok(true),
        Err(BackendError::NotFound { .. }) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn suggestion(files: &dyn FileBackend, vault: &str, name: &str) -> Result<String, ApiError> {
    for number in 2..(2 + MAX_SUGGESTION_ATTEMPTS) {
        let Some(candidate) = backend_valid_renamed_name(files, name, number)? else {
            break;
        };
        if !exists(files, vault, &candidate).await? {
            return Ok(candidate);
        }
    }
    Err(invalid(
        "No available rename suggestion could be found.",
        "name",
    ))
}

async fn exists_cached(
    files: &dyn FileBackend,
    vault: &str,
    name: &str,
    cache: &mut HashMap<String, bool>,
    lookup_count: &mut usize,
) -> Result<bool, ApiError> {
    if let Some(exists) = cache.get(name) {
        return Ok(*exists);
    }
    if *lookup_count >= MAX_PREFLIGHT_METADATA_LOOKUPS {
        return Err(invalid(
            "The upload preflight requires too many conflict checks.",
            "files",
        ));
    }
    *lookup_count += 1;
    let destination_exists = exists(files, vault, name).await?;
    cache.insert(name.to_string(), destination_exists);
    Ok(destination_exists)
}

async fn suggestion_cached(
    files: &dyn FileBackend,
    vault: &str,
    name: &str,
    cache: &mut HashMap<String, bool>,
    lookup_count: &mut usize,
) -> Result<String, ApiError> {
    for number in 2..(2 + MAX_SUGGESTION_ATTEMPTS) {
        let Some(candidate) = backend_valid_renamed_name(files, name, number)? else {
            break;
        };
        if !exists_cached(files, vault, &candidate, cache, lookup_count).await? {
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
    let atomic_create = crate::backend::atomic_file_create_available(target.backend.as_ref());
    let mut results = Vec::with_capacity(body.files.len());
    let mut existence_cache = HashMap::new();
    let mut lookup_count = 0;
    for candidate in body.files {
        let name = validate_candidate(&candidate)?;
        validate_backend_file_name(files, &candidate.name, "name")?;
        validate_backend_file_name(
            files,
            &name,
            if candidate.destination.is_empty() {
                "name"
            } else {
                "destination"
            },
        )?;
        if !atomic_create {
            results.push(UploadPreflightResult {
                client_id: candidate.client_id,
                status: "unsupported",
                existing_name: None,
                suggested_name: None,
                max_bytes: MAX_UPLOAD_BYTES,
            });
            continue;
        }
        // Metadata lookup is also the backend-specific name/path validation
        // boundary. Perform it even for oversized candidates, then report size
        // as the primary actionable result without attempting any write.
        let destination_exists = exists_cached(
            files,
            &target.context.vault,
            &name,
            &mut existence_cache,
            &mut lookup_count,
        )
        .await?;
        let (status, existing_name, suggested_name) = if candidate.size > MAX_UPLOAD_BYTES {
            ("too-large", None, None)
        } else if destination_exists {
            let suggested = suggestion_cached(
                files,
                &target.context.vault,
                &name,
                &mut existence_cache,
                &mut lookup_count,
            )
            .await?;
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
    let scoped_target = query.target_context(&state)?;
    let scoped_files = files_backend(scoped_target.backend.as_ref())?;
    let destination = query.destination.as_deref().unwrap_or("");
    if destination.len() > MAX_DESTINATION_BYTES {
        return Err(invalid("The destination is too long.", "destination"));
    }
    if let Some(name) = target_name.as_deref() {
        validate_generic_file_name(name, "target")?;
        validate_backend_file_name(scoped_files, name, "target")?;
    }
    if query.policy != Some(ConflictPolicy::Replace)
        && !crate::backend::atomic_file_create_available(scoped_target.backend.as_ref())
    {
        return Err(ApiError::Backend(BackendError::Unsupported(
            "atomic create-only file upload".into(),
        )));
    }

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
        validate_generic_file_name(&original_name, "file")?;
        validate_backend_file_name(scoped_files, &original_name, "file")?;
        let destination_name = destination_name(destination, &original_name)?;
        let name = target_name.as_deref().unwrap_or(&destination_name);
        if target_name.is_none() {
            validate_backend_file_name(
                scoped_files,
                name,
                if destination.is_empty() {
                    "file"
                } else {
                    "destination"
                },
            )?;
        }
        let content_type = field.content_type().map(str::to_string);
        if let Some(content_type) = content_type.as_deref() {
            validate_content_type(content_type)?;
        }
        let content = field
            .bytes()
            .await
            .map_err(|_| invalid("The upload could not be read.", "file"))?
            .to_vec();
        validate_upload_size(content.len() as u64)?;
        let vault = &scoped_target.context.vault;
        let files = scoped_files;
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
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::body::Bytes;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::backend::FileBackend;
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

    #[test]
    fn multipart_envelope_truthfully_allows_exact_max_file_and_rejects_max_plus_one() {
        assert!(super::MAX_MULTIPART_ENVELOPE_BYTES > super::MAX_UPLOAD_BYTES as usize);
        assert!(
            super::MAX_MULTIPART_ENVELOPE_BYTES <= super::MAX_UPLOAD_BYTES as usize + 64 * 1024
        );
        assert!(super::validate_upload_size(super::MAX_UPLOAD_BYTES).is_ok());
        assert!(super::validate_upload_size(super::MAX_UPLOAD_BYTES + 1).is_err());
    }

    #[tokio::test]
    async fn multipart_route_enforces_envelope_boundary_from_content_length_without_allocating_it()
    {
        let app = web::build_router(testutil::test_state());
        let request = |length: usize| {
            Request::post("/api/files?policy=replace")
                .header(header::HOST, "127.0.0.1:1")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                .header(header::CONTENT_LENGTH, length)
                .body(Body::from("--B--\r\n"))
                .unwrap()
        };
        let at_limit = app
            .clone()
            .oneshot(request(super::MAX_MULTIPART_ENVELOPE_BYTES))
            .await
            .unwrap();
        assert_ne!(at_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let over_limit = app
            .oneshot(request(super::MAX_MULTIPART_ENVELOPE_BYTES + 1))
            .await
            .unwrap();
        assert_eq!(over_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
    async fn destination_is_applied_to_ready_skip_and_replace_without_changing_rename_target() {
        let app = web::build_router(testutil::test_state());

        let (status, ready) = upload(
            app.clone(),
            "/api/files?destination=docs",
            "ready.txt",
            "ready-bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ready["name"], "docs/ready.txt");

        let (status, _) = upload(
            app.clone(),
            "/api/files?policy=replace&destination=docs",
            "same.txt",
            "old-bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, skipped) = upload(
            app.clone(),
            "/api/files?policy=skip&destination=docs",
            "same.txt",
            "ignored-bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(skipped["name"], "docs/same.txt");
        let before_replace = app
            .clone()
            .oneshot(
                Request::get("/api/files/docs%2Fsame.txt")
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
        assert_eq!(&before_replace[..], b"old-bytes");

        let (status, replaced) = upload(
            app.clone(),
            "/api/files?policy=replace&destination=docs",
            "same.txt",
            "new-bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replaced["name"], "docs/same.txt");
        let after_replace = app
            .clone()
            .oneshot(
                Request::get("/api/files/docs%2Fsame.txt")
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
        assert_eq!(&after_replace[..], b"new-bytes");

        let (status, renamed) = upload(
            app,
            "/api/files?policy=rename&destination=ignored&target=docs%2Frenamed.txt",
            "same.txt",
            "renamed-bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renamed["name"], "docs/renamed.txt");
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
    async fn suggestions_at_backend_name_limit_are_valid_utf8_safe_and_uploadable() {
        let backend = Arc::new(testutil::stub::StubBackend::new().with_file_name_limit(255));
        let ascii = format!("{}.txt", "a".repeat(251));
        let multibyte = format!("{}.txt", "é".repeat(125));
        {
            let mut files = backend.files.lock().unwrap();
            files.insert(ascii.clone(), (b"old".to_vec(), "text/plain".into()));
            files.insert(multibyte.clone(), (b"old".to_vec(), "text/plain".into()));
        }
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        let body = json!({"files":[
            {"client_id":"ascii", "name":ascii, "size":3,
             "content_type":"text/plain", "destination":""},
            {"client_id":"utf8", "name":multibyte, "size":3,
             "content_type":"text/plain", "destination":""}
        ]});
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/files/preflight")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let ascii_suggestion = body["results"][0]["suggested_name"]
            .as_str()
            .unwrap()
            .to_string();
        let utf8_suggestion = body["results"][1]["suggested_name"]
            .as_str()
            .unwrap()
            .to_string();
        for suggestion in [&ascii_suggestion, &utf8_suggestion] {
            assert!(suggestion.len() <= 255);
            assert!(suggestion.ends_with(" (2).txt"));
            backend.validate_file_name(suggestion).unwrap();
            assert!(!backend.files.lock().unwrap().contains_key(suggestion));
        }

        let uri = format!(
            "/api/files?policy=rename&target={}",
            url::form_urlencoded::byte_serialize(ascii_suggestion.as_bytes()).collect::<String>()
        );
        let (status, _) = upload(app, &uri, "source.txt", "new").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(backend.files.lock().unwrap()[&ascii_suggestion].0, b"new");
        assert_eq!(backend.files.lock().unwrap()[&ascii].0, b"old");
    }

    #[tokio::test]
    async fn max_key_in_long_directory_keeps_conflict_contract_with_valid_fallback() {
        let backend = Arc::new(testutil::stub::StubBackend::new().with_file_name_limit(255));
        let directory = "d".repeat(253);
        let existing = format!("{directory}/x");
        backend
            .files
            .lock()
            .unwrap()
            .insert(existing.clone(), (b"old".to_vec(), "text/plain".into()));
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        let candidate = json!({"files":[{
            "client_id":"long-dir", "name":"x", "size":3,
            "content_type":"text/plain", "destination":directory
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
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["results"][0]["status"], "conflict");
        let suggested = body["results"][0]["suggested_name"].as_str();
        if let Some(suggested) = suggested {
            backend.validate_file_name(suggested).unwrap();
            assert!(!backend.files.lock().unwrap().contains_key(suggested));
        }

        let (status, body) = upload(app, "/api/files", &existing, "new").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "xv-file-conflict");
        assert_eq!(backend.files.lock().unwrap()[&existing].0, b"old");
        let upload_suggestion = body["error"]["suggested_name"].as_str();
        if let Some(suggested) = upload_suggestion {
            backend.validate_file_name(suggested).unwrap();
            assert!(!backend.files.lock().unwrap().contains_key(suggested));
        }
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
    async fn preflight_reports_atomic_create_unsupported_without_metadata_lookup() {
        let backend = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "primary",
            crate::backend::BackendCapabilities {
                has_file_storage: true,
                has_atomic_file_create: false,
                ..Default::default()
            },
        ));
        backend.files.lock().unwrap().insert(
            "existing.txt".into(),
            (b"existing".to_vec(), "text/plain".into()),
        );
        let state = state_with_backends(backend.clone(), backend.clone());
        let app = web::build_router(state);
        let candidate = json!({"files":[{
            "client_id":"1", "name":"existing.txt", "size":1,
            "content_type":"text/plain", "destination":""
        }]});
        let response = app
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
        assert_eq!(body["results"][0]["status"], "unsupported");
        assert_eq!(backend.file_info_calls(), 0);
    }

    #[tokio::test]
    async fn unsupported_atomic_policies_fail_without_mutation_but_replace_remains_available() {
        let backend = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "primary",
            crate::backend::BackendCapabilities {
                has_file_storage: true,
                has_atomic_file_create: false,
                ..Default::default()
            },
        ));
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        for uri in [
            "/api/files",
            "/api/files?policy=skip",
            "/api/files?policy=rename&target=renamed.txt",
        ] {
            let (status, body) = upload(app.clone(), uri, "source.txt", "must-not-write").await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
            assert_eq!(body["error"]["code"], "xv-operation-unsupported");
            assert!(backend.files.lock().unwrap().is_empty());
        }
        let (status, _) = upload(app, "/api/files?policy=replace", "source.txt", "replace").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(backend.files.lock().unwrap()["source.txt"].0, b"replace");
    }

    #[tokio::test]
    async fn invalid_content_type_is_field_specific_and_redacted() {
        for content_type in ["", "not a mime", "text/plain\r\nx-secret: marker"] {
            let (status, body) = json_request(
                "POST",
                "/api/files/preflight",
                json!({"files":[{
                    "client_id":"1", "name":"a", "size":1,
                    "content_type":content_type, "destination":""
                }]}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"]["field"], "content_type");
            if !content_type.is_empty() {
                assert!(!body.to_string().contains(content_type));
            }
        }
    }

    #[tokio::test]
    async fn backend_name_validation_is_field_specific_before_metadata_or_mutation() {
        let limited = Arc::new(testutil::stub::StubBackend::new().with_file_name_limit(10));
        let limited_app = web::build_router(state_with_backends(limited.clone(), limited.clone()));
        let candidate = json!({"files":[{
            "client_id":"1", "name":"name.txt", "size":1,
            "content_type":"text/plain", "destination":"folder"
        }]});
        let response = limited_app
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
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"]["field"], "destination");
        assert_eq!(
            limited.file_name_validation_calls(),
            2,
            "preflight must validate the original name before the combined destination"
        );
        assert_eq!(limited.file_info_calls(), 0);
        assert!(limited.files.lock().unwrap().is_empty());

        let unlimited = Arc::new(testutil::stub::StubBackend::new());
        let app = web::build_router(state_with_backends(unlimited.clone(), unlimited));
        let response = app
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
    }

    #[tokio::test]
    async fn invalid_rename_target_precedes_any_multipart_body_or_backend_access() {
        let backend = Arc::new(testutil::stub::StubBackend::new());
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        let content_polls = Arc::new(AtomicUsize::new(0));
        let long_name = "x".repeat(super::MAX_NAME_BYTES + 1);
        let polls = content_polls.clone();
        let stream = futures::stream::unfold(false, move |done| {
            let polls = polls.clone();
            async move {
                if done {
                    None
                } else {
                    polls.fetch_add(1, Ordering::SeqCst);
                    Some((
                        Ok::<_, Infallible>(Bytes::from_static(
                            b"--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"source.txt\"\r\n\r\nmust-not-buffer\r\n--B--\r\n",
                        )),
                        true,
                    ))
                }
            }
        });
        let uri = format!("/api/files?policy=rename&target={long_name}");
        let response = app
            .oneshot(
                Request::post(uri)
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                    .body(Body::from_stream(stream))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"]["field"], "target");
        assert_eq!(content_polls.load(Ordering::SeqCst), 0);
        assert_eq!(backend.file_name_validation_calls(), 0);
        assert_eq!(backend.file_info_calls(), 0);
        assert!(backend.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_original_multipart_name_is_attributed_to_file_before_backend_access() {
        let backend = Arc::new(testutil::stub::StubBackend::new());
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        let long_name = "x".repeat(super::MAX_NAME_BYTES + 1);
        let (status, body) = upload(
            app,
            "/api/files?policy=replace",
            &long_name,
            "must-not-write",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["field"], "file");
        assert_eq!(backend.file_name_validation_calls(), 0);
        assert_eq!(backend.file_info_calls(), 0);
        assert!(backend.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn explicit_scope_targets_only_the_exact_alias_backend_and_vault() {
        let capabilities = crate::backend::BackendCapabilities {
            has_file_storage: true,
            has_atomic_file_create: true,
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

    #[tokio::test]
    async fn duplicate_preflight_candidates_share_bounded_metadata_lookups() {
        let backend = Arc::new(testutil::stub::StubBackend::new());
        backend.files.lock().unwrap().insert(
            "report.pdf".into(),
            (b"old".to_vec(), "application/pdf".into()),
        );
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        let files: Vec<Value> = (0..100)
            .map(|index| {
                json!({
                    "client_id": index.to_string(),
                    "name":"report.pdf",
                    "size":3,
                    "content_type":"application/pdf",
                    "destination":""
                })
            })
            .collect();
        let response = app
            .oneshot(
                Request::post("/api/files/preflight")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"files":files}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            backend.file_info_calls() <= 2,
            "duplicate candidates must share conflict and suggestion lookups"
        );
    }

    #[tokio::test]
    async fn adversarial_preflight_has_a_request_wide_metadata_lookup_budget() {
        let backend = Arc::new(testutil::stub::StubBackend::new());
        let mut candidates = Vec::new();
        {
            let mut stored = backend.files.lock().unwrap();
            for file_index in 0..30 {
                let name = format!("report-{file_index}.pdf");
                stored.insert(name.clone(), (b"old".to_vec(), "application/pdf".into()));
                for suffix in 2..101 {
                    stored.insert(
                        format!("report-{file_index} ({suffix}).pdf"),
                        (b"old".to_vec(), "application/pdf".into()),
                    );
                }
                candidates.push(json!({
                    "client_id": file_index.to_string(),
                    "name":name,
                    "size":3,
                    "content_type":"application/pdf",
                    "destination":""
                }));
            }
        }
        let app = web::build_router(state_with_backends(backend.clone(), backend.clone()));
        let response = app
            .oneshot(
                Request::post("/api/files/preflight")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"files":candidates}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            backend.file_info_calls() <= 2000,
            "preflight must stop at its request-wide lookup budget"
        );
    }
}
