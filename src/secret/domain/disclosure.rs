//! Types that carry plaintext on purpose. Everything here is a reviewed disclosure boundary.

use serde::{Deserialize, Serialize};
use tabled::Tabled;

use crate::utils::helpers::parse_connection_string;

/// Connection string component
#[derive(Debug, Clone, Serialize, Deserialize, Tabled)]
pub struct ConnectionComponent {
    #[tabled(rename = "Key")]
    pub key: String,
    #[tabled(rename = "Value")]
    pub value: String,
    #[tabled(rename = "Description")]
    pub description: String,
}

/// Human-readable description for a connection-string key. Pure string
/// mapping — no manager/backend state required.
pub fn connection_string_key_description(key: &str) -> String {
    match key.to_lowercase().as_str() {
        "server" | "hostname" => "Database server hostname or IP address".to_string(),
        "database" | "initial catalog" => "Database name".to_string(),
        "user id" | "uid" | "username" => "Username for authentication".to_string(),
        "password" | "pwd" => "Password for authentication".to_string(),
        "port" => "Port number for database connection".to_string(),
        "encrypt" | "ssl" => "Enable SSL/TLS encryption".to_string(),
        "trust server certificate" => "Trust server certificate without validation".to_string(),
        "connection timeout" => "Connection timeout in seconds".to_string(),
        "command timeout" => "Command execution timeout in seconds".to_string(),
        "application name" => "Application name for connection".to_string(),
        _ => "Connection parameter".to_string(),
    }
}

/// Parse a connection string into described components, without needing a
/// [`SecretManager`]. Wraps [`crate::utils::helpers::parse_connection_string`]
/// (the raw key/value parser) and annotates each pair with a description.
pub fn parse_connection_components(connection_string: &str) -> Vec<ConnectionComponent> {
    parse_connection_string(connection_string)
        .into_iter()
        .map(|(key, value)| ConnectionComponent {
            description: connection_string_key_description(&key),
            key,
            value,
        })
        .collect()
}
