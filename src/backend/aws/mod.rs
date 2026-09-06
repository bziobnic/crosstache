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
    endpoint_url: Option<String>,
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
                    .with_transfer_config(transfer.chunk_size_mb, transfer.max_concurrent_uploads)
                    .with_service_endpoint(configured_service_endpoint(&sdk_config, "S3")),
            )
        });
        // Without file storage there is nothing to transfer; touch both fields
        // so they don't read as dead when the `file-ops` reader is compiled out.
        #[cfg(not(feature = "file-ops"))]
        let _ = (transfer.chunk_size_mb, transfer.max_concurrent_uploads);

        Ok(Self {
            endpoint_url: configured_service_endpoint(&sdk_config, "Secrets Manager"),
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
    async fn validate_transfer_recovery_path(
        &self,
        _vault: &str,
        _path: &std::path::Path,
    ) -> Result<(), BackendError> {
        Ok(())
    }

    async fn transfer_secret_namespace(&self, vault: &str) -> Result<String, BackendError> {
        let marker = self
            .secrets_impl
            .client
            .describe_secret()
            .secret_id(encoding::marker_name(vault))
            .send()
            .await
            .map_err(|error| errors::from_describe(vault, error))?;
        let namespace = secret_namespace_from_marker(vault, &marker)?;
        // Custom service instances can issue identical synthetic ARNs.
        match self.endpoint_url.as_deref() {
            Some(endpoint) => {
                if standard_transfer_endpoint(endpoint, "secretsmanager").is_none() {
                    return Err(BackendError::Unsupported("transfer cannot prove physical identity for custom AWS secret endpoints; use a standard AWS endpoint".into()));
                }
                Ok(namespace)
            }
            None => Ok(namespace),
        }
    }

    async fn transfer_location(
        &self,
        vault: &str,
    ) -> Result<crate::backend::TransferLocation, BackendError> {
        let secrets = self.transfer_secret_namespace(vault).await?;
        #[cfg(feature = "file-ops")]
        {
            let files = self
                .files_impl
                .as_ref()
                .ok_or_else(|| {
                    BackendError::Unsupported("AWS transfer requires configured S3 storage".into())
                })?
                .transfer_namespace(vault)?;
            Ok(crate::backend::TransferLocation {
                keys: secrets.clone(),
                secrets,
                files,
            })
        }
        #[cfg(not(feature = "file-ops"))]
        {
            let _ = secrets;
            Err(BackendError::Unsupported(
                "AWS file operations unavailable".into(),
            ))
        }
    }

    fn name(&self) -> &'static str {
        "aws"
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Aws
    }

    fn capabilities(&self) -> BackendCapabilities {
        let has_file_storage = {
            #[cfg(feature = "file-ops")]
            {
                self.files_impl.is_some()
            }
            #[cfg(not(feature = "file-ops"))]
            {
                false
            }
        };
        aws_capabilities(has_file_storage)
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

fn aws_capabilities(has_file_storage: bool) -> BackendCapabilities {
    BackendCapabilities {
        has_atomic_record_conversion: false,
        has_conditional_record_conversion: false,
        has_atomic_rename: false,
        has_atomic_file_create: has_file_storage,
        has_enable_disable: false,
        has_vaults: true,
        has_file_storage,
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

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn aws_explicitly_rejects_atomic_conversion_and_enable_updates() {
        let capabilities = aws_capabilities(false);

        assert!(!capabilities.has_atomic_record_conversion);
        assert!(!capabilities.has_conditional_record_conversion);
        assert!(!capabilities.has_atomic_rename);
        assert!(!capabilities.has_atomic_file_create);
        assert!(!capabilities.has_enable_disable);
    }
}

fn secret_namespace_from_marker(
    vault: &str,
    marker: &aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput,
) -> Result<String, BackendError> {
    let invalid = || {
        BackendError::Unsupported(
            "cannot establish AWS physical vault namespace from marker".into(),
        )
    };
    let expected = encoding::marker_name(vault);
    if marker.name() != Some(expected.as_str())
        || marker.deleted_date().is_some()
        || !marker.tags().iter().any(|tag| {
            tag.key() == Some(metadata::TAG_TYPE)
                && tag.value() == Some(metadata::TAG_VALUE_VAULT_MARKER)
        })
    {
        return Err(invalid());
    }
    let parts: Vec<_> = marker.arn().ok_or_else(invalid)?.splitn(7, ':').collect();
    if parts.len() != 7
        || parts[0] != "arn"
        || parts[2] != "secretsmanager"
        || parts[3].is_empty()
        || parts[4].len() != 12
        || !parts[4].bytes().all(|b| b.is_ascii_digit())
        || parts[5] != "secret"
        || !matches!(parts[1], "aws" | "aws-cn" | "aws-us-gov")
    {
        return Err(invalid());
    }
    let suffix = parts[6]
        .strip_prefix(&format!("{expected}-"))
        .ok_or_else(invalid)?;
    if suffix.len() != 6 || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(invalid());
    }
    Ok(format!(
        "aws-secrets:{}:{}:{}:{vault}",
        parts[1], parts[3], parts[4]
    ))
}

#[cfg(test)]
mod transfer_namespace_tests {
    use super::*;
    #[test]
    fn aws_transfer_endpoint_uses_sdk_service_specific_precedence() {
        #[derive(Debug)]
        struct Services;
        impl aws_types::service_config::LoadServiceConfig for Services {
            fn load_config(
                &self,
                key: aws_types::service_config::ServiceConfigKey<'_>,
            ) -> Option<String> {
                assert_eq!(key.env(), "AWS_ENDPOINT_URL");
                assert_eq!(key.profile(), "endpoint_url");
                Some(format!(
                    "https://{}.example.test/",
                    key.service_id().replace(' ', "").to_ascii_lowercase()
                ))
            }
        }
        let config = aws_config::SdkConfig::builder()
            .service_config(Services)
            .build();
        assert_eq!(
            configured_service_endpoint(&config, "S3").as_deref(),
            Some("https://s3.example.test/")
        );
        assert_eq!(
            configured_service_endpoint(&config, "Secrets Manager").as_deref(),
            Some("https://secretsmanager.example.test/")
        );
        assert_eq!(
            standard_transfer_endpoint("https://s3.us-east-1.amazonaws.com/", "s3"),
            Some("aws")
        );
        assert_eq!(
            standard_transfer_endpoint(
                "https://secretsmanager.cn-north-1.amazonaws.com.cn/",
                "secretsmanager"
            ),
            Some("aws-cn")
        );
        assert_eq!(
            standard_transfer_endpoint("https://custom.example/", "secretsmanager"),
            None
        );
    }

    #[test]
    fn aws_transfer_namespace_binds_account_region_and_validated_marker() {
        use aws_sdk_secretsmanager::operation::describe_secret::DescribeSecretOutput;
        use aws_sdk_secretsmanager::types::Tag;
        let marker = |region: &str, account: &str| {
            DescribeSecretOutput::builder()
                .arn(format!(
                    "arn:aws:secretsmanager:{region}:{account}:secret:prod/.xv-vault-abcdef"
                ))
                .name("prod/.xv-vault")
                .tags(Tag::builder().key("xv:type").value("vault-marker").build())
                .build()
        };
        let original =
            secret_namespace_from_marker("prod", &marker("us-east-1", "123456789012")).unwrap();
        assert_ne!(
            original,
            secret_namespace_from_marker("prod", &marker("us-east-1", "123456789013")).unwrap()
        );
        assert_ne!(
            original,
            secret_namespace_from_marker("prod", &marker("us-west-2", "123456789012")).unwrap()
        );
        assert!(
            secret_namespace_from_marker("other", &marker("us-east-1", "123456789012")).is_err()
        );
    }
}

#[cfg(feature = "file-ops")]
fn canonical_transfer_endpoint(endpoint: &str) -> Result<String, BackendError> {
    let mut url = url::Url::parse(endpoint)
        .map_err(|_| BackendError::Unsupported("invalid custom AWS transfer endpoint".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BackendError::Unsupported(
            "ambiguous custom AWS transfer endpoint".into(),
        ));
    }
    if url.path().is_empty() {
        url.set_path("/");
    }
    if url.host_str().is_some_and(|host| {
        host.ends_with(".amazonaws.com")
            || host.ends_with(".amazonaws.com.cn")
            || host.ends_with(".api.aws")
            || host.ends_with(".api.amazonwebservices.com.cn")
    }) {
        return Err(BackendError::Unsupported(
            "unrecognized AWS service endpoint alias has unproven physical identity".into(),
        ));
    }
    Ok(url.to_string())
}

fn standard_transfer_endpoint(endpoint: &str, service: &str) -> Option<&'static str> {
    let url = url::Url::parse(endpoint).ok()?;
    if !(url.scheme() == "https" || (service == "s3" && url.scheme() == "http"))
        || url.port().is_some()
        || url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let (prefix, china) = if let Some(prefix) = host.strip_suffix(".amazonaws.com.cn") {
        (prefix, true)
    } else {
        (host.strip_suffix(".amazonaws.com")?, false)
    };
    let mut components = prefix.split('.');
    let endpoint_service = components.next()?;
    if endpoint_service != service && !(service == "s3" && endpoint_service == "s3-fips") {
        return None;
    }
    let mut region = components.next();
    if service == "s3" && region == Some("dualstack") {
        region = components.next();
    }
    if components.next().is_some() {
        return None;
    }
    match region {
        None if service == "s3" && !china => Some("aws"),
        Some(region) if region.starts_with("cn-") && china => Some("aws-cn"),
        Some(region) if region.starts_with("us-gov-") && !china => Some("aws-us-gov"),
        Some(region) if !china && region.contains('-') => Some("aws"),
        _ => None,
    }
}

/// Match the pinned SDK Builder::from(SdkConfig) endpoint precedence, including
/// service-specific environment/profile settings rather than only global config.
fn configured_service_endpoint(config: &aws_config::SdkConfig, service: &str) -> Option<String> {
    if config.get_origin("endpoint_url").is_client_config() {
        return config.endpoint_url().map(str::to_string);
    }
    config
        .service_config()
        .and_then(|loader| {
            loader.load_config(
                aws_types::service_config::ServiceConfigKey::builder()
                    .service_id(service)
                    .env("AWS_ENDPOINT_URL")
                    .profile("endpoint_url")
                    .build()
                    .expect("static endpoint key fields are present"),
            )
        })
        .or_else(|| config.endpoint_url().map(str::to_string))
}
