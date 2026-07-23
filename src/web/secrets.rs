use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;

use crate::secret::manager::{DeletedSecretSummary, SecretProperties};

use super::api::{ApiError, VaultQuery};
use super::WebState;

pub(crate) async fn list_deleted(
    State(state): State<Arc<WebState>>,
    Query(query): Query<VaultQuery>,
) -> Result<Json<Vec<DeletedSecretSummary>>, ApiError> {
    let deleted = state
        .backend
        .secrets()
        .list_deleted_secrets(query.vault(&state))
        .await?;
    Ok(Json(deleted))
}

pub(crate) async fn restore(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
) -> Result<Json<SecretProperties>, ApiError> {
    let restored = state
        .backend
        .secrets()
        .restore_secret(query.vault(&state), &name)
        .await?;
    Ok(Json(restored))
}

pub(crate) async fn purge(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
) -> Result<StatusCode, ApiError> {
    state
        .backend
        .secrets()
        .purge_secret(query.vault(&state), &name)
        .await?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::web::api::tests::get_json;
    use crate::web::testutil;

    #[tokio::test]
    async fn deleted_secret_can_be_listed_restored_and_is_then_absent_from_purge() {
        let app = crate::web::build_router(testutil::test_state());

        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/recover-me",
            Some(json!({ "value": "still protected" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get_json(app.clone(), "DELETE", "/api/secrets/recover-me", None).await;
        assert_eq!(status, StatusCode::OK);

        let (status, deleted) = get_json(app.clone(), "GET", "/api/secrets/deleted", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(deleted[0]["name"], "recover-me");
        assert!(deleted[0]["deleted_on"].is_string());

        let (status, _) =
            get_json(app.clone(), "POST", "/api/secrets/recover-me/restore", None).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = get_json(app, "DELETE", "/api/secrets/recover-me/purge", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn restore_collision_is_structured_and_preserves_the_deleted_secret() {
        let app = crate::web::build_router(testutil::test_state());
        for value in ["recoverable", "active"] {
            let (status, _) = get_json(
                app.clone(),
                "PUT",
                "/api/secrets/collision",
                Some(json!({ "value": value })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            if value == "recoverable" {
                let (status, _) =
                    get_json(app.clone(), "DELETE", "/api/secrets/collision", None).await;
                assert_eq!(status, StatusCode::OK);
            }
        }

        let (status, error) =
            get_json(app.clone(), "POST", "/api/secrets/collision/restore", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "xv-conflict");

        let (_, deleted) = get_json(app, "GET", "/api/secrets/deleted", None).await;
        assert_eq!(deleted.as_array().unwrap().len(), 1);
        assert_eq!(deleted[0]["name"], "collision");
    }

    #[tokio::test]
    async fn purge_permanently_removes_a_deleted_secret() {
        let app = crate::web::build_router(testutil::test_state());
        let _ = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/purge-me",
            Some(json!({ "value": "temporary" })),
        )
        .await;
        let _ = get_json(app.clone(), "DELETE", "/api/secrets/purge-me", None).await;

        let (status, _) =
            get_json(app.clone(), "DELETE", "/api/secrets/purge-me/purge", None).await;
        assert_eq!(status, StatusCode::OK);
        let (_, deleted) = get_json(app, "GET", "/api/secrets/deleted", None).await;
        assert!(deleted.as_array().unwrap().is_empty());
    }
}
