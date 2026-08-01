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

pub mod addressing;
pub mod audit;
#[cfg(feature = "aws")]
pub mod aws;
pub mod azure;
pub mod error;
#[cfg(feature = "file-ops")]
pub mod file;
pub mod local;
pub mod registry;
pub mod secret;
pub mod vault;

/// Reserved tag name for a secret's user-facing original name (before
/// backend-specific sanitization).
pub const TAG_ORIGINAL_NAME: &str = "original_name";

/// Reserved tag name recording that crosstache wrote a secret.
pub const TAG_CREATED_BY: &str = "created_by";

/// Bookkeeping tags written unconditionally on every Azure secret write —
/// `SecretManager::prepare_secret_request` (create, `src/secret/manager.rs`)
/// and `azure::secrets::build_patched_tags` (attribute-only update,
/// `src/backend/azure/secrets.rs`) — one tag slot each, always consumed
/// regardless of what the caller requests. `records::check_tag_budget`'s
/// pre-check must count these so it can't under-count relative to what the
/// backend actually writes; both call sites reference this constant
/// instead of repeating the literal strings, so the two can't drift apart.
pub const ALWAYS_WRITTEN_TAGS: [&str; 2] = [TAG_ORIGINAL_NAME, TAG_CREATED_BY];

// Re-exports for convenience.
pub use addressing::BackendRef;
pub use audit::{AuditBackend, AuditEvent};
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
    /// Azure Key Vault (the original implementation).
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
    #[cfg_attr(not(feature = "aws"), allow(dead_code))] // used by the AWS backend's name validation
    pub fn is_valid(&self, name: &str) -> bool {
        match self {
            Self::AlphanumericHyphen => name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            Self::Unrestricted => true,
            Self::AwsRelaxed => name.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '=' | '.' | '@' | '-')
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
    /// One backend call can atomically replace a secret value and all conversion metadata.
    pub has_atomic_record_conversion: bool,
    /// Backend can compare an opaque source revision and commit the complete
    /// conversion update at the same provider commit point.
    pub has_conditional_record_conversion: bool,
    /// Backend can atomically guard source revision and destination absence
    /// while moving a complete secret.
    pub has_atomic_rename: bool,
    /// File backend can create a destination only when absent at the provider
    /// commit point, without a check-then-write race.
    pub has_atomic_file_create: bool,
    /// Backend supports preserving/changing the enabled flag.
    pub has_enable_disable: bool,
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
    /// Restore a soft-deleted secret.
    pub has_restore: bool,
    /// Permanently purge a deleted secret on demand.
    pub has_purge: bool,
    /// Backend schedules permanent purge after its recovery window.
    pub has_scheduled_purge: bool,
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
    /// Maximum number of tags per secret (None = unlimited). Used by
    /// `records::check_tag_budget` to fail record writes before they
    /// exceed the backend's tag cap.
    pub max_tags: Option<usize>,
    /// Maximum length of a single tag value (None = unlimited). Used by
    /// `records::check_tag_budget` to fail record writes whose metadata
    /// field value is too long for a tag.
    pub max_tag_value_len: Option<usize>,
}

impl Default for BackendCapabilities {
    /// Returns a minimal capability set (everything disabled, unrestricted names).
    fn default() -> Self {
        Self {
            has_atomic_record_conversion: false,
            has_conditional_record_conversion: false,
            has_atomic_rename: false,
            has_atomic_file_create: false,
            has_enable_disable: false,
            has_vaults: false,
            has_file_storage: false,
            has_rbac: false,
            has_audit: false,
            has_versioning: false,
            has_soft_delete: false,
            has_restore: false,
            has_purge: false,
            has_scheduled_purge: false,
            has_secret_rotation: false,
            has_groups: false,
            has_folders: false,
            has_notes: false,
            has_expiry: false,
            max_secret_size: None,
            max_name_length: None,
            name_charset: NameCharset::Unrestricted,
            max_tags: None,
            max_tag_value_len: None,
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

    /// Access to audit log operations (optional).
    ///
    /// Backends returning `Some` here are dispatched generically by
    /// `xv audit`; Azure keeps its legacy Activity Log path and returns
    /// `None`.
    fn audit(&self) -> Option<&dyn AuditBackend> {
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

/// Whether web record conversion has every advertised and implemented
/// primitive needed for both update and no-op commits.
pub(crate) fn conditional_record_conversion_available(backend: &dyn Backend) -> bool {
    let capabilities = backend.capabilities();
    capabilities.has_atomic_record_conversion
        && capabilities.has_conditional_record_conversion
        && backend.secrets().supports_conditional_update()
        && backend.secrets().supports_revision_validation()
}

/// Whether secret rename has both the advertised guarantee and its backend
/// primitive.
pub(crate) fn atomic_rename_available(backend: &dyn Backend) -> bool {
    backend.capabilities().has_atomic_rename && backend.secrets().supports_atomic_rename()
}

/// Whether file upload conflict policies can use a real create-only primitive.
#[cfg(all(feature = "file-ops", any(feature = "ui", test)))]
pub(crate) fn atomic_file_create_available(backend: &dyn Backend) -> bool {
    backend.capabilities().has_atomic_file_create
        && backend
            .files()
            .is_some_and(FileBackend::supports_atomic_create)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn backend_kind_parses_aws() {
        assert_eq!(BackendKind::from_str("aws").unwrap(), BackendKind::Aws);
        assert_eq!(BackendKind::from_str("AWS").unwrap(), BackendKind::Aws);
        assert_eq!(
            BackendKind::from_str("secretsmanager").unwrap(),
            BackendKind::Aws
        );
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
