//! Audit log abstraction.
//!
//! Backends that can surface an audit trail (e.g. AWS via CloudTrail)
//! implement [`AuditBackend`] and expose it through
//! [`Backend::audit()`](super::Backend::audit). The CLI renders
//! [`AuditEvent`] rows in the same table/JSON shapes as the Azure
//! Activity Log path.
//!
//! Azure implements this trait with its Activity Log adapter; the CLI keeps a
//! small legacy fallback only for explicit Azure `--resource-group` overrides
//! because this trait is intentionally backend-agnostic.

use async_trait::async_trait;

use super::error::BackendError;

/// A single audit log event, backend-agnostic.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEvent {
    /// When the event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Operation name (e.g. `GetSecretValue`).
    pub operation: String,
    /// User-facing secret name (vault prefix already stripped).
    pub resource_name: String,
    /// Principal that performed the operation (username or ARN).
    pub caller: String,
    /// Outcome: `Succeeded` or the backend's error code.
    pub status: String,
    /// Source IP address, when the backend records one.
    pub source_ip: Option<String>,
    /// Backend-assigned unique event ID.
    pub event_id: String,
}

/// Audit log operations for backends that support them.
///
/// `resource_group` is an optional Azure-only override: on Azure it selects
/// the resource group whose Activity Log is queried (falling back to the
/// backend's configured default when `None`); backends without the concept
/// (e.g. AWS CloudTrail, which is account-wide) ignore it.
#[async_trait]
pub trait AuditBackend: Send + Sync {
    /// Fetch recent events for every secret in the vault, newest first.
    async fn get_vault_events(
        &self,
        vault: &str,
        resource_group: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError>;

    /// Fetch recent events for a single secret, newest first.
    async fn get_secret_events(
        &self,
        vault: &str,
        secret_name: &str,
        resource_group: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError>;
}
