//! Disclosure canary suite for the web API.
//!
//! Every route in [`crate::web::build_router`] is driven against a stub
//! backend seeded with a canary plaintext. Exactly one route —
//! `POST /api/secrets/{name}/value` — may echo it back; every other route,
//! read or write, must return a body without it.
//!
//! The enumeration below mirrors the `build_router` route table. When a
//! route is added there, add it here too: an unlisted route is an untested
//! disclosure surface.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use crate::secret::domain::{SecretRequest, SecretValue};
use crate::web::testutil;

/// Plaintext of the untyped secret `LEAKY`.
const CANARY: &str = "disclosure-canary-7f3e";
/// Plaintext inside the typed `login` record `REC`'s envelope.
const RECORD_CANARY: &str = "record-canary-9c1d";

fn canary_request(name: &str, value: &str, content_type: Option<&str>) -> SecretRequest {
    SecretRequest {
        name: name.to_string(),
        value: SecretValue::new(value.to_string()),
        content_type: content_type.map(str::to_string),
        enabled: Some(true),
        expires_on: None,
        not_before: None,
        tags: None,
        groups: None,
        note: None,
        folder: None,
    }
}

/// A stub backend holding `LEAKY` (untyped), `REC` (a typed record whose
/// envelope carries the canary), and a soft-deleted `TRASHED`.
fn seeded_state() -> Arc<crate::web::WebState> {
    let backend = Arc::new(testutil::stub::StubBackend::new());
    {
        let mut secrets = backend.secrets.lock().unwrap();
        secrets.insert("LEAKY".to_string(), canary_request("LEAKY", CANARY, None));
        secrets.insert(
            "REC".to_string(),
            canary_request(
                "REC",
                &json!({ "password": RECORD_CANARY }).to_string(),
                Some(crate::records::RECORD_CONTENT_TYPE),
            ),
        );
    }
    #[cfg(feature = "file-ops")]
    backend.files.lock().unwrap().insert(
        "notes.txt".to_string(),
        (
            b"plain attachment bytes".to_vec(),
            "text/plain".to_string(),
            std::collections::HashMap::new(),
        ),
    );
    backend.deleted.lock().unwrap().insert(
        "TRASHED".to_string(),
        canary_request("TRASHED", CANARY, None),
    );
    testutil::test_state_with_backend(backend)
}

/// Issue one request and return `(status, body as lossy UTF-8)`. Bodies are
/// read as bytes, not parsed — a route that answers with a file download or
/// a non-JSON error still has to be canary-free.
async fn raw(
    state: &Arc<crate::web::WebState>,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, String) {
    let app = crate::web::build_router(state.clone());
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[track_caller]
fn assert_no_canary(label: &str, body: &str) {
    for canary in [CANARY, RECORD_CANARY] {
        assert!(
            !body.contains(canary),
            "{label} disclosed the canary {canary}: {body}"
        );
    }
}

/// Every GET route registered in `build_router`, in table order.
fn get_routes() -> Vec<&'static str> {
    let mut routes = vec![
        "/api/health",
        "/api/context",
        "/api/vaults",
        "/api/types",
        "/api/preferences",
        "/api/secrets",
        "/api/secrets/deleted",
        "/api/secrets/LEAKY",
        "/api/secrets/REC",
        // The asset routes: the bundled UI itself must not embed a value.
        "/",
        "/app.js",
    ];
    #[cfg(feature = "file-ops")]
    routes.extend([
        "/api/attachment-renames",
        "/api/files",
        "/api/files/notes.txt",
        "/api/files/archive",
        "/api/secrets/LEAKY/attachments",
    ]);
    routes
}

#[tokio::test]
async fn every_get_route_is_value_free() {
    let state = seeded_state();
    for path in get_routes() {
        let (_status, body) = raw(&state, "GET", path, None).await;
        assert_no_canary(&format!("GET {path}"), &body);
    }
}

#[tokio::test]
async fn every_mutating_route_is_value_free() {
    let state = seeded_state();
    // (method, path, body) — the write side of the route table. Each body
    // deliberately carries the canary INBOUND, so a route that echoes its
    // own request back is caught as well as one that reads storage.
    let cases: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        (
            "POST",
            "/api/context/activate".into(),
            Some(json!({ "alias": "default", "backend": "stub", "vault": "default" })),
        ),
        (
            "POST",
            "/api/workspaces/activate".into(),
            Some(json!({ "alias": "default", "backend": "stub", "vault": "default" })),
        ),
        (
            "POST",
            "/api/folder-tokens".into(),
            Some(json!({ "surface": "secrets", "folders": ["work"] })),
        ),
        (
            "PUT",
            "/api/preferences".into(),
            Some(json!({ "theme": "dark" })),
        ),
        (
            "PUT",
            "/api/secrets/LEAKY".into(),
            Some(json!({ "value": CANARY })),
        ),
        (
            "PATCH",
            "/api/secrets/LEAKY".into(),
            Some(json!({ "note": "touched" })),
        ),
        (
            "POST",
            "/api/secrets/LEAKY/move".into(),
            Some(json!({ "folder": "work" })),
        ),
        (
            "POST",
            "/api/secrets/LEAKY/conversion/preview".into(),
            Some(json!({ "target_type": "login", "supplied_fields": { "username": "alice" } })),
        ),
        (
            "POST",
            "/api/secrets/LEAKY/rename".into(),
            Some(json!({ "new_name": "RENAMED" })),
        ),
        ("DELETE", "/api/secrets/TRASHED/purge".into(), None),
        ("DELETE", "/api/secrets/REC".into(), None),
        // `REC` is now in the trash, so restore has a real record to act on.
        ("POST", "/api/secrets/REC/restore".into(), None),
    ];
    for (method, path, body) in cases {
        let (_status, response) = raw(&state, method, &path, body).await;
        assert_no_canary(&format!("{method} {path}"), &response);
    }
}

/// Applying a conversion needs the `source_revision` its preview hands back,
/// so this boundary is driven as the two-step exchange the UI performs —
/// both halves of which read the value to rewrite it.
#[tokio::test]
async fn conversion_apply_is_value_free() {
    let state = seeded_state();
    // Write the secret through the route so the stub records a revision
    // token; conditional conversion refuses to run without one.
    let (status, _) = raw(
        &state,
        "PUT",
        "/api/secrets/LEAKY",
        Some(json!({ "value": CANARY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json!({ "target_type": "login", "supplied_fields": { "username": "alice" } });
    let (status, preview) = raw(
        &state,
        "POST",
        "/api/secrets/LEAKY/conversion/preview",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_no_canary("POST /api/secrets/LEAKY/conversion/preview", &preview);
    let preview: serde_json::Value = serde_json::from_str(&preview).unwrap();
    let revision = preview["source_revision"]
        .as_str()
        .expect("preview must hand back a source_revision")
        .to_string();

    let mut apply = body;
    apply["source_revision"] = json!(revision);
    apply["confirm_lossy"] = json!(true);
    let (status, applied) = raw(&state, "POST", "/api/secrets/LEAKY/conversion", Some(apply)).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_no_canary("POST /api/secrets/LEAKY/conversion", &applied);
}
