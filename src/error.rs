use thiserror::Error;

/// Main error type for crosstache operations
#[derive(Debug, Error)]
pub enum crosstacheError {
    #[error("Authentication failed: {0}")]
    AuthenticationError(String),
    
    #[error("Azure API error: {0}")]
    AzureApiError(String),
    
    #[error("Configuration error: {0}")]
    ConfigError(String),
    
    #[error("Secret not found: {name}")]
    SecretNotFound { name: String },
    
    #[error("Vault not found: {name}")]
    VaultNotFound { name: String },
    
    #[error("Invalid secret name: {name}")]
    InvalidSecretName { name: String },
    
    #[error("Name sanitization failed: {0}")]
    NameSanitizationError(String),
    
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    
    #[error("Network error: {0}")]
    NetworkError(String),
    
    #[error("DNS resolution failed for vault '{vault_name}': {details}")]
    DnsResolutionError { 
        vault_name: String, 
        details: String 
    },
    
    #[error("Serialization error: {0}")]
    SerializationError(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
    
    #[error("HTTP request error: {0}")]
    HttpError(#[from] reqwest::Error),
    
    #[error("UUID error: {0}")]
    UuidError(#[from] uuid::Error),
    
    #[error("Regex error: {0}")]
    RegexError(#[from] regex::Error),
    
    #[error("Configuration loading error: {0}")]
    ConfigLoadError(#[from] config::ConfigError),
    
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
    
    #[error("Operation timeout")]
    Timeout,
    
    #[error("Operation cancelled")]
    Cancelled,
    
    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl crosstacheError {
    pub fn authentication<S: Into<String>>(msg: S) -> Self {
        Self::AuthenticationError(msg.into())
    }
    
    pub fn azure_api<S: Into<String>>(msg: S) -> Self {
        Self::AzureApiError(msg.into())
    }
    
    pub fn config<S: Into<String>>(msg: S) -> Self {
        Self::ConfigError(msg.into())
    }
    
    pub fn secret_not_found<S: Into<String>>(name: S) -> Self {
        Self::SecretNotFound { name: name.into() }
    }
    
    pub fn vault_not_found<S: Into<String>>(name: S) -> Self {
        Self::VaultNotFound { name: name.into() }
    }
    
    pub fn invalid_secret_name<S: Into<String>>(name: S) -> Self {
        Self::InvalidSecretName { name: name.into() }
    }
    
    pub fn name_sanitization<S: Into<String>>(msg: S) -> Self {
        Self::NameSanitizationError(msg.into())
    }
    
    pub fn permission_denied<S: Into<String>>(msg: S) -> Self {
        Self::PermissionDenied(msg.into())
    }
    
    pub fn network<S: Into<String>>(msg: S) -> Self {
        Self::NetworkError(msg.into())
    }
    
    pub fn dns_resolution<S: Into<String>>(vault_name: S, details: S) -> Self {
        Self::DnsResolutionError { 
            vault_name: vault_name.into(), 
            details: details.into() 
        }
    }
    
    pub fn serialization<S: Into<String>>(msg: S) -> Self {
        Self::SerializationError(msg.into())
    }
    
    pub fn invalid_argument<S: Into<String>>(msg: S) -> Self {
        Self::InvalidArgument(msg.into())
    }
    
    pub fn unknown<S: Into<String>>(msg: S) -> Self {
        Self::Unknown(msg.into())
    }
}

/// Result type alias for crosstache operations
pub type Result<T> = std::result::Result<T, crosstacheError>;

/// Convert Azure Core errors to crosstacheError
impl From<azure_core::Error> for crosstacheError {
    fn from(error: azure_core::Error) -> Self {
        Self::AzureApiError(error.to_string())
    }
}

// TODO: Convert Azure Identity errors to crosstacheError
// Note: Azure Identity crate doesn't expose a specific Error type in v0.20
// We'll implement this when we integrate with actual Azure Identity APIs