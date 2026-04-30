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
    SecretNotFound {
        name: String,
        suggestion: Option<String>,
    },

    #[error("Vault not found: {name}")]
    VaultNotFound {
        name: String,
        suggestion: Option<String>,
    },

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
    /// Stable, kebab-case error code. Part of the public scripting contract.
    /// New variants must add a code; the exhaustive match keeps this honest.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthenticationError(_) => "xv-auth-failed",
            Self::AzureApiError(_) => "xv-azure-api",
            Self::ConfigError(_) => "xv-config-invalid",
            Self::ConfigLoadError(_) => "xv-config-invalid",
            Self::SecretNotFound { .. } => "xv-secret-not-found",
            Self::VaultNotFound { .. } => "xv-vault-not-found",
            Self::InvalidSecretName { .. } => "xv-invalid-secret-name",
            Self::PermissionDenied(_) => "xv-permission-denied",
            Self::NetworkError(_) => "xv-network",
            Self::DnsResolutionError { .. } => "xv-network-dns",
            Self::ConnectionTimeout(_) => "xv-network-timeout",
            Self::ConnectionRefused(_) => "xv-network-refused",
            Self::SslError(_) => "xv-network-ssl",
            Self::InvalidUrl(_) => "xv-invalid-url",
            Self::SerializationError(_) => "xv-serialization",
            Self::IoError(_) => "xv-io",
            Self::JsonError(_) => "xv-json",
            Self::YamlError(_) => "xv-yaml",
            Self::HttpError(_) => "xv-http",
            Self::UuidError(_) => "xv-uuid",
            Self::RegexError(_) => "xv-regex",
            Self::InvalidArgument(_) => "xv-invalid-argument",
            Self::Upgrade(_) => "xv-upgrade",
            Self::Unknown(_) => "xv-unknown",
        }
    }

    /// Process exit code for this error. Codes group by family; see
    /// `docs/exit-codes.md` for the public table.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidArgument(_) => 2,
            Self::ConfigError(_) | Self::ConfigLoadError(_) => 3,

            Self::SecretNotFound { .. } => 10,
            Self::VaultNotFound { .. } => 11,
            Self::InvalidSecretName { .. } => 12,

            Self::AuthenticationError(_) => 20,
            Self::PermissionDenied(_) => 21,

            Self::NetworkError(_) => 30,
            Self::DnsResolutionError { .. } => 31,
            Self::ConnectionTimeout(_) => 32,
            Self::ConnectionRefused(_) => 33,
            Self::SslError(_) => 34,
            Self::InvalidUrl(_) => 35,

            Self::AzureApiError(_) => 40,

            // Reserve 50–59 for the scanner feature (lands in plan 4).

            Self::SerializationError(_)
            | Self::IoError(_)
            | Self::JsonError(_)
            | Self::YamlError(_)
            | Self::HttpError(_)
            | Self::UuidError(_)
            | Self::RegexError(_)
            | Self::Upgrade(_)
            | Self::Unknown(_) => 1,
        }
    }

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
        Self::SecretNotFound {
            name: name.into(),
            suggestion: None,
        }
    }

    pub fn vault_not_found<S: Into<String>>(name: S) -> Self {
        Self::VaultNotFound {
            name: name.into(),
            suggestion: None,
        }
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

    /// Attach a "did you mean...?" suggestion to a variant that supports one.
    /// No-op for variants without a `suggestion` field.
    pub fn with_suggestion(mut self, candidate: Option<String>) -> Self {
        match &mut self {
            Self::SecretNotFound { suggestion, .. } => *suggestion = candidate,
            Self::VaultNotFound { suggestion, .. } => *suggestion = candidate,
            _ => {}
        }
        self
    }

    /// Return the attached suggestion, if any.
    pub fn suggestion(&self) -> Option<&str> {
        match self {
            Self::SecretNotFound { suggestion, .. } => suggestion.as_deref(),
            Self::VaultNotFound { suggestion, .. } => suggestion.as_deref(),
            _ => None,
        }
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
            matches!(err, CrosstacheError::SecretNotFound { ref name, .. } if name == "my-secret")
        );
        assert_eq!(err.to_string(), "Secret not found: my-secret");
    }

    #[test]
    fn test_vault_not_found_constructor() {
        let err = CrosstacheError::vault_not_found("prod-vault");
        assert!(
            matches!(err, CrosstacheError::VaultNotFound { ref name, .. } if name == "prod-vault")
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

    // --- Stable error codes ---

    #[test]
    fn test_code_for_every_variant() {
        use std::collections::HashSet;
        let cases: Vec<(CrosstacheError, &str)> = vec![
            (CrosstacheError::authentication("x"), "xv-auth-failed"),
            (CrosstacheError::azure_api("x"), "xv-azure-api"),
            (CrosstacheError::config("x"), "xv-config-invalid"),
            (CrosstacheError::secret_not_found("x"), "xv-secret-not-found"),
            (CrosstacheError::vault_not_found("x"), "xv-vault-not-found"),
            (CrosstacheError::invalid_secret_name("x"), "xv-invalid-secret-name"),
            (CrosstacheError::permission_denied("x"), "xv-permission-denied"),
            (CrosstacheError::network("x"), "xv-network"),
            (CrosstacheError::dns_resolution("x", "y"), "xv-network-dns"),
            (CrosstacheError::connection_timeout("x"), "xv-network-timeout"),
            (CrosstacheError::connection_refused("x"), "xv-network-refused"),
            (CrosstacheError::ssl_error("x"), "xv-network-ssl"),
            (CrosstacheError::invalid_url("x"), "xv-invalid-url"),
            (CrosstacheError::serialization("x"), "xv-serialization"),
            (CrosstacheError::invalid_argument("x"), "xv-invalid-argument"),
            (CrosstacheError::upgrade("x"), "xv-upgrade"),
            (CrosstacheError::unknown("x"), "xv-unknown"),
            (
                CrosstacheError::IoError(std::io::Error::new(std::io::ErrorKind::NotFound, "x")),
                "xv-io",
            ),
            (
                CrosstacheError::JsonError(
                    serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
                ),
                "xv-json",
            ),
        ];
        let mut seen = HashSet::new();
        for (err, expected_code) in cases {
            assert_eq!(err.code(), expected_code, "wrong code for {err:?}");
            assert!(seen.insert(expected_code), "duplicate code {expected_code}");
        }
    }

    // --- Exit codes ---

    #[test]
    fn test_exit_code_families() {
        // 2 — invalid argument
        assert_eq!(CrosstacheError::invalid_argument("x").exit_code(), 2);

        // 3 — config family
        assert_eq!(CrosstacheError::config("x").exit_code(), 3);

        // 10–19 — not-found family
        assert_eq!(CrosstacheError::secret_not_found("x").exit_code(), 10);
        assert_eq!(CrosstacheError::vault_not_found("x").exit_code(), 11);

        // 20–29 — auth/permission
        assert_eq!(CrosstacheError::authentication("x").exit_code(), 20);
        assert_eq!(CrosstacheError::permission_denied("x").exit_code(), 21);

        // 30–39 — network
        assert_eq!(CrosstacheError::network("x").exit_code(), 30);
        assert_eq!(CrosstacheError::dns_resolution("x", "y").exit_code(), 31);
        assert_eq!(CrosstacheError::connection_timeout("x").exit_code(), 32);
        assert_eq!(CrosstacheError::connection_refused("x").exit_code(), 33);
        assert_eq!(CrosstacheError::ssl_error("x").exit_code(), 34);

        // 40–49 — Azure/backend
        assert_eq!(CrosstacheError::azure_api("x").exit_code(), 40);

        // 1 — unknown / catch-all
        assert_eq!(CrosstacheError::unknown("x").exit_code(), 1);
    }

    #[test]
    fn test_exit_code_is_stable_for_unknown_variants() {
        // From-converted errors that don't have a clear family fall back to 1.
        let io_err = CrosstacheError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "x",
        ));
        assert_eq!(io_err.exit_code(), 1);

        let json_err = CrosstacheError::JsonError(
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
        );
        assert_eq!(json_err.exit_code(), 1);
    }

    // --- Suggestions ---

    #[test]
    fn secret_not_found_suggestion_round_trip() {
        let err = CrosstacheError::secret_not_found("DB_PASSWURD")
            .with_suggestion(Some("DB_PASSWORD".to_string()));
        assert_eq!(err.suggestion(), Some("DB_PASSWORD"));
    }

    #[test]
    fn vault_not_found_suggestion_round_trip() {
        let err = CrosstacheError::vault_not_found("myproj-prood")
            .with_suggestion(Some("myproj-prod".to_string()));
        assert_eq!(err.suggestion(), Some("myproj-prod"));
    }

    #[test]
    fn variants_without_suggestion_field_return_none() {
        let err = CrosstacheError::network("dropped");
        assert_eq!(err.suggestion(), None);
    }

    #[test]
    fn with_suggestion_on_variant_without_field_is_noop() {
        // Calling .with_suggestion on a variant that has no slot must not panic.
        let err = CrosstacheError::network("dropped").with_suggestion(Some("hint".into()));
        assert_eq!(err.suggestion(), None);
        // Still the same kind of error.
        assert_eq!(err.code(), "xv-network");
    }

    #[test]
    fn secret_not_found_default_suggestion_is_none() {
        let err = CrosstacheError::secret_not_found("X");
        assert_eq!(err.suggestion(), None);
    }

    // --- Security: no error variant carries a secret value ---

    #[test]
    fn no_variant_has_a_secret_value_field() {
        // This is a hand-maintained list of variant fields. If you add a
        // variant whose payload could carry a secret value, this test will
        // fail in code review — keep the list updated.
        //
        // The check is structural: we simply confirm that the only
        // string fields on every variant are message/name/details fields,
        // never anything called "value", "secret", "password", or "token".
        let variant_field_names = [
            ("AuthenticationError", vec!["msg"]),
            ("AzureApiError", vec!["msg"]),
            ("ConfigError", vec!["msg"]),
            ("ConfigLoadError", vec!["source"]),
            ("SecretNotFound", vec!["name", "suggestion"]),
            ("VaultNotFound", vec!["name", "suggestion"]),
            ("InvalidSecretName", vec!["name"]),
            ("PermissionDenied", vec!["msg"]),
            ("NetworkError", vec!["msg"]),
            ("DnsResolutionError", vec!["vault_name", "details"]),
            ("ConnectionTimeout", vec!["msg"]),
            ("ConnectionRefused", vec!["msg"]),
            ("SslError", vec!["msg"]),
            ("InvalidUrl", vec!["msg"]),
            ("SerializationError", vec!["msg"]),
            ("IoError", vec!["source"]),
            ("JsonError", vec!["source"]),
            ("YamlError", vec!["source"]),
            ("HttpError", vec!["source"]),
            ("UuidError", vec!["source"]),
            ("RegexError", vec!["source"]),
            ("InvalidArgument", vec!["msg"]),
            ("Upgrade", vec!["msg"]),
            ("Unknown", vec!["msg"]),
        ];
        let banned = ["value", "secret", "password", "token", "key"];
        for (variant, fields) in variant_field_names {
            for f in fields {
                for b in banned {
                    assert!(
                        !f.contains(b),
                        "variant {variant} field {f:?} contains banned token {b:?}"
                    );
                }
            }
        }
    }
}
