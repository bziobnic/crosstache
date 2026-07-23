//! AWS Secrets Manager backend.

pub mod audit;
pub mod auth;
pub mod config;
pub mod encoding;
pub mod errors;
#[cfg(feature = "file-ops")]
pub mod files;
pub mod metadata;
pub mod models;
pub mod secrets;
pub mod vaults;

use std::sync::Arc;

use crate::backend::error::BackendError;
use crate::backend::{
    AuditBackend, Backend, BackendCapabilities, BackendKind, NameCharset, SecretBackend,
    VaultBackend,
};
use crate::config::settings::AwsConfig;
use aws_sdk_cloudtrail::Client as CloudTrailClient;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;

#[cfg(feature = "file-ops")]
use crate::backend::FileBackend;

/// Blob transfer settings threaded into S3 file storage (chunk size and upload
/// concurrency). Sourced from the global `[blob]` config so `xv file` on AWS
/// honors `BLOB_CHUNK_SIZE_MB` / `BLOB_MAX_CONCURRENT_UPLOADS`; named AWS
/// entries fall back to [`TransferConfig::default`].
#[derive(Debug, Clone, Copy)]
pub struct TransferConfig {
    pub chunk_size_mb: usize,
    pub max_concurrent_uploads: usize,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            chunk_size_mb: 4,
            max_concurrent_uploads: 3,
        }
    }
}

pub struct AwsBackend {
    secrets_impl: Arc<secrets::AwsSecretBackend>,
    vaults_impl: Arc<vaults::AwsVaultBackend>,
    audit_impl: Arc<audit::AwsAuditBackend>,
    /// S3 file storage — present only when an S3 bucket is configured.
    #[cfg(feature = "file-ops")]
    files_impl: Option<Arc<files::AwsFileBackend>>,
}

impl AwsBackend {
    /// Build a backend from config + per-invocation overrides.
    /// Async because `aws-config::load()` is async.
    pub async fn new(
        aws_cfg: &AwsConfig,
        region_override: Option<String>,
        profile_override: Option<String>,
        transfer: TransferConfig,
    ) -> Result<Self, BackendError> {
        let sdk_config = auth::load_sdk_config(aws_cfg, region_override, profile_override).await?;
        let client = Arc::new(SecretsManagerClient::new(&sdk_config));
        let cloudtrail = Arc::new(CloudTrailClient::new(&sdk_config));

        // File storage is optional: it requires an S3 bucket to be
        // configured. No bucket -> capability stays off.
        #[cfg(feature = "file-ops")]
        let files_impl = files::resolve_bucket(aws_cfg).ok().map(|bucket| {
            let s3_client = auth::build_s3_client(aws_cfg, &sdk_config);
            Arc::new(
                files::AwsFileBackend::new(s3_client, bucket)
                    .with_transfer_config(transfer.chunk_size_mb, transfer.max_concurrent_uploads),
            )
        });
        // Without file storage there is nothing to transfer; touch both fields
        // so they don't read as dead when the `file-ops` reader is compiled out.
        #[cfg(not(feature = "file-ops"))]
        let _ = (transfer.chunk_size_mb, transfer.max_concurrent_uploads);

        Ok(Self {
            secrets_impl: Arc::new(secrets::AwsSecretBackend::new(client.clone())),
            vaults_impl: Arc::new(vaults::AwsVaultBackend::new(client)),
            audit_impl: Arc::new(audit::AwsAuditBackend::new(cloudtrail)),
            #[cfg(feature = "file-ops")]
            files_impl,
        })
    }
}

#[async_trait::async_trait]
impl Backend for AwsBackend {
    fn name(&self) -> &'static str {
        "aws"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Aws
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            has_vaults: true,
            has_file_storage: {
                #[cfg(feature = "file-ops")]
                {
                    self.files_impl.is_some()
                }
                #[cfg(not(feature = "file-ops"))]
                {
                    false
                }
            },
            has_rbac: false,
            has_audit: true,
            has_versioning: true,
            has_soft_delete: true,
            has_restore: true,
            has_purge: true,
            has_scheduled_purge: true,
            has_secret_rotation: true,
            has_groups: true,
            has_folders: true,
            has_notes: true,
            has_expiry: true,
            max_secret_size: Some(65_536),
            max_name_length: Some(encoding::MAX_NAME_LEN),
            name_charset: NameCharset::AwsRelaxed,
            max_tags: Some(50),
            max_tag_value_len: Some(256),
        }
    }

    fn secrets(&self) -> &dyn SecretBackend {
        self.secrets_impl.as_ref()
    }

    fn vaults(&self) -> Option<&dyn VaultBackend> {
        Some(self.vaults_impl.as_ref())
    }

    fn audit(&self) -> Option<&dyn AuditBackend> {
        Some(self.audit_impl.as_ref())
    }

    #[cfg(feature = "file-ops")]
    fn files(&self) -> Option<&dyn FileBackend> {
        self.files_impl.as_deref().map(|fb| fb as &dyn FileBackend)
    }

    async fn health_check(&self) -> Result<(), BackendError> {
        self.secrets_impl.health_check().await
    }
}
