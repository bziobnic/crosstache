//! Azure Activity Log-backed audit implementation.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::auth::provider::AzureAuthProvider;
use crate::backend::audit::{AuditBackend, AuditEvent};
use crate::backend::error::BackendError;
use crate::error::{CrosstacheError, Result};

use super::map_error;

/// Azure Activity Log audit adapter.
///
/// The backend trait only accepts a vault name, so Azure stores the default
/// subscription/resource group from the active config and uses them for trait
/// dispatch. CLI calls that pass `--resource-group` still use the legacy path
/// until the trait API grows an explicit resource-group parameter.
pub struct AzureAuditBackend {
    auth_provider: Arc<dyn AzureAuthProvider>,
    subscription_id: String,
    default_resource_group: String,
}

impl AzureAuditBackend {
    pub fn new(
        auth_provider: Arc<dyn AzureAuthProvider>,
        subscription_id: String,
        default_resource_group: String,
    ) -> Self {
        Self {
            auth_provider,
            subscription_id,
            default_resource_group,
        }
    }

    async fn get_vault_audit_logs(
        &self,
        resource_group: &str,
        vault_name: &str,
        days: u32,
    ) -> Result<Vec<AuditEvent>> {
        let end_time = chrono::Utc::now();
        let start_time = end_time - chrono::Duration::days(days as i64);

        let start_time_str = start_time.format("%Y-%m-%dT%H:%M:%S.%3fZ");
        let end_time_str = end_time.format("%Y-%m-%dT%H:%M:%S.%3fZ");

        let activity_url = format!(
            "https://management.azure.com/subscriptions/{}/providers/microsoft.insights/eventtypes/management/values?api-version=2015-04-01&$filter=eventTimestamp ge '{}' and eventTimestamp le '{}' and resourceUri eq '/subscriptions/{}/resourceGroups/{}/providers/Microsoft.KeyVault/vaults/{}'",
            self.subscription_id,
            start_time_str,
            end_time_str,
            self.subscription_id,
            resource_group,
            vault_name
        );

        let token = self
            .auth_provider
            .get_token(&["https://management.azure.com/.default"])
            .await?;

        let response = reqwest::Client::new()
            .get(&activity_url)
            .header("Authorization", format!("Bearer {}", token.token.secret()))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| CrosstacheError::network(format!("Failed to fetch activity logs: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_default();
            return Err(CrosstacheError::azure_api(format!(
                "Activity Log API returned {status}: {error_text}"
            )));
        }

        let activity_response: Value = response.json().await.map_err(|e| {
            CrosstacheError::serialization(format!("Failed to parse activity logs: {e}"))
        })?;

        parse_activity_log_response(activity_response)
    }

    async fn get_secret_audit_logs(
        &self,
        resource_group: &str,
        vault_name: &str,
        secret_name: &str,
        days: u32,
    ) -> Result<Vec<AuditEvent>> {
        let vault_logs = self
            .get_vault_audit_logs(resource_group, vault_name, days)
            .await?;

        Ok(vault_logs
            .into_iter()
            .filter(|log| log.resource_name.contains(secret_name))
            .collect())
    }
}

#[async_trait]
impl AuditBackend for AzureAuditBackend {
    async fn get_vault_events(
        &self,
        vault: &str,
        days: u32,
    ) -> std::result::Result<Vec<AuditEvent>, BackendError> {
        self.get_vault_audit_logs(&self.default_resource_group, vault, days)
            .await
            .map_err(map_error)
    }

    async fn get_secret_events(
        &self,
        vault: &str,
        secret_name: &str,
        days: u32,
    ) -> std::result::Result<Vec<AuditEvent>, BackendError> {
        self.get_secret_audit_logs(&self.default_resource_group, vault, secret_name, days)
            .await
            .map_err(map_error)
    }
}

fn parse_activity_log_response(response: Value) -> Result<Vec<AuditEvent>> {
    let mut entries = Vec::new();

    if let Some(value) = response.get("value").and_then(|v| v.as_array()) {
        for event in value {
            if let Ok(entry) = parse_activity_log_entry(event) {
                entries.push(entry);
            }
        }
    }

    entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));

    Ok(entries)
}

fn parse_activity_log_entry(event: &Value) -> Result<AuditEvent> {
    let timestamp = event
        .get("eventTimestamp")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok_or_else(|| CrosstacheError::serialization("Invalid timestamp in activity log"))?;

    let operation = event
        .get("operationName")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let resource_name = event
        .get("resourceId")
        .and_then(|v| v.as_str())
        .and_then(|resource_id| resource_id.split('/').next_back())
        .unwrap_or("")
        .to_string();

    let caller = event
        .get("caller")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let status = event
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(
            event
                .get("subStatus")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
        )
        .to_string();

    let event_id = event
        .get("eventDataId")
        .or_else(|| event.get("correlationId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(AuditEvent {
        timestamp,
        operation,
        resource_name,
        caller,
        status,
        source_ip: None,
        event_id,
    })
}
