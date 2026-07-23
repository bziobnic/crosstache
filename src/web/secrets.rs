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
    let target = query.target(&state)?;
    let deleted = target
        .backend
        .secrets()
        .list_deleted_secrets(&target.context.vault)
        .await?;
    Ok(Json(deleted))
}

pub(crate) async fn restore(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
) -> Result<Json<SecretProperties>, ApiError> {
    let target = query.target(&state)?;
    let restored = target
        .backend
        .secrets()
        .restore_secret(&target.context.vault, &name)
        .await?;
    Ok(Json(restored))
}

pub(crate) async fn purge(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
) -> Result<StatusCode, ApiError> {
    let target = query.target(&state)?;
    target
        .backend
        .secrets()
        .purge_secret(&target.context.vault, &name)
        .await?;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use serde_json::json;

    use crate::backend::local::LocalBackend;
    use crate::config::settings::LocalConfig;
    use crate::web::api::tests::get_json;
    use crate::web::testutil;

    fn real_local_state(temp: &tempfile::TempDir) -> Arc<crate::web::WebState> {
        let backend = LocalBackend::new(Some(&LocalConfig {
            store_path: Some(temp.path().join("store").to_string_lossy().into_owned()),
            key_file: Some(temp.path().join("key.txt").to_string_lossy().into_owned()),
            default_vault: Some("default".to_string()),
            encrypt_metadata: None,
            opaque_filenames: None,
        }))
        .unwrap();
        let backend: Arc<dyn crate::backend::Backend> = Arc::new(backend);
        let context = testutil::test_context(backend.as_ref(), "default", 30);
        let registry = Arc::new(crate::backend::BackendRegistry::new(backend.clone()));
        Arc::new(crate::web::WebState::new(
            backend,
            context,
            "test-token".to_string(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(temp.path().join("ui.json"), 30),
            registry,
        ))
    }

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

    #[tokio::test]
    async fn real_local_purge_after_restore_returns_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        let _ = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/restored",
            Some(json!({ "value": "v1" })),
        )
        .await;
        let _ = get_json(app.clone(), "DELETE", "/api/secrets/restored", None).await;
        let _ = get_json(app.clone(), "POST", "/api/secrets/restored/restore", None).await;

        let (status, error) = get_json(app, "DELETE", "/api/secrets/restored/purge", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["error"]["code"], "xv-secret-not-found");
    }

    #[tokio::test]
    async fn real_local_purge_preserves_recreated_active_version_history() {
        let temp = tempfile::tempdir().unwrap();
        let state = real_local_state(&temp);
        let app = crate::web::build_router(state.clone());
        for value in ["old", "live-v1", "live-v2"] {
            let _ = get_json(
                app.clone(),
                "PUT",
                "/api/secrets/recreated",
                Some(json!({ "value": value })),
            )
            .await;
            if value == "old" {
                let _ = get_json(app.clone(), "DELETE", "/api/secrets/recreated", None).await;
            }
        }

        let (status, _) = get_json(app, "DELETE", "/api/secrets/recreated/purge", None).await;
        assert_eq!(status, StatusCode::OK);
        let history = state
            .base_backend()
            .secrets()
            .get_secret_version("default", "recreated", "v1", true)
            .await
            .unwrap();
        assert_eq!(
            history.value.as_ref().map(|value| value.as_str()),
            Some("live-v1")
        );
    }
}
