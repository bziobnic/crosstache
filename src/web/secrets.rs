use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::records::{
    apply_conversion, preview_conversion, validate_conditional_conversion_backend,
    ConversionPreview, ConversionRequest,
};
use crate::secret::manager::{DeletedSecretSummary, SecretProperties};

use super::api::{ApiError, VaultQuery};
use super::WebState;

const MAX_CONVERSION_FIELDS: usize = 64;
const MAX_CONVERSION_FIELD_NAME_BYTES: usize = 128;
const MAX_CONVERSION_FIELD_VALUE_BYTES: usize = 1024 * 1024;
const MAX_CONVERSION_TOTAL_VALUE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CONVERSION_REQUEST_BYTES: usize = MAX_CONVERSION_TOTAL_VALUE_BYTES + 64 * 1024;
pub(crate) const MAX_RENAME_REQUEST_BYTES: usize = 4 * 1024;
const MAX_WEB_SECRET_NAME_BYTES: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversionBody {
    target_type: Option<String>,
    target: Option<ConversionTargetBody>,
    #[serde(default)]
    supplied_fields: BTreeMap<String, String>,
    #[serde(default)]
    confirm_lossy: bool,
    source_revision: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ConversionTargetBody {
    Typed { target_type: String },
    Plain,
}

#[derive(Serialize)]
pub(crate) struct ConversionResult {
    secret: SecretProperties,
    summary: ConversionPreview,
}

#[derive(Serialize)]
pub(crate) struct ConversionPreviewResponse {
    #[serde(flatten)]
    summary: ConversionPreview,
    source_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameBody {
    new_name: String,
}

fn structured_error(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    hint: &'static str,
    field: Option<&'static str>,
) -> ApiError {
    ApiError::Structured {
        status,
        error: Box::new(super::errors::ApiErrorBody::new(
            code, message, hint, field, None,
        )),
    }
}

fn dynamic_validation_error(message: &'static str, field: &str) -> ApiError {
    ApiError::Structured {
        status: StatusCode::BAD_REQUEST,
        error: Box::new(super::errors::ApiErrorBody::new(
            "xv-invalid-argument",
            message,
            "Correct the highlighted field and try again.",
            Some(field),
            None,
        )),
    }
}

fn validate_conversion_body(
    state: &WebState,
    body: &ConversionBody,
) -> Result<ConversionRequest, ApiError> {
    let target_type = match (&body.target, &body.target_type) {
        (None, Some(target_type)) => Some(target_type),
        (Some(ConversionTargetBody::Typed { target_type }), None) => Some(target_type),
        (Some(ConversionTargetBody::Plain), None) => None,
        _ => {
            return Err(ApiError::Validation {
                status: StatusCode::BAD_REQUEST,
                message: "Choose exactly one valid conversion target.",
                field: Some("target"),
            });
        }
    };
    if target_type
        .is_some_and(|name| name.is_empty() || name.len() > MAX_CONVERSION_FIELD_NAME_BYTES)
    {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Choose a valid target type.",
            field: Some("target_type"),
        });
    }
    if body
        .source_revision
        .as_ref()
        .is_some_and(|revision| revision.is_empty() || revision.len() > 256)
    {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Use the source revision returned by conversion preview.",
            field: Some("source_revision"),
        });
    }
    let target = match target_type {
        Some(target_type) => Some(
            state
                .types
                .iter()
                .find(|record_type| record_type.name == *target_type)
                .ok_or(ApiError::Validation {
                    status: StatusCode::BAD_REQUEST,
                    message: "Choose a known target type.",
                    field: Some("target_type"),
                })?,
        ),
        None => None,
    };
    if body.supplied_fields.len() > MAX_CONVERSION_FIELDS {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Too many supplied fields were provided.",
            field: Some("supplied_fields"),
        });
    }
    let mut total_value_bytes = 0usize;
    for (name, value) in &body.supplied_fields {
        if name.is_empty()
            || name.len() > MAX_CONVERSION_FIELD_NAME_BYTES
            || target.is_none_or(|target| target.field(name).is_none())
        {
            return Err(dynamic_validation_error(
                "A supplied field is not declared by the target type.",
                &format!("supplied_fields.{name}"),
            ));
        }
        if value.len() > MAX_CONVERSION_FIELD_VALUE_BYTES {
            return Err(dynamic_validation_error(
                "A supplied field value is too large.",
                &format!("supplied_fields.{name}"),
            ));
        }
        total_value_bytes = total_value_bytes.saturating_add(value.len());
    }
    if total_value_bytes > MAX_CONVERSION_TOTAL_VALUE_BYTES {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "The supplied field values are too large.",
            field: Some("supplied_fields"),
        });
    }

    let mut request = match target_type {
        Some(target_type) => ConversionRequest::to_type(target_type.clone()),
        None => ConversionRequest::plain(),
    };
    request.supplied_fields = body.supplied_fields.clone();
    request.confirm_lossy = body.confirm_lossy;
    Ok(request)
}

fn conversion_service_error(error: crate::error::CrosstacheError) -> ApiError {
    let message = error.to_string();
    let field = message
        .split_once("required field '")
        .and_then(|(_, suffix)| suffix.split_once('\''))
        .map(|(name, _)| format!("supplied_fields.{name}"))
        .unwrap_or_else(|| "target_type".to_string());
    dynamic_validation_error(
        "The secret cannot be converted with the selected target and supplied fields.",
        &field,
    )
}

fn conversion_apply_error(error: crate::error::CrosstacheError) -> ApiError {
    match error {
        crate::error::CrosstacheError::Conflict(_) => structured_error(
            StatusCode::CONFLICT,
            "xv-conversion-source-changed",
            "The secret changed after this conversion was previewed.",
            "Preview the conversion again before applying it.",
            Some("source_revision"),
        ),
        crate::error::CrosstacheError::ConfigError(_) => ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "The converted secret exceeds a backend validation limit.",
            field: Some("target_type"),
        },
        other => ApiError::App(other),
    }
}

fn redact_conversion_properties(properties: &mut SecretProperties) {
    properties.value = None;
    properties.tags.clear();
}

fn conversion_backend_preflight(backend: &dyn crate::backend::Backend) -> Result<(), ApiError> {
    validate_conditional_conversion_backend(backend).map_err(|_| {
        structured_error(
            StatusCode::NOT_IMPLEMENTED,
            "xv-operation-unsupported",
            "This backend does not support atomic record conversion.",
            "Choose a backend that supports conversion and try again.",
            Some("target_type"),
        )
    })
}

async fn bounded_json<T: DeserializeOwned>(
    request: Request,
    max_bytes: usize,
    field: Option<&'static str>,
    message: &'static str,
    hint: &'static str,
) -> Result<T, ApiError> {
    let is_json = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let media_type = value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            media_type == "application/json"
                || (media_type.starts_with("application/") && media_type.ends_with("+json"))
        });
    if !is_json {
        return Err(structured_error(
            StatusCode::BAD_REQUEST,
            "xv-invalid-request",
            message,
            hint,
            field,
        ));
    }
    let bytes = to_bytes(request.into_body(), max_bytes)
        .await
        .map_err(|_| {
            structured_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "xv-request-too-large",
                "The request is too large to process.",
                "Reduce the request size and try again.",
                field,
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        structured_error(
            StatusCode::BAD_REQUEST,
            "xv-invalid-request",
            message,
            hint,
            field,
        )
    })
}

pub(crate) async fn preview_conversion_route(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
    request: Request,
) -> Result<Json<ConversionPreviewResponse>, ApiError> {
    let body: ConversionBody = bounded_json(
        request,
        MAX_CONVERSION_REQUEST_BYTES,
        Some("target_type"),
        "The conversion request could not be understood.",
        "Correct the highlighted fields and try again.",
    )
    .await?;
    let request = validate_conversion_body(&state, &body)?;
    let target = query.target(&state)?;
    conversion_backend_preflight(target.backend.as_ref())?;
    let snapshot = target
        .backend
        .secrets()
        .get_secret_snapshot(&target.context.vault, &name, true)
        .await?;
    let preview = preview_conversion(&snapshot.properties, &state.types, request)
        .map_err(conversion_service_error)?;
    Ok(Json(ConversionPreviewResponse {
        summary: preview,
        source_revision: snapshot.revision,
    }))
}

pub(crate) async fn apply_conversion_route(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
    request: Request,
) -> Result<Json<ConversionResult>, ApiError> {
    let body: ConversionBody = bounded_json(
        request,
        MAX_CONVERSION_REQUEST_BYTES,
        Some("target_type"),
        "The conversion request could not be understood.",
        "Correct the highlighted fields and try again.",
    )
    .await?;
    let request = validate_conversion_body(&state, &body)?;
    let target = query.target(&state)?;
    conversion_backend_preflight(target.backend.as_ref())?;
    let snapshot = target
        .backend
        .secrets()
        .get_secret_snapshot(&target.context.vault, &name, true)
        .await?;
    let preview = preview_conversion(&snapshot.properties, &state.types, request)
        .map_err(conversion_service_error)?;
    if preview.requires_confirmation {
        return Err(structured_error(
            StatusCode::CONFLICT,
            "xv-conversion-confirmation-required",
            "This conversion would lose or expose fields.",
            "Review the conversion summary and explicitly confirm the change.",
            Some("confirm_lossy"),
        ));
    }
    let Some(source_revision) = body.source_revision.as_deref() else {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Preview this conversion before applying it.",
            field: Some("source_revision"),
        });
    };
    if snapshot.revision != source_revision {
        return Err(structured_error(
            StatusCode::CONFLICT,
            "xv-conversion-source-changed",
            "The secret changed after this conversion was previewed.",
            "Preview the conversion again before applying it.",
            Some("source_revision"),
        ));
    }
    let summary = preview.clone();
    let mut secret = apply_conversion(
        target.backend.as_ref(),
        &target.context.vault,
        &name,
        source_revision,
        preview,
    )
    .await
    .map_err(conversion_apply_error)?;
    redact_conversion_properties(&mut secret);
    Ok(Json(ConversionResult { secret, summary }))
}

fn validate_secret_name(
    name: &str,
    capabilities: &crate::backend::BackendCapabilities,
) -> Result<(), ApiError> {
    let max_length = capabilities
        .max_name_length
        .unwrap_or(MAX_WEB_SECRET_NAME_BYTES)
        .min(MAX_WEB_SECRET_NAME_BYTES);
    if name.is_empty()
        || name.len() > max_length
        || name.chars().any(char::is_control)
        || !capabilities.name_charset.is_valid(name)
    {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Enter a valid secret name for the selected backend.",
            field: Some("name"),
        });
    }
    Ok(())
}

fn rename_backend_error(error: crate::backend::error::BackendError) -> ApiError {
    match error {
        crate::backend::error::BackendError::SourceRevisionConflict { .. } => structured_error(
            StatusCode::CONFLICT,
            "xv-rename-source-changed",
            "The source secret changed before the rename could commit.",
            "Refresh the secret and retry the rename.",
            Some("source_revision"),
        ),
        crate::backend::error::BackendError::DestinationExists { .. } => structured_error(
            StatusCode::CONFLICT,
            "xv-rename-destination-exists",
            "A secret with the new name already exists.",
            "Choose a different name and try again.",
            Some("name"),
        ),
        crate::backend::error::BackendError::AttachmentsPresent { .. } => structured_error(
            StatusCode::CONFLICT,
            "xv-attachments-block-rename",
            "This secret has attachments and cannot be renamed safely.",
            "Remove the attachments first, or keep the current secret name.",
            Some("name"),
        ),
        crate::backend::error::BackendError::Conflict(_) => structured_error(
            StatusCode::CONFLICT,
            "xv-conflict",
            "A secret with the new name already exists.",
            "Choose a different name and try again.",
            Some("name"),
        ),
        crate::backend::error::BackendError::InvalidArgument(_) => ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Enter a different valid secret name.",
            field: Some("name"),
        },
        other => ApiError::Backend(other),
    }
}

pub(crate) async fn rename(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Query(query): Query<VaultQuery>,
    request: Request,
) -> Result<Json<SecretProperties>, ApiError> {
    let body: RenameBody = bounded_json(
        request,
        MAX_RENAME_REQUEST_BYTES,
        Some("name"),
        "The rename request could not be understood.",
        "Enter only a valid new secret name and try again.",
    )
    .await?;
    let target = query.target(&state)?;
    if !target.backend.capabilities().has_atomic_rename
        || !target.backend.secrets().supports_atomic_rename()
    {
        return Err(structured_error(
            StatusCode::NOT_IMPLEMENTED,
            "xv-operation-unsupported",
            "This backend does not support conflict-safe rename.",
            "Choose a backend with atomic rename support and try again.",
            Some("name"),
        ));
    }
    let capabilities = target.backend.capabilities();
    validate_secret_name(&name, &capabilities)?;
    validate_secret_name(&body.new_name, &capabilities)?;
    super::api::reject_reserved_attachment_key(&name)?;
    super::api::reject_reserved_attachment_key(&body.new_name)?;
    if body.new_name == name {
        return Err(ApiError::Validation {
            status: StatusCode::BAD_REQUEST,
            message: "Enter a name that is different from the current name.",
            field: Some("name"),
        });
    }
    #[cfg(feature = "file-ops")]
    if let Some(files) = target.backend.files() {
        let attachments =
            crate::secret::attachments::list_attachments(files, &target.context.vault, &name)
                .await?;
        if !attachments.is_empty() {
            return Err(structured_error(
                StatusCode::CONFLICT,
                "xv-attachments-block-rename",
                "This secret has attachments and cannot be renamed safely.",
                "Remove the attachments first, or keep the current secret name.",
                Some("name"),
            ));
        }
    }
    let snapshot = target
        .backend
        .secrets()
        .get_secret_snapshot(&target.context.vault, &name, false)
        .await?;
    if target
        .backend
        .secrets()
        .secret_exists(&target.context.vault, &body.new_name)
        .await?
    {
        return Err(structured_error(
            StatusCode::CONFLICT,
            "xv-rename-destination-exists",
            "A secret with the new name already exists.",
            "Choose a different name and try again.",
            Some("name"),
        ));
    }

    let mut renamed = target
        .backend
        .secrets()
        .rename_secret_if_revision(
            &target.context.vault,
            &name,
            &body.new_name,
            &snapshot.revision,
        )
        .await
        .map_err(rename_backend_error)?;
    renamed.value = None;
    renamed.tags.clear();
    Ok(Json(renamed))
}

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

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

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

    fn scoped_state(
        primary: Arc<testutil::stub::StubBackend>,
        stage: Arc<testutil::stub::StubBackend>,
    ) -> Arc<crate::web::WebState> {
        let primary_trait: Arc<dyn crate::backend::Backend> = primary;
        let stage_trait: Arc<dyn crate::backend::Backend> = stage;
        let registry = Arc::new(crate::backend::BackendRegistry::for_test(
            "primary",
            vec![("primary", primary_trait.clone()), ("stage", stage_trait)],
        ));
        let mut context = testutil::test_context(primary_trait.as_ref(), "payments", 30);
        context.workspace.configured = true;
        context.workspace.alias = "work".into();
        context.workspace.entries = vec![
            crate::web::context::WorkspaceEntrySummary {
                alias: "work".into(),
                backend: "primary".into(),
                vault: "payments".into(),
                default: true,
            },
            crate::web::context::WorkspaceEntrySummary {
                alias: "stage".into(),
                backend: "stage".into(),
                vault: "sandbox".into(),
                default: false,
            },
        ];
        let path = std::env::temp_dir()
            .join(format!("xv-web-scoped-test-{}", uuid::Uuid::new_v4()))
            .join("ui.json");
        Arc::new(crate::web::WebState::new(
            primary_trait,
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(path, 30),
            registry,
        ))
    }

    fn atomic_stub(name: &'static str) -> Arc<testutil::stub::StubBackend> {
        Arc::new(testutil::stub::StubBackend::with_capabilities(
            name,
            crate::backend::BackendCapabilities {
                has_atomic_record_conversion: true,
                has_conditional_record_conversion: true,
                has_atomic_rename: true,
                has_enable_disable: true,
                has_groups: true,
                has_folders: true,
                has_notes: true,
                has_expiry: true,
                max_name_length: Some(255),
                ..Default::default()
            },
        ))
    }

    fn state_with_types(
        backend: Arc<testutil::stub::StubBackend>,
        types: Vec<crate::records::RecordType>,
    ) -> Arc<crate::web::WebState> {
        let backend_trait: Arc<dyn crate::backend::Backend> = backend;
        let context = testutil::test_context(backend_trait.as_ref(), "vault", 30);
        let path = std::env::temp_dir()
            .join(format!("xv-web-types-test-{}", uuid::Uuid::new_v4()))
            .join("ui.json");
        Arc::new(crate::web::WebState::new(
            backend_trait.clone(),
            context,
            "test-token".into(),
            types,
            crate::web::preferences::PreferenceStore::new(path, 30),
            Arc::new(crate::backend::BackendRegistry::new(backend_trait)),
        ))
    }

    async fn put_login_record(app: axum::Router, name: &str) {
        let (status, _) = get_json(
            app,
            "PUT",
            &format!("/api/secrets/{name}"),
            Some(json!({
                "value": r#"{"password":"route-secret-value"}"#,
                "content_type": crate::records::RECORD_CONTENT_TYPE,
                "tags": {
                    crate::records::TYPE_TAG: "login",
                    "f.username": "route-public-value"
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    async fn put_plain_secret(app: axum::Router, name: &str, value: &str) {
        let (status, _) = get_json(
            app,
            "PUT",
            &format!("/api/secrets/{name}"),
            Some(json!({"value": value})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn conversion_preview_is_value_free_and_names_actual_loss() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;

        let (status, preview) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"api-key"})),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(preview["dropped"], json!(["username"]));
        assert_eq!(preview["renamed"], json!(["password -> key"]));
        assert_eq!(preview["requires_confirmation"], true);
        let serialized = preview.to_string();
        assert!(!serialized.contains("route-secret-value"));
        assert!(!serialized.contains("route-public-value"));
    }

    #[tokio::test]
    async fn record_to_plain_preview_and_apply_use_the_strict_plain_target() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;

        let (status, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target":{"kind":"plain"}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(preview["target_type"], serde_json::Value::Null);
        assert_eq!(preview["retained"], json!(["password"]));
        assert_eq!(preview["dropped"], json!(["username"]));
        let revision = preview["source_revision"]
            .as_str()
            .expect("opaque source revision");

        let (status, confirmation) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target":{"kind":"plain"},
                "source_revision":revision
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            confirmation["error"]["code"],
            "xv-conversion-confirmation-required"
        );

        let (status, converted) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target":{"kind":"plain"},
                "source_revision":revision,
                "confirm_lossy":true
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(converted["secret"]["content_type"], "");
        assert_eq!(converted["secret"]["tags"], json!({}));
        assert!(converted["secret"]["value"].is_null());
        assert!(!converted.to_string().contains("route-secret-value"));
        assert!(!converted.to_string().contains("route-public-value"));
    }

    #[tokio::test]
    async fn conversion_and_rename_responses_strip_all_internal_record_tags() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "convert-me").await;
        put_login_record(app.clone(), "rename-me").await;

        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/convert-me/conversion/preview",
            Some(json!({"target_type":"database"})),
        )
        .await;
        let (status, converted) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/convert-me/conversion",
            Some(json!({
                "target_type":"database",
                "source_revision":preview["source_revision"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(converted["secret"]["tags"], json!({}));

        let (status, renamed) = get_json(
            app,
            "POST",
            "/api/secrets/rename-me/rename",
            Some(json!({"new_name":"renamed"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renamed["tags"], json!({}));
        for marker in [
            crate::records::TYPE_TAG,
            "f.username",
            "route-public-value",
            "route-secret-value",
        ] {
            assert!(!converted.to_string().contains(marker));
            assert!(!renamed.to_string().contains(marker));
        }
    }

    #[tokio::test]
    async fn bounded_json_accepts_case_insensitive_vendor_json_with_ows() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;
        let request = Request::post("/api/secrets/login/conversion/preview")
            .header(header::HOST, "127.0.0.1:1")
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(
                header::CONTENT_TYPE,
                "Application/Vnd.Crosstache+Json ; charset=utf-8",
            )
            .body(Body::from(r#"{"target_type":"database"}"#))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn rename_rejects_attached_secrets_before_any_mutation() {
        let backend = atomic_stub("attachments");
        backend.files.lock().unwrap().insert(
            crate::secret::attachments::attachment_blob_name("source", "proof.txt"),
            (
                b"encrypted-attachment".to_vec(),
                "application/octet-stream".into(),
            ),
        );
        let app = crate::web::build_router(state_with_types(
            backend.clone(),
            crate::records::builtin_types(),
        ));
        put_plain_secret(app.clone(), "source", "source-value").await;

        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "xv-attachments-block-rename");
        assert!(backend.secrets.lock().unwrap().contains_key("source"));
        assert!(!backend.secrets.lock().unwrap().contains_key("destination"));
        assert_eq!(backend.files.lock().unwrap().len(), 1);
    }

    #[cfg(feature = "file-ops")]
    #[tokio::test]
    async fn rename_rejects_a_persisted_destination_attachment_after_its_secret_is_deleted() {
        use std::collections::HashMap;

        use crate::blob::models::{FileListRequest, FileUploadRequest};

        let temp = tempfile::tempdir().unwrap();
        let state = real_local_state(&temp);
        let backend = state.base_backend();
        let app = crate::web::build_router(state);

        put_plain_secret(app.clone(), "destination", "destination-value").await;
        backend
            .files()
            .unwrap()
            .upload_file(
                "default",
                FileUploadRequest {
                    name: crate::secret::attachments::attachment_blob_name(
                        "destination",
                        "proof.txt",
                    ),
                    content: b"encrypted-destination-attachment".to_vec(),
                    content_type: Some("application/octet-stream".into()),
                    groups: Vec::new(),
                    metadata: HashMap::new(),
                    tags: HashMap::new(),
                },
                None,
            )
            .await
            .unwrap();
        backend
            .secrets()
            .delete_secret("default", "destination")
            .await
            .unwrap();
        put_plain_secret(app.clone(), "source", "source-value").await;

        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "xv-attachments-block-rename");
        let source = backend
            .secrets()
            .get_secret("default", "source", true)
            .await
            .unwrap();
        assert_eq!(
            source.value.as_deref().map(|value| value.as_str()),
            Some("source-value")
        );
        assert!(!backend
            .secrets()
            .secret_exists("default", "destination")
            .await
            .unwrap());
        let source_attachments = backend
            .files()
            .unwrap()
            .list_files(
                "default",
                FileListRequest {
                    prefix: Some(crate::secret::attachments::attachment_prefix("source")),
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        let destination_attachments = backend
            .files()
            .unwrap()
            .list_files(
                "default",
                FileListRequest {
                    prefix: Some(crate::secret::attachments::attachment_prefix("destination")),
                    groups: None,
                    limit: None,
                    delimiter: None,
                },
            )
            .await
            .unwrap();
        assert!(source_attachments.is_empty());
        assert_eq!(destination_attachments.len(), 1);
    }

    #[tokio::test]
    async fn conversion_apply_requires_stable_confirmation_and_returns_safe_summary() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"api-key"})),
        )
        .await;
        let source_revision = preview["source_revision"].as_str().unwrap();

        let (status, error) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({"target_type":"api-key","confirm_lossy":false})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            error["error"]["code"],
            "xv-conversion-confirmation-required"
        );

        let (status, converted) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"api-key",
                "confirm_lossy":true,
                "source_revision":source_revision
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            converted["secret"]["content_type"],
            crate::records::RECORD_CONTENT_TYPE
        );
        assert_eq!(converted["secret"]["tags"], json!({}));
        assert_eq!(converted["summary"]["dropped"], json!(["username"]));
        assert!(converted["secret"]["value"].is_null());
        let serialized = converted.to_string();
        assert!(!serialized.contains("route-secret-value"));
        assert!(!serialized.contains("route-public-value"));
    }

    #[tokio::test]
    async fn conversion_request_rejects_unknown_and_unrelated_fields() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;

        for body in [
            json!({"target_type":"missing"}),
            json!({"target_type":"api-key","metadata":{"note":"unrelated"}}),
            json!({"target_type":"api-key","supplied_fields":{"unknown":"value"}}),
        ] {
            let (status, error) = get_json(
                app.clone(),
                "POST",
                "/api/secrets/login/conversion/preview",
                Some(body),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(error["error"]["field"].is_string());
            assert!(!error.to_string().contains("route-secret-value"));
        }
    }

    #[tokio::test]
    async fn conversion_and_rename_requests_enforce_bounded_shapes() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;
        let too_many: serde_json::Map<String, serde_json::Value> = (0..65)
            .map(|index| (format!("field-{index}"), json!("bounded-marker")))
            .collect();
        for body in [
            json!({"target_type":"x".repeat(129)}),
            json!({"target_type":"api-key","supplied_fields":too_many}),
            json!({
                "target_type":"api-key",
                "supplied_fields":{"account":"x".repeat(super::MAX_CONVERSION_FIELD_VALUE_BYTES + 1)}
            }),
        ] {
            let (status, error) = get_json(
                app.clone(),
                "POST",
                "/api/secrets/login/conversion/preview",
                Some(body),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(error["error"]["field"].is_string());
            assert!(!error.to_string().contains("bounded-marker"));
        }

        let (status, oversized) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({
                "target_type":"api-key",
                "supplied_fields":{
                    "account":"x".repeat(super::MAX_CONVERSION_REQUEST_BYTES + 1)
                }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(oversized["error"]["code"], "xv-request-too-large");

        let rename_app = crate::web::build_router(testutil::test_state());
        put_plain_secret(rename_app.clone(), "source", "bounded-secret-value").await;
        let (status, error) = get_json(
            rename_app,
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"x".repeat(super::MAX_WEB_SECRET_NAME_BYTES + 1)})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["field"], "name");
        assert!(!error.to_string().contains("bounded-secret-value"));
    }

    #[tokio::test]
    async fn conversion_apply_rejects_a_stale_preview_without_changing_the_new_source() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"api-key"})),
        )
        .await;
        let source_revision = preview["source_revision"]
            .as_str()
            .expect("preview source revision")
            .to_string();

        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/login",
            Some(json!({"value":"newer-source-value"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, stale) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"api-key",
                "confirm_lossy":true,
                "source_revision":source_revision
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["error"]["code"], "xv-conversion-source-changed");

        let (_, current) = get_json(app, "POST", "/api/secrets/login/value", None).await;
        assert_eq!(current["value"], "newer-source-value");
    }

    #[tokio::test]
    async fn conversion_cas_rejects_an_edit_between_route_snapshot_and_commit() {
        let backend = Arc::new(testutil::stub::StubBackend::with_conversion_cas_race(
            "cas-race",
            "edit-between-check-and-write",
        ));
        let app = crate::web::build_router(state_with_types(
            backend.clone(),
            crate::records::builtin_types(),
        ));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"database"})),
        )
        .await;

        let (status, error) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"database",
                "source_revision":preview["source_revision"]
            })),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "xv-conversion-source-changed");
        let (_, current) = get_json(app, "POST", "/api/secrets/login/value", None).await;
        assert_eq!(current["value"], "edit-between-check-and-write");
    }

    #[tokio::test]
    async fn same_type_no_op_apply_still_rejects_a_revision_race() {
        let backend = Arc::new(testutil::stub::StubBackend::with_conversion_cas_race(
            "same-type-race",
            r#"{"username":"newer","password":"newer"}"#,
        ));
        let app =
            crate::web::build_router(state_with_types(backend, crate::records::builtin_types()));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"login"})),
        )
        .await;

        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"login",
                "source_revision":preview["source_revision"]
            })),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "xv-conversion-source-changed");
        assert_eq!(error["error"]["field"], "source_revision");
    }

    #[tokio::test]
    async fn conversion_revision_rejects_delete_recreate_with_the_same_version_label() {
        let temp = tempfile::tempdir().unwrap();
        let app = crate::web::build_router(real_local_state(&temp));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"database"})),
        )
        .await;
        let (status, _) = get_json(app.clone(), "DELETE", "/api/secrets/login", None).await;
        assert_eq!(status, StatusCode::OK);
        put_login_record(app.clone(), "login").await;

        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"database",
                "source_revision":preview["source_revision"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "xv-conversion-source-changed");
    }

    #[tokio::test]
    async fn concurrent_conversion_tabs_keep_their_exact_attached_workspace_targets() {
        let primary = atomic_stub("primary");
        let stage = atomic_stub("stage");
        let app = crate::web::build_router(scoped_state(primary.clone(), stage.clone()));
        let primary_scope = "?alias=work&backend=primary&vault=payments";
        let stage_scope = "?alias=stage&backend=stage&vault=sandbox";
        for scope in [primary_scope, stage_scope] {
            let (status, _) = get_json(
                app.clone(),
                "PUT",
                &format!("/api/secrets/login{scope}"),
                Some(json!({
                    "value": r#"{"password":"scoped-secret-value"}"#,
                    "content_type": crate::records::RECORD_CONTENT_TYPE,
                    "tags": {
                        crate::records::TYPE_TAG: "login",
                        "f.username": "scoped-user"
                    }
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let (_, primary_preview) = get_json(
            app.clone(),
            "POST",
            &format!("/api/secrets/login/conversion/preview{primary_scope}"),
            Some(json!({"target_type":"database"})),
        )
        .await;
        let (_, stage_preview) = get_json(
            app.clone(),
            "POST",
            &format!("/api/secrets/login/conversion/preview{stage_scope}"),
            Some(json!({"target_type":"api-key"})),
        )
        .await;

        let primary_tab = tokio::spawn(get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion?alias=work&backend=primary&vault=payments",
            Some(json!({
                "target_type":"database",
                "confirm_lossy":true,
                "source_revision":primary_preview["source_revision"]
            })),
        ));
        let stage_tab = tokio::spawn(get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion?alias=stage&backend=stage&vault=sandbox",
            Some(json!({
                "target_type":"api-key",
                "confirm_lossy":true,
                "source_revision":stage_preview["source_revision"]
            })),
        ));
        let ((primary_status, _), (stage_status, _)) =
            tokio::try_join!(primary_tab, stage_tab).unwrap();
        assert_eq!(primary_status, StatusCode::OK);
        assert_eq!(stage_status, StatusCode::OK);

        assert_eq!(
            primary.secrets.lock().unwrap()["login"]
                .tags
                .as_ref()
                .unwrap()[crate::records::TYPE_TAG],
            "database"
        );
        assert_eq!(
            stage.secrets.lock().unwrap()["login"]
                .tags
                .as_ref()
                .unwrap()[crate::records::TYPE_TAG],
            "api-key"
        );
        let (_, base) = get_json(app, "GET", "/api/context", None).await;
        assert_eq!(base["backend"], "primary");
        assert_eq!(base["vault"], "payments");
    }

    #[tokio::test]
    async fn unsupported_conversion_is_rejected_before_a_malformed_source_is_read() {
        let backend = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "unsupported",
            crate::backend::BackendCapabilities::default(),
        ));
        let app =
            crate::web::build_router(state_with_types(backend, crate::records::builtin_types()));
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/broken",
            Some(json!({
                "value":"malformed-secret-envelope",
                "content_type":crate::records::RECORD_CONTENT_TYPE,
                "tags":{crate::records::TYPE_TAG:"login"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/broken/conversion/preview",
            Some(json!({"target_type":"api-key"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(error["error"]["code"], "xv-operation-unsupported");
    }

    #[tokio::test]
    async fn update_cas_without_revision_validation_is_rejected_before_source_read() {
        let backend = Arc::new(testutil::stub::StubBackend::new().without_revision_validation());
        let app =
            crate::web::build_router(state_with_types(backend, crate::records::builtin_types()));

        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/missing/conversion/preview",
            Some(json!({"target_type":"api-key"})),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(error["error"]["code"], "xv-operation-unsupported");
    }

    #[tokio::test]
    async fn conversion_requires_missing_fields_and_never_echoes_supplied_values() {
        use crate::records::{FieldDef, FieldKind, RecordType, TypeSource};

        let required_type = RecordType {
            name: "required-target".into(),
            source: TypeSource::Project,
            fields: vec![
                FieldDef {
                    name: "account".into(),
                    kind: FieldKind::Metadata,
                    required: true,
                    primary: false,
                },
                FieldDef {
                    name: "password".into(),
                    kind: FieldKind::Secret,
                    required: true,
                    primary: true,
                },
            ],
        };
        let mut types = crate::records::builtin_types();
        types.push(required_type);
        let backend = atomic_stub("required");
        let app = crate::web::build_router(state_with_types(backend, types));
        put_login_record(app.clone(), "login").await;

        let (status, missing) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"required-target"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(missing["error"]["field"], "supplied_fields.account");

        let (status, preview) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({
                "target_type":"required-target",
                "supplied_fields":{"account":"supplied-sensitive-marker"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!preview.to_string().contains("supplied-sensitive-marker"));
    }

    #[tokio::test]
    async fn sensitivity_exposure_requires_confirmation_without_echoing_the_value() {
        use crate::records::{FieldDef, FieldKind, RecordType, TypeSource};

        let protected = RecordType {
            name: "protected".into(),
            source: TypeSource::Project,
            fields: vec![
                FieldDef {
                    name: "password".into(),
                    kind: FieldKind::Secret,
                    required: true,
                    primary: true,
                },
                FieldDef {
                    name: "token".into(),
                    kind: FieldKind::Secret,
                    required: false,
                    primary: false,
                },
            ],
        };
        let exposed = RecordType {
            name: "exposed".into(),
            source: TypeSource::Project,
            fields: vec![
                FieldDef {
                    name: "password".into(),
                    kind: FieldKind::Secret,
                    required: true,
                    primary: true,
                },
                FieldDef {
                    name: "token".into(),
                    kind: FieldKind::Metadata,
                    required: false,
                    primary: false,
                },
            ],
        };
        let backend = atomic_stub("sensitivity");
        let app =
            crate::web::build_router(state_with_types(backend.clone(), vec![protected, exposed]));
        let (status, _) = get_json(
            app.clone(),
            "PUT",
            "/api/secrets/record",
            Some(json!({
                "value":r#"{"password":"password-marker","token":"exposed-marker"}"#,
                "content_type":crate::records::RECORD_CONTENT_TYPE,
                "tags":{crate::records::TYPE_TAG:"protected"}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/record/conversion/preview",
            Some(json!({"target_type":"exposed"})),
        )
        .await;
        assert_eq!(preview["exposed"], json!(["token"]));
        assert_eq!(preview["requires_confirmation"], true);
        assert!(!preview.to_string().contains("exposed-marker"));

        let (status, converted) = get_json(
            app,
            "POST",
            "/api/secrets/record/conversion",
            Some(json!({
                "target_type":"exposed",
                "confirm_lossy":true,
                "source_revision":preview["source_revision"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!converted.to_string().contains("exposed-marker"));
        assert_eq!(
            backend.secrets.lock().unwrap()["record"]
                .tags
                .as_ref()
                .unwrap()["f.token"],
            "exposed-marker"
        );
    }

    #[tokio::test]
    async fn malformed_envelope_and_tag_budget_fail_without_mutation() {
        let malformed_backend = atomic_stub("malformed");
        let malformed_app = crate::web::build_router(state_with_types(
            malformed_backend,
            crate::records::builtin_types(),
        ));
        let _ = get_json(
            malformed_app.clone(),
            "PUT",
            "/api/secrets/broken",
            Some(json!({
                "value":"malformed-envelope-marker",
                "content_type":crate::records::RECORD_CONTENT_TYPE,
                "tags":{crate::records::TYPE_TAG:"login"}
            })),
        )
        .await;
        let (status, error) = get_json(
            malformed_app,
            "POST",
            "/api/secrets/broken/conversion/preview",
            Some(json!({"target_type":"database"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!error.to_string().contains("malformed-envelope-marker"));

        let limited = Arc::new(testutil::stub::StubBackend::with_capabilities(
            "limited",
            crate::backend::BackendCapabilities {
                has_atomic_record_conversion: true,
                has_conditional_record_conversion: true,
                has_enable_disable: true,
                has_groups: true,
                has_folders: true,
                has_notes: true,
                has_expiry: true,
                max_tags: Some(1),
                ..Default::default()
            },
        ));
        let app = crate::web::build_router(state_with_types(
            limited.clone(),
            crate::records::builtin_types(),
        ));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"database"})),
        )
        .await;
        let (status, _) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"database",
                "source_revision":preview["source_revision"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            limited.secrets.lock().unwrap()["login"]
                .tags
                .as_ref()
                .unwrap()[crate::records::TYPE_TAG],
            "login"
        );
    }

    #[tokio::test]
    async fn conversion_backend_failure_keeps_internal_details_and_values_out_of_json() {
        let failing = Arc::new(testutil::stub::StubBackend::with_update_error(
            "failing",
            crate::backend::BackendCapabilities {
                has_atomic_record_conversion: true,
                has_conditional_record_conversion: true,
                has_enable_disable: true,
                has_groups: true,
                has_folders: true,
                has_notes: true,
                has_expiry: true,
                ..Default::default()
            },
            "sensitive-internal-backend-marker",
        ));
        let app =
            crate::web::build_router(state_with_types(failing, crate::records::builtin_types()));
        put_login_record(app.clone(), "login").await;
        let (_, preview) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/login/conversion/preview",
            Some(json!({"target_type":"database"})),
        )
        .await;
        let (status, error) = get_json(
            app,
            "POST",
            "/api/secrets/login/conversion",
            Some(json!({
                "target_type":"database",
                "source_revision":preview["source_revision"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["error"]["code"], "xv-unknown");
        let serialized = error.to_string();
        assert!(!serialized.contains("sensitive-internal-backend-marker"));
        assert!(!serialized.contains("route-secret-value"));
        assert!(!serialized.contains("route-public-value"));
    }

    #[tokio::test]
    async fn rename_moves_only_the_named_secret_and_returns_no_value() {
        let app = crate::web::build_router(testutil::test_state());
        put_plain_secret(app.clone(), "source", "rename-secret-value").await;

        let (status, renamed) = get_json(
            app.clone(),
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renamed["original_name"], "destination");
        assert!(renamed["value"].is_null());
        assert!(!renamed.to_string().contains("rename-secret-value"));

        let (source_status, _) = get_json(app.clone(), "GET", "/api/secrets/source", None).await;
        let (destination_status, _) = get_json(app, "GET", "/api/secrets/destination", None).await;
        assert_eq!(source_status, StatusCode::NOT_FOUND);
        assert_eq!(destination_status, StatusCode::OK);
    }

    #[tokio::test]
    async fn rename_rejects_collision_noop_and_unrelated_metadata_on_name_field() {
        let app = crate::web::build_router(testutil::test_state());
        put_plain_secret(app.clone(), "source", "source-value").await;
        put_plain_secret(app.clone(), "destination", "destination-value").await;

        for (body, expected_status) in [
            (json!({"new_name":"destination"}), StatusCode::CONFLICT),
            (json!({"new_name":"source"}), StatusCode::BAD_REQUEST),
            (
                json!({"new_name":"other","note":"unrelated"}),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let (status, error) = get_json(
                app.clone(),
                "POST",
                "/api/secrets/source/rename",
                Some(body),
            )
            .await;
            assert_eq!(status, expected_status);
            assert_eq!(error["error"]["field"], "name");
            assert!(!error.to_string().contains("source-value"));
            assert!(!error.to_string().contains("destination-value"));
        }

        let (_, source) = get_json(app.clone(), "POST", "/api/secrets/source/value", None).await;
        let (_, destination) = get_json(app, "POST", "/api/secrets/destination/value", None).await;
        assert!(source.to_string().contains("source-value"));
        assert!(destination.to_string().contains("destination-value"));
    }

    #[tokio::test]
    async fn rename_source_revision_and_destination_conflicts_have_distinct_codes() {
        let source_race_backend = Arc::new(testutil::stub::StubBackend::with_rename_source_race(
            "rename-source-race",
        ));
        let source_race_app = crate::web::build_router(state_with_types(
            source_race_backend,
            crate::records::builtin_types(),
        ));
        put_plain_secret(source_race_app.clone(), "source", "value").await;
        let (source_status, source_error) = get_json(
            source_race_app,
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;
        assert_eq!(source_status, StatusCode::CONFLICT);
        assert_eq!(source_error["error"]["code"], "xv-rename-source-changed");
        assert_eq!(source_error["error"]["field"], "source_revision");

        let destination_app = crate::web::build_router(testutil::test_state());
        put_plain_secret(destination_app.clone(), "source", "source-value").await;
        put_plain_secret(destination_app.clone(), "destination", "destination-value").await;
        let (destination_status, destination_error) = get_json(
            destination_app,
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;
        assert_eq!(destination_status, StatusCode::CONFLICT);
        assert_eq!(
            destination_error["error"]["code"],
            "xv-rename-destination-exists"
        );
        assert_eq!(destination_error["error"]["field"], "name");
    }

    #[tokio::test]
    async fn rename_missing_source_and_atomic_backend_failure_are_safe() {
        let (status, missing) = get_json(
            crate::web::build_router(testutil::test_state()),
            "POST",
            "/api/secrets/missing/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(missing["error"]["code"], "xv-secret-not-found");

        let backend = Arc::new(testutil::stub::StubBackend::with_delete_error(
            "partial",
            "sensitive backend failure",
        ));
        let backend_trait: Arc<dyn crate::backend::Backend> = backend.clone();
        let context = testutil::test_context(backend_trait.as_ref(), "vault", 30);
        let root = tempfile::tempdir().unwrap();
        let state = Arc::new(crate::web::WebState::new(
            backend_trait.clone(),
            context,
            "test-token".into(),
            crate::records::builtin_types(),
            crate::web::preferences::PreferenceStore::new(root.path().join("ui.json"), 30),
            Arc::new(crate::backend::BackendRegistry::new(backend_trait)),
        ));
        let app = crate::web::build_router(state);
        put_plain_secret(app.clone(), "source", "partial-secret-value").await;

        let (status, partial) = get_json(
            app,
            "POST",
            "/api/secrets/source/rename",
            Some(json!({"new_name":"destination"})),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(partial["error"]["code"], "xv-backend-internal");
        assert!(!partial.to_string().contains("sensitive backend failure"));
        assert!(!partial.to_string().contains("partial-secret-value"));
        assert!(backend.secrets.lock().unwrap().contains_key("source"));
        assert!(!backend.secrets.lock().unwrap().contains_key("destination"));
    }

    #[tokio::test]
    async fn concurrent_rename_tabs_keep_their_exact_attached_workspace_targets() {
        let primary = atomic_stub("primary");
        let stage = atomic_stub("stage");
        let app = crate::web::build_router(scoped_state(primary.clone(), stage.clone()));
        for scope in [
            "?alias=work&backend=primary&vault=payments",
            "?alias=stage&backend=stage&vault=sandbox",
        ] {
            let (status, _) = get_json(
                app.clone(),
                "PUT",
                &format!("/api/secrets/source{scope}"),
                Some(json!({"value":"tab-secret-value"})),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }

        let primary_tab = tokio::spawn(get_json(
            app.clone(),
            "POST",
            "/api/secrets/source/rename?alias=work&backend=primary&vault=payments",
            Some(json!({"new_name":"primary-name"})),
        ));
        let stage_tab = tokio::spawn(get_json(
            app.clone(),
            "POST",
            "/api/secrets/source/rename?alias=stage&backend=stage&vault=sandbox",
            Some(json!({"new_name":"stage-name"})),
        ));
        let ((primary_status, _), (stage_status, _)) =
            tokio::try_join!(primary_tab, stage_tab).unwrap();
        assert_eq!(primary_status, StatusCode::OK);
        assert_eq!(stage_status, StatusCode::OK);
        assert!(primary.secrets.lock().unwrap().contains_key("primary-name"));
        assert!(!primary.secrets.lock().unwrap().contains_key("stage-name"));
        assert!(stage.secrets.lock().unwrap().contains_key("stage-name"));
        assert!(!stage.secrets.lock().unwrap().contains_key("primary-name"));

        let (_, base) = get_json(app, "GET", "/api/context", None).await;
        assert_eq!(base["backend"], "primary");
        assert_eq!(base["vault"], "payments");
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
