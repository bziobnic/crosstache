use thiserror::Error;

/// Main error type for crosstache operations
#[derive(Debug, Error)]
pub enum CrosstacheError {
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

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("DNS resolution failed for vault '{vault_name}': {details}")]
    DnsResolutionError { vault_name: String, details: String },

    #[error("Connection timeout: {0}")]
    ConnectionTimeout(String),

    #[error("Connection refused: {0}")]
    ConnectionRefused(String),

    #[error("SSL/TLS error: {0}")]
    SslError(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

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

    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl CrosstacheError {
    pub fn authentication<S: Into<String>>(msg: S) -> Self {
        Self::AuthenticationError(msg.into())
    }

    pub fn azure_api<S: Into<String>>(msg: S) -> Self {
        Self::AzureApiError(msg.into())
    }

    pub fn config<S: Into<String>>(msg: S) -> Self {
        Self::ConfigError(msg.into())
    }

    pub fn vault_not_found<S: Into<String>>(name: S) -> Self {
        Self::VaultNotFound { name: name.into() }
    }

    pub fn invalid_secret_name<S: Into<String>>(name: S) -> Self {
        Self::InvalidSecretName { name: name.into() }
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
            details: details.into(),
        }
    }

    pub fn connection_timeout<S: Into<String>>(msg: S) -> Self {
        Self::ConnectionTimeout(msg.into())
    }

    pub fn connection_refused<S: Into<String>>(msg: S) -> Self {
        Self::ConnectionRefused(msg.into())
    }

    pub fn ssl_error<S: Into<String>>(msg: S) -> Self {
        Self::SslError(msg.into())
    }

    pub fn invalid_url<S: Into<String>>(msg: S) -> Self {
        Self::InvalidUrl(msg.into())
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
pub type Result<T> = std::result::Result<T, CrosstacheError>;

/// Convert Azure Core errors to CrosstacheError
impl From<azure_core::Error> for CrosstacheError {
    fn from(error: azure_core::Error) -> Self {
        Self::AzureApiError(error.to_string())
    }
}

// TODO: Convert Azure Identity errors to CrosstacheError
// Note: Azure Identity crate doesn't expose a specific Error type in v0.20
// We'll implement this when we integrate with actual Azure Identity APIs
