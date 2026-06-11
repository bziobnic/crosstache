//! AWS SDK error -> BackendError mapping.
//!
//! The AWS SDK exposes per-operation error enums (e.g. `GetSecretValueError`).
//! We provide free functions for each operation we use, so call sites stay
//! readable and the mapping is exhaustive over the variants we observe.

use crate::backend::error::BackendError;
use aws_sdk_secretsmanager::operation::create_secret::CreateSecretError;
use aws_sdk_secretsmanager::operation::delete_secret::DeleteSecretError;
use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretError;
use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueError;
use aws_sdk_secretsmanager::operation::list_secret_version_ids::ListSecretVersionIdsError;
use aws_sdk_secretsmanager::operation::list_secrets::ListSecretsError;
use aws_sdk_secretsmanager::operation::put_secret_value::PutSecretValueError;
use aws_sdk_secretsmanager::operation::restore_secret::RestoreSecretError;
use aws_sdk_secretsmanager::operation::rotate_secret::RotateSecretError;
use aws_sdk_secretsmanager::operation::tag_resource::TagResourceError;
use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceError;
use aws_sdk_secretsmanager::operation::update_secret::UpdateSecretError;
use aws_sdk_secretsmanager::operation::update_secret_version_stage::UpdateSecretVersionStageError;
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_runtime_api::http::Response;

fn generic<E: std::fmt::Display>(op: &str, e: E) -> BackendError {
    BackendError::Internal(format!("aws {op}: {e}"))
}

/// Build the standard credential-remediation hint shown when the AWS SDK
/// cannot resolve credentials. Centralised so every code path (per-operation
/// SdkError handlers, the lightweight `health_check`, and any future
/// non-SdkError wrappers) prints the same actionable text.
pub(crate) fn aws_credentials_hint(op: &str) -> String {
    format!(
        "No AWS credentials resolved (operation: {op}). \
Try `aws configure`, set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, \
or run `eval \"$(aws configure export-credentials --format env)\"` after `aws login`."
    )
}

/// Walk an error's source chain looking for tell-tale strings produced by the
/// AWS credential providers (e.g. `CredentialsNotLoaded`, "no credentials",
/// "ProviderError", "credentials provider"). Used as a defensive fallback for
/// SdkError variants — notably `ConstructionFailure` and edge-case
/// `DispatchFailure`s where `is_user()` may be false but the underlying cause
/// is still credential resolution.
fn looks_like_credential_error(err: &(dyn std::error::Error + 'static)) -> bool {
    fn matches(s: &str) -> bool {
        let lower = s.to_ascii_lowercase();
        lower.contains("credentialsnotloaded")
            || lower.contains("no credentials")
            || lower.contains("credentials not loaded")
            || lower.contains("credentials provider")
            || lower.contains("providererror")
            || lower.contains("provider error")
            || lower.contains("no credentials in chain")
            || lower.contains("failed to load credentials")
    }
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    let mut depth = 0;
    while let Some(e) = cur {
        if matches(&e.to_string()) {
            return true;
        }
        depth += 1;
        if depth > 16 {
            break;
        }
        cur = e.source();
    }
    false
}

fn handle_sdk<E: std::fmt::Display + std::fmt::Debug, R: std::fmt::Debug>(
    op: &str,
    e: SdkError<E, R>,
) -> BackendError {
    match e {
        SdkError::TimeoutError(_) => BackendError::Network(format!("aws {op}: timeout")),
        SdkError::DispatchFailure(ref df) if df.is_user() => {
            // Credential resolution failure — not a network error.
            BackendError::AuthenticationFailed(aws_credentials_hint(op))
        }
        SdkError::DispatchFailure(ref df)
            if df
                .as_connector_error()
                .map(|c| looks_like_credential_error(c))
                .unwrap_or(false) =>
        {
            // Defensive: some SDK versions/transports route credential
            // failures through DispatchFailure without flagging `is_user()`.
            BackendError::AuthenticationFailed(aws_credentials_hint(op))
        }
        SdkError::DispatchFailure(_) => {
            BackendError::Network(format!("aws {op}: dispatch failure"))
        }
        SdkError::ConstructionFailure(ref _cf)
            if format!("{e:?}").to_ascii_lowercase().contains("credential") =>
        {
            // Credential providers can fail before the request is dispatched
            // (e.g. missing profile, invalid IMDS response), surfacing as a
            // ConstructionFailure. Classify as auth so users get the hint
            // instead of an opaque "Internal" error.
            BackendError::AuthenticationFailed(aws_credentials_hint(op))
        }
        SdkError::ServiceError(svc) => BackendError::Internal(format!("aws {op}: {}", svc.err())),
        other => generic(op, format!("{other:?}")),
    }
}

pub fn from_create(e: SdkError<CreateSecretError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        match svc.err() {
            CreateSecretError::ResourceExistsException(inner) => {
                return BackendError::Conflict(inner.to_string())
            }
            CreateSecretError::InvalidRequestException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            CreateSecretError::InvalidParameterException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            CreateSecretError::LimitExceededException(_) => {
                return BackendError::RateLimited {
                    retry_after_secs: None,
                }
            }
            _ => {}
        }
    }
    handle_sdk("CreateSecret", e)
}

pub fn from_get_value(name: &str, e: SdkError<GetSecretValueError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        match svc.err() {
            GetSecretValueError::ResourceNotFoundException(_) => {
                return BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }
            }
            GetSecretValueError::DecryptionFailure(inner) => {
                return BackendError::Internal(format!("decryption failed: {inner}"))
            }
            GetSecretValueError::InvalidRequestException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            _ => {}
        }
    }
    handle_sdk("GetSecretValue", e)
}

pub fn from_describe(name: &str, e: SdkError<DescribeSecretError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let DescribeSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("DescribeSecret", e)
}

pub fn from_list(e: SdkError<ListSecretsError, Response>) -> BackendError {
    handle_sdk("ListSecrets", e)
}

pub fn from_delete(name: &str, e: SdkError<DeleteSecretError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let DeleteSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("DeleteSecret", e)
}

pub fn from_put_value(name: &str, e: SdkError<PutSecretValueError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let PutSecretValueError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("PutSecretValue", e)
}

pub fn from_update(name: &str, e: SdkError<UpdateSecretError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let UpdateSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("UpdateSecret", e)
}

pub fn from_restore(name: &str, e: SdkError<RestoreSecretError, Response>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        match svc.err() {
            RestoreSecretError::ResourceNotFoundException(_) => {
                return BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }
            }
            RestoreSecretError::InvalidRequestException(inner) => {
                // Typically means the secret is not scheduled for deletion.
                return BackendError::InvalidArgument(format!(
                    "Secret is not scheduled for deletion: {inner}"
                ));
            }
            _ => {}
        }
    }
    handle_sdk("RestoreSecret", e)
}

pub fn from_list_versions(
    name: &str,
    e: SdkError<ListSecretVersionIdsError, Response>,
) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let ListSecretVersionIdsError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("ListSecretVersionIds", e)
}

pub fn from_update_stage(
    name: &str,
    e: SdkError<UpdateSecretVersionStageError, Response>,
) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let UpdateSecretVersionStageError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("UpdateSecretVersionStage", e)
}

pub fn from_tag(e: SdkError<TagResourceError, Response>) -> BackendError {
    handle_sdk("TagResource", e)
}

pub fn from_untag(e: SdkError<UntagResourceError, Response>) -> BackendError {
    handle_sdk("UntagResource", e)
}

/// Remediation hint shown when `RotateSecret` is rejected because the secret
/// has no rotation Lambda configured (AWS reports this as
/// `InvalidRequestException`).
pub(crate) fn rotation_lambda_hint(secret_id: &str) -> String {
    format!(
        "No rotation Lambda appears to be configured for this secret. \
Configure one with `aws secretsmanager rotate-secret --secret-id {secret_id} \
--rotation-lambda-arn <lambda-arn>`, then retry `xv rotate --native`."
    )
}

pub fn from_rotate(
    name: &str,
    secret_id: &str,
    e: SdkError<RotateSecretError, Response>,
) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        match svc.err() {
            RotateSecretError::ResourceNotFoundException(_) => {
                return BackendError::NotFound {
                    name: name.to_string(),
                    suggestion: None,
                }
            }
            RotateSecretError::InvalidRequestException(inner) => {
                // InvalidRequestException covers several failure modes
                // (secret deleted/being deleted, rotation already in
                // progress, no Lambda configured, ...). Only append the
                // configure-a-Lambda hint when the message actually says
                // no rotation Lambda is associated; otherwise surface the
                // real error untouched. (`message()` is an inherent method
                // on the modeled exception type; no trait import needed.)
                let msg = inner.message().unwrap_or_default().to_lowercase();
                let no_lambda = msg.contains("lambda")
                    && (msg.contains("no rotation")
                        || msg.contains("not configured")
                        || msg.contains("no lambda")
                        || msg.contains("rotation lambda arn"));
                return if no_lambda {
                    BackendError::InvalidArgument(format!(
                        "{inner}. {}",
                        rotation_lambda_hint(secret_id)
                    ))
                } else {
                    BackendError::InvalidArgument(inner.to_string())
                };
            }
            RotateSecretError::InvalidParameterException(inner) => {
                return BackendError::InvalidArgument(inner.to_string())
            }
            other => {
                // AccessDenied is not a modeled variant on RotateSecretError;
                // it arrives as an unmodeled service error. Classify by code
                // so users get a permissions message instead of "Internal".
                use aws_sdk_secretsmanager::error::ProvideErrorMetadata;
                if other.code() == Some("AccessDeniedException") {
                    return BackendError::PermissionDenied(format!(
                        "secretsmanager:RotateSecret denied for '{name}': {}",
                        other.message().unwrap_or("access denied")
                    ));
                }
            }
        }
    }
    handle_sdk("RotateSecret", e)
}

#[cfg(all(test, feature = "aws"))]
mod tests {
    use super::*;
    use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
    use aws_smithy_runtime_api::client::result::ConnectorError;

    fn make_user_dispatch_failure() -> SdkError<ListSecretsError, HttpResponse> {
        let inner = std::io::Error::other("no credentials in chain");
        SdkError::dispatch_failure(ConnectorError::user(Box::new(inner)))
    }

    fn make_timeout_error() -> SdkError<ListSecretsError, HttpResponse> {
        let inner = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        SdkError::dispatch_failure(ConnectorError::timeout(Box::new(inner)))
    }

    #[test]
    fn dispatch_failure_user_maps_to_auth_error() {
        let err = from_list(make_user_dispatch_failure());
        assert!(
            matches!(err, BackendError::AuthenticationFailed(_)),
            "expected AuthenticationFailed, got: {err:?}"
        );
        let BackendError::AuthenticationFailed(msg) = err else {
            unreachable!()
        };
        assert!(msg.contains("aws configure"), "hint missing: {msg}");
        assert!(msg.contains("AWS_ACCESS_KEY_ID"), "hint missing: {msg}");
    }

    #[test]
    fn dispatch_failure_timeout_maps_to_network() {
        let err = from_list(make_timeout_error());
        assert!(
            matches!(err, BackendError::Network(_)),
            "expected Network, got: {err:?}"
        );
    }

    /// Regression test for `docs/UX-REVIEW.md` §P0-2: a credential-resolution
    /// failure whose payload doesn't have `is_user()` set must still map to
    /// `AuthenticationFailed` so the user sees the `aws configure` hint
    /// instead of a "dispatch failure" / "network" message.
    #[test]
    fn dispatch_failure_with_credentials_string_maps_to_auth() {
        // Build a non-user DispatchFailure carrying an inner error whose
        // Display contains a credential-provider hint. `ConnectorError::io`
        // does not set the user flag.
        let inner = std::io::Error::other("CredentialsNotLoaded: failed to load credentials");
        let sdk: SdkError<ListSecretsError, HttpResponse> =
            SdkError::dispatch_failure(ConnectorError::io(Box::new(inner)));
        let err = from_list(sdk);
        assert!(
            matches!(err, BackendError::AuthenticationFailed(_)),
            "expected AuthenticationFailed, got: {err:?}"
        );
        let BackendError::AuthenticationFailed(msg) = err else {
            unreachable!()
        };
        assert!(msg.contains("aws configure"), "hint missing: {msg}");
    }

    /// Regression test: the `health_check` lightweight ping (used at backend
    /// construction time and by `xv list` precondition checks) used to map
    /// every SDK error to `BackendError::Network`, masking credential
    /// failures. It must now route through `from_list` and surface
    /// `AuthenticationFailed` for credential-resolution failures.
    #[tokio::test]
    async fn health_check_classifies_credential_errors_as_auth() {
        // We can't easily build a real SecretsManagerClient that fails in a
        // specific way without spinning up a mock, so instead we mirror the
        // call site by piping the same SdkError variants through `from_list`
        // (the mapper now used inside `health_check`) and asserting on the
        // classification. This is a contract test — `health_check`'s
        // production code path is `req.send().await.map_err(from_list)`, so
        // any mapper change is caught here.
        let err = from_list(make_user_dispatch_failure());
        assert!(
            matches!(err, BackendError::AuthenticationFailed(_)),
            "health_check would have returned: {err:?}"
        );
    }

    #[test]
    fn rotation_lambda_hint_includes_cli_remediation() {
        let hint = rotation_lambda_hint("myproj-kv/db-password");
        assert!(hint.contains("aws secretsmanager rotate-secret"), "{hint}");
        assert!(hint.contains("--secret-id myproj-kv/db-password"), "{hint}");
        assert!(hint.contains("--rotation-lambda-arn"), "{hint}");
    }
}
