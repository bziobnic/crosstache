//! AWS SDK error -> BackendError mapping.
//!
//! The AWS SDK exposes per-operation error enums (e.g. `GetSecretValueError`).
//! We provide free functions for each operation we use, so call sites stay
//! readable and the mapping is exhaustive over the variants we observe.

use crate::backend::error::BackendError;
use aws_sdk_secretsmanager::operation::create_secret::{CreateSecretError, CreateSecretOutput};
use aws_sdk_secretsmanager::operation::delete_secret::{DeleteSecretError, DeleteSecretOutput};
use aws_sdk_secretsmanager::operation::describe_secret::{DescribeSecretError, DescribeSecretOutput};
use aws_sdk_secretsmanager::operation::get_secret_value::{GetSecretValueError, GetSecretValueOutput};
use aws_sdk_secretsmanager::operation::list_secret_version_ids::{
    ListSecretVersionIdsError, ListSecretVersionIdsOutput,
};
use aws_sdk_secretsmanager::operation::list_secrets::{ListSecretsError, ListSecretsOutput};
use aws_sdk_secretsmanager::operation::put_secret_value::{PutSecretValueError, PutSecretValueOutput};
use aws_sdk_secretsmanager::operation::restore_secret::{RestoreSecretError, RestoreSecretOutput};
use aws_sdk_secretsmanager::operation::tag_resource::{TagResourceError, TagResourceOutput};
use aws_sdk_secretsmanager::operation::untag_resource::{UntagResourceError, UntagResourceOutput};
use aws_sdk_secretsmanager::operation::update_secret::{UpdateSecretError, UpdateSecretOutput};
use aws_sdk_secretsmanager::operation::update_secret_version_stage::{
    UpdateSecretVersionStageError, UpdateSecretVersionStageOutput,
};
use aws_smithy_runtime_api::client::result::SdkError;

fn generic<E: std::fmt::Display>(op: &str, e: E) -> BackendError {
    BackendError::Internal(format!("aws {op}: {e}"))
}

fn handle_sdk<E: std::fmt::Display + std::fmt::Debug, R: std::fmt::Debug>(op: &str, e: SdkError<E, R>) -> BackendError {
    match e {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) => {
            BackendError::Network(format!("aws {op}: timeout or dispatch failure"))
        }
        SdkError::ServiceError(svc) => {
            BackendError::Internal(format!("aws {op}: {}", svc.err()))
        }
        other => generic(op, format!("{other:?}")),
    }
}

pub fn from_create(e: SdkError<CreateSecretError, CreateSecretOutput>) -> BackendError {
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

pub fn from_get_value(name: &str, e: SdkError<GetSecretValueError, GetSecretValueOutput>) -> BackendError {
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

pub fn from_describe(name: &str, e: SdkError<DescribeSecretError, DescribeSecretOutput>) -> BackendError {
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

pub fn from_list(e: SdkError<ListSecretsError, ListSecretsOutput>) -> BackendError {
    handle_sdk("ListSecrets", e)
}

pub fn from_delete(name: &str, e: SdkError<DeleteSecretError, DeleteSecretOutput>) -> BackendError {
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

pub fn from_put_value(name: &str, e: SdkError<PutSecretValueError, PutSecretValueOutput>) -> BackendError {
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

pub fn from_update(name: &str, e: SdkError<UpdateSecretError, UpdateSecretOutput>) -> BackendError {
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

pub fn from_restore(name: &str, e: SdkError<RestoreSecretError, RestoreSecretOutput>) -> BackendError {
    if let SdkError::ServiceError(svc) = &e {
        if let RestoreSecretError::ResourceNotFoundException(_) = svc.err() {
            return BackendError::NotFound {
                name: name.to_string(),
                suggestion: None,
            };
        }
    }
    handle_sdk("RestoreSecret", e)
}

pub fn from_list_versions(
    name: &str,
    e: SdkError<ListSecretVersionIdsError, ListSecretVersionIdsOutput>,
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
    e: SdkError<UpdateSecretVersionStageError, UpdateSecretVersionStageOutput>,
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

pub fn from_tag(e: SdkError<TagResourceError, TagResourceOutput>) -> BackendError {
    handle_sdk("TagResource", e)
}

pub fn from_untag(e: SdkError<UntagResourceError, UntagResourceOutput>) -> BackendError {
    handle_sdk("UntagResource", e)
}
