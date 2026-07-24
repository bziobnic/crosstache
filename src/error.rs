use thiserror::Error;

/// Display-safe setup failure for desktop recovery and diagnostics.
///
/// Every string is sanitized before construction. The raw provider error is
/// retained only as bounded, redacted diagnostics; it must never be placed in
/// `message`, `hint`, or a scope field.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[allow(dead_code)] // Consumed by the desktop recovery adapter in Task 3.
pub struct SafeSetupError {
    pub code: String,
    pub operation: String,
    pub backend: String,
    pub vault: String,
    pub message: String,
    pub hint: String,
    pub diagnostics: String,
}

#[allow(dead_code)] // Consumed by the desktop recovery adapter in Task 3.
impl SafeSetupError {
    /// Create a generic safe setup failure from an untrusted diagnostic.
    pub fn from_message(message: &str) -> Self {
        Self::from_parts(
            "xv-backend-internal",
            "setup",
            "unknown",
            "",
            "Setup could not be completed.",
            "Review the setup fields and provider login, then try again.",
            message,
        )
    }

    /// Classify an application failure and retain only redacted diagnostics.
    pub fn from_error(
        operation: &str,
        backend: &str,
        vault: &str,
        error: &CrosstacheError,
    ) -> Self {
        use CrosstacheError::*;

        let (code, message, hint) = match error {
            AuthenticationError(_) => (
                "xv-auth-failed",
                "Authentication with the selected backend failed.",
                setup_auth_hint(backend),
            ),
            ConfigError(_)
            | ConfigLoadError(_)
            | InvalidArgument(_)
            | InvalidUrl(_)
            | SerializationError(_) => (
                "xv-config-invalid",
                "The setup configuration is invalid.",
                "Review the setup fields and try again.",
            ),
            PermissionDenied(_) => (
                "xv-permission-denied",
                "The selected identity does not have permission to list this vault.",
                setup_permission_hint(backend),
            ),
            NetworkError(_)
            | DnsResolutionError { .. }
            | ConnectionTimeout(_)
            | ConnectionRefused(_)
            | SslError(_)
            | HttpError(_) => (
                "xv-network",
                "The selected backend could not be reached.",
                "Check the network connection and provider service, then try again.",
            ),
            _ => (
                "xv-backend-internal",
                "The selected backend could not complete setup verification.",
                setup_backend_hint(backend),
            ),
        };

        Self::from_parts(
            code,
            operation,
            backend,
            vault,
            message,
            hint,
            &error.to_string(),
        )
    }

    fn from_parts(
        code: &str,
        operation: &str,
        backend: &str,
        vault: &str,
        message: &str,
        hint: &str,
        diagnostics: &str,
    ) -> Self {
        Self {
            code: safe_setup_text(code, 64),
            operation: safe_setup_text(operation, 64),
            backend: safe_setup_text(backend, 64),
            vault: safe_setup_text(vault, 256),
            message: safe_setup_text(message, 512),
            hint: safe_setup_text(hint, 512),
            diagnostics: redact_setup_diagnostics(diagnostics),
        }
    }
}

fn setup_auth_hint(backend: &str) -> &'static str {
    match backend {
        "azure" => "Run 'az login', select the intended tenant and subscription, then try again.",
        "aws" => {
            "Run 'aws sso login' or refresh the configured AWS credential chain, then try again."
        }
        _ => "Check the configured local identity and try again.",
    }
}

fn setup_permission_hint(backend: &str) -> &'static str {
    match backend {
        "azure" => "Check Azure Key Vault data-plane access for the selected identity.",
        "aws" => "Check IAM permission to list secrets for the configured AWS identity.",
        _ => "Check access to the configured local store and key.",
    }
}

fn setup_backend_hint(backend: &str) -> &'static str {
    match backend {
        "azure" => "Check Azure service status and the non-secret setup fields, then try again.",
        "aws" => "Check AWS service status and the non-secret setup fields, then try again.",
        _ => "Check the local store configuration and try again.",
    }
}

fn safe_setup_text(value: &str, max_chars: usize) -> String {
    crate::utils::format::sanitize_control_chars(value)
        .chars()
        .take(max_chars)
        .collect()
}

fn looks_like_opaque_token(candidate: &str) -> bool {
    if is_safe_camel_case_identifier(candidate) || is_safe_diagnostic_scalar(candidate) {
        return false;
    }

    let has_upper = candidate.bytes().any(|byte| byte.is_ascii_uppercase());
    let has_lower = candidate.bytes().any(|byte| byte.is_ascii_lowercase());
    let has_digit = candidate.bytes().any(|byte| byte.is_ascii_digit());
    let punctuation_kinds = b"._~+/=-"
        .iter()
        .filter(|punctuation| candidate.as_bytes().contains(punctuation))
        .count();
    let is_long_hex = candidate.len() >= 32
        && has_digit
        && candidate.bytes().all(|byte| byte.is_ascii_hexdigit());
    let is_long_single_case_alpha =
        candidate.len() >= 32 && candidate.bytes().all(|byte| byte.is_ascii_alphabetic());
    let is_long_alphanumeric = candidate.len() >= 24
        && has_digit
        && candidate.bytes().all(|byte| byte.is_ascii_alphanumeric());
    let has_token_padding = candidate.len() >= 20 && candidate.ends_with('=');

    punctuation_kinds >= 2
        || (has_digit && punctuation_kinds >= 1)
        || (has_upper && has_lower && has_digit)
        || is_long_hex
        || is_long_single_case_alpha
        || is_long_alphanumeric
        || has_token_padding
}

fn is_safe_camel_case_identifier(candidate: &str) -> bool {
    if !candidate.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !candidate
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_uppercase())
    {
        return false;
    }

    candidate
        .as_bytes()
        .windows(2)
        .filter(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
        .count()
        >= 2
}

fn is_safe_diagnostic_scalar(candidate: &str) -> bool {
    let Some((key, value)) = candidate.split_once('=') else {
        return false;
    };
    if value.contains('=') {
        return false;
    }

    let key = key.to_ascii_lowercase();
    if key.ends_with("version") {
        return numeric_segments(value, &['.', '-']);
    }
    if key == "retry-after" {
        return value
            .strip_suffix("-seconds")
            .or_else(|| value.strip_suffix("-milliseconds"))
            .is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
            })
            || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()));
    }
    if key == "request-id" {
        return value.strip_prefix("req-").is_some_and(|number| {
            !number.is_empty()
                && number.len() <= 12
                && number.bytes().all(|byte| byte.is_ascii_digit())
        });
    }
    key == "status-code" && value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn numeric_segments(value: &str, separators: &[char]) -> bool {
    !value.is_empty()
        && value
            .split(|character| separators.contains(&character))
            .all(|segment| {
                !segment.is_empty()
                    && segment.len() <= 4
                    && segment.bytes().all(|byte| byte.is_ascii_digit())
            })
        && value
            .chars()
            .any(|character| separators.contains(&character))
}

fn redact_setup_diagnostics(value: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    fn pattern(slot: &'static OnceLock<Regex>, expression: &str) -> &'static Regex {
        slot.get_or_init(|| Regex::new(expression).expect("setup redaction regex must compile"))
    }

    static AUTH_HEADER: OnceLock<Regex> = OnceLock::new();
    static URL: OnceLock<Regex> = OnceLock::new();
    static SENSITIVE_PAIR: OnceLock<Regex> = OnceLock::new();
    static AUTH_SCHEME: OnceLock<Regex> = OnceLock::new();
    static GUID: OnceLock<Regex> = OnceLock::new();
    static AWS_ACCESS_KEY: OnceLock<Regex> = OnceLock::new();
    static AWS_ACCOUNT_ID: OnceLock<Regex> = OnceLock::new();
    static JWT: OnceLock<Regex> = OnceLock::new();
    static OPAQUE_TOKEN: OnceLock<Regex> = OnceLock::new();
    static UNC_PATH: OnceLock<Regex> = OnceLock::new();
    static WINDOWS_PATH: OnceLock<Regex> = OnceLock::new();
    static UNIX_PATH: OnceLock<Regex> = OnceLock::new();

    // Bound attacker/provider-controlled input before repeated decoding and
    // matching. Decode a small fixed number of layers so encoded URLs,
    // headers, query credentials, and userinfo reach the same full-entity
    // redaction rules as their literal forms.
    let mut safe: String = value.chars().take(16_384).collect();
    for _ in 0..4 {
        let decoded = percent_encoding::percent_decode_str(&safe)
            .decode_utf8_lossy()
            .into_owned();
        if decoded == safe {
            break;
        }
        safe = decoded;
    }
    safe = crate::utils::format::sanitize_control_chars(&safe);

    // Redact broad, structured entities before token/path substrings. This
    // prevents later patterns from leaving a prefix or suffix of the same
    // credential visible.
    safe = pattern(
        &AUTH_HEADER,
        r"(?im)\b(?:authorization|proxy-authorization|x-api-key|x-amz-security-token|cookie|set-cookie)\s*:\s*[^\r\n]+",
    )
    .replace_all(&safe, "[AUTH HEADER REDACTED]")
    .into_owned();
    safe = pattern(&URL, r#"(?i)\b(?:https?|ftp)://[^\s<>"';]+"#)
        .replace_all(&safe, "[URL REDACTED]")
        .into_owned();
    safe = pattern(
        &SENSITIVE_PAIR,
        r#"(?i)\b(?:authorization|proxy-authorization|client[_-]?secret|secret[_-]?access[_-]?key|aws[_-]?secret[_-]?access[_-]?key|access[_-]?key(?:[_-]?id)?|access[_-]?token|refresh[_-]?token|id[_-]?token|password|passwd|token|api[_-]?key|accountkey|sharedaccesssignature|sig(?:nature)?|credential|client[_-]?assertion)\b\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s;,]+)"#,
    )
    .replace_all(&safe, "[CREDENTIAL REDACTED]")
    .into_owned();
    safe = pattern(
        &AUTH_SCHEME,
        r"(?i)\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]+",
    )
    .replace_all(&safe, "[AUTH CREDENTIAL REDACTED]")
    .into_owned();
    safe = pattern(
        &GUID,
        r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
    )
    .replace_all(&safe, "[IDENTIFIER REDACTED]")
    .into_owned();
    safe = pattern(&AWS_ACCESS_KEY, r"(?i)\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")
        .replace_all(&safe, "[IDENTIFIER REDACTED]")
        .into_owned();
    safe = pattern(&AWS_ACCOUNT_ID, r"\b[0-9]{12}\b")
        .replace_all(&safe, "[IDENTIFIER REDACTED]")
        .into_owned();
    safe = pattern(
        &UNC_PATH,
        r#"(?i)(?:\\\\|//)[^\\/\s;,'"]+(?:[\\/][^\\/\s;,'"]+)+"#,
    )
    .replace_all(&safe, "[PATH REDACTED]")
    .into_owned();
    safe = pattern(
        &WINDOWS_PATH,
        r#"(?i)\b[A-Z]:\\(?:[^\\\s;,'"]+\\?)*[^\\\s;,'"]*"#,
    )
    .replace_all(&safe, "[PATH REDACTED]")
    .into_owned();
    safe = pattern(&UNIX_PATH, r#"(^|[\s=:'"(])((?:/[^\s;,'"()]+)+)"#)
        .replace_all(&safe, "$1[PATH REDACTED]")
        .into_owned();
    safe = pattern(
        &JWT,
        r"\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
    )
    .replace_all(&safe, "[TOKEN REDACTED]")
    .into_owned();
    safe = pattern(&OPAQUE_TOKEN, r"[A-Za-z0-9._~+/=-]{20,}")
        .replace_all(&safe, |captures: &regex::Captures<'_>| {
            let candidate = &captures[0];
            if looks_like_opaque_token(candidate) {
                "[TOKEN REDACTED]".to_string()
            } else {
                candidate.to_string()
            }
        })
        .into_owned();

    // Truncate only after all replacements, using Unicode scalar boundaries.
    safe_setup_text(&safe, 2048)
}

/// Render the `EnvNotDefined` message. When `available` is empty the
/// `.xv.toml` in question defines zero `[env.*]` blocks at all (a
/// types-only project file, see #331) rather than simply lacking the
/// requested name, so the message says so instead of printing a
/// confusing empty "available: " list.
fn format_env_not_defined(name: &str, available: &[String]) -> String {
    if available.is_empty() {
        format!(
            "Environment '{name}' requested, but .xv.toml defines no environments (no [env.*] blocks)"
        )
    } else {
        format!(
            "Environment '{name}' not defined in .xv.toml; available: {}",
            available.join(", ")
        )
    }
}

/// Render the `AmbiguousSecret` message: names every workspace alias the
/// secret was found in and gives the qualified `alias:name` form for each,
/// per spec §Read semantics.
fn format_ambiguous_secret(name: &str, candidates: &[String]) -> String {
    let qualified: Vec<String> = candidates.iter().map(|a| format!("{a}:{name}")).collect();
    format!(
        "{name} exists in {} — qualify as {}",
        candidates.join(", "),
        qualified.join(" or ")
    )
}

/// Main error type for crosstache operations
#[derive(Debug, Error)]
pub enum CrosstacheError {
    #[error("Authentication failed: {0}")]
    AuthenticationError(String),

    #[error("Azure API error: {0}")]
    AzureApiError(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Backend '{backend}' is not available: {reason}")]
    #[allow(dead_code)]
    BackendUnavailable { backend: String, reason: String },

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

    #[error("{}", format_env_not_defined(name, available))]
    EnvNotDefined {
        name: String,
        available: Vec<String>,
    },

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

    #[error("Scan detected {count} potential leak(s)")]
    ScanLeakDetected { count: usize },

    #[error("Rename of secret '{source}' to '{destination}' in vault '{vault}' is incomplete: the new secret was created, but deleting the original failed: {cause}. Both secrets still exist and no secret material was lost. Next steps: with vault '{vault}' active, verify the new secret (`xv get {destination}`), then delete the original (`xv delete {source}`) or retry the deletion later.")]
    RenameIncomplete {
        source: String,
        destination: String,
        vault: String,
        #[source]
        cause: Box<CrosstacheError>,
    },

    #[error("{}", format_ambiguous_secret(name, candidates))]
    AmbiguousSecret {
        name: String,
        /// Workspace aliases where `name` was found, in a stable order
        /// (matches the workspace's entry order).
        candidates: Vec<String>,
    },

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
            Self::Conflict(_) => "xv-conflict",
            Self::RateLimited(_) => "xv-rate-limited",
            Self::ConfigError(_) => "xv-config-invalid",
            Self::ConfigLoadError(_) => "xv-config-invalid",
            Self::BackendUnavailable { .. } => "xv-backend-unavailable",
            Self::SecretNotFound { .. } => "xv-secret-not-found",
            Self::VaultNotFound { .. } => "xv-vault-not-found",
            Self::EnvNotDefined { .. } => "xv-env-not-defined",
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
            Self::ScanLeakDetected { .. } => "xv-scan-leak-detected",
            Self::RenameIncomplete { .. } => "xv-rename-incomplete",
            Self::AmbiguousSecret { .. } => "xv-ambiguous-secret",
            Self::Unknown(_) => "xv-unknown",
        }
    }

    /// Process exit code for this error. Codes group by family; see
    /// `docs/exit-codes.md` for the public table.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::InvalidArgument(_) => 2,
            Self::ConfigError(_) | Self::ConfigLoadError(_) | Self::EnvNotDefined { .. } => 3,
            Self::BackendUnavailable { .. } => 3,

            Self::SecretNotFound { .. } => 10,
            Self::VaultNotFound { .. } => 11,

            Self::AuthenticationError(_) => 20,
            Self::PermissionDenied(_) => 21,

            Self::NetworkError(_) => 30,
            Self::DnsResolutionError { .. } => 31,
            Self::ConnectionTimeout(_) => 32,
            Self::ConnectionRefused(_) => 33,
            Self::SslError(_) => 34,
            Self::InvalidUrl(_) => 35,

            Self::AzureApiError(_) => 40,
            Self::Conflict(_) => 41,
            Self::RateLimited(_) => 42,
            Self::RenameIncomplete { .. } => 43,

            // 10–19 — secret-family errors (workspace ambiguity is a
            // read-resolution failure, same family as "secret not found").
            Self::AmbiguousSecret { .. } => 13,

            // 50–59 — policy/scan findings
            Self::ScanLeakDetected { .. } => 50,

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

    #[allow(dead_code)]
    pub fn conflict<S: Into<String>>(msg: S) -> Self {
        Self::Conflict(msg.into())
    }

    #[allow(dead_code)]
    pub fn rate_limited<S: Into<String>>(msg: S) -> Self {
        Self::RateLimited(msg.into())
    }

    pub fn config<S: Into<String>>(msg: S) -> Self {
        Self::ConfigError(msg.into())
    }

    #[allow(dead_code)]
    pub fn backend_unavailable<S: Into<String>, R: Into<String>>(backend: S, reason: R) -> Self {
        Self::BackendUnavailable {
            backend: backend.into(),
            reason: reason.into(),
        }
    }

    #[allow(dead_code)]
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

    pub fn env_not_defined<S: Into<String>>(name: S, available: Vec<String>) -> Self {
        Self::EnvNotDefined {
            name: name.into(),
            available,
        }
    }

    /// Build the `EnvNotDefined` variant for a `.xv.toml` that defines zero
    /// `[env.*]` blocks at all, reached when an explicit `--env`/`XV_ENV`
    /// selection is made against such a file (issue #331). The implicit
    /// "no envs defined, no selection" case never reaches this — see
    /// `crate::config::project::resolve_env`, which treats that as "no
    /// active profile" rather than an error.
    pub fn env_not_defined_no_envs<S: Into<String>>(name: S) -> Self {
        Self::EnvNotDefined {
            name: name.into(),
            available: Vec::new(),
        }
    }

    pub fn scan_leak_detected(count: usize) -> Self {
        Self::ScanLeakDetected { count }
    }

    /// Build the `AmbiguousSecret` variant (exit 13, `xv-ambiguous-secret`):
    /// an unqualified read matched `name` in two or more attached workspace
    /// vaults. `candidates` are the aliases it was found in.
    pub fn ambiguous_secret<S: Into<String>>(name: S, candidates: Vec<String>) -> Self {
        Self::AmbiguousSecret {
            name: name.into(),
            candidates,
        }
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
    fn test_permission_denied_constructor() {
        let err = CrosstacheError::permission_denied("read not allowed");
        assert!(matches!(err, CrosstacheError::PermissionDenied(ref s) if s == "read not allowed"));
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
        assert!(matches!(err, CrosstacheError::ConnectionTimeout(ref s) if s == "30s elapsed"));
        assert_eq!(err.to_string(), "Connection timeout: 30s elapsed");
    }

    #[test]
    fn test_connection_refused_constructor() {
        let err = CrosstacheError::connection_refused("port 443 closed");
        assert!(matches!(err, CrosstacheError::ConnectionRefused(ref s) if s == "port 443 closed"));
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
        assert!(matches!(err, CrosstacheError::SerializationError(ref s) if s == "bad JSON"));
        assert_eq!(err.to_string(), "Serialization error: bad JSON");
    }

    #[test]
    fn test_invalid_argument_constructor() {
        let err = CrosstacheError::invalid_argument("--format missing");
        assert!(matches!(err, CrosstacheError::InvalidArgument(ref s) if s == "--format missing"));
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
        assert!(matches!(err, CrosstacheError::Unknown(ref s) if s == "something went wrong"));
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
            (CrosstacheError::conflict("x"), "xv-conflict"),
            (CrosstacheError::rate_limited("x"), "xv-rate-limited"),
            (CrosstacheError::config("x"), "xv-config-invalid"),
            (
                CrosstacheError::backend_unavailable("aws", "not compiled in"),
                "xv-backend-unavailable",
            ),
            (
                CrosstacheError::secret_not_found("x"),
                "xv-secret-not-found",
            ),
            (CrosstacheError::vault_not_found("x"), "xv-vault-not-found"),
            (
                CrosstacheError::permission_denied("x"),
                "xv-permission-denied",
            ),
            (CrosstacheError::network("x"), "xv-network"),
            (CrosstacheError::dns_resolution("x", "y"), "xv-network-dns"),
            (
                CrosstacheError::connection_timeout("x"),
                "xv-network-timeout",
            ),
            (
                CrosstacheError::connection_refused("x"),
                "xv-network-refused",
            ),
            (CrosstacheError::ssl_error("x"), "xv-network-ssl"),
            (CrosstacheError::invalid_url("x"), "xv-invalid-url"),
            (CrosstacheError::serialization("x"), "xv-serialization"),
            (
                CrosstacheError::invalid_argument("x"),
                "xv-invalid-argument",
            ),
            (CrosstacheError::upgrade("x"), "xv-upgrade"),
            (
                CrosstacheError::RenameIncomplete {
                    source: "old".into(),
                    destination: "new".into(),
                    vault: "v".into(),
                    cause: Box::new(CrosstacheError::unknown("x")),
                },
                "xv-rename-incomplete",
            ),
            (CrosstacheError::unknown("x"), "xv-unknown"),
            (
                CrosstacheError::ambiguous_secret(
                    "DB_PASSWORD",
                    vec!["work".into(), "stage".into()],
                ),
                "xv-ambiguous-secret",
            ),
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
        assert_eq!(
            CrosstacheError::backend_unavailable("aws", "x").exit_code(),
            3
        );

        // 10–19 — not-found family
        assert_eq!(CrosstacheError::secret_not_found("x").exit_code(), 10);
        assert_eq!(CrosstacheError::vault_not_found("x").exit_code(), 11);
        assert_eq!(
            CrosstacheError::ambiguous_secret("x", vec!["a".into(), "b".into()]).exit_code(),
            13
        );

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
        assert_eq!(CrosstacheError::conflict("x").exit_code(), 41);
        assert_eq!(CrosstacheError::rate_limited("x").exit_code(), 42);
        assert_eq!(
            CrosstacheError::RenameIncomplete {
                source: "old".into(),
                destination: "new".into(),
                vault: "v".into(),
                cause: Box::new(CrosstacheError::network("x")),
            }
            .exit_code(),
            43
        );

        // 1 — unknown / catch-all
        assert_eq!(CrosstacheError::unknown("x").exit_code(), 1);
    }

    #[test]
    fn ambiguous_secret_message_lists_qualified_forms() {
        let err =
            CrosstacheError::ambiguous_secret("DB_PASSWORD", vec!["work".into(), "stage".into()]);
        let msg = err.to_string();
        assert!(msg.contains("DB_PASSWORD exists in work, stage"), "{msg}");
        assert!(msg.contains("work:DB_PASSWORD"), "{msg}");
        assert!(msg.contains("stage:DB_PASSWORD"), "{msg}");
        assert_eq!(err.code(), "xv-ambiguous-secret");
        assert_eq!(err.exit_code(), 13);
    }

    #[test]
    fn test_exit_code_is_stable_for_unknown_variants() {
        // From-converted errors that don't have a clear family fall back to 1.
        let io_err =
            CrosstacheError::IoError(std::io::Error::new(std::io::ErrorKind::NotFound, "x"));
        assert_eq!(io_err.exit_code(), 1);

        let json_err = CrosstacheError::JsonError(
            serde_json::from_str::<serde_json::Value>("not json").unwrap_err(),
        );
        assert_eq!(json_err.exit_code(), 1);
    }

    #[test]
    fn rename_incomplete_names_both_copies_and_the_recovery_steps() {
        let err = CrosstacheError::RenameIncomplete {
            source: "old-name".into(),
            destination: "new-name".into(),
            vault: "my-vault".into(),
            cause: Box::new(CrosstacheError::network("dial tcp: timeout")),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("'old-name'") && msg.contains("'new-name'"),
            "{msg}"
        );
        assert!(msg.contains("vault 'my-vault'"), "{msg}");
        assert!(msg.contains("Both secrets still exist"), "{msg}");
        assert!(
            msg.contains("`xv get new-name`") && msg.contains("`xv delete old-name`"),
            "recovery steps missing: {msg}"
        );
        assert!(
            msg.contains("dial tcp: timeout"),
            "cause not surfaced: {msg}"
        );
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

    // --- Security: serialized/diagnostic surfaces must not grow secret values ---

    #[derive(Clone, Copy)]
    struct SecuritySurface<'a> {
        category: &'a str,
        name: &'a str,
        fields: &'a [&'a str],
        allowed_value_like_fields: &'a [&'a str],
    }

    fn assert_no_value_like_fields(surfaces: &[SecuritySurface<'_>]) {
        let banned = [
            "value", "secret", "password", "token", "key", "raw", "match",
        ];
        for surface in surfaces {
            for field in surface.fields {
                for token in banned {
                    let value_like = field.to_lowercase().contains(token);
                    let explicitly_allowed = surface.allowed_value_like_fields.contains(field);
                    assert!(
                        !value_like || explicitly_allowed,
                        "{} {} field {field:?} contains value-like token {token:?}; \
                         either rename it or explicitly justify it in allowed_value_like_fields",
                        surface.category,
                        surface.name,
                    );
                }
            }
        }
    }

    #[test]
    fn serialized_security_surfaces_have_no_value_like_fields() {
        // Hand-maintained structural guard for surfaces that are safe only if
        // they carry metadata, never secret material. If a future field really
        // is metadata despite a scary name (for example `secret_name`), add a
        // local allowlist entry so code review sees the justification.
        let surfaces = [
            // Error variants: all payloads are names, messages, counts, causes,
            // or suggestions. No variant should carry a secret value.
            SecuritySurface {
                category: "error variant",
                name: "AuthenticationError",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "AzureApiError",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "Conflict",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "RateLimited",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "ConfigError",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "BackendUnavailable",
                fields: &["backend", "reason"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "SecretNotFound",
                fields: &["name", "suggestion"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "VaultNotFound",
                fields: &["name", "suggestion"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "EnvNotDefined",
                fields: &["name", "available"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "PermissionDenied",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "NetworkError",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "DnsResolutionError",
                fields: &["vault_name", "details"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "ConnectionTimeout",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "ConnectionRefused",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "SslError",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "InvalidUrl",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "SerializationError",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "IoError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "JsonError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "YamlError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "HttpError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "UuidError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "RegexError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "ConfigLoadError",
                fields: &["source"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "InvalidArgument",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "Upgrade",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "ScanLeakDetected",
                fields: &["count"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "RenameIncomplete",
                fields: &["source", "destination", "vault", "cause"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "AmbiguousSecret",
                fields: &["name", "candidates"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "error variant",
                name: "Unknown",
                fields: &["msg"],
                allowed_value_like_fields: &[],
            },
            // Cache entries/status are safe metadata envelopes. `data` is the
            // typed payload selected by callers; the envelope itself must not
            // introduce a value/secret/password/token field.
            SecuritySurface {
                category: "cache entry",
                name: "CacheEntry",
                fields: &["created_at", "ttl_secs", "vault_name", "entry_type", "data"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "cache entry",
                name: "CacheEntryInfo",
                fields: &["key", "created_at", "expires_at", "size_bytes", "is_stale"],
                allowed_value_like_fields: &["key"],
            },
            SecuritySurface {
                category: "cache entry",
                name: "CacheStatus",
                fields: &[
                    "cache_dir",
                    "enabled",
                    "ttl_secs",
                    "entry_count",
                    "total_size_bytes",
                    "entries",
                ],
                allowed_value_like_fields: &[],
            },
            // Scan findings are serialized to JSON/YAML and printed to stderr;
            // they may identify the secret by name but must never carry bytes
            // that matched the scanner.
            SecuritySurface {
                category: "scan finding",
                name: "Finding",
                fields: &[
                    "file",
                    "line",
                    "col",
                    "secret_name",
                    "vault",
                    "kind",
                    "severity",
                ],
                allowed_value_like_fields: &["secret_name"],
            },
            SecuritySurface {
                category: "scan finding",
                name: "FindingKind",
                fields: &["secret-value", "pattern", "high-entropy"],
                allowed_value_like_fields: &["secret-value"],
            },
            SecuritySurface {
                category: "scan finding",
                name: "Severity",
                fields: &["critical", "high", "medium", "low"],
                allowed_value_like_fields: &[],
            },
            // Common structured output/cache payloads are metadata summaries.
            // SecretSummary intentionally contains only names/properties; full
            // SecretProperties/SecretRequest can carry values and must never be
            // added here as cache-safe or leak-scan-safe payloads.
            SecuritySurface {
                category: "structured output",
                name: "SecretSummary",
                fields: &[
                    "name",
                    "original_name",
                    "note",
                    "folder",
                    "groups",
                    "updated_on",
                    "enabled",
                    "expires_on",
                    "content_type",
                ],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "VaultSummary",
                fields: &["name", "location", "resource_group", "status", "created_at"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "FileInfo",
                fields: &[
                    "name",
                    "size",
                    "content_type",
                    "last_modified",
                    "etag",
                    "groups",
                    "metadata",
                    "tags",
                ],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "BlobListItem::Directory",
                fields: &["name", "full_path"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "FileRow",
                fields: &["kind", "name", "size", "content_type", "modified", "groups"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "AuditRow",
                fields: &["timestamp", "operation", "resource", "caller", "status"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "FindRow",
                fields: &["name", "score", "folder", "groups"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "EnvRow",
                fields: &["name", "active", "backend", "vault", "resource_group"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "structured output",
                name: "ContextItem",
                fields: &[
                    "status",
                    "vault",
                    "resource_group",
                    "last_used",
                    "usage_count",
                ],
                allowed_value_like_fields: &[],
            },
            // `config show`'s row type intentionally carries non-secret
            // config settings (booleans, paths, ttls) under generic
            // `key`/`value` column names; it never carries secret material,
            // so both value-like tokens are explicitly allowed here.
            SecuritySurface {
                category: "structured output",
                name: "ConfigItem",
                fields: &["key", "value", "source"],
                allowed_value_like_fields: &["key", "value"],
            },
            // Structured error output and diagnostics expose only envelope
            // metadata. Logs/tracing should log messages, codes, and safe names
            // rather than adding fields for raw values or matched bytes.
            SecuritySurface {
                category: "structured output",
                name: "error envelope",
                fields: &["error", "code", "message", "exit_code", "suggestion"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "log output",
                name: "plain error",
                fields: &["code", "message", "hint", "suggestion"],
                allowed_value_like_fields: &[],
            },
            SecuritySurface {
                category: "tracing output",
                name: "diagnostic event",
                fields: &["target", "level", "message", "error", "backend"],
                allowed_value_like_fields: &[],
            },
        ];

        assert_no_value_like_fields(&surfaces);
    }

    // --- EnvNotDefined ---

    #[test]
    fn test_env_not_defined_constructor() {
        let err = CrosstacheError::env_not_defined(
            "staging",
            vec!["dev".to_string(), "prod".to_string()],
        );
        assert!(matches!(
            err,
            CrosstacheError::EnvNotDefined { ref name, ref available }
                if name == "staging" && available == &vec!["dev".to_string(), "prod".to_string()]
        ));
        assert_eq!(err.code(), "xv-env-not-defined");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn test_env_not_defined_display_lists_available() {
        let err = CrosstacheError::env_not_defined(
            "staging",
            vec!["dev".to_string(), "prod".to_string()],
        );
        let s = err.to_string();
        assert!(
            s.contains("staging"),
            "message must include the missing env name"
        );
        assert!(s.contains("dev"), "message must list 'dev' as available");
        assert!(s.contains("prod"), "message must list 'prod' as available");
    }

    // --- ScanLeakDetected ---

    #[test]
    fn test_scan_leak_detected_constructor() {
        let err = CrosstacheError::scan_leak_detected(3);
        assert!(matches!(
            err,
            CrosstacheError::ScanLeakDetected { count: 3 }
        ));
        assert_eq!(err.code(), "xv-scan-leak-detected");
        assert_eq!(err.exit_code(), 50);
    }

    #[test]
    fn test_scan_leak_detected_display_includes_count() {
        let err = CrosstacheError::scan_leak_detected(7);
        let s = err.to_string();
        assert!(s.contains("7"), "message must include finding count");
        assert!(
            s.to_lowercase().contains("leak") || s.to_lowercase().contains("finding"),
            "message must say 'leak' or 'finding'"
        );
    }
}
