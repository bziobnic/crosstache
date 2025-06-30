//! Authentication provider trait and implementations
//! 
//! This module defines the authentication provider trait and provides
//! implementations for various Azure authentication methods.

use async_trait::async_trait;
use azure_core::auth::{AccessToken, TokenCredential};
use azure_identity::{DefaultAzureCredential, ClientSecretCredential, TokenCredentialOptions};
use reqwest::{Client, header::HeaderMap};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use crate::error::{crosstacheError, Result};

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
        let credential = Arc::new(DefaultAzureCredential::create(TokenCredentialOptions::default())
            .map_err(|e| crosstacheError::authentication(format!("Failed to create DefaultAzureCredential: {}", e)))?);
        let http_client = Client::new();
        
        Ok(Self {
            credential,
            http_client,
            tenant_id: None,
        })
    }
    
    /// Create a new DefaultAzureCredentialProvider with specific tenant
    pub fn with_tenant(tenant_id: String) -> Result<Self> {
        // Note: Azure Identity v0.20 may have different API for setting tenant
        let credential = Arc::new(DefaultAzureCredential::create(TokenCredentialOptions::default())
            .map_err(|e| crosstacheError::authentication(format!("Failed to create DefaultAzureCredential: {}", e)))?);
        let http_client = Client::new();
        
        Ok(Self {
            credential,
            http_client,
            tenant_id: Some(tenant_id),
        })
    }
    
    /// Get user information from Microsoft Graph API
    async fn get_user_info(&self, access_token: &str) -> Result<Value> {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", format!("Bearer {}", access_token).parse()
            .map_err(|e| crosstacheError::authentication(format!("Invalid token format: {}", e)))?);
        
        let response = self.http_client
            .get("https://graph.microsoft.com/v1.0/me")
            .headers(headers)
            .send()
            .await
            .map_err(|e| crosstacheError::network(format!("Failed to call Graph API: {}", e)))?;
        
        if !response.status().is_success() {
            return Err(crosstacheError::authentication(format!(
                "Graph API error: HTTP {}", response.status()
            )));
        }
        
        let user_info: Value = response.json().await
            .map_err(|e| crosstacheError::serialization(format!("Failed to parse user info: {}", e)))?;
        
        Ok(user_info)
    }
}

#[async_trait]
impl AzureAuthProvider for DefaultAzureCredentialProvider {
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken> {
        let token_response = self.credential
            .get_token(scopes)
            .await
            .map_err(|e| crosstacheError::authentication(format!("Failed to get token: {}", e)))?;
        
        Ok(token_response)
    }
    
    async fn get_tenant_id(&self) -> Result<String> {
        if let Some(tenant_id) = &self.tenant_id {
            return Ok(tenant_id.clone());
        }
        
        // Get tenant ID from token or Graph API
        let token = self.get_token(&["https://graph.microsoft.com/.default"]).await?;
        let user_info = self.get_user_info(&token.token.secret()).await?;
        
        user_info.get("tenantId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| crosstacheError::authentication("Unable to determine tenant ID".to_string()))
    }
    
    async fn get_object_id(&self) -> Result<String> {
        let token = self.get_token(&["https://graph.microsoft.com/.default"]).await?;
        let user_info = self.get_user_info(&token.token.secret()).await?;
        
        user_info.get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| crosstacheError::authentication("Unable to determine object ID".to_string()))
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
            .map_err(|e| crosstacheError::config(format!("Invalid authority URL: {}", e)))?;
        
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
        headers.insert("Authorization", format!("Bearer {}", access_token).parse()
            .map_err(|e| crosstacheError::authentication(format!("Invalid token format: {}", e)))?);
        
        let url = format!("https://graph.microsoft.com/v1.0/servicePrincipals?$filter=appId eq '{}'", self.client_id);
        let response = self.http_client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| crosstacheError::network(format!("Failed to call Graph API: {}", e)))?;
        
        if !response.status().is_success() {
            return Err(crosstacheError::authentication(format!(
                "Graph API error: HTTP {}", response.status()
            )));
        }
        
        let sp_info: Value = response.json().await
            .map_err(|e| crosstacheError::serialization(format!("Failed to parse service principal info: {}", e)))?;
        
        Ok(sp_info)
    }
}

#[async_trait]
impl AzureAuthProvider for ClientSecretProvider {
    async fn get_token(&self, scopes: &[&str]) -> Result<AccessToken> {
        let token_response = self.credential
            .get_token(scopes)
            .await
            .map_err(|e| crosstacheError::authentication(format!("Failed to get token: {}", e)))?;
        
        Ok(token_response)
    }
    
    async fn get_tenant_id(&self) -> Result<String> {
        Ok(self.tenant_id.clone())
    }
    
    async fn get_object_id(&self) -> Result<String> {
        let token = self.get_token(&["https://graph.microsoft.com/.default"]).await?;
        let sp_info = self.get_service_principal_info(&token.token.secret()).await?;
        
        sp_info.get("value")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|sp| sp.get("id"))
            .and_then(|id| id.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| crosstacheError::authentication("Unable to determine service principal object ID".to_string()))
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
                    Ok(Box::new(DefaultAzureCredentialProvider::with_tenant(tenant_id.clone())?))
                } else {
                    Ok(Box::new(DefaultAzureCredentialProvider::new()?))
                }
            }
            "clientsecret" => {
                let tenant_id = config.get("tenant_id")
                    .ok_or_else(|| crosstacheError::config("tenant_id is required for client secret authentication"))?;
                let client_id = config.get("client_id")
                    .ok_or_else(|| crosstacheError::config("client_id is required for client secret authentication"))?;
                let client_secret = config.get("client_secret")
                    .ok_or_else(|| crosstacheError::config("client_secret is required for client secret authentication"))?;
                
                Ok(Box::new(ClientSecretProvider::new(
                    tenant_id.clone(),
                    client_id.clone(),
                    client_secret.clone(),
                )?))
            }
            _ => Err(crosstacheError::config(format!("Unsupported authentication provider: {}", provider_type)))
        }
    }
}