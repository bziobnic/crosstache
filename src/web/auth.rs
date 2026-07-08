//! Request authentication for the web UI API.
//!
//! Threat model: the network is loopback-only; the real attacker is another
//! web page in the user's browser issuing requests to 127.0.0.1 (CSRF / DNS
//! rebinding). The bearer token is the gate; Host/Origin checks are a free
//! second layer that specifically kills DNS rebinding (attacker-controlled
//! hostname in Host).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};

use super::WebState;

fn is_loopback_name(host_port: &str) -> bool {
    // Only strip a real numeric port; "localhost:evil.example.com" must not
    // parse as name "localhost".
    let name = match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host_port,
    };
    name == "127.0.0.1" || name == "localhost"
}

pub(crate) async fn require_auth(
    State(state): State<Arc<WebState>>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !is_loopback_name(host) {
        return Err((StatusCode::FORBIDDEN, "invalid Host header"));
    }

    if let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok())
    {
        let ok = origin
            .strip_prefix("http://")
            .map(is_loopback_name)
            .unwrap_or(false);
        if !ok {
            return Err((StatusCode::FORBIDDEN, "invalid Origin header"));
        }
    }

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    // Digest comparison makes timing differences useless to an attacker.
    if Sha256::digest(provided.as_bytes()) != Sha256::digest(state.token.as_bytes()) {
        return Err((StatusCode::UNAUTHORIZED, "missing or invalid token"));
    }

    Ok(next.run(req).await)
}

/// Secrets must never land in any browser/proxy cache.
pub(crate) async fn no_store(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn protected_app(token: &str) -> Router {
        let state = crate::web::testutil::test_state_with_token(token);
        // Last .layer() is outermost: no_store must wrap require_auth so
        // auth rejections also carry Cache-Control: no-store.
        Router::new()
            .route("/api/ping", get(|| async { "pong" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_auth,
            ))
            .layer(axum::middleware::from_fn(no_store))
            .with_state(state)
    }

    async fn send(app: Router, auth: Option<&str>, host: &str, origin: Option<&str>) -> Response {
        let mut req = HttpRequest::get("/api/ping").header(header::HOST, host);
        if let Some(a) = auth {
            req = req.header(header::AUTHORIZATION, a);
        }
        if let Some(o) = origin {
            req = req.header(header::ORIGIN, o);
        }
        app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn rejects_missing_token() {
        let res = send(protected_app("sekrit"), None, "127.0.0.1:1", None).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.headers()["cache-control"], "no-store");
    }

    #[tokio::test]
    async fn rejects_fake_port_host() {
        let res = send(
            protected_app("sekrit"),
            Some("Bearer sekrit"),
            "localhost:evil.example.com",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_wrong_token() {
        let res = send(
            protected_app("sekrit"),
            Some("Bearer nope"),
            "127.0.0.1:1",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_bad_host() {
        let res = send(
            protected_app("sekrit"),
            Some("Bearer sekrit"),
            "evil.example.com:1",
            None,
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_cross_site_origin() {
        let res = send(
            protected_app("sekrit"),
            Some("Bearer sekrit"),
            "127.0.0.1:1",
            Some("https://evil.example.com"),
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn accepts_valid_request_and_sets_no_store() {
        let res = send(
            protected_app("sekrit"),
            Some("Bearer sekrit"),
            "localhost:9999",
            Some("http://localhost:9999"),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["cache-control"], "no-store");
    }
}
