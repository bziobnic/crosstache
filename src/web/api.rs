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
                    InvalidSecretName { .. } | InvalidArgument(_) => StatusCode::BAD_REQUEST,
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
            let vaults = v.list_vaults().await?;
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
        assert!(json_body.to_string().find("hunter2").is_none());

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
}
