//! JSON API handlers. Parse → delegate to backend traits → serialize.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;

use crate::backend::error::BackendError;
use crate::error::CrosstacheError;
use crate::secret::manager::{
    FieldUpdate, SecretProperties, SecretRequest, SecretSummary, SecretUpdateRequest,
};

use super::WebState;

pub(crate) enum ApiError {
    App(CrosstacheError),
    /// Ad-hoc status/message for handler-level validation errors that don't
    /// map to a `CrosstacheError` variant. Constructed starting in Task 4's
    /// secret handlers (request parsing/validation).
    #[allow(dead_code)]
    Status(StatusCode, String),
}

impl From<CrosstacheError> for ApiError {
    fn from(e: CrosstacheError) -> Self {
        Self::App(e)
    }
}

impl From<BackendError> for ApiError {
    fn from(e: BackendError) -> Self {
        Self::App(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::Status(s, m) => (s, m),
            ApiError::App(e) => {
                use CrosstacheError::*;
                let status = match &e {
                    SecretNotFound { .. } | VaultNotFound { .. } => StatusCode::NOT_FOUND,
                    PermissionDenied(_) => StatusCode::FORBIDDEN,
                    AuthenticationError(_) => StatusCode::UNAUTHORIZED,
                    Conflict(_) => StatusCode::CONFLICT,
                    RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
                    InvalidArgument(_) => StatusCode::BAD_REQUEST,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                (status, e.to_string())
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

pub(crate) async fn get_context(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let caps = state.backend.capabilities();
    Json(json!({
        "backend": state.backend.name(),
        "vault": state.vault,
        "capabilities": {
            "vaults": caps.has_vaults,
            "files": caps.has_file_storage,
            "folders": caps.has_folders,
            "groups": caps.has_groups,
            "notes": caps.has_notes,
            "expiry": caps.has_expiry,
        }
    }))
}

pub(crate) async fn list_vaults(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match state.backend.vaults() {
        Some(v) => {
            let vaults = v.list_vaults(None).await?;
            Ok(Json(json!({ "vaults": vaults })))
        }
        None => Ok(Json(json!({ "vaults": [{ "name": state.vault }] }))),
    }
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    vault: Option<String>,
    group: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct VaultQuery {
    vault: Option<String>,
}

impl VaultQuery {
    fn vault<'a>(&'a self, state: &'a WebState) -> &'a str {
        self.vault.as_deref().unwrap_or(&state.vault)
    }
}

pub(crate) async fn list_secrets(
    State(state): State<Arc<WebState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SecretSummary>>, ApiError> {
    let vault = q.vault.as_deref().unwrap_or(&state.vault);
    let secrets = state
        .backend
        .secrets()
        .list_secrets(vault, q.group.as_deref())
        .await?;
    Ok(Json(secrets))
}

pub(crate) async fn get_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
) -> Result<Json<SecretProperties>, ApiError> {
    let props = state
        .backend
        .secrets()
        .get_secret(q.vault(&state), &name, false)
        .await?;
    Ok(Json(props))
}

pub(crate) async fn reveal_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let props = state
        .backend
        .secrets()
        .get_secret(q.vault(&state), &name, true)
        .await?;
    Ok(Json(
        json!({ "value": props.value.as_ref().map(|v| v.as_str()) }),
    ))
}

#[derive(Deserialize)]
pub(crate) struct PutSecretBody {
    value: String,
    content_type: Option<String>,
    expires_on: Option<DateTime<Utc>>,
    not_before: Option<DateTime<Utc>>,
    tags: Option<HashMap<String, String>>,
    groups: Option<Vec<String>>,
    note: Option<String>,
    folder: Option<String>,
}

pub(crate) async fn put_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
    Json(body): Json<PutSecretBody>,
) -> Result<Json<SecretProperties>, ApiError> {
    let request = SecretRequest {
        name: name.clone(),
        value: Zeroizing::new(body.value),
        content_type: body.content_type,
        enabled: Some(true),
        expires_on: body.expires_on,
        not_before: body.not_before,
        tags: body.tags,
        groups: body.groups,
        note: body.note,
        folder: body.folder,
    };
    let props = state
        .backend
        .secrets()
        .set_secret(q.vault(&state), request)
        .await?;
    Ok(Json(props))
}

/// Metadata-only update. Optional string fields: absent = unchanged,
/// "" = clear, anything else = set. Groups/tags replace wholesale when present.
#[derive(Deserialize)]
pub(crate) struct PatchSecretBody {
    enabled: Option<bool>,
    expires_on: Option<String>,
    not_before: Option<String>,
    tags: Option<HashMap<String, String>>,
    groups: Option<Vec<String>>,
    note: Option<String>,
    folder: Option<String>,
}

fn str_field(v: Option<String>) -> FieldUpdate<String> {
    match v {
        None => FieldUpdate::Unchanged,
        Some(s) if s.is_empty() => FieldUpdate::Clear,
        Some(s) => FieldUpdate::Set(s),
    }
}

fn date_field(v: Option<String>) -> Result<FieldUpdate<DateTime<Utc>>, ApiError> {
    match v {
        None => Ok(FieldUpdate::Unchanged),
        Some(s) if s.is_empty() => Ok(FieldUpdate::Clear),
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| FieldUpdate::Set(d.with_timezone(&Utc)))
            .map_err(|e| {
                ApiError::Status(StatusCode::BAD_REQUEST, format!("bad timestamp '{s}': {e}"))
            }),
    }
}

pub(crate) async fn patch_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
    Json(body): Json<PatchSecretBody>,
) -> Result<Json<SecretProperties>, ApiError> {
    let request = SecretUpdateRequest {
        name: name.clone(),
        value: None,
        content_type: None,
        enabled: body.enabled,
        expires_on: date_field(body.expires_on)?,
        not_before: date_field(body.not_before)?,
        tags: body.tags,
        groups: body.groups,
        note: str_field(body.note),
        folder: str_field(body.folder),
        replace_tags: true,
        replace_groups: true,
    };
    let props = state
        .backend
        .secrets()
        .update_secret(q.vault(&state), &name, request)
        .await?;
    Ok(Json(props))
}

pub(crate) async fn delete_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
) -> Result<StatusCode, ApiError> {
    state
        .backend
        .secrets()
        .delete_secret(q.vault(&state), &name)
        .await?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
pub(crate) struct MoveBody {
    new_name: Option<String>,
    folder: Option<String>,
}

pub(crate) async fn move_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
    Json(body): Json<MoveBody>,
) -> Result<Json<SecretProperties>, ApiError> {
    let vault = q.vault(&state).to_string();
    match (body.new_name, body.folder) {
        (Some(new_name), None) => {
            let props = state
                .backend
                .secrets()
                .rename_secret(&vault, &name, &new_name)
                .await?;
            Ok(Json(props))
        }
        (None, Some(folder)) => {
            let request = SecretUpdateRequest {
                name: name.clone(),
                value: None,
                content_type: None,
                enabled: None,
                expires_on: FieldUpdate::Unchanged,
                not_before: FieldUpdate::Unchanged,
                tags: None,
                groups: None,
                note: FieldUpdate::Unchanged,
                folder: str_field(Some(folder)),
                replace_tags: false,
                replace_groups: false,
            };
            let props = state
                .backend
                .secrets()
                .update_secret(&vault, &name, request)
                .await?;
            Ok(Json(props))
        }
        _ => Err(ApiError::Status(
            StatusCode::BAD_REQUEST,
            "provide exactly one of new_name or folder".into(),
        )),
    }
}

#[cfg(feature = "file-ops")]
pub(crate) mod files {
    use super::*;
    use crate::backend::FileBackend;
    use crate::blob::models::{FileListRequest, FileUploadRequest};
    use axum::extract::Multipart;
    use axum::http::header;
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

    /// RFC 5987 `attr-char`: ALPHA / DIGIT / "!" / "#" / "$" / "&" / "+" / "-"
    /// / "." / "^" / "_" / "`" / "|" / "~". Everything else (including all
    /// non-ASCII bytes, which `AsciiSet` always encodes) gets percent-encoded.
    const ATTR_CHAR: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'!')
        .remove(b'#')
        .remove(b'$')
        .remove(b'&')
        .remove(b'+')
        .remove(b'-')
        .remove(b'.')
        .remove(b'^')
        .remove(b'_')
        .remove(b'`')
        .remove(b'|')
        .remove(b'~');

    fn files_backend(state: &WebState) -> Result<&dyn FileBackend, ApiError> {
        state.backend.files().ok_or_else(|| {
            ApiError::Status(
                StatusCode::NOT_IMPLEMENTED,
                format!("the {} backend has no file storage", state.backend.name()),
            )
        })
    }

    #[derive(Deserialize)]
    pub(crate) struct FilesQuery {
        vault: Option<String>,
        prefix: Option<String>,
    }

    pub(crate) async fn list_files(
        State(state): State<Arc<WebState>>,
        Query(q): Query<FilesQuery>,
    ) -> Result<Json<Vec<crate::blob::models::FileInfo>>, ApiError> {
        let vault = q.vault.as_deref().unwrap_or(&state.vault);
        let request = FileListRequest {
            prefix: q.prefix,
            groups: None,
            limit: None,
            delimiter: None,
        };
        Ok(Json(
            files_backend(&state)?.list_files(vault, request).await?,
        ))
    }

    pub(crate) async fn upload_file(
        State(state): State<Arc<WebState>>,
        Query(q): Query<VaultQuery>,
        mut multipart: Multipart,
    ) -> Result<Json<crate::blob::models::FileInfo>, ApiError> {
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| ApiError::Status(StatusCode::BAD_REQUEST, format!("bad multipart: {e}")))?
        {
            if field.name() != Some("file") {
                continue;
            }
            let name = field.file_name().map(str::to_string).ok_or_else(|| {
                ApiError::Status(StatusCode::BAD_REQUEST, "file part needs a filename".into())
            })?;
            let content_type = field.content_type().map(str::to_string);
            let content = field
                .bytes()
                .await
                .map_err(|e| {
                    ApiError::Status(StatusCode::BAD_REQUEST, format!("read upload: {e}"))
                })?
                .to_vec();
            let request = FileUploadRequest {
                name,
                content,
                content_type,
                groups: Vec::new(),
                metadata: std::collections::HashMap::new(),
                tags: std::collections::HashMap::new(),
            };
            let info = files_backend(&state)?
                .upload_file(q.vault(&state), request, None)
                .await?;
            return Ok(Json(info));
        }
        Err(ApiError::Status(
            StatusCode::BAD_REQUEST,
            "multipart body needs a 'file' part".into(),
        ))
    }

    pub(crate) async fn download_file(
        State(state): State<Arc<WebState>>,
        Path(name): Path<String>,
        Query(q): Query<VaultQuery>,
    ) -> Result<Response, ApiError> {
        let vault = q.vault(&state);
        let backend = files_backend(&state)?;
        let info = backend.get_file_info(vault, &name).await?;
        let bytes = backend.download_file(vault, &name, None).await?;
        // Escape \ then " so an untrusted filename can't break out of the
        // quoted-string and forge extra header parameters. CRLF is already
        // rejected by HeaderValue parsing.
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        // HeaderValue is ASCII-only, so a non-ASCII name (e.g. "résumé.pdf")
        // can't go in the plain `filename=` param. Per RFC 5987/6266, ship an
        // ASCII fallback (non-ASCII/control bytes replaced with '_') alongside
        // a percent-encoded `filename*=UTF-8''...` param for clients that
        // support it.
        let ascii_fallback: String = escaped
            .chars()
            .map(|c| {
                if c.is_ascii() && !c.is_control() {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let pct_encoded = utf8_percent_encode(&name, ATTR_CHAR);
        let content_disposition =
            format!("attachment; filename=\"{ascii_fallback}\"; filename*=UTF-8''{pct_encoded}");
        Ok((
            [
                (header::CONTENT_TYPE, info.content_type),
                (header::CONTENT_DISPOSITION, content_disposition),
            ],
            bytes,
        )
            .into_response())
    }

    pub(crate) async fn delete_file(
        State(state): State<Arc<WebState>>,
        Path(name): Path<String>,
        Query(q): Query<VaultQuery>,
    ) -> Result<StatusCode, ApiError> {
        files_backend(&state)?
            .delete_file(q.vault(&state), &name)
            .await?;
        Ok(StatusCode::OK)
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::web::testutil;

    pub(crate) async fn get_json(
        app: axum::Router,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:1")
            .header(header::AUTHORIZATION, "Bearer test-token");
        let req = match body {
            Some(v) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    #[tokio::test]
    async fn context_reports_backend_and_capabilities() {
        let app = crate::web::build_router(testutil::test_state());
        let (status, json) = get_json(app, "GET", "/api/context", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["backend"], "stub");
        assert_eq!(json["vault"], "default");
        assert_eq!(json["capabilities"]["folders"], true);
        #[cfg(feature = "file-ops")]
        assert_eq!(json["capabilities"]["files"], true);
        #[cfg(not(feature = "file-ops"))]
        assert_eq!(json["capabilities"]["files"], false);
    }

    #[tokio::test]
    async fn vaults_falls_back_to_current_when_unsupported() {
        let app = crate::web::build_router(testutil::test_state());
        let (status, json) = get_json(app, "GET", "/api/vaults", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["vaults"][0]["name"], "default");
    }

    #[tokio::test]
    async fn api_requires_token() {
        let app = crate::web::build_router(testutil::test_state());
        let res = app
            .oneshot(
                Request::get("/api/context")
                    .header(header::HOST, "127.0.0.1:1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn secret_crud_roundtrip() {
        let app = crate::web::build_router(testutil::test_state());

        // create
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/db-pass",
            Some(json!({"value": "hunter2", "folder": "proj/db", "note": "primary"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // list shows metadata, not value
        let (status, json_body) = get_json(app.clone(), "GET", "/api/secrets", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body[0]["name"], "db-pass");
        assert_eq!(json_body[0]["folder"], "proj/db");
        assert!(!json_body.to_string().contains("hunter2"));

        // metadata get has null value
        let (status, json_body) = get_json(app.clone(), "GET", "/api/secrets/db-pass", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(json_body["value"].is_null());

        // reveal
        let (status, json_body) =
            get_json(app.clone(), "POST", "/api/secrets/db-pass/value", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body["value"], "hunter2");

        // patch metadata: set note, clear folder ("" = clear)
        let (status, _) = get_json(
            app.clone(),
            "PATCH",
            "/api/secrets/db-pass",
            Some(json!({"note": "rotated", "folder": ""})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, json_body) = get_json(app.clone(), "GET", "/api/secrets", None).await;
        assert_eq!(json_body[0]["note"], "rotated");
        assert!(json_body[0]["folder"].is_null());

        // rename via move
        let (status, _) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/db-pass/move",
            Some(json!({"new_name": "db-pass-2"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // delete
        let (status, _) = get_json(app.clone(), "DELETE", "/api/secrets/db-pass-2", None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get_json(app.clone(), "GET", "/api/secrets/db-pass-2", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn move_folder_only_updates_folder() {
        let app = crate::web::build_router(testutil::test_state());
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/a",
            Some(json!({"value": "v"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/a/move",
            Some(json!({"folder": "new-folder"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json_body) = get_json(app, "GET", "/api/secrets", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body[0]["folder"], "new-folder");
    }

    #[tokio::test]
    async fn put_preserves_content_type_and_custom_tags_across_edits() {
        let app = crate::web::build_router(testutil::test_state());

        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/api-key",
            Some(json!({
                "value": "v1",
                "content_type": "text/plain",
                "tags": {"custom": "kept"},
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json_body) = get_json(app.clone(), "GET", "/api/secrets/api-key", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body["content_type"], "text/plain");
        assert_eq!(json_body["tags"]["custom"], "kept");

        // Simulate a value edit that echoes the previously-fetched
        // content_type/tags back (as the fixed frontend now does) and
        // confirm they still survive a second PUT.
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/api-key",
            Some(json!({
                "value": "v2",
                "content_type": "text/plain",
                "tags": {"custom": "kept"},
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json_body) = get_json(app, "GET", "/api/secrets/api-key", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body["content_type"], "text/plain");
        assert_eq!(json_body["tags"]["custom"], "kept");
    }

    #[tokio::test]
    async fn move_rejects_both_rename_and_folder() {
        let app = crate::web::build_router(testutil::test_state());
        get_json(
            app.clone(),
            "PUT",
            "/api/secrets/a",
            Some(json!({"value": "v"})),
        )
        .await;
        let (status, _) = get_json(
            app,
            "POST",
            "/api/secrets/a/move",
            Some(json!({"new_name": "b", "folder": "f"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn file_upload_list_download_delete() {
        let app = crate::web::build_router(testutil::test_state());

        // upload (multipart)
        let body = "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"notes.txt\"\r\nContent-Type: text/plain\r\n\r\nhello files\r\n--B--\r\n";
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/files")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // list
        let (status, json_body) = get_json(app.clone(), "GET", "/api/files", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body[0]["name"], "notes.txt");

        // download
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/files/notes.txt")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "text/plain");
        assert!(res.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("notes.txt"));
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"hello files");

        // delete
        let (status, _) = get_json(app.clone(), "DELETE", "/api/files/notes.txt", None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get_json(app, "GET", "/api/files/notes.txt", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn upload_over_default_axum_body_limit_succeeds() {
        let app = crate::web::build_router(testutil::test_state());

        // 3 MB, over axum's default 2 MB body limit, to confirm the
        // DefaultBodyLimit layer in build_router actually raises the cap.
        let big = "a".repeat(3 * 1024 * 1024);
        let mut body = Vec::new();
        body.extend_from_slice(b"--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(big.as_bytes());
        body.extend_from_slice(b"\r\n--B--\r\n");

        let res = app
            .clone()
            .oneshot(
                Request::post("/api/files")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let (status, json_body) = get_json(app, "GET", "/api/files", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body[0]["name"], "big.bin");
        assert_eq!(json_body[0]["size"], 3 * 1024 * 1024);
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_escapes_quotes_in_content_disposition() {
        let app = crate::web::build_router(testutil::test_state());

        // upload a file whose name contains a double quote
        let body = "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"he\\\"llo.txt\"\r\nContent-Type: text/plain\r\n\r\nx\r\n--B--\r\n";
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/files")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // download it (quote percent-encoded in the path)
        let res = app
            .oneshot(
                Request::get("/api/files/he%22llo.txt")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let cd = res.headers()["content-disposition"].to_str().unwrap();
        assert!(cd.contains("filename=\"he\\\"llo.txt\""));
        assert!(cd.contains("filename*=UTF-8''he%22llo.txt"));
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn download_non_ascii_filename_uses_rfc5987_encoding() {
        let app = crate::web::build_router(testutil::test_state());

        // upload a file with a non-ASCII name ('é' below is a literal UTF-8 char)
        let body = "--B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"résumé.txt\"\r\nContent-Type: text/plain\r\n\r\nx\r\n--B--\r\n";
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/files")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .oneshot(
                Request::get("/api/files/r%C3%A9sum%C3%A9.txt")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let cd = res.headers()["content-disposition"].to_str().unwrap();
        assert!(cd.contains("filename*=UTF-8''r%C3%A9sum%C3%A9.txt"));
    }
}
