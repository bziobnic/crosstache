//! Append-only policy decision log.
//!
//! This is a standalone use of the local audit log's HMAC chain machinery,
//! rather than an independently implemented chain format.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;

use super::{AgentIdentity, Decision, Operation};
use crate::backend::error::BackendError;
#[cfg(test)]
use crate::backend::local::audit::AuditRecord;
use crate::backend::local::audit::{LocalAuditLog, PolicyDecisionRecord};
use crate::backend::local::crypto::load_identity;
use crate::utils::helpers::create_private_dir;

const DECISION_VAULT: &str = "decisions";

/// Persistent decision sink used by an enforced backend.
pub(crate) struct DecisionLog {
    chain: LocalAuditLog,
}

impl DecisionLog {
    pub(crate) fn open_default() -> Result<Self, BackendError> {
        let state = state_dir()?;
        create_private_dir(&state)
            .map_err(|e| BackendError::Internal(format!("create agent state directory: {e}")))?;
        let key_path = state.join("agent-decisions.age-key");
        let identity = load_or_create_identity(&key_path)?;
        Ok(Self {
            chain: LocalAuditLog::new_standalone(state.join("agent-decisions.jsonl"), &identity),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(path: PathBuf) -> Self {
        let identity = age::x25519::Identity::generate();
        Self {
            chain: LocalAuditLog::new_standalone(path, &identity),
        }
    }

    pub(crate) fn record(
        &self,
        identity: &AgentIdentity,
        workspace: &str,
        resource: &str,
        operation: Operation,
        decision: &Decision,
        policy_version: &str,
    ) -> Result<(), BackendError> {
        let (label, deny_reason) = match decision {
            Decision::Allow { .. } => ("allow", None),
            Decision::Deny { reason } => ("deny", Some(reason.to_string())),
        };
        let requested_resource = format!("{workspace}/{resource}");
        self.chain.record_policy_decision(
            DECISION_VAULT,
            &PolicyDecisionRecord {
                identity,
                operation: operation.as_str(),
                resource_name: &requested_resource,
                decision: label,
                matched_rule: decision.matched_rule(),
                deny_reason: deny_reason.as_deref(),
                policy_version,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn read_all(&self) -> Result<Vec<AuditRecord>, BackendError> {
        self.chain.read_all(DECISION_VAULT)
    }
}

fn state_dir() -> Result<PathBuf, BackendError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(root).join("xv"));
        }
        let home = dirs::home_dir().ok_or_else(|| {
            BackendError::Internal(
                "cannot locate agent decision log: HOME is not set and no home directory is available"
                    .into(),
            )
        })?;
        Ok(home.join(".local/state/xv"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        dirs::data_local_dir()
            .map(|root| root.join("xv"))
            .ok_or_else(|| {
                BackendError::Internal(
                    "cannot locate a directory for the agent decision log".into(),
                )
            })
    }
}

fn load_or_create_identity(path: &Path) -> Result<age::x25519::Identity, BackendError> {
    if path.exists() {
        return load_identity(path);
    }
    let identity = age::x25519::Identity::generate();
    let rendered = identity.to_string();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(rendered.expose_secret().as_bytes())
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|e| {
                    BackendError::Internal(format!(
                        "write agent decision-log key {}: {e}",
                        path.display()
                    ))
                })?;
            Ok(identity)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => load_identity(path),
        Err(error) => Err(BackendError::Internal(format!(
            "create agent decision-log key {}: {error}",
            path.display()
        ))),
    }
}
