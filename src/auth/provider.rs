//! Authentication provider trait and implementations
//!
//! This module defines the authentication provider trait and provides
//! implementations for various Azure authentication methods.

use crate::error::{CrosstacheError, Result};
use crate::utils::network::{classify_network_error, create_http_client, NetworkConfig};
use async_trait::async_trait;
use azure_core::auth::{AccessToken, TokenCredential};
use azure_identity::{ClientSecretCredential, DefaultAzureCredential, TokenCredentialOptions};
use base64::Engine;
use reqwest::{header::HeaderMap, Client};
use serde_json::Value;
use std::collections::HashMap;
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

    CrosstacheError::authentication(format!("{}\n\n{}", error, help_message))
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

    CrosstacheError::authentication(format!("{}\n\n{}", error, help_message))
}

/// Trait for Azure authentication providers
#[async_trait]
pub trait AzureAuthProvider: Send + Sync {
    /// Get an access token for the specified scopes
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken>;

    /// Get the tenant ID
    async fn get_tenant_id(&self) -> Result<String>;

    /// Get the object ID for the current user/service principal
    async fn get_object_id(&self) -> Result<String>;

    /// Get the client ID (if applicable)
    async fn get_client_id(&self) -> Result<Option<String>>;

    /// Sign out and clear cached credentials
    async fn sign_out(&self) -> Result<()>;

    /// Get the underlying token credential for Azure SDK usage
    fn get_token_credential(&self) -> Arc<dyn TokenCredential>;
}

/// Default Azure Credential Provider using DefaultAzureCredential
pub struct DefaultAzureCredentialProvider {
    credential: Arc<DefaultAzureCredential>,
    http_client: Client,
    tenant_id: Option<String>,
}

impl DefaultAzureCredentialProvider {
    /// Create a new DefaultAzureCredentialProvider
    pub fn new() -> Result<Self> {
        // Try to get tenant ID from Azure CLI to configure the credential
        let tenant_id = match std::process::Command::new("az")
            .args(&["account", "show", "--query", "tenantId", "-o", "tsv"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let tid = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !tid.is_empty() && tid != "00000000-0000-0000-0000-000000000000" {
                    Some(tid)
                } else {
                    None
                }
            },
            _ => None
        };

        let credential = Arc::new(
            DefaultAzureCredential::create(TokenCredentialOptions::default())
                .map_err(|e| create_user_friendly_credential_error(e))?,
        );
        let network_config = NetworkConfig::default();
        let http_client = create_http_client(&network_config)?;

        Ok(Self {
            credential,
            http_client,
            tenant_id,
        })
    }

    /// Create a new DefaultAzureCredentialProvider with specific tenant
    pub fn with_tenant(tenant_id: String) -> Result<Self> {
        // Note: Azure Identity v0.20 may have different API for setting tenant
        let credential = Arc::new(
            DefaultAzureCredential::create(TokenCredentialOptions::default())
                .map_err(|e| create_user_friendly_credential_error(e))?,
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
            format!("Bearer {}", access_token).parse().map_err(|e| {
                CrosstacheError::authentication(format!("Invalid token format: {}", e))
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
            CrosstacheError::serialization(format!("Failed to parse user info: {}", e))
        })?;

        Ok(user_info)
    }

    /// Extract tenant ID from JWT token
    fn extract_tenant_from_token(&self, token: &str) -> Result<String> {
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
        while payload_padded.len() % 4 != 0 {
            payload_padded.push('=');
        }
        
        let decoded_bytes = base64::engine::general_purpose::URL_SAFE
            .decode(payload_padded)
            .map_err(|e| {
                CrosstacheError::authentication(format!("Failed to decode JWT payload: {}", e))
            })?;

        // Parse JSON
        let claims: Value = serde_json::from_slice(&decoded_bytes).map_err(|e| {
            CrosstacheError::authentication(format!("Failed to parse JWT claims: {}", e))
        })?;

        // Extract tenant ID from 'tid' claim
        let tenant_id = claims
            .get("tid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                CrosstacheError::authentication("Unable to find tenant ID in token".to_string())
            })?;

        // Validate the tenant ID is a proper GUID and not nil
        if tenant_id == "00000000-0000-0000-0000-000000000000" || tenant_id.is_empty() {
            return Err(CrosstacheError::authentication(
                "Invalid or empty tenant ID in token".to_string(),
            ));
        }

        Ok(tenant_id)
    }
}

#[async_trait]
impl AzureAuthProvider for DefaultAzureCredentialProvider {
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken> {
        let token_response = self
            .credential
            .get_token(scopes)
            .await
            .map_err(|e| create_user_friendly_token_error(e))?;

        Ok(token_response)
    }

    async fn get_tenant_id(&self) -> Result<String> {
        if let Some(tenant_id) = &self.tenant_id {
            return Ok(tenant_id.clone());
        }

        // First try to get tenant ID from environment variable
        if let Ok(env_tenant_id) = std::env::var("AZURE_TENANT_ID") {
            if !env_tenant_id.is_empty() && env_tenant_id != "00000000-0000-0000-0000-000000000000" {
                return Ok(env_tenant_id);
            }
        }

        // Use Azure CLI as the primary method since it's most reliable
        match std::process::Command::new("az")
            .args(&["account", "show", "--query", "tenantId", "-o", "tsv"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let tenant_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !tenant_id.is_empty() && tenant_id != "00000000-0000-0000-0000-000000000000" {
                    return Ok(tenant_id);
                }
            },
            _ => {}
        }

        // Fallback: try to get tenant ID from token claims
        let token = self
            .get_token(&["https://graph.microsoft.com/.default"])
            .await?;
        
        // Extract tenant ID from JWT token
        match self.extract_tenant_from_token(&token.token.secret()) {
            Ok(tenant_id) => Ok(tenant_id),
            Err(_) => Err(CrosstacheError::authentication("Unable to determine tenant ID from any source".to_string()))
        }
    }

    async fn get_object_id(&self) -> Result<String> {
        let token = self
            .get_token(&["https://graph.microsoft.com/.default"])
            .await?;
        let user_info = self.get_user_info(&token.token.secret()).await?;

        user_info
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                CrosstacheError::authentication("Unable to determine object ID".to_string())
            })
    }

    async fn get_client_id(&self) -> Result<Option<String>> {
        // DefaultAzureCredential may not expose client ID directly
        Ok(None)
    }

    async fn sign_out(&self) -> Result<()> {
        // DefaultAzureCredential doesn't have a direct sign-out method
        // This would typically involve clearing token caches
        Ok(())
    }

    fn get_token_credential(&self) -> Arc<dyn TokenCredential> {
        self.credential.clone()
    }
}

/// Client Secret Authentication Provider
pub struct ClientSecretProvider {
    credential: Arc<ClientSecretCredential>,
    http_client: Client,
    tenant_id: String,
    client_id: String,
}

impl ClientSecretProvider {
    /// Create a new ClientSecretProvider
    pub fn new(tenant_id: String, client_id: String, client_secret: String) -> Result<Self> {
        // Note: Azure Identity v0.20 has a different API for ClientSecretCredential
        // We'll need to adapt this based on the actual API
        let http_client = Client::new();
        let authority = format!("https://login.microsoftonline.com/{}", tenant_id);
        let authority_url = url::Url::parse(&authority)
            .map_err(|e| CrosstacheError::config(format!("Invalid authority URL: {}", e)))?;

        let http_client_arc = Arc::new(reqwest::Client::new());
        let credential = Arc::new(ClientSecretCredential::new(
            http_client_arc.clone(),
            authority_url,
            client_secret,
            tenant_id.clone(),
            client_id.clone(),
        ));

        Ok(Self {
            credential,
            http_client,
            tenant_id,
            client_id,
        })
    }

    /// Get service principal information from Microsoft Graph API
    async fn get_service_principal_info(&self, access_token: &str) -> Result<Value> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", access_token).parse().map_err(|e| {
                CrosstacheError::authentication(format!("Invalid token format: {}", e))
            })?,
        );

        let url = format!(
            "https://graph.microsoft.com/v1.0/servicePrincipals?$filter=appId eq '{}'",
            self.client_id
        );
        let response = self
            .http_client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| CrosstacheError::network(format!("Failed to call Graph API: {}", e)))?;

        if !response.status().is_success() {
            return Err(CrosstacheError::authentication(format!(
                "Graph API error: HTTP {}",
                response.status()
            )));
        }

        let sp_info: Value = response.json().await.map_err(|e| {
            CrosstacheError::serialization(format!("Failed to parse service principal info: {}", e))
        })?;

        Ok(sp_info)
    }
}

#[async_trait]
impl AzureAuthProvider for ClientSecretProvider {
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken> {
        let token_response = self
            .credential
            .get_token(scopes)
            .await
            .map_err(|e| create_user_friendly_token_error(e))?;

        Ok(token_response)
    }

    async fn get_tenant_id(&self) -> Result<String> {
        Ok(self.tenant_id.clone())
    }

    async fn get_object_id(&self) -> Result<String> {
        let token = self
            .get_token(&["https://graph.microsoft.com/.default"])
            .await?;
        let sp_info = self
            .get_service_principal_info(&token.token.secret())
            .await?;

        sp_info
            .get("value")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|sp| sp.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                CrosstacheError::authentication(
                    "Unable to determine service principal object ID".to_string(),
                )
            })
    }

    async fn get_client_id(&self) -> Result<Option<String>> {
        Ok(Some(self.client_id.clone()))
    }

    async fn sign_out(&self) -> Result<()> {
        // Client secret credentials don't require sign-out
        Ok(())
    }

    fn get_token_credential(&self) -> Arc<dyn TokenCredential> {
        self.credential.clone()
    }
}

/// Authentication provider factory
pub struct AuthProviderFactory;

impl AuthProviderFactory {
    /// Create an authentication provider based on configuration
    pub fn create_provider(
        provider_type: &str,
        config: &HashMap<String, String>,
    ) -> Result<Box<dyn AzureAuthProvider>> {
        match provider_type.to_lowercase().as_str() {
            "default" | "defaultazurecredential" => {
                if let Some(tenant_id) = config.get("tenant_id") {
                    Ok(Box::new(DefaultAzureCredentialProvider::with_tenant(
                        tenant_id.clone(),
                    )?))
                } else {
                    Ok(Box::new(DefaultAzureCredentialProvider::new()?))
                }
            }
            "clientsecret" => {
                let tenant_id = config.get("tenant_id").ok_or_else(|| {
                    CrosstacheError::config(
                        "tenant_id is required for client secret authentication",
                    )
                })?;
                let client_id = config.get("client_id").ok_or_else(|| {
                    CrosstacheError::config(
                        "client_id is required for client secret authentication",
                    )
                })?;
                let client_secret = config.get("client_secret").ok_or_else(|| {
                    CrosstacheError::config(
                        "client_secret is required for client secret authentication",
                    )
                })?;

                Ok(Box::new(ClientSecretProvider::new(
                    tenant_id.clone(),
                    client_id.clone(),
                    client_secret.clone(),
                )?))
            }
            _ => Err(CrosstacheError::config(format!(
                "Unsupported authentication provider: {}",
                provider_type
            ))),
        }
    }
}
