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
use aws_sdk_secretsmanager::operation::tag_resource::TagResourceError;
use aws_sdk_secretsmanager::operation::untag_resource::UntagResourceError;
use aws_sdk_secretsmanager::operation::update_secret::UpdateSecretError;
use aws_sdk_secretsmanager::operation::update_secret_version_stage::UpdateSecretVersionStageError;
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_runtime_api::http::Response;

fn generic<E: std::fmt::Display>(op: &str, e: E) -> BackendError {
    BackendError::Internal(format!("aws {op}: {e}"))
}

fn handle_sdk<E: std::fmt::Display + std::fmt::Debug, R: std::fmt::Debug>(
    op: &str,
    e: SdkError<E, R>,
) -> BackendError {
    match e {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) => {
            BackendError::Network(format!("aws {op}: timeout or dispatch failure"))
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
