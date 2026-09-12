//! Types that carry plaintext on purpose. Everything here is a reviewed disclosure boundary.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tabled::Tabled;

use crate::utils::helpers::parse_connection_string;

/// A secret whose plaintext has been deliberately released as a plain
/// `String` for serialization at a reviewed boundary.
///
/// Constructed only by [`crate::secret::domain::Secret::disclose`], so
/// `grep -rn "\.disclose(" src` is the complete list of boundaries that
/// serialize a *whole secret value*. The one field-level exception is
/// `xv get --record --format json|yaml`, which serializes decoded envelope
/// fields via `expose_secret` rather than `disclose`; both are listed in
/// `docs/security.md`. `Debug` and `Serialize` are derived on purpose:
/// unlike [`crate::secret::domain::SecretValue`], this type exists to be
/// shown. `value` is deliberately a plain `String`, not zeroized: its only
/// consumers (the `serde_json`/`serde_yaml`/`csv` writers) copy it into
/// unzeroized buffers anyway, so zeroizing here would not add protection.
#[derive(Debug, Clone, Serialize)]
pub struct DisclosedSecret {
    pub name: String,
    pub value: String,
    pub content_type: String,
    pub tags: HashMap<String, String>,
}

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
/// `SecretManager`. Wraps [`crate::utils::helpers::parse_connection_string`]
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
