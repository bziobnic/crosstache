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
    Backend(BackendError),
    Structured {
        status: StatusCode,
        error: Box<super::errors::ApiErrorBody>,
    },
    Validation {
        status: StatusCode,
        message: &'static str,
        field: Option<&'static str>,
    },
}

impl From<CrosstacheError> for ApiError {
    fn from(e: CrosstacheError) -> Self {
        Self::App(e)
    }
}

impl From<BackendError> for ApiError {
    fn from(e: BackendError) -> Self {
        Self::Backend(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            ApiError::App(error) => super::errors::crosstache_error(error),
            ApiError::Backend(error) => super::errors::backend_error(error),
            ApiError::Structured { status, error } => (status, *error),
            ApiError::Validation {
                status,
                message,
                field,
            } => (
                status,
                super::errors::ApiErrorBody::validation(message, field),
            ),
        };
        (status, Json(super::errors::ApiErrorEnvelope { error })).into_response()
    }
}

fn validation_error(
    status: StatusCode,
    message: &'static str,
    field: Option<&'static str>,
) -> ApiError {
    ApiError::Validation {
        status,
        message,
        field,
    }
}

pub(crate) async fn list_vaults(
    State(state): State<Arc<WebState>>,
    Query(q): Query<VaultQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let target = q.target(&state)?;
    match target.backend.vaults() {
        Some(v) => {
            let vaults = v.list_vaults(None).await?;
            Ok(Json(json!({ "vaults": vaults })))
        }
        None => Ok(Json(
            json!({ "vaults": [{ "name": target.context.vault }] }),
        )),
    }
}

pub(crate) async fn list_types(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    Json(json!({ "types": state.types }))
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    alias: Option<String>,
    backend: Option<String>,
    vault: Option<String>,
    group: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct VaultQuery {
    alias: Option<String>,
    backend: Option<String>,
    vault: Option<String>,
}

impl VaultQuery {
    pub(crate) fn target(&self, state: &WebState) -> Result<super::ScopedWebTarget, ApiError> {
        state.scoped_target(
            self.alias.as_deref(),
            self.backend.as_deref(),
            self.vault.as_deref(),
        )
    }
}

pub(crate) async fn list_secrets(
    State(state): State<Arc<WebState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SecretSummary>>, ApiError> {
    let target =
        state.scoped_target(q.alias.as_deref(), q.backend.as_deref(), q.vault.as_deref())?;
    let secrets = target
        .backend
        .secrets()
        .list_secrets(&target.context.vault, q.group.as_deref())
        .await?;
    Ok(Json(secrets))
}

pub(crate) async fn get_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
) -> Result<Json<SecretProperties>, ApiError> {
    let target = q.target(&state)?;
    let props = target
        .backend
        .secrets()
        .get_secret(&target.context.vault, &name, false)
        .await?;
    Ok(Json(props))
}

pub(crate) async fn reveal_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let target = q.target(&state)?;
    let props = target
        .backend
        .secrets()
        .get_secret(&target.context.vault, &name, true)
        .await?;
    Ok(Json(
        json!({ "value": props.value.as_ref().map(|v| v.as_str()) }),
    ))
}

#[derive(Deserialize)]
pub(crate) struct PutSecretBody {
    value: String,
    content_type: Option<String>,
    /// Absent defaults to enabled — the UI echoes the current state on edits
    /// so a value change never re-enables a disabled secret.
    enabled: Option<bool>,
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
    reject_reserved_attachment_key(&name)?;
    let request = SecretRequest {
        name: name.clone(),
        value: Zeroizing::new(body.value),
        content_type: body.content_type,
        enabled: Some(body.enabled.unwrap_or(true)),
        expires_on: body.expires_on,
        not_before: body.not_before,
        tags: body.tags,
        groups: body.groups,
        note: body.note,
        folder: body.folder,
    };
    let target = q.target(&state)?;
    let props = target
        .backend
        .secrets()
        .set_secret(&target.context.vault, request)
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

fn date_field(
    v: Option<String>,
    field: &'static str,
) -> Result<FieldUpdate<DateTime<Utc>>, ApiError> {
    match v {
        None => Ok(FieldUpdate::Unchanged),
        Some(s) if s.is_empty() => Ok(FieldUpdate::Clear),
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| FieldUpdate::Set(d.with_timezone(&Utc)))
            .map_err(|_| {
                validation_error(
                    StatusCode::BAD_REQUEST,
                    "Enter a valid timestamp.",
                    Some(field),
                )
            }),
    }
}

pub(crate) async fn patch_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
    Json(body): Json<PatchSecretBody>,
) -> Result<Json<SecretProperties>, ApiError> {
    reject_reserved_attachment_key(&name)?;
    let request = SecretUpdateRequest {
        name: name.clone(),
        expected_revision: None,
        value: None,
        content_type: None,
        enabled: body.enabled,
        expires_on: date_field(body.expires_on, "expires_on")?,
        not_before: date_field(body.not_before, "not_before")?,
        tags: body.tags,
        groups: body.groups,
        note: str_field(body.note),
        folder: str_field(body.folder),
        replace_tags: true,
        replace_groups: true,
    };
    let target = q.target(&state)?;
    let props = target
        .backend
        .secrets()
        .update_secret(&target.context.vault, &name, request)
        .await?;
    Ok(Json(props))
}

pub(crate) async fn delete_secret(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(q): Query<VaultQuery>,
) -> Result<StatusCode, ApiError> {
    reject_reserved_attachment_key(&name)?;
    let target = q.target(&state)?;
    target
        .backend
        .secrets()
        .delete_secret(&target.context.vault, &name)
        .await?;
    Ok(StatusCode::OK)
}

/// Blunt refusal for the reserved attachment-encryption-key secret — the web
/// UI has no confirm plumbing to warn (unlike the CLI's `confirm_destructive`
/// prompts), so writing, deleting, or renaming it from here is rejected
/// outright.
pub(crate) fn reject_reserved_attachment_key(name: &str) -> Result<(), ApiError> {
    if name == crate::secret::attachments::ATTACHMENT_KEY_SECRET {
        return Err(validation_error(
            StatusCode::BAD_REQUEST,
            "The attachment encryption key cannot be changed from the web interface.",
            Some("name"),
        ));
    }
    Ok(())
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
    let target = q.target(&state)?;
    let vault = &target.context.vault;
    match (body.new_name, body.folder) {
        (Some(new_name), None) => {
            reject_reserved_attachment_key(&name)?;
            let props = target
                .backend
                .secrets()
                .rename_secret(vault, &name, &new_name)
                .await?;
            Ok(Json(props))
        }
        (None, Some(folder)) => {
            let request = SecretUpdateRequest {
                name: name.clone(),
                expected_revision: None,
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
            let props = target
                .backend
                .secrets()
                .update_secret(vault, &name, request)
                .await?;
            Ok(Json(props))
        }
        _ => Err(validation_error(
            StatusCode::BAD_REQUEST,
            "Provide either a new name or a destination folder.",
            Some("name"),
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

    fn files_backend(backend: &dyn crate::backend::Backend) -> Result<&dyn FileBackend, ApiError> {
        backend.files().ok_or_else(|| {
            validation_error(
                StatusCode::NOT_IMPLEMENTED,
                "This backend does not provide file storage.",
                None,
            )
        })
    }

    #[derive(Deserialize)]
    pub(crate) struct FilesQuery {
        alias: Option<String>,
        backend: Option<String>,
        vault: Option<String>,
        prefix: Option<String>,
    }

    pub(crate) async fn list_files(
        State(state): State<Arc<WebState>>,
        Query(q): Query<FilesQuery>,
    ) -> Result<Json<Vec<crate::blob::models::FileInfo>>, ApiError> {
        let target =
            state.scoped_target(q.alias.as_deref(), q.backend.as_deref(), q.vault.as_deref())?;
        let request = FileListRequest {
            prefix: q.prefix,
            groups: None,
            limit: None,
            delimiter: None,
        };
        Ok(Json(
            files_backend(target.backend.as_ref())?
                .list_files(&target.context.vault, request)
                .await?,
        ))
    }

    /// List a secret's attachments (blobs under `attachments/<secret>/`).
    pub(crate) async fn list_attachments(
        State(state): State<Arc<WebState>>,
        Path(name): Path<String>,
        Query(q): Query<VaultQuery>,
    ) -> Result<Json<Vec<crate::blob::models::FileInfo>>, ApiError> {
        Ok(Json(
            crate::secret::attachments::list_attachments(
                files_backend(&state)?,
                q.vault(&state),
                &name,
            )
            .await?,
        ))
    }

    pub(crate) async fn upload_file(
        State(state): State<Arc<WebState>>,
        Query(q): Query<VaultQuery>,
        mut multipart: Multipart,
    ) -> Result<Json<crate::blob::models::FileInfo>, ApiError> {
        while let Some(field) = multipart.next_field().await.map_err(|_| {
            validation_error(
                StatusCode::BAD_REQUEST,
                "The upload could not be read.",
                Some("file"),
            )
        })? {
            if field.name() != Some("file") {
                continue;
            }
            let name = field.file_name().map(str::to_string).ok_or_else(|| {
                validation_error(
                    StatusCode::BAD_REQUEST,
                    "Choose a file with a name.",
                    Some("file"),
                )
            })?;
            let content_type = field.content_type().map(str::to_string);
            let content = field
                .bytes()
                .await
                .map_err(|_| {
                    validation_error(
                        StatusCode::BAD_REQUEST,
                        "The upload could not be read.",
                        Some("file"),
                    )
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
            let target = q.target(&state)?;
            let info = files_backend(target.backend.as_ref())?
                .upload_file(&target.context.vault, request, None)
                .await?;
            return Ok(Json(info));
        }
        Err(validation_error(
            StatusCode::BAD_REQUEST,
            "Choose a file to upload.",
            Some("file"),
        ))
    }

    pub(crate) async fn download_file(
        State(state): State<Arc<WebState>>,
        Path(name): Path<String>,
        Query(q): Query<VaultQuery>,
    ) -> Result<Response, ApiError> {
        let target = q.target(&state)?;
        let vault = &target.context.vault;
        let backend = files_backend(target.backend.as_ref())?;
        let info = backend.get_file_info(vault, &name).await?;
        // Same transparent decryption as `xv file download`: content flagged
        // `xv_encrypted: age` (secret attachments, `xv file upload --encrypt`)
        // is decrypted with the vault's attachment key; everything else passes
        // through untouched.
        let bytes = crate::secret::attachments::download_decrypted(
            state.backend.secrets(),
            backend,
            vault,
            &name,
            None,
        )
        .await?;
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
        let target = q.target(&state)?;
        files_backend(target.backend.as_ref())?
            .delete_file(&target.context.vault, &name)
            .await?;
        Ok(StatusCode::OK)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;
    use zeroize::Zeroizing;

    use crate::secret::manager::SecretRequest;
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

    async fn raw_api_response(
        app: axum::Router,
        method: &str,
        path: &str,
        body: &'static str,
        content_type: Option<&str>,
        authorization: Option<&str>,
    ) -> axum::response::Response {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:1");
        if let Some(content_type) = content_type {
            req = req.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(authorization) = authorization {
            req = req.header(header::AUTHORIZATION, authorization);
        }
        app.oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }

    async fn assert_api_error(
        response: axum::response::Response,
        expected_status: StatusCode,
        expected_code: &str,
        forbidden_text: Option<&str>,
    ) {
        assert_eq!(response.status(), expected_status);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        if let Some(forbidden_text) = forbidden_text {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(forbidden_text),
                "error envelope leaked request or framework text: {forbidden_text}"
            );
        }
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["code"], expected_code);
        assert!(json["error"]["message"].is_string());
        assert!(json["error"]["hint"].is_string());
    }

    #[tokio::test]
    async fn context_reports_backend_and_capabilities() {
        let app = crate::web::build_router(testutil::test_state());
        let (status, json) = get_json(app, "GET", "/api/context", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["backend"], "stub");
        assert_eq!(json["vault"], "default");
        assert_eq!(json["capabilities"]["folders"], true);
        assert_eq!(json["capabilities"]["soft_delete"], true);
        assert_eq!(json["capabilities"]["restore"], true);
        assert_eq!(json["capabilities"]["purge"], true);
        assert_eq!(json["capabilities"]["scheduled_purge"], false);
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

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn secret_attachments_list_and_decrypt_on_download() {
        use crate::secret::attachments;

        let state = testutil::test_state();
        let files = state.backend.files().unwrap();
        // One encrypted attachment for db-cert + an unrelated plain file that
        // must not leak into the attachment listing.
        attachments::upload_encrypted(
            state.backend.secrets(),
            files,
            "default",
            crate::blob::models::FileUploadRequest {
                name: attachments::attachment_blob_name("db-cert", "cert.pem"),
                content: b"BEGIN CERT".to_vec(),
                content_type: Some("application/x-pem-file".to_string()),
                groups: Vec::new(),
                metadata: std::collections::HashMap::new(),
                tags: std::collections::HashMap::new(),
            },
            None,
        )
        .await
        .unwrap();
        files
            .upload_file(
                "default",
                crate::blob::models::FileUploadRequest {
                    name: "notes.txt".to_string(),
                    content: b"plain".to_vec(),
                    content_type: None,
                    groups: Vec::new(),
                    metadata: std::collections::HashMap::new(),
                    tags: std::collections::HashMap::new(),
                },
                None,
            )
            .await
            .unwrap();
        let app = crate::web::build_router(state);

        let (status, json_body) =
            get_json(app.clone(), "GET", "/api/secrets/db-cert/attachments", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body.as_array().unwrap().len(), 1);
        assert_eq!(json_body[0]["name"], "attachments/db-cert/cert.pem");

        // No attachments → empty list, not an error.
        let (status, json_body) =
            get_json(app.clone(), "GET", "/api/secrets/other/attachments", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body.as_array().unwrap().len(), 0);

        // Downloading the attachment returns plaintext, not age ciphertext.
        let res = app
            .oneshot(
                Request::get("/api/files/attachments%2Fdb-cert%2Fcert.pem")
                    .header(header::HOST, "127.0.0.1:1")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"BEGIN CERT");
    }

    #[tokio::test]
    async fn api_normalizes_auth_and_router_failures_to_safe_envelopes() {
        let app = crate::web::build_router(testutil::test_state());
        assert_api_error(
            raw_api_response(app.clone(), "GET", "/api/context", "", None, None).await,
            StatusCode::UNAUTHORIZED,
            "xv-auth-required",
            None,
        )
        .await;
        assert_api_error(
            raw_api_response(
                app.clone(),
                "GET",
                "/api/context",
                "",
                None,
                Some("Bearer wrong-token"),
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "xv-auth-required",
            Some("wrong-token"),
        )
        .await;
        assert_api_error(
            raw_api_response(
                app.clone(),
                "PUT",
                "/api/secrets/new-secret",
                "{\"value\":\"malformed-json-marker\"",
                Some("application/json"),
                Some("Bearer test-token"),
            )
            .await,
            StatusCode::BAD_REQUEST,
            "xv-invalid-request",
            Some("malformed-json-marker"),
        )
        .await;
        let query_app = axum::Router::new()
            .route(
                "/api/query",
                axum::routing::get(
                    |_q: axum::extract::Query<std::collections::HashMap<String, u8>>| async {},
                ),
            )
            .layer(axum::middleware::from_fn(crate::web::normalize_api_errors))
            .layer(axum::middleware::from_fn(crate::web::auth::no_store));
        assert_api_error(
            raw_api_response(
                query_app,
                "GET",
                "/api/query?limit=not-a-number",
                "",
                None,
                None,
            )
            .await,
            StatusCode::BAD_REQUEST,
            "xv-invalid-request",
            Some("not-a-number"),
        )
        .await;
        assert_api_error(
            raw_api_response(
                app.clone(),
                "POST",
                "/api/files",
                "not a multipart body",
                Some("text/plain"),
                Some("Bearer test-token"),
            )
            .await,
            StatusCode::BAD_REQUEST,
            "xv-invalid-request",
            Some("not a multipart body"),
        )
        .await;
        assert_api_error(
            raw_api_response(
                app,
                "GET",
                "/api/not-a-route",
                "",
                None,
                Some("Bearer test-token"),
            )
            .await,
            StatusCode::NOT_FOUND,
            "xv-api-route-not-found",
            None,
        )
        .await;
    }

    #[tokio::test]
    async fn api_normalizes_body_limit_rejections_without_a_large_allocation() {
        let app = axum::Router::new()
            .route(
                "/api/limited",
                axum::routing::post(|_: axum::Json<serde_json::Value>| async {}),
            )
            .layer(axum::extract::DefaultBodyLimit::max(1))
            .layer(axum::middleware::from_fn(crate::web::normalize_api_errors))
            .layer(axum::middleware::from_fn(crate::web::auth::no_store));
        assert_api_error(
            raw_api_response(
                app,
                "POST",
                "/api/limited",
                "{}",
                Some("application/json"),
                None,
            )
            .await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "xv-request-too-large",
            None,
        )
        .await;
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
    async fn missing_secret_has_stable_error() {
        let app = crate::web::build_router(testutil::test_state());
        let (status, json) = get_json(app, "GET", "/api/secrets/missing", None).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["code"], "xv-secret-not-found");
        assert_eq!(json["error"]["message"], "Secret 'missing' was not found.");
        assert!(json["error"]["hint"].as_str().unwrap().contains("Refresh"));
    }

    #[tokio::test]
    async fn reserved_attachment_key_rejects_delete_and_rename_but_allows_folder_move() {
        let state = testutil::test_state();
        let reserved = crate::secret::attachments::ATTACHMENT_KEY_SECRET;
        // Seed the reserved key directly through the backend trait, since
        // PUT now rejects it too (see `reserved_attachment_key_rejects_put`
        // below) — there's no API path left to create it.
        state
            .base_backend()
            .secrets()
            .set_secret(
                "default",
                SecretRequest {
                    name: reserved.to_string(),
                    value: Zeroizing::new("AGE-SECRET-KEY-1...".to_string()),
                    content_type: None,
                    enabled: None,
                    expires_on: None,
                    not_before: None,
                    tags: None,
                    groups: None,
                    note: None,
                    folder: None,
                },
            )
            .await
            .unwrap();
        let app = crate::web::build_router(state);

        // Delete is refused outright.
        let (status, _) = get_json(
            app.clone(),
            "DELETE",
            &format!("/api/secrets/{reserved}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Rename (the destructive form of move) is refused outright.
        let (status, _) = get_json(
            app.clone(),
            "POST",
            &format!("/api/secrets/{reserved}/move"),
            Some(json!({"new_name": "renamed-key"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A folder-only move doesn't displace the key, so it's still allowed.
        let (status, _) = get_json(
            app.clone(),
            "POST",
            &format!("/api/secrets/{reserved}/move"),
            Some(json!({"folder": "infra"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // The key is untouched under its reserved name.
        let (status, _) = get_json(
            app.clone(),
            "GET",
            &format!("/api/secrets/{reserved}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn reserved_attachment_key_rejects_put_and_patch() {
        let app = crate::web::build_router(testutil::test_state());
        let reserved = crate::secret::attachments::ATTACHMENT_KEY_SECRET;

        // Creating/overwriting the reserved key via PUT is refused outright
        // — the web UI has no confirm plumbing, unlike `xv set`.
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            &format!("/api/secrets/{reserved}"),
            Some(json!({"value": "AGE-SECRET-KEY-1..."})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Metadata-only PATCH is refused outright too, even though it never
        // touches the secret value — consistent blunt rejection surface.
        let (status, _) = get_json(
            app.clone(),
            "PATCH",
            &format!("/api/secrets/{reserved}"),
            Some(json!({"note": "hi"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // The key was never created, so it doesn't exist.
        let (status, _) = get_json(
            app.clone(),
            "GET",
            &format!("/api/secrets/{reserved}"),
            None,
        )
        .await;
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
    async fn put_preserves_enabled_and_not_before_across_edits() {
        let app = crate::web::build_router(testutil::test_state());

        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/disabled-key",
            Some(json!({
                "value": "v1",
                "enabled": false,
                "not_before": "2027-01-01T00:00:00Z",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json_body) =
            get_json(app.clone(), "GET", "/api/secrets/disabled-key", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body["enabled"], false);
        assert_eq!(json_body["not_before"], "2027-01-01T00:00:00Z");

        // A value edit echoing the fetched state (as the frontend does) must
        // not re-enable the secret or drop its not-before constraint.
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/disabled-key",
            Some(json!({
                "value": "v2",
                "enabled": false,
                "not_before": "2027-01-01T00:00:00Z",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, json_body) = get_json(app, "GET", "/api/secrets/disabled-key", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_body["enabled"], false);
        assert_eq!(json_body["not_before"], "2027-01-01T00:00:00Z");
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

    #[tokio::test]
    async fn types_returns_builtin_types() {
        let app = crate::web::build_router(testutil::test_state());
        let (status, json_body) = get_json(app, "GET", "/api/types", None).await;
        assert_eq!(status, StatusCode::OK);
        let types = json_body["types"].as_array().unwrap();
        let login = types.iter().find(|t| t["name"] == "login").unwrap();
        // login's declared field order and shape, exactly as builtin_types() defines
        assert_eq!(login["source"], "builtin");
        assert_eq!(login["fields"][0]["name"], "username");
        assert_eq!(login["fields"][0]["kind"], "metadata");
        assert_eq!(login["fields"][0]["required"], true);
        assert_eq!(login["fields"][2]["name"], "password");
        assert_eq!(login["fields"][2]["kind"], "secret");
        assert_eq!(login["fields"][2]["primary"], true);
        // all three builtins present
        for name in ["login", "api-key", "database"] {
            assert!(types.iter().any(|t| t["name"] == name), "{name} missing");
        }
    }

    #[tokio::test]
    async fn record_roundtrip_preserves_envelope_and_field_tags() {
        let app = crate::web::build_router(testutil::test_state());

        // PUT a typed record the way the record drawer saves one
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/gh-login",
            Some(json!({
                "value": r#"{"password":"hunter2"}"#,
                "content_type": "application/vnd.xv.record",
                "tags": {"xv-type": "login", "f.username": "bob"},
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // metadata GET: record markers survive, value stays null
        let (status, meta) = get_json(app.clone(), "GET", "/api/secrets/gh-login", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(meta["content_type"], "application/vnd.xv.record");
        assert_eq!(meta["tags"]["xv-type"], "login");
        assert_eq!(meta["tags"]["f.username"], "bob");
        assert!(meta["value"].is_null());

        // the list never leaks envelope contents
        let (_, list) = get_json(app.clone(), "GET", "/api/secrets", None).await;
        assert!(!list.to_string().contains("hunter2"));

        // reveal returns the raw envelope for the drawer to parse
        let (_, revealed) = get_json(app, "POST", "/api/secrets/gh-login/value", None).await;
        assert_eq!(revealed["value"], r#"{"password":"hunter2"}"#);
    }
}
