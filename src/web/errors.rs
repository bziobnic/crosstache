//! Stable, safe error envelopes for the embedded web API.

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::Value;

use crate::backend::error::BackendError;
use crate::error::CrosstacheError;

#[derive(Serialize)]
pub(crate) struct ApiErrorBody {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) hint: &'static str,
    pub(crate) field: Option<&'static str>,
    pub(crate) details: Option<Value>,
}

#[derive(Serialize)]
pub(crate) struct ApiErrorEnvelope {
    pub(crate) error: ApiErrorBody,
}

impl ApiErrorBody {
    pub(crate) fn validation(message: &'static str, field: Option<&'static str>) -> Self {
        Self {
            code: "xv-invalid-argument",
            message: message.into(),
            hint: "Correct the highlighted field and try again.",
            field,
            details: None,
        }
    }
}

pub(crate) fn crosstache_error(error: CrosstacheError) -> (StatusCode, ApiErrorBody) {
    use CrosstacheError::*;

    let code = error.code();
    let status = match &error {
        SecretNotFound { .. } | VaultNotFound { .. } => StatusCode::NOT_FOUND,
        PermissionDenied(_) => StatusCode::FORBIDDEN,
        AuthenticationError(_) => StatusCode::UNAUTHORIZED,
        Conflict(_) => StatusCode::CONFLICT,
        RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
        InvalidArgument(_) | InvalidUrl(_) => StatusCode::BAD_REQUEST,
        BackendUnavailable { .. }
        | NetworkError(_)
        | DnsResolutionError { .. }
        | ConnectionTimeout(_)
        | ConnectionRefused(_)
        | SslError(_)
        | AzureApiError(_) => StatusCode::BAD_GATEWAY,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let (message, hint) = match error {
        AuthenticationError(_) => (
            "Authentication failed.".into(),
            "Check your session and try again.",
        ),
        AzureApiError(_) => (
            "The backend service could not complete the request.".into(),
            "Try again shortly. If this continues, check the backend service.",
        ),
        Conflict(_) => (
            "The requested change conflicts with existing data.".into(),
            "Refresh the vault and choose a different name or retry the change.",
        ),
        RateLimited(_) => (
            "The backend is rate limiting requests.".into(),
            "Wait a moment, then try again.",
        ),
        ConfigError(_) | ConfigLoadError(_) => (
            "The application configuration is invalid.".into(),
            "Check the application configuration and try again.",
        ),
        BackendUnavailable { .. } => (
            "The selected backend is unavailable.".into(),
            "Check the backend configuration and connection, then retry.",
        ),
        SecretNotFound { name, .. } => (
            format!("Secret '{name}' was not found."),
            "Refresh the vault or choose another secret.",
        ),
        VaultNotFound { name, .. } => (
            format!("Vault '{name}' was not found."),
            "Refresh the vault list or choose another vault.",
        ),
        EnvNotDefined { .. } => (
            "The selected environment is not defined.".into(),
            "Choose a configured environment and try again.",
        ),
        PermissionDenied(_) => (
            "You do not have permission for this action.".into(),
            "Check your backend role or choose a permitted vault.",
        ),
        NetworkError(_)
        | DnsResolutionError { .. }
        | ConnectionTimeout(_)
        | ConnectionRefused(_)
        | SslError(_) => (
            "Unable to reach the backend service.".into(),
            "Check your network connection and backend address, then retry.",
        ),
        InvalidUrl(_) => (
            "The request contains an invalid address.".into(),
            "Check the backend address and try again.",
        ),
        SerializationError(_)
        | IoError(_)
        | JsonError(_)
        | YamlError(_)
        | HttpError(_)
        | UuidError(_)
        | RegexError(_)
        | Upgrade(_)
        | Unknown(_) => (
            "The request could not be completed.".into(),
            "Try again. If the problem continues, check the application logs.",
        ),
        InvalidArgument(_) => (
            "The request contains invalid data.".into(),
            "Correct the request and try again.",
        ),
        ScanLeakDetected { .. } => (
            "The operation was blocked by a security finding.".into(),
            "Review the finding and remove the sensitive value before retrying.",
        ),
        RenameIncomplete { .. } => (
            "The secret was renamed, but the original could not be removed.".into(),
            "Refresh the vault and verify both secrets before retrying deletion.",
        ),
        AmbiguousSecret { .. } => (
            "The secret name is ambiguous.".into(),
            "Choose a unique secret name and try again.",
        ),
    };

    (
        status,
        ApiErrorBody {
            code,
            message,
            hint,
            field: None,
            details: None,
        },
    )
}

pub(crate) fn backend_error(error: BackendError) -> (StatusCode, ApiErrorBody) {
    use BackendError::*;

    match error {
        NotFound { name, .. } => (
            StatusCode::NOT_FOUND,
            ApiErrorBody {
                code: "xv-secret-not-found",
                message: format!("Secret '{name}' was not found."),
                hint: "Refresh the vault or choose another secret.",
                field: None,
                details: None,
            },
        ),
        VaultNotFound { name, .. } => (
            StatusCode::NOT_FOUND,
            ApiErrorBody {
                code: "xv-vault-not-found",
                message: format!("Vault '{name}' was not found."),
                hint: "Refresh the vault list or choose another vault.",
                field: None,
                details: None,
            },
        ),
        AuthenticationFailed(_) => generic(
            StatusCode::UNAUTHORIZED,
            "xv-auth-failed",
            "Authentication failed.",
            "Check your session and try again.",
        ),
        PermissionDenied(_) => generic(
            StatusCode::FORBIDDEN,
            "xv-permission-denied",
            "You do not have permission for this action.",
            "Check your backend role or choose a permitted vault.",
        ),
        Unsupported(_) => generic(
            StatusCode::NOT_IMPLEMENTED,
            "xv-operation-unsupported",
            "This backend does not support that action.",
            "Choose a supported action or backend and try again.",
        ),
        InvalidArgument(_) => generic(
            StatusCode::BAD_REQUEST,
            "xv-invalid-argument",
            "The request contains invalid data.",
            "Correct the request and try again.",
        ),
        Conflict(_) => generic(
            StatusCode::CONFLICT,
            "xv-conflict",
            "The requested change conflicts with existing data.",
            "Refresh the vault and choose a different name or retry the change.",
        ),
        RateLimited { .. } => generic(
            StatusCode::TOO_MANY_REQUESTS,
            "xv-rate-limited",
            "The backend is rate limiting requests.",
            "Wait a moment, then try again.",
        ),
        Network(_) => generic(
            StatusCode::BAD_GATEWAY,
            "xv-network",
            "Unable to reach the backend service.",
            "Check your network connection and backend address, then retry.",
        ),
        RenameIncomplete { .. } => generic(
            StatusCode::INTERNAL_SERVER_ERROR,
            "xv-rename-incomplete",
            "The secret was renamed, but the original could not be removed.",
            "Refresh the vault and verify both secrets before retrying deletion.",
        ),
        Internal(_) | Other(_) => generic(
            StatusCode::INTERNAL_SERVER_ERROR,
            "xv-backend-internal",
            "The backend could not complete the request.",
            "Try again. If the problem continues, check the application logs.",
        ),
    }
}

fn generic(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    hint: &'static str,
) -> (StatusCode, ApiErrorBody) {
    (
        status,
        ApiErrorBody {
            code,
            message: message.into(),
            hint,
            field: None,
            details: None,
        },
    )
}
