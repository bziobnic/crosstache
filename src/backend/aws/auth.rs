//! AWS SDK config builder. Loads credentials via the aws-config default chain.
//!
//! No xv-specific credential priority abstraction (unlike Azure's
//! `--credential-priority`). AWS users have strong opinions about credential
//! resolution; the SDK chain is industry standard and we don't try to model it.

use crate::backend::aws::config::AwsConfig;
use crate::backend::error::BackendError;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;

/// Load the shared AWS `SdkConfig` from the resolved `AwsConfig` plus
/// per-invocation overrides (region, profile from CLI flags or env vars).
/// Service clients (Secrets Manager, CloudTrail, S3) are built from this
/// one config so they share credentials, region, and endpoint settings.
pub async fn load_sdk_config(
    aws_cfg: &AwsConfig,
    region_override: Option<String>,
    profile_override: Option<String>,
) -> Result<aws_config::SdkConfig, BackendError> {
    let region = region_override
        .or_else(|| aws_cfg.region.clone())
        .or_else(|| std::env::var("AWS_REGION").ok())
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .ok_or_else(|| {
            BackendError::AuthenticationFailed(
                "AWS region not set: provide [aws].region, AWS_REGION, or --region".into(),
            )
        })?;

    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region));

    let profile = profile_override.or_else(|| aws_cfg.profile.clone());
    if let Some(ref p) = profile {
        loader = loader.profile_name(p);
    }

    if let Some(ref endpoint) = aws_cfg.endpoint_url {
        if !endpoint.is_empty() {
            loader = loader.endpoint_url(endpoint);
        }
    }

    Ok(loader.load().await)
}

/// Build a `SecretsManagerClient` from the resolved `AwsConfig` plus
/// per-invocation overrides (region, profile from CLI flags or env vars).
#[allow(dead_code)] // Convenience wrapper retained for callers that need only Secrets Manager.
pub async fn build_client(
    aws_cfg: &AwsConfig,
    region_override: Option<String>,
    profile_override: Option<String>,
) -> Result<SecretsManagerClient, BackendError> {
    let sdk_config = load_sdk_config(aws_cfg, region_override, profile_override).await?;
    Ok(SecretsManagerClient::new(&sdk_config))
}

/// Build an S3 client for `xv file` storage from an already-loaded SDK config.
///
/// When an `endpoint_url` override is configured (LocalStack, MinIO, other
/// S3-compatible APIs), path-style addressing is forced because those
/// endpoints typically don't resolve virtual-hosted bucket subdomains.
#[cfg(feature = "file-ops")]
pub fn build_s3_client(
    aws_cfg: &AwsConfig,
    sdk_config: &aws_config::SdkConfig,
) -> aws_sdk_s3::Client {
    let mut builder = aws_sdk_s3::config::Builder::from(sdk_config);
    if aws_cfg
        .endpoint_url
        .as_deref()
        .is_some_and(|e| !e.is_empty())
    {
        builder = builder.force_path_style(true);
    }
    aws_sdk_s3::Client::from_conf(builder.build())
}
