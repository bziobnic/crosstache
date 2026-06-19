//! Authentication provider trait and implementations
//!
//! This module defines the authentication provider trait and provides
//! implementations for various Azure authentication methods.

use crate::error::{CrosstacheError, Result};
use crate::utils::network::{classify_network_error, create_http_client, NetworkConfig};
use async_trait::async_trait;
use azure_core::auth::{AccessToken, TokenCredential};
use azure_identity::{
    AppServiceManagedIdentityCredential, AzureCliCredential, DefaultAzureCredential,
    EnvironmentCredential, TokenCredentialOptions, VirtualMachineManagedIdentityCredential,
};
use base64::Engine;
use reqwest::{header::HeaderMap, Client};
use serde_json::Value;
use std::sync::Arc;

/// Creates a user-friendly error message for credential creation failures
fn create_user_friendly_credential_error(error: azure_core::Error) -> CrosstacheError {
    let error_str = error.to_string().to_lowercase();

    let help_message = if error_str.contains("no credentials")
        || error_str.contains("authentication failed")
    {
        "Azure authentication failed. Please try one of the following:

1. Sign in to Azure CLI:
   az login

2. Set environment variables for service principal:
   export AZURE_CLIENT_ID=your-client-id
   export AZURE_CLIENT_SECRET=your-client-secret
   export AZURE_TENANT_ID=your-tenant-id

3. Use managed identity if running on Azure resources

4. Sign in to Visual Studio Code with Azure account

For more details, see: https://docs.microsoft.com/en-us/azure/developer/rust/authentication"
    } else if error_str.contains("network") || error_str.contains("connection") {
        "Network connection failed while attempting to authenticate. Please check:

1. Your internet connection
2. Corporate firewall settings
3. Proxy configuration

If behind a corporate firewall, you may need to configure proxy settings."
    } else if error_str.contains("tenant") {
        "Tenant-related authentication error. Please verify:

1. Your tenant ID is correct
2. Your account has access to the specified tenant
3. The tenant allows the authentication method you're using

You can find your tenant ID in the Azure portal or with: az account show"
    } else {
        "Azure authentication failed. Common solutions:

1. Run 'az login' to authenticate with Azure CLI
2. Verify your Azure account permissions
3. Check your network connection
4. Ensure you have access to the Azure subscription

For detailed troubleshooting, see: https://docs.microsoft.com/en-us/azure/developer/rust/authentication"
    };

    CrosstacheError::authentication(format!("{error}\n\n{help_message}"))
}

/// Creates a user-friendly error message for token acquisition failures
fn create_user_friendly_token_error(error: azure_core::Error) -> CrosstacheError {
    let error_str = error.to_string().to_lowercase();

    let help_message = if error_str.contains("403") || error_str.contains("forbidden") {
        "Access denied. Please verify:

1. Your account has the necessary permissions
2. You have access to the Azure subscription
3. The resource you're trying to access exists
4. Your authentication hasn't expired (try 'az login' again)"
    } else if error_str.contains("401") || error_str.contains("unauthorized") {
        "Authentication expired or invalid. Please try:

1. Re-authenticate with Azure CLI: az login
2. Check if your credentials are still valid
3. Verify your tenant ID and subscription access"
    } else if error_str.contains("timeout") || error_str.contains("network") {
        "Network timeout or connection issue. Please check:

1. Your internet connection
2. Corporate firewall or proxy settings
3. Azure service availability"
    } else if error_str.contains("scope") {
        "Invalid scope or permission issue. This may indicate:

1. Your account doesn't have the required permissions
2. The requested resource or service is not available
3. Your authentication method doesn't support the requested operation"
    } else {
        "Token acquisition failed. Common solutions:

1. Re-authenticate: az login
2. Check your permissions and subscription access
3. Verify your network connection
4. Ensure the Azure service is available"
    };

    CrosstacheError::authentication(format!("{error}\n\n{help_message}"))
}

/// Maximum time to wait for an `az` CLI invocation before giving up.
///
/// The Azure CLI can hang indefinitely on a stuck network call or an
/// interactive auth prompt; without a bound, a single `az account show` would
/// wedge the whole process. 10s is generous for a local metadata query.
const AZ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Maximum bytes of `az` stderr we retain for diagnostics (avoid unbounded
/// capture if the CLI spews).
const AZ_STDERR_CAP: usize = 4096;

/// Run `az <args>` with a bounded timeout and return trimmed stdout on success.
///
/// Centralizes every `az` subprocess invocation so the timeout and stderr cap
/// are enforced uniformly. The child is spawned with stdin closed (so it can
/// never block waiting for interactive input) and is killed if it does not
/// finish within [`AZ_TIMEOUT`]. Returns `None` on any failure (spawn error,
/// non-zero exit, timeout) — callers treat `az` as a best-effort tenant hint
/// and fall back to other sources, so a soft failure is the correct contract.
fn az_output(args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new("az")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    // Poll for completion up to AZ_TIMEOUT, then kill. std's Child has no
    // wait-with-timeout, so poll try_wait on a short interval — cheap for a
    // sub-second-to-few-second command and avoids pulling in an extra crate.
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= AZ_TIMEOUT {
                    // Best-effort kill + reap so we don't leak a zombie.
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::debug!("az {:?} timed out after {:?}", args, AZ_TIMEOUT);
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                tracing::debug!("az {:?} wait failed: {e}", args);
                return None;
            }
        }
    };

    if !status.success() {
        if let Some(err) = child.stderr.take() {
            let mut buf = Vec::new();
            let _ = err.take(AZ_STDERR_CAP as u64).read_to_end(&mut buf);
            tracing::debug!(
                "az {:?} exited {}: {}",
                args,
                status,
                String::from_utf8_lossy(&buf).trim()
            );
        }
        return None;
    }

    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Resolve the current tenant id via `az account show`, rejecting the nil GUID
/// and anything that is not a well-formed GUID. Centralized so both the
/// constructor and `get_tenant_id` share one bounded, validated path.
fn az_account_tenant_id() -> Option<String> {
    let tid = az_output(&["account", "show", "--query", "tenantId", "-o", "tsv"])?;
    if tid != "00000000-0000-0000-0000-000000000000" && crate::utils::helpers::is_guid(&tid) {
        Some(tid)
    } else {
        None
    }
}

/// Pre-compiled UUID regex, reused across all resolve_user_to_object_id calls.
static UUID_REGEX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("UUID regex is valid")
});

/// Trait for Azure authentication providers
#[async_trait]
pub trait AzureAuthProvider: Send + Sync {
    /// Get an access token for the specified scopes
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken>;

    /// Get the tenant ID
    async fn get_tenant_id(&self) -> Result<String>;

    /// Get the object ID for the current user/service principal
    async fn get_object_id(&self) -> Result<String>;

    /// Get the underlying token credential for Azure SDK usage
    fn get_token_credential(&self) -> Arc<dyn TokenCredential>;

    /// Resolve a user identifier (email, UPN, or object ID) to an Azure AD object ID
    async fn resolve_user_to_object_id(&self, user: &str) -> Result<String>;
}

/// Default Azure Credential Provider using DefaultAzureCredential
pub struct DefaultAzureCredentialProvider {
    credential: Arc<dyn TokenCredential>,
    http_client: Client,
    tenant_id: Option<String>,
}

impl DefaultAzureCredentialProvider {
    /// Create a new DefaultAzureCredentialProvider
    pub fn new() -> Result<Self> {
        Self::with_credential_priority(crate::config::settings::AzureCredentialType::Default)
    }

    /// Create a new DefaultAzureCredentialProvider with specific credential priority
    pub fn with_credential_priority(
        priority: crate::config::settings::AzureCredentialType,
    ) -> Result<Self> {
        // Try to get tenant ID from Azure CLI to configure the credential.
        // Uses the centralized, timeout-bounded helper so a hung `az` cannot
        // wedge construction; failure is soft (tenant_id stays None).
        let tenant_id = az_account_tenant_id();

        let credential = Self::create_prioritized_credential(priority)?;
        let network_config = NetworkConfig::default();
        let http_client = create_http_client(&network_config)?;

        Ok(Self {
            credential,
            http_client,
            tenant_id,
        })
    }

    /// Create a credential chain based on the specified priority
    fn create_prioritized_credential(
        priority: crate::config::settings::AzureCredentialType,
    ) -> Result<Arc<dyn TokenCredential>> {
        use crate::config::settings::AzureCredentialType;

        match priority {
            AzureCredentialType::Cli => {
                // AzureCliCredential doesn't have a fallback constructor, just use it directly
                Ok(Arc::new(AzureCliCredential::new()) as Arc<dyn TokenCredential>)
            }
            AzureCredentialType::ManagedIdentity => {
                // Use a specific managed identity credential rather than the full DefaultAzureCredential
                // chain. App Service / Azure Functions expose IDENTITY_ENDPOINT; VMs use IMDS.
                if std::env::var("IDENTITY_ENDPOINT").is_ok() {
                    // Running in App Service or Azure Functions
                    match AppServiceManagedIdentityCredential::create(
                        TokenCredentialOptions::default(),
                    ) {
                        Ok(cred) => Ok(Arc::new(cred) as Arc<dyn TokenCredential>),
                        Err(_) => {
                            // IDENTITY_ENDPOINT was set but credential creation failed; fall back
                            // to the VM IMDS endpoint as a best-effort.
                            Ok(Arc::new(VirtualMachineManagedIdentityCredential::new(
                                TokenCredentialOptions::default(),
                            )) as Arc<dyn TokenCredential>)
                        }
                    }
                } else {
                    // Running on a VM, ACI, AKS node, or similar — use the IMDS endpoint.
                    Ok(Arc::new(VirtualMachineManagedIdentityCredential::new(
                        TokenCredentialOptions::default(),
                    )) as Arc<dyn TokenCredential>)
                }
            }
            AzureCredentialType::Environment => {
                // Try Environment credentials with proper create method
                match EnvironmentCredential::create(TokenCredentialOptions::default()) {
                    Ok(cred) => Ok(Arc::new(cred) as Arc<dyn TokenCredential>),
                    Err(_) => {
                        // Fall back to default if environment vars are not set
                        Ok(Arc::new(
                            DefaultAzureCredential::create(TokenCredentialOptions::default())
                                .map_err(create_user_friendly_credential_error)?,
                        ) as Arc<dyn TokenCredential>)
                    }
                }
            }
            AzureCredentialType::Default => {
                // Use the default credential chain
                Ok(Arc::new(
                    DefaultAzureCredential::create(TokenCredentialOptions::default())
                        .map_err(create_user_friendly_credential_error)?,
                ) as Arc<dyn TokenCredential>)
            }
        }
    }

    /// Create a new DefaultAzureCredentialProvider with specific tenant
    #[allow(dead_code)]
    pub fn with_tenant(tenant_id: String) -> Result<Self> {
        // Note: Azure Identity v0.20 may have different API for setting tenant
        let credential = Arc::new(
            DefaultAzureCredential::create(TokenCredentialOptions::default())
                .map_err(create_user_friendly_credential_error)?,
        );
        let network_config = NetworkConfig::default();
        let http_client = create_http_client(&network_config)?;

        Ok(Self {
            credential,
            http_client,
            tenant_id: Some(tenant_id),
        })
    }

    /// Get user information from Microsoft Graph API
    async fn get_user_info(&self, access_token: &str) -> Result<Value> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {access_token}").parse().map_err(|e| {
                CrosstacheError::authentication(format!("Invalid token format: {e}"))
            })?,
        );

        let graph_url = "https://graph.microsoft.com/v1.0/me";
        let response = self
            .http_client
            .get(graph_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, graph_url))?;

        if !response.status().is_success() {
            return Err(CrosstacheError::authentication(format!(
                "Graph API error: HTTP {}",
                response.status()
            )));
        }

        let user_info: Value = response.json().await.map_err(|e| {
            CrosstacheError::serialization(format!("Failed to parse user info: {e}"))
        })?;

        Ok(user_info)
    }

    /// Extract the tenant id (`tid` claim) from an Azure AD access token.
    ///
    /// ## Trust boundary
    ///
    /// This decodes the JWT payload **without verifying the signature**, which
    /// is safe *only because of where the token comes from*: it is a token this
    /// process just obtained from Azure AD through the credential SDK over TLS
    /// (`get_token`), not an attacker-supplied value. We are not authenticating
    /// the caller with it — we use the `tid` claim purely as a last-resort hint
    /// to discover our own tenant when `az` and `AZURE_TENANT_ID` are both
    /// unavailable. The token is never trusted for authorization decisions.
    ///
    /// Prefer, in order: the cached `tenant_id`, `AZURE_TENANT_ID`, and
    /// `az account show` — all wired ahead of this in `get_tenant_id`. This
    /// path is the final fallback.
    ///
    /// Even so, we validate the claim *shape* (a well-formed, non-nil GUID and
    /// a sane `exp`) rather than blindly trusting whatever the payload decodes
    /// to, so a malformed or truncated token yields a clear error instead of a
    /// garbage tenant id flowing downstream.
    fn extract_tenant_from_token(&self, token: &str) -> Result<String> {
        tenant_id_from_jwt(token)
    }
}

/// Pure claim-extraction core of [`DefaultAzureCredentialProvider::extract_tenant_from_token`],
/// split out as a free function so the trust-boundary validation is unit-testable
/// without constructing a credential provider. See that method for the full
/// trust-boundary rationale.
fn tenant_id_from_jwt(token: &str) -> Result<String> {
    // JWT tokens have three parts separated by dots: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(CrosstacheError::authentication(
            "Invalid JWT token format".to_string(),
        ));
    }

    // Decode the payload (second part)
    let payload = parts[1];

    // For base64url decoding, add padding if needed
    let mut payload_padded = payload.to_string();
    while !payload_padded.len().is_multiple_of(4) {
        payload_padded.push('=');
    }

    let decoded_bytes = base64::engine::general_purpose::URL_SAFE
        .decode(payload_padded)
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to decode JWT payload: {e}"))
        })?;

    // Parse JSON
    let claims: Value = serde_json::from_slice(&decoded_bytes)
        .map_err(|e| CrosstacheError::authentication(format!("Failed to parse JWT claims: {e}")))?;

    // Validate the `exp` claim shape if present: it must be a positive
    // integer (seconds since epoch). A token whose `exp` is malformed is not
    // the well-formed AAD token we expect; reject rather than read a tenant id
    // out of a suspect payload.
    if let Some(exp) = claims.get("exp") {
        let exp_ok = exp.as_u64().map(|v| v > 0).unwrap_or(false);
        if !exp_ok {
            return Err(CrosstacheError::authentication(
                "JWT 'exp' claim is malformed".to_string(),
            ));
        }
    }

    // Extract tenant ID from 'tid' claim
    let tenant_id = claims
        .get("tid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            CrosstacheError::authentication("Unable to find tenant ID in token".to_string())
        })?;

    // Validate claim SHAPE: the tenant id must be a well-formed, non-nil GUID.
    // The old check only rejected the nil GUID and empty string, letting any
    // arbitrary string through as a "tenant id".
    if tenant_id == "00000000-0000-0000-0000-000000000000"
        || !crate::utils::helpers::is_guid(&tenant_id)
    {
        return Err(CrosstacheError::authentication(
            "Invalid or malformed tenant ID in token".to_string(),
        ));
    }

    Ok(tenant_id)
}

#[async_trait]
impl AzureAuthProvider for DefaultAzureCredentialProvider {
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken> {
        let token_response = self
            .credential
            .get_token(scopes)
            .await
            .map_err(create_user_friendly_token_error)?;

        Ok(token_response)
    }

    async fn get_tenant_id(&self) -> Result<String> {
        if let Some(tenant_id) = &self.tenant_id {
            return Ok(tenant_id.clone());
        }

        // First try to get tenant ID from environment variable
        if let Ok(env_tenant_id) = std::env::var("AZURE_TENANT_ID") {
            if !env_tenant_id.is_empty() && env_tenant_id != "00000000-0000-0000-0000-000000000000"
            {
                return Ok(env_tenant_id);
            }
        }

        // Use Azure CLI as the primary method since it's most reliable.
        // Centralized helper enforces a timeout so a hung `az` can't block.
        if let Some(tenant_id) = az_account_tenant_id() {
            return Ok(tenant_id);
        }

        // Fallback: try to get tenant ID from token claims
        let token = self
            .get_token(&["https://graph.microsoft.com/.default"])
            .await?;

        // Extract tenant ID from JWT token
        match self.extract_tenant_from_token(token.token.secret()) {
            Ok(tenant_id) => Ok(tenant_id),
            Err(_) => Err(CrosstacheError::authentication(
                "Unable to determine tenant ID from any source".to_string(),
            )),
        }
    }

    async fn get_object_id(&self) -> Result<String> {
        let token = self
            .get_token(&["https://graph.microsoft.com/.default"])
            .await?;
        let user_info = self.get_user_info(token.token.secret()).await?;

        user_info
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                CrosstacheError::authentication("Unable to determine object ID".to_string())
            })
    }

    fn get_token_credential(&self) -> Arc<dyn TokenCredential> {
        self.credential.clone()
    }

    async fn resolve_user_to_object_id(&self, user: &str) -> Result<String> {
        // If it's already a UUID, return as-is
        if UUID_REGEX.is_match(user) {
            return Ok(user.to_string());
        }

        // Otherwise resolve via Graph API
        let token = self
            .get_token(&["https://graph.microsoft.com/.default"])
            .await?;
        let graph_url =
            crate::utils::url_helpers::graph_url("https://graph.microsoft.com/v1.0/users", &[user]);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", token.token.secret())
                .parse()
                .map_err(|e| {
                    CrosstacheError::authentication(format!("Invalid token format: {e}"))
                })?,
        );

        let response = self
            .http_client
            .get(&graph_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| classify_network_error(&e, &graph_url))?;

        if !response.status().is_success() {
            return Err(CrosstacheError::authentication(format!(
                "Failed to resolve user '{}': HTTP {}",
                user,
                response.status()
            )));
        }

        let user_info: serde_json::Value = response.json().await.map_err(|e| {
            CrosstacheError::serialization(format!("Failed to parse user info: {e}"))
        })?;

        user_info
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                CrosstacheError::authentication(format!(
                    "Could not resolve object ID for user '{user}'"
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// Build a fake JWT (header.payload.signature) with the given payload JSON.
    /// Signature is a placeholder — `tenant_id_from_jwt` does not verify it (by
    /// design; see the trust-boundary docs), so the value is irrelevant.
    fn make_jwt(payload_json: &str) -> String {
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        format!(
            "{}.{}.{}",
            b64(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64(payload_json.as_bytes()),
            b64(b"not-verified")
        )
    }

    #[test]
    fn extracts_valid_tenant_guid() {
        let jwt = make_jwt(r#"{"tid":"72f988bf-86f1-41af-91ab-2d7cd011db47","exp":9999999999}"#);
        let tid = tenant_id_from_jwt(&jwt).unwrap();
        assert_eq!(tid, "72f988bf-86f1-41af-91ab-2d7cd011db47");
    }

    #[test]
    fn rejects_non_guid_tid() {
        // A `tid` that decodes fine but isn't a GUID must be rejected, not
        // passed through as a "tenant id".
        let jwt = make_jwt(r#"{"tid":"not-a-guid","exp":9999999999}"#);
        assert!(tenant_id_from_jwt(&jwt).is_err());
    }

    #[test]
    fn rejects_nil_guid_tid() {
        let jwt = make_jwt(r#"{"tid":"00000000-0000-0000-0000-000000000000","exp":9999999999}"#);
        assert!(tenant_id_from_jwt(&jwt).is_err());
    }

    #[test]
    fn rejects_malformed_exp() {
        // exp present but not a positive integer → reject.
        let jwt = make_jwt(r#"{"tid":"72f988bf-86f1-41af-91ab-2d7cd011db47","exp":"soon"}"#);
        assert!(tenant_id_from_jwt(&jwt).is_err());
    }

    #[test]
    fn rejects_missing_tid() {
        let jwt = make_jwt(r#"{"exp":9999999999}"#);
        assert!(tenant_id_from_jwt(&jwt).is_err());
    }

    #[test]
    fn rejects_wrong_segment_count() {
        assert!(tenant_id_from_jwt("only.two").is_err());
        assert!(tenant_id_from_jwt("a.b.c.d").is_err());
    }
}
