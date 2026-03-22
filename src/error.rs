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

    #[error("YAML error: {0}")]
    YamlError(#[from] serde_yaml::Error),

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

    #[error("Upgrade error: {0}")]
    Upgrade(String),

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

    pub fn secret_not_found<S: Into<String>>(name: S) -> Self {
        Self::SecretNotFound { name: name.into() }
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

    pub fn upgrade<S: Into<String>>(msg: S) -> Self {
        Self::Upgrade(msg.into())
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

// Note: azure_identity v0.21 does not expose a standalone public Error type.
// Authentication failures from azure_identity surface as azure_core::Error,
// which is already converted via the From<azure_core::Error> impl above.
// No separate From<azure_identity::Error> impl is needed.

#[cfg(test)]
mod tests {
    use super::*;

    // --- Constructor methods ---

    #[test]
    fn test_authentication_constructor() {
        let err = CrosstacheError::authentication("bad token");
        assert!(matches!(err, CrosstacheError::AuthenticationError(ref s) if s == "bad token"));
        assert_eq!(err.to_string(), "Authentication failed: bad token");
    }

    #[test]
    fn test_azure_api_constructor() {
        let err = CrosstacheError::azure_api("429 Too Many Requests");
        assert!(
            matches!(err, CrosstacheError::AzureApiError(ref s) if s == "429 Too Many Requests")
        );
        assert_eq!(err.to_string(), "Azure API error: 429 Too Many Requests");
    }

    #[test]
    fn test_config_constructor() {
        let err = CrosstacheError::config("missing vault");
        assert!(matches!(err, CrosstacheError::ConfigError(ref s) if s == "missing vault"));
        assert_eq!(err.to_string(), "Configuration error: missing vault");
    }

    #[test]
    fn test_secret_not_found_constructor() {
        let err = CrosstacheError::secret_not_found("my-secret");
        assert!(
            matches!(err, CrosstacheError::SecretNotFound { ref name } if name == "my-secret")
        );
        assert_eq!(err.to_string(), "Secret not found: my-secret");
    }

    #[test]
    fn test_vault_not_found_constructor() {
        let err = CrosstacheError::vault_not_found("prod-vault");
        assert!(
            matches!(err, CrosstacheError::VaultNotFound { ref name } if name == "prod-vault")
        );
        assert_eq!(err.to_string(), "Vault not found: prod-vault");
    }

    #[test]
    fn test_invalid_secret_name_constructor() {
        let err = CrosstacheError::invalid_secret_name("bad/name");
        assert!(
            matches!(err, CrosstacheError::InvalidSecretName { ref name } if name == "bad/name")
        );
        assert_eq!(err.to_string(), "Invalid secret name: bad/name");
    }

    #[test]
    fn test_permission_denied_constructor() {
        let err = CrosstacheError::permission_denied("read not allowed");
        assert!(
            matches!(err, CrosstacheError::PermissionDenied(ref s) if s == "read not allowed")
        );
        assert_eq!(err.to_string(), "Permission denied: read not allowed");
    }

    #[test]
    fn test_network_constructor() {
        let err = CrosstacheError::network("connection dropped");
        assert!(matches!(err, CrosstacheError::NetworkError(ref s) if s == "connection dropped"));
        assert_eq!(err.to_string(), "Network error: connection dropped");
    }

    #[test]
    fn test_dns_resolution_constructor() {
        let err = CrosstacheError::dns_resolution("my-vault", "NXDOMAIN");
        assert!(
            matches!(err, CrosstacheError::DnsResolutionError { ref vault_name, ref details }
                if vault_name == "my-vault" && details == "NXDOMAIN")
        );
        assert_eq!(
            err.to_string(),
            "DNS resolution failed for vault 'my-vault': NXDOMAIN"
        );
    }

    #[test]
    fn test_connection_timeout_constructor() {
        let err = CrosstacheError::connection_timeout("30s elapsed");
        assert!(
            matches!(err, CrosstacheError::ConnectionTimeout(ref s) if s == "30s elapsed")
        );
        assert_eq!(err.to_string(), "Connection timeout: 30s elapsed");
    }

    #[test]
    fn test_connection_refused_constructor() {
        let err = CrosstacheError::connection_refused("port 443 closed");
        assert!(
            matches!(err, CrosstacheError::ConnectionRefused(ref s) if s == "port 443 closed")
        );
        assert_eq!(err.to_string(), "Connection refused: port 443 closed");
    }

    #[test]
    fn test_ssl_error_constructor() {
        let err = CrosstacheError::ssl_error("certificate expired");
        assert!(matches!(err, CrosstacheError::SslError(ref s) if s == "certificate expired"));
        assert_eq!(err.to_string(), "SSL/TLS error: certificate expired");
    }

    #[test]
    fn test_invalid_url_constructor() {
        let err = CrosstacheError::invalid_url("not a url");
        assert!(matches!(err, CrosstacheError::InvalidUrl(ref s) if s == "not a url"));
        assert_eq!(err.to_string(), "Invalid URL: not a url");
    }

    #[test]
    fn test_serialization_constructor() {
        let err = CrosstacheError::serialization("bad JSON");
        assert!(
            matches!(err, CrosstacheError::SerializationError(ref s) if s == "bad JSON")
        );
        assert_eq!(err.to_string(), "Serialization error: bad JSON");
    }

    #[test]
    fn test_invalid_argument_constructor() {
        let err = CrosstacheError::invalid_argument("--format missing");
        assert!(
            matches!(err, CrosstacheError::InvalidArgument(ref s) if s == "--format missing")
        );
        assert_eq!(err.to_string(), "Invalid argument: --format missing");
    }

    #[test]
    fn test_upgrade_constructor() {
        let err = CrosstacheError::upgrade("version mismatch");
        assert!(matches!(err, CrosstacheError::Upgrade(ref s) if s == "version mismatch"));
        assert_eq!(err.to_string(), "Upgrade error: version mismatch");
    }

    #[test]
    fn test_unknown_constructor() {
        let err = CrosstacheError::unknown("something went wrong");
        assert!(
            matches!(err, CrosstacheError::Unknown(ref s) if s == "something went wrong")
        );
        assert_eq!(err.to_string(), "Unknown error: something went wrong");
    }

    // --- From impls ---

    #[test]
    fn test_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = CrosstacheError::from(io_err);
        assert!(matches!(err, CrosstacheError::IoError(_)));
        assert!(err.to_string().contains("IO error"));
    }

    #[test]
    fn test_from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err = CrosstacheError::from(json_err);
        assert!(matches!(err, CrosstacheError::JsonError(_)));
        assert!(err.to_string().contains("JSON error"));
    }

    // --- Debug impl (derived) ---

    #[test]
    fn test_debug_format() {
        let err = CrosstacheError::authentication("test");
        let debug = format!("{err:?}");
        assert!(debug.contains("AuthenticationError"));
        assert!(debug.contains("test"));
    }

    // --- String ownership: constructors accept both &str and String ---

    #[test]
    fn test_constructors_accept_owned_string() {
        let msg = String::from("owned message");
        let err = CrosstacheError::network(msg);
        assert_eq!(err.to_string(), "Network error: owned message");
    }
}
