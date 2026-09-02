//! Agent identity resolution, policy evaluation, enforcement, and decision auditing.

mod decision_log;
pub(crate) use decision_log::DecisionLog;
pub mod enforce;
pub mod identity;
pub mod policy;
pub mod resolve;

#[allow(unused_imports)]
// public facade; the binary's duplicate module tree does not use every export
pub use identity::{AgentIdentity, IdentitySource};
#[allow(unused_imports)]
// public facade; the binary's duplicate module tree does not use every export
pub use policy::{AccessRequest, Decision, DenyReason, Operation};

/// Agent context carried through an allowed backend future so local audit
/// records can bind policy attribution into their v2 MAC preimage.
#[derive(Clone)]
pub(crate) struct AuditContext {
    pub identity: AgentIdentity,
    pub policy_version: String,
    pub decision: String,
}

tokio::task_local! {
    pub(crate) static AUDIT_CONTEXT: AuditContext;
}
