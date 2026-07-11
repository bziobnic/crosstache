//! CloudTrail-backed audit log for the AWS backend.
//!
//! Implements [`AuditBackend`] by calling the CloudTrail `LookupEvents` API
//! filtered to the Secrets Manager event source, then post-filtering event
//! resource names by the vault prefix (`vault/secret` naming scheme).
//!
//! CloudTrail returns events newest-first and retains 90 days of management
//! events. Pagination continues until CloudTrail confirms the requested time
//! window is exhausted so unrelated activity cannot hide older vault events.

use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_cloudtrail::primitives::DateTime as AwsDateTime;
use aws_sdk_cloudtrail::types::{Event, LookupAttribute, LookupAttributeKey};
use aws_sdk_cloudtrail::Client as CloudTrailClient;

use crate::backend::audit::{AuditBackend, AuditEvent};
use crate::backend::aws::encoding::strip_prefix;
use crate::backend::aws::errors;
use crate::backend::error::BackendError;

/// The CloudTrail event source for AWS Secrets Manager.
const SECRETS_MANAGER_EVENT_SOURCE: &str = "secretsmanager.amazonaws.com";

/// `LookupEvents` page size (API maximum is 50).
const PAGE_SIZE: i32 = 50;

pub struct AwsAuditBackend {
    client: Arc<CloudTrailClient>,
}

impl AwsAuditBackend {
    pub fn new(client: Arc<CloudTrailClient>) -> Self {
        Self { client }
    }

    /// Query CloudTrail for Secrets Manager events in the lookback window,
    /// keeping only events whose secret belongs to `vault` (and, when given,
    /// matches `secret_filter`).
    async fn lookup(
        &self,
        vault: &str,
        secret_filter: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError> {
        let end_time = chrono::Utc::now();
        let start_time = end_time - chrono::Duration::days(days as i64);

        // For a single-secret audit, scope the CloudTrail lookup to that
        // secret's full (vault-prefixed) name via ResourceName. An
        // EventSource-wide scan pages newest-first across ALL Secrets
        // Manager activity in the account, so in busy accounts the
        // MAX_PAGES window could be exhausted before reaching events for
        // the requested secret. Vault-wide audit keeps the EventSource
        // scan (CloudTrail allows only one lookup attribute per call) and
        // filters per event in `map_event`.
        let attribute = match secret_filter {
            Some(secret) => LookupAttribute::builder()
                .attribute_key(LookupAttributeKey::ResourceName)
                .attribute_value(format!("{vault}/{secret}"))
                .build(),
            None => LookupAttribute::builder()
                .attribute_key(LookupAttributeKey::EventSource)
                .attribute_value(SECRETS_MANAGER_EVENT_SOURCE)
                .build(),
        }
        .map_err(|e| {
            BackendError::Internal(format!("aws LookupEvents: invalid lookup attribute: {e}"))
        })?;

        let mut events: Vec<AuditEvent> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut req = self
                .client
                .lookup_events()
                .lookup_attributes(attribute.clone())
                .start_time(AwsDateTime::from_secs(start_time.timestamp()))
                .end_time(AwsDateTime::from_secs(end_time.timestamp()))
                .max_results(PAGE_SIZE);
            if let Some(ref token) = next_token {
                req = req.next_token(token);
            }

            let resp = req.send().await.map_err(errors::from_lookup_events)?;

            for event in resp.events() {
                if let Some(row) = map_event(event, vault, secret_filter) {
                    events.push(row);
                }
            }

            let new_token = resp.next_token().map(|t| t.to_string());
            // Defensive: a repeated token would loop forever.
            if new_token.is_none() || new_token == next_token {
                break;
            }
            next_token = new_token;
        }

        // CloudTrail already returns newest-first, but make the contract
        // explicit (and stable across pages).
        events.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        Ok(events)
    }
}

#[async_trait]
impl AuditBackend for AwsAuditBackend {
    async fn get_vault_events(
        &self,
        vault: &str,
        _resource_group: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError> {
        // CloudTrail is account-wide; resource groups don't apply on AWS.
        self.lookup(vault, None, days).await
    }

    async fn get_secret_events(
        &self,
        vault: &str,
        secret_name: &str,
        _resource_group: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError> {
        self.lookup(vault, Some(secret_name), days).await
    }
}

// ---------------------------------------------------------------------------
// Pure event -> row mapping helpers (unit-tested, no AWS calls)
// ---------------------------------------------------------------------------

/// Extract the secret name from a Secrets Manager ARN.
///
/// ARNs look like `arn:aws:secretsmanager:<region>:<account>:secret:<name>-XXXXXX`
/// where AWS appends a random 6-character suffix to the name. Returns the
/// tail verbatim when no suffix is recognized.
fn secret_name_from_arn(arn: &str) -> Option<String> {
    let tail = arn.split(":secret:").nth(1)?;
    match tail.rsplit_once('-') {
        Some((name, suffix))
            if suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            Some(name.to_string())
        }
        _ => Some(tail.to_string()),
    }
}

/// Normalize an identifier that may be either a full Secrets Manager ARN or
/// a plain secret name into the plain name.
fn normalize_secret_id(id: &str) -> Option<String> {
    if id.starts_with("arn:") {
        secret_name_from_arn(id)
    } else {
        Some(id.to_string())
    }
}

/// Resolve the full (vault-prefixed) secret name an event refers to, trying
/// the event's resource list first, then `requestParameters.secretId`, then
/// `requestParameters.name` (CloudTrail logs `CreateSecret` with `name`
/// rather than `secretId`) from the raw CloudTrail record.
fn full_secret_name(event: &Event, raw: &serde_json::Value) -> Option<String> {
    for resource in event.resources() {
        if resource.resource_type() != Some("AWS::SecretsManager::Secret") {
            continue;
        }
        if let Some(name) = resource.resource_name().and_then(normalize_secret_id) {
            return Some(name);
        }
    }
    let params = raw.get("requestParameters")?;
    ["secretId", "name"]
        .iter()
        .find_map(|key| params.get(key).and_then(|v| v.as_str()))
        .and_then(normalize_secret_id)
}

/// Map a CloudTrail event to an [`AuditEvent`] row.
///
/// Returns `None` when the event has no usable timestamp, refers to no
/// secret, or its secret is outside the `vault` prefix (or doesn't match
/// `secret_filter` when one is given).
fn map_event(event: &Event, vault: &str, secret_filter: Option<&str>) -> Option<AuditEvent> {
    let raw: serde_json::Value = event
        .cloud_trail_event()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);

    let inner_name = strip_prefix(vault, &full_secret_name(event, &raw)?)?;
    if let Some(filter) = secret_filter {
        if inner_name != filter {
            return None;
        }
    }

    let timestamp = event
        .event_time()
        .and_then(|t| chrono::DateTime::from_timestamp(t.secs(), t.subsec_nanos()))
        .or_else(|| {
            raw.get("eventTime")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })?;

    let operation = event
        .event_name()
        .or_else(|| raw.get("eventName").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();

    let caller = event
        .username()
        .or_else(|| {
            raw.get("userIdentity")
                .and_then(|u| u.get("arn"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            raw.get("userIdentity")
                .and_then(|u| u.get("principalId"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("unknown")
        .to_string();

    let status = raw
        .get("errorCode")
        .and_then(|v| v.as_str())
        .unwrap_or("Succeeded")
        .to_string();

    let source_ip = raw
        .get("sourceIPAddress")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let event_id = event
        .event_id()
        .or_else(|| raw.get("eventID").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    Some(AuditEvent {
        timestamp,
        operation,
        resource_name: inner_name,
        caller,
        status,
        source_ip,
        event_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_cloudtrail::types::Resource;

    const VAULT: &str = "myproj-kv";

    fn arn(full_name: &str) -> String {
        format!("arn:aws:secretsmanager:us-east-1:123456789012:secret:{full_name}-Ab1Cd2")
    }

    fn raw_event(error_code: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "eventVersion": "1.08",
            "eventTime": "2026-06-01T12:34:56Z",
            "eventName": "GetSecretValue",
            "sourceIPAddress": "203.0.113.7",
            "userIdentity": { "arn": "arn:aws:iam::123456789012:user/alice" },
            "requestParameters": { "secretId": format!("{VAULT}/db-password") },
        });
        if let Some(code) = error_code {
            v["errorCode"] = serde_json::json!(code);
        }
        v.to_string()
    }

    fn sample_event() -> Event {
        Event::builder()
            .event_id("evt-123")
            .event_name("GetSecretValue")
            .event_time(AwsDateTime::from_secs(1_780_000_000))
            .username("alice")
            .resources(
                Resource::builder()
                    .resource_type("AWS::SecretsManager::Secret")
                    .resource_name(arn(&format!("{VAULT}/db-password")))
                    .build(),
            )
            .cloud_trail_event(raw_event(None))
            .build()
    }

    #[test]
    fn secret_name_from_arn_strips_random_suffix() {
        assert_eq!(
            secret_name_from_arn(&arn("myproj-kv/db-password")),
            Some("myproj-kv/db-password".to_string())
        );
    }

    #[test]
    fn secret_name_from_arn_keeps_tail_without_suffix() {
        // No recognizable 6-char alphanumeric suffix — return tail verbatim.
        assert_eq!(
            secret_name_from_arn("arn:aws:secretsmanager:us-east-1:1:secret:plain"),
            Some("plain".to_string())
        );
        assert_eq!(secret_name_from_arn("arn:aws:iam::1:user/alice"), None);
    }

    #[test]
    fn map_event_extracts_all_fields() {
        let row = map_event(&sample_event(), VAULT, None).expect("event should map");
        assert_eq!(row.operation, "GetSecretValue");
        assert_eq!(row.resource_name, "db-password");
        assert_eq!(row.caller, "alice");
        assert_eq!(row.status, "Succeeded");
        assert_eq!(row.source_ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(row.event_id, "evt-123");
        assert_eq!(row.timestamp.timestamp(), 1_780_000_000);
    }

    #[test]
    fn map_event_filters_other_vaults() {
        assert!(map_event(&sample_event(), "other-vault", None).is_none());
    }

    #[test]
    fn map_event_applies_secret_filter() {
        assert!(map_event(&sample_event(), VAULT, Some("db-password")).is_some());
        assert!(map_event(&sample_event(), VAULT, Some("api-key")).is_none());
    }

    #[test]
    fn map_event_reports_error_code_as_status() {
        let event = Event::builder()
            .event_id("evt-denied")
            .event_name("GetSecretValue")
            .event_time(AwsDateTime::from_secs(1_780_000_000))
            .username("mallory")
            .resources(
                Resource::builder()
                    .resource_type("AWS::SecretsManager::Secret")
                    .resource_name(arn(&format!("{VAULT}/db-password")))
                    .build(),
            )
            .cloud_trail_event(raw_event(Some("AccessDenied")))
            .build();
        let row = map_event(&event, VAULT, None).expect("event should map");
        assert_eq!(row.status, "AccessDenied");
    }

    #[test]
    fn map_event_falls_back_to_request_parameters() {
        // No resources list — the secret name comes from
        // requestParameters.secretId in the raw record.
        let event = Event::builder()
            .event_id("evt-456")
            .event_name("GetSecretValue")
            .event_time(AwsDateTime::from_secs(1_780_000_000))
            .cloud_trail_event(raw_event(None))
            .build();
        let row = map_event(&event, VAULT, None).expect("event should map");
        assert_eq!(row.resource_name, "db-password");
        // No username on the event — caller falls back to userIdentity.arn.
        assert_eq!(row.caller, "arn:aws:iam::123456789012:user/alice");
    }

    #[test]
    fn map_event_drops_events_without_secret() {
        // ListSecrets-style event: no resources, no secretId.
        let event = Event::builder()
            .event_id("evt-789")
            .event_name("ListSecrets")
            .event_time(AwsDateTime::from_secs(1_780_000_000))
            .cloud_trail_event(r#"{"eventName":"ListSecrets"}"#)
            .build();
        assert!(map_event(&event, VAULT, None).is_none());
    }

    #[test]
    fn map_event_falls_back_to_request_parameters_name() {
        // CreateSecret logs requestParameters.name (not secretId); with no
        // resources list the name fallback must still resolve the secret.
        let raw = format!(
            r#"{{"eventName":"CreateSecret","requestParameters":{{"name":"{VAULT}/db-password"}},"userIdentity":{{"arn":"arn:aws:iam::123456789012:user/alice"}}}}"#
        );
        let event = Event::builder()
            .event_id("evt-create")
            .event_name("CreateSecret")
            .event_time(AwsDateTime::from_secs(1_780_000_000))
            .cloud_trail_event(raw)
            .build();
        let row = map_event(&event, VAULT, None).expect("CreateSecret event should map");
        assert_eq!(row.resource_name, "db-password");
        assert_eq!(row.operation, "CreateSecret");
    }

    #[test]
    fn map_event_uses_raw_timestamp_when_event_time_missing() {
        let event = Event::builder()
            .event_id("evt-raw-ts")
            .event_name("GetSecretValue")
            .cloud_trail_event(raw_event(None))
            .build();
        let row = map_event(&event, VAULT, None).expect("event should map");
        assert_eq!(
            row.timestamp,
            chrono::DateTime::parse_from_rfc3339("2026-06-01T12:34:56Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );
    }

    #[test]
    fn map_event_handles_malformed_raw_json() {
        // Unparseable CloudTrailEvent payload — resource list still resolves
        // the name; status defaults to Succeeded; no source IP.
        let event = Event::builder()
            .event_id("evt-bad-json")
            .event_name("DeleteSecret")
            .event_time(AwsDateTime::from_secs(1_780_000_000))
            .username("alice")
            .resources(
                Resource::builder()
                    .resource_type("AWS::SecretsManager::Secret")
                    .resource_name(arn(&format!("{VAULT}/db-password")))
                    .build(),
            )
            .cloud_trail_event("{not-json")
            .build();
        let row = map_event(&event, VAULT, None).expect("event should map");
        assert_eq!(row.status, "Succeeded");
        assert_eq!(row.source_ip, None);
    }
}
