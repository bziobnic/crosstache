use crate::error::{crosstacheError, Result};
use reqwest::Client;
use std::time::Duration;

/// Configuration for HTTP client with proper timeouts and user-friendly error handling
pub struct NetworkConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub user_agent: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
            user_agent: format!("crosstache/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Create a properly configured HTTP client with timeouts
pub fn create_http_client(config: &NetworkConfig) -> Result<Client> {
    Client::builder()
        .connect_timeout(config.connect_timeout)
        .timeout(config.request_timeout)
        .user_agent(&config.user_agent)
        .build()
        .map_err(|e| crosstacheError::network(format!("Failed to create HTTP client: {}", e)))
}

/// Enhanced network error classification and user-friendly error messages
pub fn classify_network_error(error: &reqwest::Error, url: &str) -> crosstacheError {
    // Extract vault name from URL for better error messages
    let vault_name = extract_vault_name_from_url(url);

    // Check for timeout errors
    if error.is_timeout() {
        return crosstacheError::connection_timeout(format!(
            "Connection to Azure Key Vault '{}' timed out. This might be due to network issues or the vault being unreachable.",
            vault_name
        ));
    }

    // Check for connection errors (often DNS-related)
    if error.is_connect() {
        // Try to determine if it's a DNS issue
        if is_dns_resolution_error(error) {
            return crosstacheError::dns_resolution(
                vault_name.clone(),
                format!("Unable to resolve vault hostname. Please check if the vault name '{}' is correct and the vault exists.", vault_name)
            );
        }

        // Check for connection refused
        if error
            .to_string()
            .to_lowercase()
            .contains("connection refused")
        {
            return crosstacheError::connection_refused(format!(
                "Connection to Azure Key Vault '{}' was refused. The service may be temporarily unavailable.", 
                vault_name
            ));
        }

        return crosstacheError::network(format!(
            "Failed to connect to Azure Key Vault '{}'. Please check your network connection and verify the vault name.", 
            vault_name
        ));
    }

    // Check for SSL/TLS errors
    if error.to_string().to_lowercase().contains("ssl")
        || error.to_string().to_lowercase().contains("tls")
        || error.to_string().to_lowercase().contains("certificate")
    {
        return crosstacheError::ssl_error(format!(
            "SSL/TLS connection error when accessing vault '{}'. This may be due to certificate issues or network security policies.", 
            vault_name
        ));
    }

    // Check for invalid URL errors
    if error.is_request() {
        return crosstacheError::invalid_url(format!(
            "Invalid request to vault '{}'. Please check the vault name and URL format.",
            vault_name
        ));
    }

    // Check for specific HTTP status codes that indicate network issues
    if let Some(status) = error.status() {
        match status.as_u16() {
            503 => return crosstacheError::network(format!(
                "Azure Key Vault '{}' service is temporarily unavailable. Please try again later.", 
                vault_name
            )),
            502 | 504 => return crosstacheError::network(format!(
                "Gateway error when accessing vault '{}'. The Azure service may be experiencing issues.", 
                vault_name
            )),
            _ => {}
        }
    }

    // Default network error with helpful message
    crosstacheError::network(format!(
        "Network error when accessing vault '{}': {}. Please check your internet connection and try again.", 
        vault_name, error
    ))
}

/// Enhanced DNS error detection
fn is_dns_resolution_error(error: &reqwest::Error) -> bool {
    let error_msg = error.to_string().to_lowercase();
    let dns_indicators = [
        "dns",
        "name resolution",
        "resolve",
        "lookup",
        "name or service not known",
        "nodename nor servname provided",
        "temporary failure in name resolution",
        "no such host",
        "host not found",
        "getaddrinfo failed",
        "name resolution failed",
        "could not resolve host",
    ];

    dns_indicators
        .iter()
        .any(|&indicator| error_msg.contains(indicator))
}

/// Extract vault name from Azure Key Vault URL
fn extract_vault_name_from_url(url: &str) -> String {
    // Parse URL to extract vault name from format: https://{vault}.vault.azure.net/...
    if let Ok(parsed_url) = url::Url::parse(url) {
        if let Some(host) = parsed_url.host_str() {
            if host.ends_with(".vault.azure.net") {
                // Extract vault name (everything before .vault.azure.net)
                return host.replace(".vault.azure.net", "");
            }
        }
    }

    // Fallback: try to extract from string pattern
    if url.contains(".vault.azure.net") {
        if let Some(start) = url.find("://") {
            if let Some(end) = url[start + 3..].find(".vault.azure.net") {
                return url[start + 3..start + 3 + end].to_string();
            }
        }
    }

    // Last resort: return a generic placeholder
    "unknown-vault".to_string()
}

/// Check if a network error is retryable
pub fn is_retryable_error(error: &crosstacheError) -> bool {
    match error {
        crosstacheError::ConnectionTimeout(_) => true,
        crosstacheError::NetworkError(msg) => {
            // Retry on temporary network issues
            let msg_lower = msg.to_lowercase();
            msg_lower.contains("timeout")
                || msg_lower.contains("temporary")
                || msg_lower.contains("503")
                || msg_lower.contains("502")
                || msg_lower.contains("504")
        }
        crosstacheError::AzureApiError(msg) => {
            // Retry on specific Azure API errors
            let msg_lower = msg.to_lowercase();
            msg_lower.contains("503")
                || msg_lower.contains("502")
                || msg_lower.contains("504")
                || msg_lower.contains("throttled")
        }
        crosstacheError::DnsResolutionError { .. } => false, // DNS errors are usually not transient
        crosstacheError::ConnectionRefused(_) => false, // Connection refused is usually persistent
        crosstacheError::SslError(_) => false, // SSL errors are usually configuration issues
        crosstacheError::InvalidUrl(_) => false, // URL errors are not retryable
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_vault_name_from_url() {
        let url = "https://test-vault.vault.azure.net/secrets/test-secret";
        assert_eq!(extract_vault_name_from_url(url), "test-vault");
    }

    #[test]
    fn test_is_retryable_error() {
        let timeout_error = crosstacheError::connection_timeout("timeout");
        assert!(is_retryable_error(&timeout_error));

        let dns_error = crosstacheError::dns_resolution("vault", "DNS failed");
        assert!(!is_retryable_error(&dns_error));
    }
}
