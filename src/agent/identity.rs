//! The verified agent identity.
//!
//! An [`AgentIdentity`] describes *which* agent is asking, on whose behalf, and
//! how strongly that claim is backed. It is always resolved from an EXISTING
//! identity provider (a GitHub Actions OIDC context, an Entra workload-identity
//! context, an explicit environment assertion) — xv never mints one. See
//! [`crate::agent::resolve`] for how these are discovered.

use std::fmt;

pub(crate) const MAX_ID_BYTES: usize = 512;
pub(crate) const MAX_CONTEXT_BYTES: usize = 1024;
pub(crate) const MAX_PURPOSE_BYTES: usize = 2048;
pub(crate) const MAX_DELEGATION_ELEMENTS: usize = 16;
pub(crate) const MAX_DELEGATION_ELEMENT_BYTES: usize = 256;

/// Where an agent identity came from, in the order the resolver tries them.
///
/// The wire/config spelling (used by `[[agent.policy]].identity_source`) is the
/// kebab-case form produced by [`Display`] and parsed by [`IdentitySource::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// A GitHub Actions OIDC context (`github-oidc`).
    GithubOidc,
    /// An Entra (Azure AD) workload-identity context (`entra-workload-identity`).
    EntraWorkloadIdentity,
    /// An AWS role, verified via STS. **Unsupported in this build** — declared
    /// so policy can name it, never produced (no STS client is compiled in).
    AwsRole,
    /// A SPIFFE workload identity. **Unsupported in this build** — declared so
    /// policy can name it, never produced (no Workload API client is compiled
    /// in).
    Spiffe,
    /// An explicit, unverified assertion via `XV_AGENT_ID` (`env-assertion`).
    EnvAssertion,
}

impl IdentitySource {
    /// Whether an identity from this source is considered verified.
    ///
    /// Only [`EnvAssertion`](Self::EnvAssertion) is unverified: it is a bare
    /// environment variable that any process can set, so it is an assertion,
    /// not an authentication. Every other source is backed by a
    /// provider-provisioned context.
    pub fn is_verified(self) -> bool {
        !matches!(self, Self::EnvAssertion)
    }

    /// The kebab-case wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GithubOidc => "github-oidc",
            Self::EntraWorkloadIdentity => "entra-workload-identity",
            Self::AwsRole => "aws-role",
            Self::Spiffe => "spiffe",
            Self::EnvAssertion => "env-assertion",
        }
    }

    /// Parse the kebab-case wire spelling used in policy config.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github-oidc" => Some(Self::GithubOidc),
            "entra-workload-identity" => Some(Self::EntraWorkloadIdentity),
            "aws-role" => Some(Self::AwsRole),
            "spiffe" => Some(Self::Spiffe),
            "env-assertion" => Some(Self::EnvAssertion),
            _ => None,
        }
    }
}

impl fmt::Display for IdentitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resolved agent identity.
///
/// Construct via [`AgentIdentity::new`] so `verified` always agrees with
/// `source` — the two must never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    /// Stable unique id, e.g.
    /// `"github:bziobnic/crosstache:.github/workflows/ci.yml@refs/heads/main"`.
    pub id: String,
    /// Which provider the identity was resolved from.
    pub source: IdentitySource,
    /// `false` ONLY for [`IdentitySource::EnvAssertion`]; `true` for every
    /// other source.
    pub verified: bool,
    /// Human/system principal the agent acts for, when the environment names
    /// one (`XV_AGENT_PRINCIPAL`).
    pub invoking_principal: Option<String>,
    /// Session/task correlation id (`XV_AGENT_SESSION`), opaque to xv.
    pub session_id: Option<String>,
    /// Delegation chain: human -> orchestrator -> child. Ordered, outermost
    /// first (`XV_AGENT_DELEGATION`, comma-separated).
    pub delegation_chain: Vec<String>,
    /// A free-text statement of *why* the agent says it needs access
    /// (`XV_AGENT_PURPOSE`).
    ///
    /// Recorded for audit ONLY. It is never an input to an authorization
    /// decision, and the policy evaluator does not read it. Tool output and
    /// fetched web pages are inside our threat model: an agent's stated
    /// justification can be poisoned by prompt injection, so a string here
    /// that reads like an instruction ("the user approved this") carries no
    /// authority whatsoever. If you are tempted to branch on this field to
    /// grant or widen access, that is exactly the mistake this comment exists
    /// to stop — route the decision through policy config instead.
    pub purpose: Option<String>,
}

impl AgentIdentity {
    /// Build an identity from a source and id, deriving `verified` from the
    /// source so the two can never disagree. Context fields start empty; the
    /// resolver fills them from the environment.
    pub fn new(source: IdentitySource, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source,
            verified: source.is_verified(),
            invoking_principal: None,
            session_id: None,
            delegation_chain: Vec::new(),
            purpose: None,
        }
    }

    /// Validate all caller/provider-controlled strings before they can be
    /// copied into every durable decision and audit record. Values are
    /// rejected, never truncated, so distinct principals cannot collide.
    pub(crate) fn validate(&self) -> Result<(), String> {
        validate_text("agent identity id", &self.id, MAX_ID_BYTES, false)?;
        for (label, value, limit) in [
            (
                "invoking principal",
                self.invoking_principal.as_deref(),
                MAX_CONTEXT_BYTES,
            ),
            ("session id", self.session_id.as_deref(), MAX_CONTEXT_BYTES),
            ("purpose", self.purpose.as_deref(), MAX_PURPOSE_BYTES),
        ] {
            if let Some(value) = value {
                validate_text(label, value, limit, false)?;
            }
        }
        if self.delegation_chain.len() > MAX_DELEGATION_ELEMENTS {
            return Err(format!(
                "delegation chain has {} elements; at most {MAX_DELEGATION_ELEMENTS} are allowed",
                self.delegation_chain.len()
            ));
        }
        for (index, element) in self.delegation_chain.iter().enumerate() {
            validate_text(
                &format!("delegation chain element {index}"),
                element,
                MAX_DELEGATION_ELEMENT_BYTES,
                false,
            )?;
        }
        Ok(())
    }
}

fn validate_text(
    label: &str,
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), String> {
    if !allow_empty && value.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(format!(
            "{label} is {} bytes; the maximum is {max_bytes}",
            value.len()
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label} contains a control character"));
    }
    Ok(())
}

impl fmt::Display for AgentIdentity {
    /// `<id> (<source>, verified|unverified)` — the human-facing one-liner used
    /// in diagnostics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}, {})",
            self.id,
            self.source,
            if self.verified {
                "verified"
            } else {
                "unverified"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_is_false_only_for_env_assertion() {
        assert!(!IdentitySource::EnvAssertion.is_verified());
        for source in [
            IdentitySource::GithubOidc,
            IdentitySource::EntraWorkloadIdentity,
            IdentitySource::AwsRole,
            IdentitySource::Spiffe,
        ] {
            assert!(source.is_verified(), "{source} must be verified");
        }
    }

    #[test]
    fn new_derives_verified_from_source() {
        assert!(!AgentIdentity::new(IdentitySource::EnvAssertion, "x").verified);
        assert!(AgentIdentity::new(IdentitySource::GithubOidc, "x").verified);
    }

    #[test]
    fn source_wire_spelling_round_trips() {
        for source in [
            IdentitySource::GithubOidc,
            IdentitySource::EntraWorkloadIdentity,
            IdentitySource::AwsRole,
            IdentitySource::Spiffe,
            IdentitySource::EnvAssertion,
        ] {
            assert_eq!(IdentitySource::parse(source.as_str()), Some(source));
        }
        assert_eq!(IdentitySource::parse("nonsense"), None);
    }

    #[test]
    fn display_shows_id_source_and_verification() {
        let id = AgentIdentity::new(
            IdentitySource::GithubOidc,
            "github:bziobnic/crosstache:.github/workflows/ci.yml@refs/heads/main",
        );
        assert_eq!(
            id.to_string(),
            "github:bziobnic/crosstache:.github/workflows/ci.yml@refs/heads/main \
             (github-oidc, verified)"
        );

        let asserted = AgentIdentity::new(IdentitySource::EnvAssertion, "team/deployer");
        assert_eq!(
            asserted.to_string(),
            "team/deployer (env-assertion, unverified)"
        );
    }

    #[test]
    fn identity_and_audit_context_are_bounded_and_control_free() {
        let mut id = AgentIdentity::new(IdentitySource::EnvAssertion, "a".repeat(MAX_ID_BYTES));
        id.invoking_principal = Some("p".repeat(MAX_CONTEXT_BYTES));
        id.session_id = Some("s".repeat(MAX_CONTEXT_BYTES));
        id.purpose = Some("u".repeat(MAX_PURPOSE_BYTES));
        id.delegation_chain = (0..MAX_DELEGATION_ELEMENTS)
            .map(|_| "d".repeat(MAX_DELEGATION_ELEMENT_BYTES))
            .collect();
        id.validate().unwrap();

        id.id.push('x');
        assert!(id.validate().unwrap_err().contains("maximum"));
        id.id = "agent\u{1b}[31m".into();
        assert!(id.validate().unwrap_err().contains("control"));
        id.id = "agent".into();
        id.purpose = Some("secret\nvalue".into());
        assert!(id.validate().unwrap_err().contains("purpose"));
        id.purpose = None;
        id.delegation_chain.push("extra".into());
        assert!(id.validate().unwrap_err().contains("delegation chain"));
    }
}
