//! Embedded localhost web UI. Feature-gated on `ui`.
//! See `docs/superpowers/specs/2026-07-08-web-ui-design.md`.

use std::sync::Arc;

use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use rand::Rng;

use crate::backend::{Backend, BackendRegistry};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};

pub(crate) mod api;
pub(crate) mod auth;
#[cfg(test)]
pub(crate) mod testutil;

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_JS: &str = include_str!("assets/app.js");
const STYLE_CSS: &str = include_str!("assets/style.css");

/// Shared state for all handlers.
pub(crate) struct WebState {
    pub backend: Arc<dyn Backend>,
    pub token: String,
    /// Default vault, resolved once at startup. Requests may override per-call.
    pub vault: String,
    /// Record types (builtin + [types.*] config), resolved once at startup.
    pub types: Vec<crate::records::RecordType>,
}

pub(crate) fn build_router(state: Arc<WebState>) -> Router {
    let api = Router::new()
        .route("/context", get(api::get_context))
        .route("/vaults", get(api::list_vaults))
        .route("/types", get(api::list_types))
        .route("/secrets", get(api::list_secrets))
        .route(
            "/secrets/{name}",
            get(api::get_secret)
                .put(api::put_secret)
                .patch(api::patch_secret)
                .delete(api::delete_secret),
        )
        .route("/secrets/{name}/value", post(api::reveal_secret))
        .route("/secrets/{name}/move", post(api::move_secret));

    #[cfg(feature = "file-ops")]
    let api = api
        .route(
            "/files",
            get(api::files::list_files).post(api::files::upload_file),
        )
        .route(
            "/files/{name}",
            get(api::files::download_file).delete(api::files::delete_file),
        );

    let api = api
        // Raise axum's default 2MB request body cap so file uploads aren't
        // rejected with 413. Uploads buffer fully in memory (FileUploadRequest
        // { content: Vec<u8> }), so keep an explicit cap rather than removing
        // the limit entirely.
        .layer(axum::extract::DefaultBodyLimit::max(100 * 1024 * 1024))
        // Last .layer() is outermost: no_store must wrap require_auth so
        // auth rejections also carry Cache-Control: no-store (see auth.rs).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(axum::middleware::from_fn(auth::no_store));

    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route(
            "/app.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/javascript")],
                    APP_JS,
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async { ([(axum::http::header::CONTENT_TYPE, "text/css")], STYLE_CSS) }),
        )
        .nest("/api", api)
        .with_state(state)
}

/// Entry point for `xv ui`.
pub async fn run_web(
    config: Config,
    registry: Option<&BackendRegistry>,
    port: Option<u16>,
    no_open: bool,
) -> Result<()> {
    let registry = registry.ok_or_else(|| {
        CrosstacheError::config("backend initialization failed; `xv ui` needs a working backend")
    })?;
    let vault = crate::cli::helpers::resolve_vault_for_trait(&config, Some(registry)).await?;
    let backend = registry.active_arc();
    // Fail loud at startup on a broken [types.*] block, matching the CLI's
    // eager type-resolution paths.
    let types = config.resolve_record_types().await?;

    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    let token = hex::encode(buf);

    let state = Arc::new(WebState {
        backend,
        token: token.clone(),
        vault,
        types,
    });
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .await
        .map_err(|e| CrosstacheError::config(format!("failed to bind 127.0.0.1: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| CrosstacheError::config(format!("local_addr: {e}")))?;
    let url = format!("http://127.0.0.1:{}/?token={token}", addr.port());

    println!("xv ui listening at {url}");
    println!("Press Ctrl-C to stop.");
    if !no_open {
        if let Err(e) = opener::open_browser(&url) {
            eprintln!("could not open browser ({e}); open the URL above manually");
        }
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| CrosstacheError::config(format!("web server error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_index_and_assets() {
        let app = build_router(testutil::test_state());
        for (path, ct) in [
            ("/", "text/html; charset=utf-8"),
            ("/app.js", "application/javascript"),
            ("/style.css", "text/css"),
        ] {
            let res = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{path}");
            let got = res.headers()["content-type"].to_str().unwrap().to_string();
            assert_eq!(got, ct, "{path}");
        }
    }

    #[test]
    fn ui_persists_token_for_tab_reloads() {
        assert!(APP_JS.contains("sessionStorage.setItem(TOKEN_STORAGE_KEY"));
        assert!(APP_JS.contains("sessionStorage.getItem(TOKEN_STORAGE_KEY)"));
        assert!(!APP_JS.contains("localStorage"));
    }

    #[test]
    fn ui_has_persistent_missing_token_recovery() {
        assert!(INDEX_HTML.contains("id=\"auth-recovery\""));
        assert!(INDEX_HTML.contains("Reopen the URL printed by"));
        assert!(INDEX_HTML.contains("<code>xv ui</code>"));
        assert!(APP_JS.contains("showAuthRecovery"));
    }
}
