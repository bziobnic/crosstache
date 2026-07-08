//! JSON API handlers. Parse → delegate to backend traits → serialize.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::backend::error::BackendError;
use crate::error::CrosstacheError;

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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
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
}
