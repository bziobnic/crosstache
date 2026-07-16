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
const UI_MODEL_JS: &str = include_str!("assets/ui-model.js");
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

/// A bound web UI server that has not started accepting requests yet.
///
/// Keeping binding separate from serving lets native shells obtain the
/// tokenized URL before they navigate their webview to it.
pub struct PreparedWebServer {
    url: String,
    listener: tokio::net::TcpListener,
    app: Router,
}

impl PreparedWebServer {
    /// The loopback-only URL for this server, including its session token.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Serve until the supplied shutdown signal completes.
    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        axum::serve(self.listener, self.app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| CrosstacheError::config(format!("web server error: {e}")))
    }

    /// Serve until the process exits.
    #[allow(dead_code)] // Used by the desktop crate; the xv binary module copy does not call it.
    pub async fn serve(self) -> Result<()> {
        axum::serve(self.listener, self.app)
            .await
            .map_err(|e| CrosstacheError::config(format!("web server error: {e}")))
    }
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
            "/ui-model.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/javascript")],
                    UI_MODEL_JS,
                )
            }),
        )
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

/// Bind the embedded web UI and return it without starting the accept loop.
pub async fn prepare_web(
    config: Config,
    registry: Option<&BackendRegistry>,
    port: Option<u16>,
) -> Result<PreparedWebServer> {
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

    Ok(PreparedWebServer { url, listener, app })
}

/// Entry point for `xv ui`.
pub async fn run_web(
    config: Config,
    registry: Option<&BackendRegistry>,
    port: Option<u16>,
    no_open: bool,
) -> Result<()> {
    let server = prepare_web(config, registry, port).await?;
    let url = server.url().to_string();

    println!("xv ui listening at {url}");
    println!("Press Ctrl-C to stop.");
    if !no_open {
        if let Err(e) = opener::open_browser(&url) {
            eprintln!("could not open browser ({e}); open the URL above manually");
        }
    }

    server
        .serve_with_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
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
            ("/ui-model.js", "application/javascript"),
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

    #[test]
    fn ui_auth_recovery_hides_header_controls_but_keeps_brand() {
        assert!(INDEX_HTML.contains("id=\"vault-context\""));
        assert!(INDEX_HTML.contains("id=\"vault-tabs\""));
        assert!(INDEX_HTML.contains("class=\"brand\""));
        assert!(APP_JS.contains("$('#vault-context').hidden = true;"));
        assert!(APP_JS.contains("$('#vault-tabs').hidden = true;"));
        assert!(!APP_JS.contains("$('#app-header').hidden = true;"));
    }

    #[test]
    fn ui_auth_recovery_cannot_be_dismissed_by_tabs() {
        assert!(APP_JS.contains("authRecoveryActive = true;"));
        assert!(APP_JS.contains("if (authRecoveryActive) return;"));
    }

    #[test]
    fn ui_guards_list_loads_against_stale_responses() {
        assert!(APP_JS.contains("let secretLoadGeneration = 0"));
        assert!(APP_JS.contains("let fileLoadGeneration = 0"));
        assert!(APP_JS.contains("async function loadSecrets(vault)"));
        assert!(APP_JS.contains("async function loadFiles(vault)"));
        assert!(APP_JS.contains("if (generation !== secretLoadGeneration) return"));
        assert!(APP_JS.contains("if (generation !== fileLoadGeneration) return"));
    }

    #[test]
    fn ui_renders_purposeful_list_states() {
        assert!(APP_JS.contains("function showListState(tbody, kind, state, cols)"));
        assert!(APP_JS.contains("for (let index = 0; index < 3; index++)"));
        assert!(APP_JS.contains("button.onclick = () => openDrawer(null)"));
        assert!(APP_JS.contains("button.onclick = () => $('#file-input').click()"));
        assert!(APP_JS.contains("showListState($('#secrets-table tbody'), 'secrets', 'loading'"));
        assert!(APP_JS.contains("showListState($('#files-table tbody'), 'files', 'failed'"));
        assert!(STYLE_CSS.contains(".skeleton-row"));
        assert!(STYLE_CSS.contains(".empty-state"));
    }

    #[test]
    fn ui_skeleton_keeps_spanning_table_cell_semantics() {
        assert!(APP_JS.contains("content.className = 'skeleton-content'"));
        assert!(STYLE_CSS.contains(".skeleton-content { display:grid;"));
        assert!(!STYLE_CSS.contains(".skeleton-row td { display:grid;"));
    }

    #[test]
    fn ui_resets_list_summaries_for_current_load_generation() {
        assert!(APP_JS.contains("function setListLoadStatus(kind, state)"));
        for marker in [
            "setListLoadStatus('secrets', 'loading');",
            "setListLoadStatus('secrets', 'failed');",
            "setListLoadStatus('files', 'loading');",
            "setListLoadStatus('files', 'failed');",
            "Loading secrets…",
            "Secrets unavailable",
            "Loading files…",
            "Files unavailable",
        ] {
            assert!(APP_JS.contains(marker), "missing {marker}");
        }

        let secret_load = APP_JS
            .split_once("async function loadSecrets(vault) {")
            .unwrap()
            .1
            .split_once("function renderSecrets()")
            .unwrap()
            .0;
        assert!(secret_load.contains("secrets = [];\n  setListLoadStatus('secrets', 'loading');"));
        assert!(
            secret_load
                .find("if (generation !== secretLoadGeneration) return false;")
                .unwrap()
                < secret_load
                    .find("setListLoadStatus('secrets', 'failed');")
                    .unwrap(),
            "a stale failed secret request must not overwrite the current vault status"
        );

        let file_load = APP_JS
            .split_once("async function loadFiles(vault) {")
            .unwrap()
            .1
            .split_once("function renderFiles()")
            .unwrap()
            .0;
        assert!(file_load.contains("files = [];\n  setListLoadStatus('files', 'loading');"));
        assert!(
            file_load
                .find("if (generation !== fileLoadGeneration) return false;")
                .unwrap()
                < file_load
                    .find("setListLoadStatus('files', 'failed');")
                    .unwrap(),
            "a stale failed file request must not overwrite the current vault status"
        );
    }

    #[test]
    fn ui_stops_stale_init_before_loading_files() {
        assert!(APP_JS.contains("if (!(await loadSecrets(vault))) return;"));
    }

    #[test]
    fn ui_guards_drawer_loads_against_stale_responses() {
        assert!(APP_JS.contains("let drawerGeneration = 0"));
        assert!(APP_JS.contains("if (generation !== drawerGeneration) return"));
    }

    #[test]
    fn ui_resets_secret_delete_confirmation_on_drawer_transitions() {
        assert!(APP_JS.contains("function resetConfirmation"));
        assert!(APP_JS.contains("resetConfirmation($('#delete'), 'Delete')"));
    }

    #[test]
    fn ui_file_actions_are_named_and_delete_is_confirmed() {
        assert!(APP_JS.contains("dl.textContent = 'Download'"));
        assert!(APP_JS.contains("del.dataset.defaultLabel = 'Delete'"));
        assert!(APP_JS.contains("armConfirmation(del, 'Really delete?')"));
        assert!(!APP_JS.contains("dl.textContent = '⬇'"));
        assert!(!APP_JS.contains("del.textContent = '✕'"));
    }

    #[test]
    fn ui_unifies_actions_upload_and_feedback_components() {
        for marker in [
            "class=\"search-field\"",
            "class=\"dropzone-content\"",
            "role=\"status\"",
            "aria-live=\"polite\"",
        ] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
        }
        assert!(APP_JS.contains("t.className = `toast ${isError ? 'error' : 'success'}`"));
        assert!(APP_JS.contains("t.replaceChildren(icon(isError ? 'alert' : 'check')"));
        assert!(APP_JS.contains("dl.className = 'button secondary compact'"));
        assert!(APP_JS.contains("dl.prepend(icon('download'))"));
        assert!(APP_JS.contains("del.className = 'button danger compact'"));
        assert!(STYLE_CSS.contains(".bulk-toolbar {"));
        assert!(STYLE_CSS.contains(".dropzone-content {"));
        assert!(STYLE_CSS.contains(".toast.success {"));
        assert!(
            !STYLE_CSS.contains("#toast {"),
            "legacy toast id styles override the unified toast component"
        );
    }

    #[test]
    fn ui_file_delete_success_refreshes_current_vault_independent_of_list_generation() {
        assert!(APP_JS.contains("let fileActionGeneration = 0"));
        assert!(APP_JS.contains("fileActionGeneration++;"));
        assert!(APP_JS.contains("function isCurrentFileAction(generation, vault)"));
        assert!(APP_JS.contains("generation === fileActionGeneration"));
        assert!(APP_JS.contains("vault === currentVault"));
        assert!(
            !APP_JS.contains("generation === fileLoadGeneration &&\n    vault === currentVault")
        );
        assert!(APP_JS.contains("if (!isCurrentFileAction(generation, vault)) return;"));
        assert!(APP_JS.contains("await reconcileFilesAfterDelete(generation, vault);"));
    }

    #[test]
    fn ui_delete_buttons_enter_non_repeatable_pending_state() {
        assert!(APP_JS.contains("function beginPendingAction(button, label)"));
        assert!(APP_JS.contains("button.disabled = true;"));
        assert!(APP_JS.contains("button.disabled = false;"));
        assert!(APP_JS.contains("beginPendingAction(btn, 'Deleting…')"));
        assert!(APP_JS.contains("beginPendingAction(del, 'Deleting…')"));
    }

    #[test]
    fn ui_file_delete_pending_state_survives_same_vault_rerenders() {
        assert!(APP_JS.contains("const pendingFileDeletes = new Map()"));
        assert!(APP_JS.contains("function isFileDeletePending(vault, name)"));
        assert!(APP_JS.contains("function setFileDeletePending(vault, name, generation)"));
        assert!(APP_JS.contains("function clearFileDeletePending(vault, name, generation)"));
        assert!(APP_JS.contains("pendingFileDeletes.clear();"));
        assert!(APP_JS.contains("if (isFileDeletePending(vault, name)) return;"));
        assert!(APP_JS.contains("setFileDeletePending(vault, name, generation);"));
        assert!(APP_JS.contains("del.textContent = pending ? 'Deleting…' : 'Delete';"));
        assert!(APP_JS.contains("del.disabled = pending;"));
    }

    #[test]
    fn ui_reports_current_file_reconciliation_failures() {
        assert!(APP_JS.contains("async function reconcileFilesAfterDelete(generation, vault)"));
        assert!(APP_JS.contains("await reconcileFilesAfterDelete(generation, vault);"));
        assert!(
            APP_JS.contains("if (!isCurrentFileAction(generation, vault)) return;\n    fail(e);")
        );
    }

    #[test]
    fn ui_guards_drawer_action_continuations_by_selection() {
        assert!(APP_JS.contains("function isCurrentDrawer(generation, selection)"));
        assert!(
            APP_JS
                .matches("if (!isCurrentDrawer(generation, selection)) return;")
                .count()
                >= 8
        );
    }

    #[test]
    fn ui_hides_and_clears_drawer_while_selection_loads() {
        assert!(APP_JS.contains(
            "async function openDrawer(name) {\n  const generation = ++drawerGeneration;\n  $('#drawer').hidden = true;"
        ));
        assert!(APP_JS.contains("function clearDrawerState()"));
    }

    #[test]
    fn ui_exposes_selection_controls_for_both_tables() {
        for id in [
            "select-secrets",
            "select-files",
            "select-all-secrets",
            "select-all-files",
            "secret-bulk-bar",
            "file-bulk-bar",
        ] {
            assert!(INDEX_HTML.contains(&format!("id=\"{id}\"")), "{id}");
        }
    }

    #[test]
    fn ui_marks_group_children_for_indentation() {
        assert!(APP_JS.contains("renderRow(it, true)"));
        assert!(APP_JS.contains("tr.classList.add('folder-child')"));
        assert!(APP_JS.contains("td.classList.add('item-name')"));
        assert!(STYLE_CSS.contains(".folder-child .item-name"));
    }

    #[test]
    fn ui_selection_uses_visible_items_and_mixed_header_state() {
        assert!(APP_JS.contains("function syncSelectionUi(kind, visibleIds)"));
        assert!(APP_JS.contains("selectAll.indeterminate = selectedVisible > 0 && !allVisible"));
        assert!(APP_JS.contains("for (const id of visibleIds)"));
        assert!(APP_JS.contains("clearSelection('secrets')"));
        assert!(APP_JS.contains("clearSelection('files')"));
    }

    #[test]
    fn ui_bulk_actions_are_bounded_and_reuse_item_routes() {
        assert!(APP_JS.contains("async function runBounded(items, limit, operation)"));
        assert!(APP_JS.contains("runBounded(items, 4"));
        assert!(APP_JS.contains("api('DELETE', `/api/secrets/"));
        assert!(APP_JS.contains("api('DELETE', `/api/files/"));
        assert!(APP_JS.contains("/move${vaultQS(vault)}`, { folder }"));
    }

    #[test]
    fn ui_bulk_deletes_require_confirmation() {
        assert!(APP_JS.contains("armConfirmation(button, `Delete ${items.length} secrets?`)"));
        assert!(APP_JS.contains("armConfirmation(button, `Delete ${items.length} files?`)"));
    }

    #[test]
    fn ui_cancelling_selection_restores_bulk_controls() {
        assert!(APP_JS.contains("function resetSelectionControls(kind)"));
        assert!(APP_JS.contains("if (!enabled) {\n    resetSelectionControls(kind);"));
        assert!(APP_JS.contains("resetConfirmation(moveButton, 'Move');"));
        assert!(APP_JS.contains("cancelButton.disabled = false;"));
    }

    #[test]
    fn ui_selection_changes_reset_bulk_confirmation() {
        assert!(APP_JS.contains("function resetBulkConfirmation(kind)"));
        assert!(
            APP_JS.matches("resetBulkConfirmation(kind);").count() >= 4,
            "every selection mutation path must disarm bulk delete"
        );
    }

    #[test]
    fn ui_bulk_actions_reconcile_same_vault_after_tab_switch() {
        assert!(APP_JS.contains("const selectionIsCurrent = generation === state.generation;"));
        assert!(
            APP_JS
                .matches("if (vault !== currentVault) return;")
                .count()
                >= 4,
            "bulk delete and move must reconcile same-vault data independently of selection state"
        );
        assert!(!APP_JS
            .contains("if (generation !== state.generation || vault !== currentVault) return;"));
    }

    #[test]
    fn ui_preserves_failed_file_load_state_during_bulk_recovery() {
        assert!(APP_JS.contains("let filesState = 'ready';"));
        assert!(APP_JS.contains("filesState = 'loading';"));
        assert!(APP_JS.contains("filesState = 'failed';"));
        assert!(APP_JS.contains("function renderFiles() {\n  if (filesState !== 'ready') return;"));
    }

    #[test]
    fn ui_uses_wait_cursor_only_for_pending_buttons() {
        assert!(APP_JS.contains("button.classList.add('pending');"));
        assert!(APP_JS.contains("button.classList.remove('pending');"));
        assert!(STYLE_CSS.contains("button:disabled { cursor:not-allowed;"));
        assert!(STYLE_CSS.contains("button.pending:disabled { cursor:wait;"));
    }

    #[test]
    fn ui_has_semantic_visual_shell_and_tokens() {
        for marker in [
            "id=\"app-header\"",
            "class=\"app-header-inner\"",
            "class=\"brand-mark\"",
            "class=\"brand-name\"",
            "class=\"vault-context\"",
            "class=\"tab-list\"",
            "id=\"secret-item-count\"",
            "id=\"file-item-count\"",
        ] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
        }
        for token in [
            "--color-canvas:",
            "--color-surface:",
            "--color-surface-subtle:",
            "--color-text:",
            "--color-text-muted:",
            "--color-border:",
            "--color-accent:",
            "--color-accent-quiet:",
            "--color-danger:",
            "--shadow-raised:",
        ] {
            assert!(STYLE_CSS.contains(token), "missing {token}");
        }
    }

    #[test]
    fn ui_has_embedded_icons_and_data_surface_summaries() {
        for marker in [
            "id=\"xv-icon-sprite\"",
            "id=\"icon-secret\"",
            "id=\"icon-folder\"",
            "id=\"icon-check\"",
            "id=\"icon-alert\"",
            "class=\"data-surface\"",
            "id=\"secret-list-summary\"",
            "id=\"file-list-summary\"",
        ] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
        }
        assert!(APP_JS.contains("function icon(name)"));
        assert!(
            APP_JS.contains("function setListSummary(kind, visibleCount, totalCount, folderCount)")
        );
        assert!(APP_JS.contains("icon('secret')"));
        assert!(APP_JS.contains("icon('file')"));
        assert!(APP_JS.contains("icon(open ? 'chevron-down' : 'chevron-right')"));
        assert!(APP_JS.contains("content.className = 'folder-cell-content'"));
        assert!(STYLE_CSS.contains(".folder-cell-content { display:flex;"));
        assert!(!STYLE_CSS.contains(".folder-cell { display:flex;"));
    }

    #[test]
    fn ui_has_structured_drawer_and_button_hierarchy() {
        for marker in [
            "class=\"drawer-header\"",
            "id=\"drawer-kicker\"",
            "class=\"drawer-body\"",
            "class=\"drawer-footer\"",
            "class=\"button primary\"",
            "class=\"button ghost\"",
            "class=\"button danger\"",
        ] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
        }
        assert!(APP_JS.contains("label.className = 'form-field'"));
        assert!(APP_JS
            .contains("$('#drawer-kicker').textContent = name ? 'Edit secret' : 'Create secret'"));
        assert!(STYLE_CSS.contains(".drawer-footer {"));
        assert!(STYLE_CSS.contains("position:sticky"));
        assert!(!STYLE_CSS.contains("#drawer label {"));
        assert!(!STYLE_CSS.contains("#drawer input, #drawer textarea {"));
        assert!(STYLE_CSS.contains(".form-field { display:block;"));
        assert!(STYLE_CSS.contains("input, select, textarea { width:100%;"));
        assert!(STYLE_CSS.contains("textarea { padding:.65rem; resize:vertical; }"));
    }

    #[test]
    fn ui_bulk_move_uses_pending_button_state() {
        assert!(APP_JS.contains("beginPendingAction(moveButton, 'Moving…');"));
        assert!(APP_JS.contains("resetConfirmation(moveButton, 'Move');"));
    }

    #[test]
    fn ui_bulk_toolbar_sync_does_not_depend_on_table_render() {
        assert!(APP_JS.contains("function updateSelectionControls(kind)"));
        assert!(APP_JS.contains(
            "function setBulkPending(kind, pending, label) {\n  const state = selectionState(kind);"
        ));
        assert!(APP_JS.contains("updateSelectionControls(kind);\n  renderSelectionKind(kind);"));
    }

    #[test]
    fn ui_has_dark_responsive_and_accessible_visual_rules() {
        for marker in [
            "class=\"column-groups\"",
            "class=\"column-note\"",
            "class=\"column-file-type\"",
            "aria-label=\"Vault content\"",
            "aria-labelledby=\"drawer-title\"",
        ] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
        }
        for rule in [
            "@media (prefers-color-scheme: dark)",
            "@media (max-width: 48rem)",
            "@media (max-width: 34rem)",
            "@media (prefers-reduced-motion: reduce)",
            ":focus-visible",
            ".column-groups",
            ".column-note",
            ".column-file-type",
        ] {
            assert!(STYLE_CSS.contains(rule), "missing {rule}");
        }
    }

    #[test]
    fn ui_keeps_file_table_cells_semantic_and_phone_actions_visible() {
        assert!(APP_JS.contains("content.className = 'item-name-content'"));
        assert!(APP_JS.contains("actions.className = 'file-actions-content'"));
        assert!(!STYLE_CSS.contains(".item-name { display:flex;"));
        assert!(!STYLE_CSS.contains(".file-actions { display:flex;"));
        assert!(STYLE_CSS.contains(".item-name-content { display:flex;"));
        assert!(STYLE_CSS.contains(".file-actions-content { display:flex;"));
        for marker in ["column-file-size", "column-file-modified"] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
            assert!(APP_JS.contains(marker), "missing {marker}");
        }
        for marker in [
            "column-secret-name",
            "column-secret-folder",
            "column-secret-updated",
            "column-file-name",
        ] {
            assert!(INDEX_HTML.contains(marker), "missing {marker}");
            assert!(APP_JS.contains(marker), "missing {marker}");
        }
        assert!(STYLE_CSS.contains(
            ".column-file-size, .column-file-type, .column-file-modified { display:none; }"
        ));
        assert!(!STYLE_CSS.contains("#secrets-table, #files-table { table-layout:auto; }"));
        for rule in [
            "#secrets-table:not(.selection-mode) .column-secret-name { width:50%; }",
            "#secrets-table:not(.selection-mode) .column-secret-folder { width:22%; }",
            "#secrets-table:not(.selection-mode) .column-secret-updated { width:28%; }",
            ".selection-column { width:12.36%; }",
            "#secrets-table.selection-mode .column-secret-name { width:37.64%; }",
            "#files-table:not(.selection-mode) .column-file-name { width:46%; }",
            "#files-table:not(.selection-mode) .file-actions { width:54%; }",
            "#files-table.selection-mode .column-file-name { width:87.64%; }",
        ] {
            assert!(STYLE_CSS.contains(rule), "missing {rule}");
        }
        for calc_width in [
            "width:calc(50% - 2.75rem)",
            "width:calc(100% - 12rem)",
            "width:calc(100% - 2.75rem)",
        ] {
            assert!(!STYLE_CSS.contains(calc_width), "unexpected {calc_width}");
        }
        assert!(STYLE_CSS.contains("#files-table.selection-mode .file-actions { display:none; }"));
    }

    #[test]
    fn ui_row_and_upload_workflows_are_keyboard_operable() {
        assert!(INDEX_HTML.contains("id=\"browse-files\""));
        assert!(INDEX_HTML.contains("<button id=\"browse-files\""));
        assert!(!INDEX_HTML.contains("<label class=\"linkish\""));
        assert!(APP_JS.contains("$('#browse-files').onclick = () => $('#file-input').click();"));
        assert!(APP_JS.contains("function itemNameCell(kind, name, activate, accessibleLabel)"));
        assert!(APP_JS.contains("button.className = 'item-name-content row-action'"));
        assert!(APP_JS.contains("`Edit secret ${name}`"));
        assert!(APP_JS.contains("`Select file ${name}`"));
        assert!(STYLE_CSS.contains(".row-action:focus-visible"));
    }

    #[test]
    fn ui_compact_controls_keep_minimum_interaction_height() {
        assert!(STYLE_CSS.contains(".tab { min-height:2.25rem;"));
        assert!(STYLE_CSS.contains(".button.compact { min-height:2.25rem;"));
        assert!(STYLE_CSS.contains(".linkish { min-height:2.25rem;"));
    }
}
