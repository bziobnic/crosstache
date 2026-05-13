//! Backend abstraction layer.
//!
//! This module defines the trait hierarchy that every secrets backend must
//! implement. The core traits are:
//!
//! - [`Backend`] — lifecycle, capability negotiation, sub-trait accessors.
//! - [`SecretBackend`] — CRUD operations on secrets (required).
//! - [`VaultBackend`] — vault/namespace management (optional).
//! - [`FileBackend`] — file/blob storage (optional).
//!
//! Each backend declares its capabilities via [`BackendCapabilities`]. CLI
//! and TUI layers use this to gracefully degrade when a feature is absent
//! (e.g. the local backend has no RBAC).
//!
//! See also: [`BackendError`] for the backend-agnostic error type, and
//! [`BackendRegistry`] for runtime backend resolution.

pub mod azure;
pub mod error;
#[cfg(feature = "file-ops")]
pub mod file;
pub mod local;
pub mod registry;
pub mod secret;
pub mod vault;

// Re-exports for convenience.
pub use error::BackendError;
pub use registry::BackendRegistry;
pub use secret::SecretBackend;
pub use vault::VaultBackend;

#[cfg(feature = "file-ops")]
pub use file::FileBackend;

use async_trait::async_trait;

// ---------------------------------------------------------------------------
// Backend kind enum
// ---------------------------------------------------------------------------

/// Identifies which backend implementation is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// Azure Key Vault (the original, and currently only, implementation).
    Azure,
    /// Local age-encrypted file backend (Phase 2).
    Local,
    /// AWS Secrets Manager backend (Phase 3).
    Aws,
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Azure => write!(f, "azure"),
            Self::Local => write!(f, "local"),
            Self::Aws => write!(f, "aws"),
        }
    }
}

impl std::str::FromStr for BackendKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "azure" | "az" | "keyvault" => Ok(Self::Azure),
            "local" | "file" | "age" => Ok(Self::Local),
            "aws" | "secretsmanager" | "asm" => Ok(Self::Aws),
            _ => Err(format!(
                "unknown backend kind: {s}. Valid options: azure, local, aws"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Name charset
// ---------------------------------------------------------------------------

/// Describes what characters are valid in secret names for a backend.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — consumed by future backends.
pub enum NameCharset {
    /// Only `[a-zA-Z0-9-]` — Azure Key Vault's constraint.
    AlphanumericHyphen,
    /// Any printable character (the backend encodes as needed).
    Unrestricted,
    /// AWS Secrets Manager: `[a-zA-Z0-9/_+=.@-]`.
    AwsRelaxed,
    /// Custom validation function.
    Custom(fn(&str) -> bool),
}

impl NameCharset {
    /// Returns true if `name` is valid under this charset.
    pub fn is_valid(&self, name: &str) -> bool {
        match self {
            Self::AlphanumericHyphen => name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            Self::Unrestricted => true,
            Self::AwsRelaxed => name.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '/' | '_' | '+' | '=' | '.' | '@' | '-')
            }),
            Self::Custom(f) => f(name),
        }
    }
}

// ---------------------------------------------------------------------------
// Backend capabilities
// ---------------------------------------------------------------------------

/// Describes what a backend can do. Used by CLI/TUI for graceful degradation.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — consumed by future backends.
pub struct BackendCapabilities {
    /// Multi-vault/namespace support.
    pub has_vaults: bool,
    /// File/blob storage.
    pub has_file_storage: bool,
    /// Access control / sharing.
    pub has_rbac: bool,
    /// Audit log / activity events.
    pub has_audit: bool,
    /// Secret version history.
    pub has_versioning: bool,
    /// Recoverable (soft) deletion.
    pub has_soft_delete: bool,
    /// Scheduled secret rotation.
    pub has_secret_rotation: bool,
    /// Secret grouping / tagging.
    pub has_groups: bool,
    /// Hierarchical folder organization.
    pub has_folders: bool,
    /// Secret annotations / notes.
    pub has_notes: bool,
    /// Expiration dates on secrets.
    pub has_expiry: bool,
    /// Maximum secret value size in bytes (None = unlimited).
    pub max_secret_size: Option<usize>,
    /// Maximum secret name length (None = unlimited).
    pub max_name_length: Option<usize>,
    /// Valid character set for secret names.
    pub name_charset: NameCharset,
}

impl Default for BackendCapabilities {
    /// Returns a minimal capability set (everything disabled, unrestricted names).
    fn default() -> Self {
        Self {
            has_vaults: false,
            has_file_storage: false,
            has_rbac: false,
            has_audit: false,
            has_versioning: false,
            has_soft_delete: false,
            has_secret_rotation: false,
            has_groups: false,
            has_folders: false,
            has_notes: false,
            has_expiry: false,
            max_secret_size: None,
            max_name_length: None,
            name_charset: NameCharset::Unrestricted,
        }
    }
}

// ---------------------------------------------------------------------------
// Core Backend trait
// ---------------------------------------------------------------------------

/// Core trait every backend must implement.
///
/// Provides lifecycle management (health check), capability negotiation,
/// and access to the sub-trait objects (`secrets()`, `vaults()`, `files()`).
#[allow(dead_code)] // Infrastructure for Phase 2 pluggability — consumed by future backends.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Human-readable backend name, e.g. `"azure"`, `"local"`.
    fn name(&self) -> &'static str;

    /// The kind of backend.
    fn kind(&self) -> BackendKind;

    /// Declared capabilities of this backend.
    fn capabilities(&self) -> BackendCapabilities;

    /// Access to secret operations (required — every backend manages secrets).
    fn secrets(&self) -> &dyn SecretBackend;

    /// Access to vault/namespace operations (optional).
    fn vaults(&self) -> Option<&dyn VaultBackend> {
        None
    }

    /// Access to file/blob operations (optional).
    #[cfg(feature = "file-ops")]
    fn files(&self) -> Option<&dyn FileBackend> {
        None
    }

    /// Validate configuration and connectivity. Called once at startup.
    async fn health_check(&self) -> Result<(), BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn backend_kind_parses_aws() {
        assert_eq!(BackendKind::from_str("aws").unwrap(), BackendKind::Aws);
        assert_eq!(BackendKind::from_str("AWS").unwrap(), BackendKind::Aws);
        assert_eq!(BackendKind::from_str("secretsmanager").unwrap(), BackendKind::Aws);
    }

    #[test]
    fn backend_kind_aws_displays_as_aws() {
        assert_eq!(format!("{}", BackendKind::Aws), "aws");
    }

    #[test]
    fn aws_relaxed_charset_accepts_aws_chars() {
        let cs = NameCharset::AwsRelaxed;
        assert!(cs.is_valid("myproj/db-password"));
        assert!(cs.is_valid("api_key+v2"));
        assert!(cs.is_valid("alice@example.com"));
        assert!(cs.is_valid("v1.2.3"));
    }

    #[test]
    fn aws_relaxed_charset_rejects_invalid_chars() {
        let cs = NameCharset::AwsRelaxed;
        assert!(!cs.is_valid("has space"));
        assert!(!cs.is_valid("has*star"));
        assert!(!cs.is_valid("has(paren)"));
    }
}
