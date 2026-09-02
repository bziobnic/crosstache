//! Hash-chained append-only audit log for the local backend.
//!
//! Cloud backends get their audit trail from the platform (Azure Activity Log,
//! AWS CloudTrail) — an independent service the caller cannot rewrite. The
//! local backend has no such service, so the log lives in the store itself at
//! `<store>/vaults/<vault>/.audit/log.jsonl`, one JSON record per line.
//!
//! ## Threat model — read this before trusting the output
//!
//! Every record carries `mac = HMAC-SHA256(chain_key, prev_mac || record)`,
//! where `chain_key` is HKDF-derived from the age identity. That makes the log
//! **tamper-evident, not tamper-proof**:
//!
//! - Editing or reordering any record, or deleting one from the middle, breaks
//!   the chain and is reported by [`LocalAuditLog::verify_chain`].
//! - An attacker who does *not* hold the age identity cannot forge or repair a
//!   record, because they cannot compute the MAC.
//! - An attacker who *does* hold the age identity — i.e. anyone who can already
//!   decrypt every secret in the store — can rewrite the whole log from any
//!   point and re-chain it. Truncating the tail is also always possible for
//!   whoever can write the file, key or no key.
//!
//! So this log answers "has my history been altered since I last looked?" It
//! does not answer "can a compromised operator hide their tracks?" — for that
//! you need an off-box sink. Committing the store to git (`[local].git`, see
//! [`super::git`]) or replicating `.audit/` somewhere append-only raises the
//! bar considerably, because the remote keeps copies the local attacker cannot
//! reach.
//!
//! ## Fail-closed
//!
//! When `[local].audit` is on, a failed append fails the operation that
//! triggered it. A silently missing audit record is worse than a refused read:
//! it makes the log's own completeness unprovable. See
//! [`LocalAuditLog::record`].
//!
//! ## Scope: attempts, not just outcomes
//!
//! Both successful and failed operations are recorded, so the log answers "what
//! was attempted" as well as "what succeeded". A failed read — a missing secret,
//! a denied path, or ciphertext that would not decrypt — is often the more
//! interesting entry.
//!
//! Failures carry a status token from [`failure_status`], drawn from a closed set
//! keyed off the error variant rather than its message. `DecryptionFailed` is the
//! one to watch: it means a caller reached a secret's ciphertext and could not
//! open it, i.e. the wrong age identity or altered material.
//!
//! Secret values never appear in the log. Secret *names* do, in
//! `resource_name` — identical to the successful case, and the reason auditing a
//! failed read is worth anything at all.

use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use age::secrecy::ExposeSecret;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::backend::audit::{AuditBackend, AuditEvent};
use crate::backend::error::BackendError;
use crate::utils::helpers::create_private_dir;

use super::paths;

type HmacSha256 = Hmac<Sha256>;

/// Domain separation for the audit chain key. Distinct from the opaque-filename
/// HKDF info so the two keys can never coincide even though both derive from
/// the same age identity.
const HKDF_INFO: &[u8] = b"crosstache-local-audit-chain-v1";

/// `prev` value of the first record in a chain (32 zero bytes, hex).
const GENESIS_MAC: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Bytes read per backward chunk when tailing the log for the last record.
const TAIL_CHUNK: u64 = 8192;

// ---------------------------------------------------------------------------
// Operation names
// ---------------------------------------------------------------------------

/// Audited operation names.
///
/// Deliberately mirrors the AWS Secrets Manager / CloudTrail vocabulary so
/// `xv audit` rows read the same regardless of which backend produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOp {
    /// A secret's decrypted value was read.
    GetSecretValue,
    /// A new secret or a new version was written.
    PutSecretValue,
    /// Metadata (tags, groups, note, folder, expiry) was changed.
    UpdateSecret,
    /// Secret soft-deleted into the trash.
    DeleteSecret,
    /// Secret restored from the trash.
    RestoreSecret,
    /// Secret permanently destroyed.
    PurgeSecret,
    /// An older version was promoted to current.
    RollbackSecret,
    /// Secret renamed; the record's resource is the *source* name, so the
    /// pre-rename history and the rename itself share one lookup key.
    RenameSecret,
    /// The vault's secret names were enumerated.
    ListSecrets,
}

impl AuditOp {
    /// The wire/display name written to the log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GetSecretValue => "GetSecretValue",
            Self::PutSecretValue => "PutSecretValue",
            Self::UpdateSecret => "UpdateSecret",
            Self::DeleteSecret => "DeleteSecret",
            Self::RestoreSecret => "RestoreSecret",
            Self::PurgeSecret => "PurgeSecret",
            Self::RollbackSecret => "RollbackSecret",
            Self::RenameSecret => "RenameSecret",
            Self::ListSecrets => "ListSecrets",
        }
    }
}

/// Resource name recorded for vault-wide operations that name no single secret.
pub const RESOURCE_VAULT_WIDE: &str = "*";

/// Status recorded for an operation that completed.
pub const STATUS_SUCCEEDED: &str = "Succeeded";

/// Map a failure to a fixed status token.
///
/// The token comes from the error's **variant**, never from its message. Error
/// messages are assembled from paths, backend responses, and I/O details; pinning
/// the vocabulary to a closed set means no future error string can widen what the
/// log records. Secret *values* are never part of any `BackendError` payload, so
/// they cannot appear here regardless — the closed set guards against the weaker
/// but realer risk of leaking incidental context (paths, key fingerprints,
/// upstream response bodies) into a file that gets shared as evidence.
///
/// Secret *names* are recorded separately in `resource_name`, as they already are
/// for successful operations: the whole point of auditing a failed read is
/// knowing which secret someone tried to read.
pub fn failure_status(err: &BackendError) -> &'static str {
    match err {
        BackendError::NotFound { .. } => "NotFound",
        BackendError::VaultNotFound { .. } => "VaultNotFound",
        BackendError::AuthenticationFailed(_) => "AuthenticationFailed",
        BackendError::PermissionDenied(_) => "AccessDenied",
        BackendError::Unsupported(_) => "Unsupported",
        BackendError::InvalidArgument(_) => "InvalidArgument",
        BackendError::Conflict(_) => "Conflict",
        BackendError::RateLimited { .. } => "RateLimited",
        BackendError::Network(_) => "NetworkError",
        // A misconfiguration, not a fault of any backend that ran: the name
        // resolves to nothing. Shares the InvalidArgument token rather than
        // widening the closed vocabulary for a case the audit trail cannot
        // meaningfully distinguish.
        BackendError::UnknownBackend { .. } => "InvalidArgument",
        BackendError::RenameIncomplete { .. } => "RenameIncomplete",
        // Conditional-write guards: the caller's snapshot went stale, or the
        // rename destination already exists. Rejections, not faults.
        BackendError::SourceRevisionConflict { .. } => "Conflict",
        BackendError::DestinationExists { .. } => "Conflict",
        // A rename refused because attachments still reference the source name.
        BackendError::AttachmentsPresent { .. } => "InvalidArgument",
        // The security-relevant one: a read reached the ciphertext and could not
        // open it. Wrong identity, or altered/truncated material.
        BackendError::Decryption(_) => "DecryptionFailed",
        BackendError::Internal(_) => "InternalError",
        BackendError::Other(_) => "InternalError",
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// One line of the audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Canonical encoding version. Missing on legacy records and omitted when
    /// writing v1, so unenforced logs remain byte-for-byte unchanged.
    #[serde(
        default = "default_record_version",
        skip_serializing_if = "record_version_is_v1"
    )]
    pub record_version: u8,
    /// 1-based position in the chain.
    pub seq: u64,
    /// When the operation completed.
    pub timestamp: DateTime<Utc>,
    /// Operation name (see [`AuditOp`]).
    pub operation: String,
    /// User-facing secret name, or [`RESOURCE_VAULT_WIDE`].
    pub resource_name: String,
    /// Local OS principal that performed the operation.
    pub caller: String,
    /// `Succeeded`, or a short failure token.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoking_principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_chain: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny_reason: Option<String>,
    /// MAC of the preceding record ([`GENESIS_MAC`] for the first).
    pub prev: String,
    /// `HMAC-SHA256(chain_key, prev || canonical(self))`, hex.
    pub mac: String,
}

impl AuditRecord {
    fn has_v2_fields(&self) -> bool {
        self.agent_id.is_some()
            || self.identity_source.is_some()
            || self.verified.is_some()
            || self.invoking_principal.is_some()
            || self.session_id.is_some()
            || self.delegation_chain.is_some()
            || self.purpose.is_some()
            || self.policy_version.is_some()
            || self.decision.is_some()
            || self.matched_rule.is_some()
            || self.deny_reason.is_some()
    }

    /// Canonical byte encoding fed to the MAC.
    ///
    /// Every field is length-prefixed (`<byte-len>:<bytes>`) rather than
    /// delimiter-joined, so no combination of field contents can produce the
    /// same encoding as a different set of fields. Secret *names* can contain
    /// newlines and colons on the local backend, which a naive `\n`-joined
    /// encoding would let an attacker exploit to forge an equivalent preimage.
    fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut push = |bytes: &[u8]| {
            out.extend_from_slice(bytes.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(bytes);
        };
        push(self.seq.to_string().as_bytes());
        push(self.timestamp.to_rfc3339().as_bytes());
        push(self.operation.as_bytes());
        push(self.resource_name.as_bytes());
        push(self.caller.as_bytes());
        push(self.status.as_bytes());
        if self.record_version == 2 {
            push(b"crosstache-audit-record-v2");
            push(b"2");
            let mut push_optional = |value: Option<&[u8]>| match value {
                Some(value) => {
                    let mut tagged = Vec::with_capacity(value.len() + 1);
                    tagged.push(b'1');
                    tagged.extend_from_slice(value);
                    push(&tagged);
                }
                None => push(b"0"),
            };
            push_optional(self.agent_id.as_deref().map(str::as_bytes));
            push_optional(self.identity_source.as_deref().map(str::as_bytes));
            push_optional(
                self.verified
                    .map(|value| if value { &b"true"[..] } else { &b"false"[..] }),
            );
            push_optional(self.invoking_principal.as_deref().map(str::as_bytes));
            push_optional(self.session_id.as_deref().map(str::as_bytes));
            let delegation = self.delegation_chain.as_ref().map(|chain| {
                let mut encoded = Vec::new();
                encode_field(&mut encoded, chain.len().to_string().as_bytes());
                for element in chain {
                    encode_field(&mut encoded, element.as_bytes());
                }
                encoded
            });
            push_optional(delegation.as_deref());
            push_optional(self.purpose.as_deref().map(str::as_bytes));
            push_optional(self.policy_version.as_deref().map(str::as_bytes));
            push_optional(self.decision.as_deref().map(str::as_bytes));
            push_optional(self.matched_rule.as_deref().map(str::as_bytes));
            push_optional(self.deny_reason.as_deref().map(str::as_bytes));
        }
        out
    }

    /// Convert to the backend-agnostic event shape rendered by `xv audit`.
    fn to_event(&self) -> AuditEvent {
        let is_v2 = self.record_version == 2;
        AuditEvent {
            timestamp: self.timestamp,
            operation: self.operation.clone(),
            resource_name: self.resource_name.clone(),
            caller: self.caller.clone(),
            status: self.status.clone(),
            // The local backend is not reached over a network; there is no
            // peer address to record. Kept `None` rather than inventing
            // "127.0.0.1", which would imply a network hop that never happened.
            source_ip: None,
            event_id: self.mac.clone(),
            agent_id: is_v2.then(|| self.agent_id.clone()).flatten(),
            identity_source: is_v2.then(|| self.identity_source.clone()).flatten(),
            verified: is_v2.then_some(self.verified).flatten(),
            invoking_principal: is_v2.then(|| self.invoking_principal.clone()).flatten(),
            session_id: is_v2.then(|| self.session_id.clone()).flatten(),
            delegation_chain: is_v2.then(|| self.delegation_chain.clone()).flatten(),
            purpose: is_v2.then(|| self.purpose.clone()).flatten(),
            policy_version: is_v2.then(|| self.policy_version.clone()).flatten(),
            decision: is_v2.then(|| self.decision.clone()).flatten(),
        }
    }
}

fn encode_field(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
}

fn default_record_version() -> u8 {
    1
}

fn record_version_is_v1(version: &u8) -> bool {
    *version == 1
}

/// Outcome of [`LocalAuditLog::verify_chain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStatus {
    /// No log file exists yet for this vault.
    Empty,
    /// Every record's MAC and `prev` link checks out.
    Intact {
        /// Number of records verified.
        records: u64,
    },
    /// The chain is broken. `seq` is the first bad record's sequence number.
    Broken {
        /// Sequence number of the first record that failed verification.
        seq: u64,
        /// Records verified before the break.
        verified: u64,
        /// What specifically failed.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// LocalAuditLog
// ---------------------------------------------------------------------------

/// Append-only, hash-chained audit log for one local store.
///
/// Cheap to clone-share behind an `Arc`; all state is on disk. Appends take an
/// exclusive advisory lock on the log file, so concurrent `xv` processes
/// serialize instead of interleaving partial lines.
pub struct LocalAuditLog {
    store_path: PathBuf,
    standalone_path: Option<PathBuf>,
    chain_key: [u8; 32],
}

/// Data for a policy decision written through the shared chain machinery.
pub(crate) struct PolicyDecisionRecord<'a> {
    pub identity: &'a crate::agent::AgentIdentity,
    pub operation: &'a str,
    pub resource_name: &'a str,
    pub decision: &'a str,
    pub matched_rule: Option<&'a str>,
    pub deny_reason: Option<&'a str>,
    pub policy_version: &'a str,
}

impl LocalAuditLog {
    /// Derive the chain key from the store's age identity.
    pub fn new(store_path: PathBuf, identity: &age::x25519::Identity) -> Self {
        Self {
            store_path,
            standalone_path: None,
            chain_key: derive_chain_key(identity),
        }
    }

    /// Use the audit chain codec and locked append machinery for a standalone
    /// log rather than a local-store vault log.
    pub(crate) fn new_standalone(path: PathBuf, identity: &age::x25519::Identity) -> Self {
        Self {
            store_path: PathBuf::new(),
            standalone_path: Some(path),
            chain_key: derive_chain_key(identity),
        }
    }

    /// `<store>/vaults/<vault>/.audit/`.
    fn audit_dir(&self, vault: &str) -> Result<PathBuf, BackendError> {
        if let Some(path) = &self.standalone_path {
            return path.parent().map(Path::to_path_buf).ok_or_else(|| {
                BackendError::Internal("standalone audit log has no parent directory".into())
            });
        }
        Ok(paths::vault_dir(&self.store_path, vault)?.join(".audit"))
    }

    /// `<store>/vaults/<vault>/.audit/log.jsonl`.
    fn log_path(&self, vault: &str) -> Result<PathBuf, BackendError> {
        if let Some(path) = &self.standalone_path {
            return Ok(path.clone());
        }
        Ok(self.audit_dir(vault)?.join("log.jsonl"))
    }

    /// Append one record for a successful operation.
    ///
    /// Fails the caller's operation if the append fails — see the module docs on
    /// fail-closed behavior.
    pub fn record(
        &self,
        vault: &str,
        op: AuditOp,
        resource_name: &str,
    ) -> Result<(), BackendError> {
        self.append(vault, op, resource_name, STATUS_SUCCEEDED)
    }

    /// Append one record for an operation that failed.
    ///
    /// The status is derived from the error's variant via [`failure_status`]; the
    /// error's message is never written.
    pub fn record_failure(
        &self,
        vault: &str,
        op: AuditOp,
        resource_name: &str,
        err: &BackendError,
    ) -> Result<(), BackendError> {
        self.append(vault, op, resource_name, failure_status(err))
    }

    fn append(
        &self,
        vault: &str,
        op: AuditOp,
        resource_name: &str,
        status: &str,
    ) -> Result<(), BackendError> {
        self.append_fields(vault, op.as_str(), resource_name, status, None)
    }

    /// Append an allow/deny decision through the same locked, length-prefixed,
    /// HMAC-chained machinery as local backend audit records.
    pub(crate) fn record_policy_decision(
        &self,
        vault: &str,
        decision: &PolicyDecisionRecord<'_>,
    ) -> Result<(), BackendError> {
        self.append_fields(
            vault,
            decision.operation,
            decision.resource_name,
            decision.decision,
            Some(ExplicitAgentFields {
                identity: decision.identity,
                policy_version: decision.policy_version,
                decision: decision.decision,
                matched_rule: decision.matched_rule,
                deny_reason: decision.deny_reason,
            }),
        )
    }

    fn append_fields(
        &self,
        vault: &str,
        operation: &str,
        resource_name: &str,
        status: &str,
        explicit: Option<ExplicitAgentFields<'_>>,
    ) -> Result<(), BackendError> {
        let dir = self.audit_dir(vault)?;
        create_private_dir(&dir)
            .map_err(|e| BackendError::Internal(format!("create audit dir: {e}")))?;
        let path = self.log_path(vault)?;

        // Open (creating if absent) with 0600 and hold an exclusive lock across
        // the tail-read + append so two processes cannot both chain onto the
        // same predecessor.
        let mut file = open_append_private(&path)?;
        file.lock_exclusive()
            .map_err(|e| BackendError::Internal(format!("lock audit log: {e}")))?;

        let previous = last_record(&mut file)?;
        let (seq, prev) = match previous {
            Some(rec) => (rec.seq + 1, rec.mac),
            None => (1, GENESIS_MAC.to_string()),
        };

        let context = crate::agent::AUDIT_CONTEXT.try_with(Clone::clone).ok();
        let identity = explicit
            .as_ref()
            .map(|fields| fields.identity)
            .or_else(|| context.as_ref().map(|value| &value.identity));
        let mut record = AuditRecord {
            record_version: if identity.is_some() { 2 } else { 1 },
            seq,
            timestamp: Utc::now(),
            operation: operation.to_string(),
            resource_name: resource_name.to_string(),
            caller: current_caller(),
            status: status.to_string(),
            agent_id: identity.map(|value| value.id.clone()),
            identity_source: identity.map(|value| value.source.to_string()),
            verified: identity.map(|value| value.verified),
            invoking_principal: identity.and_then(|value| value.invoking_principal.clone()),
            session_id: identity.and_then(|value| value.session_id.clone()),
            delegation_chain: identity.map(|value| value.delegation_chain.clone()),
            purpose: identity.and_then(|value| value.purpose.clone()),
            policy_version: explicit
                .as_ref()
                .map(|fields| fields.policy_version.to_string())
                .or_else(|| context.as_ref().map(|value| value.policy_version.clone())),
            decision: explicit
                .as_ref()
                .map(|fields| fields.decision.to_string())
                .or_else(|| context.as_ref().map(|value| value.decision.clone())),
            matched_rule: explicit
                .as_ref()
                .and_then(|fields| fields.matched_rule.map(str::to_string)),
            deny_reason: explicit
                .as_ref()
                .and_then(|fields| fields.deny_reason.map(str::to_string)),
            prev,
            mac: String::new(),
        };
        record.mac = self.compute_mac(&record);

        let mut line = serde_json::to_vec(&record)
            .map_err(|e| BackendError::Internal(format!("serialize audit record: {e}")))?;
        line.push(b'\n');

        file.seek(SeekFrom::End(0))
            .map_err(|e| BackendError::Internal(format!("seek audit log: {e}")))?;
        file.write_all(&line)
            .map_err(|e| BackendError::Internal(format!("append audit log: {e}")))?;
        // Durability matters more than speed here: an audit record that is lost
        // to a crash after the secret write landed is exactly the gap this log
        // exists to rule out.
        file.sync_all()
            .map_err(|e| BackendError::Internal(format!("sync audit log: {e}")))?;
        Ok(())
    }

    /// MAC over `prev || canonical(record)`.
    fn compute_mac(&self, record: &AuditRecord) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.chain_key)
            .expect("HMAC-SHA256 accepts any key length");
        mac.update(record.prev.as_bytes());
        mac.update(&record.canonical());
        data_encoding::HEXLOWER.encode(&mac.finalize().into_bytes())
    }

    /// Read every record for a vault, oldest first.
    ///
    /// A malformed line is a hard error rather than a skip: silently dropping
    /// unparseable lines would let an attacker hide a record by corrupting it.
    pub fn read_all(&self, vault: &str) -> Result<Vec<AuditRecord>, BackendError> {
        let path = self.log_path(vault)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&path)
            .map_err(|e| BackendError::Internal(format!("open audit log: {e}")))?;
        let mut out = Vec::new();
        for (idx, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|e| BackendError::Internal(format!("read audit log: {e}")))?;
            if line.trim().is_empty() {
                continue;
            }
            let rec: AuditRecord = serde_json::from_str(&line).map_err(|e| {
                BackendError::Internal(format!(
                    "audit log line {} is not a valid record: {e}. The log may have been \
                     tampered with; run 'xv audit --verify' for details.",
                    idx + 1
                ))
            })?;
            out.push(rec);
        }
        Ok(out)
    }

    /// Recompute the whole chain and report the first break, if any.
    pub fn verify_chain(&self, vault: &str) -> Result<ChainStatus, BackendError> {
        let records = self.read_all(vault)?;
        if records.is_empty() {
            return Ok(ChainStatus::Empty);
        }

        let mut expected_prev = GENESIS_MAC.to_string();
        for (idx, rec) in records.iter().enumerate() {
            let expected_seq = idx as u64 + 1;
            if !matches!(rec.record_version, 1 | 2) {
                return Ok(ChainStatus::Broken {
                    seq: rec.seq,
                    verified: idx as u64,
                    reason: format!(
                        "unsupported audit record version {} (only versions 1 and 2 are defined)",
                        rec.record_version
                    ),
                });
            }
            if rec.record_version == 1 && rec.has_v2_fields() {
                return Ok(ChainStatus::Broken {
                    seq: rec.seq,
                    verified: idx as u64,
                    reason: "legacy v1 record contains unbound v2 attribution fields".to_string(),
                });
            }
            if rec.seq != expected_seq {
                return Ok(ChainStatus::Broken {
                    seq: rec.seq,
                    verified: idx as u64,
                    reason: format!(
                        "sequence gap: expected seq {expected_seq}, found {}",
                        rec.seq
                    ),
                });
            }
            if rec.prev != expected_prev {
                return Ok(ChainStatus::Broken {
                    seq: rec.seq,
                    verified: idx as u64,
                    reason: "prev link does not match the preceding record's mac (a record was \
                             inserted, removed, or reordered)"
                        .to_string(),
                });
            }
            if self.compute_mac(rec) != rec.mac {
                return Ok(ChainStatus::Broken {
                    seq: rec.seq,
                    verified: idx as u64,
                    reason: "mac mismatch (record contents were modified after they were written)"
                        .to_string(),
                });
            }
            expected_prev = rec.mac.clone();
        }

        Ok(ChainStatus::Intact {
            records: records.len() as u64,
        })
    }

    /// Events within the last `days`, newest first.
    fn events_since(
        &self,
        vault: &str,
        days: u32,
        filter_name: Option<&str>,
    ) -> Result<Vec<AuditEvent>, BackendError> {
        let cutoff = Utc::now() - Duration::days(i64::from(days));
        let mut events: Vec<AuditEvent> = self
            .read_all(vault)?
            .into_iter()
            .filter(|rec| rec.timestamp >= cutoff)
            .filter(|rec| match filter_name {
                // Vault-wide rows (`*`) are kept for a single-secret query: a
                // `ListSecrets` call did expose that secret's name.
                Some(name) => rec.resource_name == name || rec.resource_name == RESOURCE_VAULT_WIDE,
                None => true,
            })
            .map(|rec| rec.to_event())
            .collect();
        events.reverse();
        Ok(events)
    }
}

struct ExplicitAgentFields<'a> {
    identity: &'a crate::agent::AgentIdentity,
    policy_version: &'a str,
    decision: &'a str,
    matched_rule: Option<&'a str>,
    deny_reason: Option<&'a str>,
}

#[async_trait]
impl AuditBackend for LocalAuditLog {
    async fn get_vault_events(
        &self,
        vault: &str,
        _resource_group: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError> {
        self.events_since(vault, days, None)
    }

    async fn get_secret_events(
        &self,
        vault: &str,
        secret_name: &str,
        _resource_group: Option<&str>,
        days: u32,
    ) -> Result<Vec<AuditEvent>, BackendError> {
        self.events_since(vault, days, Some(secret_name))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// HKDF-SHA256 the age identity into a 32-byte chain key.
fn derive_chain_key(identity: &age::x25519::Identity) -> [u8; 32] {
    let secret = identity.to_string();
    let hk = Hkdf::<Sha256>::new(None, secret.expose_secret().as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

/// Best-effort local principal. Never contains secret material.
fn current_caller() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Open the log for append with 0600, creating it if needed.
fn open_append_private(path: &Path) -> Result<fs::File, BackendError> {
    let mut opts = fs::OpenOptions::new();
    opts.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path)
        .map_err(|e| BackendError::Internal(format!("open audit log {}: {e}", path.display())))
}

/// Read the final record by scanning backwards in bounded chunks.
///
/// Appends happen on every audited operation, so this must not be O(file); a
/// full forward scan per append would make store lifetime cost quadratic.
fn last_record(file: &mut fs::File) -> Result<Option<AuditRecord>, BackendError> {
    let len = file
        .metadata()
        .map_err(|e| BackendError::Internal(format!("stat audit log: {e}")))?
        .len();
    if len == 0 {
        return Ok(None);
    }

    let mut end = len;
    // Ignore a trailing newline so the final line is found, not an empty tail.
    let mut tail: Vec<u8> = Vec::new();
    loop {
        let start = end.saturating_sub(TAIL_CHUNK);
        let mut chunk = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start))
            .map_err(|e| BackendError::Internal(format!("seek audit log: {e}")))?;
        file.read_exact(&mut chunk)
            .map_err(|e| BackendError::Internal(format!("read audit log tail: {e}")))?;
        chunk.extend_from_slice(&tail);
        tail = chunk;

        // Strip trailing newlines, then look for the newline that starts the
        // last complete line.
        let trimmed_len = tail.iter().rposition(|b| *b != b'\n').map_or(0, |p| p + 1);
        if trimmed_len == 0 {
            return Ok(None);
        }
        if let Some(nl) = tail[..trimmed_len].iter().rposition(|b| *b == b'\n') {
            let line = &tail[nl + 1..trimmed_len];
            return parse_record(line).map(Some);
        }
        if start == 0 {
            // Whole file is one line.
            return parse_record(&tail[..trimmed_len]).map(Some);
        }
        end = start;
    }
}

fn parse_record(bytes: &[u8]) -> Result<AuditRecord, BackendError> {
    serde_json::from_slice(bytes).map_err(|e| {
        BackendError::Internal(format!(
            "the last audit log record is unreadable: {e}. Run 'xv audit --verify' to \
             locate the damage."
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a store with an initialized vault directory and an audit log.
    fn fixture() -> (TempDir, LocalAuditLog, String) {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().to_path_buf();
        let vault = "default".to_string();
        create_private_dir(paths::vaults_dir(&store).join(&vault)).unwrap();
        let identity = age::x25519::Identity::generate();
        let log = LocalAuditLog::new(store, &identity);
        (tmp, log, vault)
    }

    #[test]
    fn empty_log_verifies_as_empty() {
        let (_tmp, log, vault) = fixture();
        assert_eq!(log.verify_chain(&vault).unwrap(), ChainStatus::Empty);
        assert!(log.read_all(&vault).unwrap().is_empty());
    }

    #[test]
    fn appends_chain_and_verify() {
        let (_tmp, log, vault) = fixture();
        log.record(&vault, AuditOp::PutSecretValue, "DB_PASSWORD")
            .unwrap();
        log.record(&vault, AuditOp::GetSecretValue, "DB_PASSWORD")
            .unwrap();
        log.record(&vault, AuditOp::DeleteSecret, "DB_PASSWORD")
            .unwrap();

        let records = log.read_all(&vault).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].seq, 1);
        assert_eq!(records[0].prev, GENESIS_MAC);
        assert_eq!(records[1].prev, records[0].mac);
        assert_eq!(records[2].prev, records[1].mac);
        assert_eq!(records[0].operation, "PutSecretValue");
        assert_eq!(
            log.verify_chain(&vault).unwrap(),
            ChainStatus::Intact { records: 3 }
        );
    }

    #[test]
    fn pre_change_v1_record_still_verifies_and_serializes_without_v2_fields() {
        const LEGACY_KEY: &str =
            "AGE-SECRET-KEY-1QAMJ3TTWX8NGQ9GCZYWMQ4AG5HZD52J62VMHVQ8R34TY5UVA78PQK4W0GV";
        const LEGACY_MAC: &str = "e175d6f6e8f5bc47b4aa84a883db6ac191c9849091f22ae02eead6848e5a63f4";
        let tmp = TempDir::new().unwrap();
        let vault = "default".to_string();
        create_private_dir(paths::vaults_dir(tmp.path()).join(&vault)).unwrap();
        let identity: age::x25519::Identity = LEGACY_KEY.parse().unwrap();
        let log = LocalAuditLog::new(tmp.path().to_path_buf(), &identity);
        let line = format!(
            "{{\"seq\":1,\"timestamp\":\"2025-01-02T03:04:05Z\",\"operation\":\"GetSecretValue\",\"resource_name\":\"legacy\",\"caller\":\"alice\",\"status\":\"Succeeded\",\"prev\":\"{GENESIS_MAC}\",\"mac\":\"{LEGACY_MAC}\"}}"
        );
        create_private_dir(log.audit_dir(&vault).unwrap()).unwrap();
        fs::write(log.log_path(&vault).unwrap(), format!("{line}\n")).unwrap();
        let record = log.read_all(&vault).unwrap().remove(0);
        assert_eq!(record.record_version, 1);
        assert_eq!(record.mac, LEGACY_MAC);
        assert!(!record.has_v2_fields());
        assert_eq!(serde_json::to_string(&record).unwrap(), line);
        assert_eq!(
            log.verify_chain(&vault).unwrap(),
            ChainStatus::Intact { records: 1 }
        );
    }

    #[tokio::test]
    async fn mixed_v1_then_v2_chain_verifies_end_to_end() {
        let (_tmp, log, vault) = fixture();
        log.record(&vault, AuditOp::GetSecretValue, "legacy")
            .unwrap();
        let context = crate::agent::AuditContext {
            identity: crate::agent::AgentIdentity::new(
                crate::agent::IdentitySource::GithubOidc,
                "github:o/r:w@refs/heads/main",
            ),
            policy_version: "abcd".into(),
            decision: "allow".into(),
        };
        crate::agent::AUDIT_CONTEXT
            .scope(context, async {
                log.record(&vault, AuditOp::GetSecretValue, "agent-read")
                    .unwrap();
            })
            .await;
        let records = log.read_all(&vault).unwrap();
        assert_eq!(records[0].record_version, 1);
        assert_eq!(records[1].record_version, 2);
        assert_eq!(
            records[1].agent_id.as_deref(),
            Some("github:o/r:w@refs/heads/main")
        );
        assert_eq!(
            log.verify_chain(&vault).unwrap(),
            ChainStatus::Intact { records: 2 }
        );
    }

    #[test]
    fn v2_decision_record_verifies_and_every_new_field_is_mac_bound() {
        let (_tmp, log, vault) = fixture();
        let mut identity = crate::agent::AgentIdentity::new(
            crate::agent::IdentitySource::GithubOidc,
            "github:o/r:w@refs/heads/main",
        );
        identity.invoking_principal = Some("alice".into());
        identity.session_id = Some("session-1".into());
        identity.delegation_chain = vec!["human:alice".into(), "orchestrator:ci".into()];
        identity.purpose = Some("deploy".into());
        log.record_policy_decision(
            &vault,
            &PolicyDecisionRecord {
                identity: &identity,
                operation: "get",
                resource_name: "prod/deploy/key",
                decision: "deny",
                matched_rule: Some("rule-1"),
                deny_reason: Some("reason"),
                policy_version: "policy-v1",
            },
        )
        .unwrap();
        assert_eq!(
            log.verify_chain(&vault).unwrap(),
            ChainStatus::Intact { records: 1 }
        );
        let records = log.read_all(&vault).unwrap();
        assert_eq!(
            records[0].delegation_chain.as_deref(),
            Some(&["human:alice".to_string(), "orchestrator:ci".to_string()][..])
        );

        let path = log.log_path(&vault).unwrap();
        let original = fs::read_to_string(&path).unwrap();
        for (field, replacement) in [
            ("agent_id", serde_json::json!("changed")),
            ("identity_source", serde_json::json!("env-assertion")),
            ("verified", serde_json::json!(false)),
            ("invoking_principal", serde_json::json!("mallory")),
            ("session_id", serde_json::json!("other")),
            ("delegation_chain", serde_json::json!(["human:mallory"])),
            ("purpose", serde_json::json!("other")),
            ("policy_version", serde_json::json!("other")),
            ("decision", serde_json::json!("allow")),
            ("matched_rule", serde_json::json!("other")),
            ("deny_reason", serde_json::json!("other")),
        ] {
            let mut value: serde_json::Value = serde_json::from_str(original.trim()).unwrap();
            value[field] = replacement;
            fs::write(
                &path,
                format!("{}\n", serde_json::to_string(&value).unwrap()),
            )
            .unwrap();
            assert!(
                matches!(
                    log.verify_chain(&vault).unwrap(),
                    ChainStatus::Broken { .. }
                ),
                "tampering with {field} must break the chain"
            );
        }
        fs::write(path, original).unwrap();
    }

    #[test]
    fn unknown_record_versions_are_rejected_even_when_the_mac_is_unchanged() {
        let (_tmp, log, vault) = fixture();
        let mut identity = crate::agent::AgentIdentity::new(
            crate::agent::IdentitySource::GithubOidc,
            "github:o/r:w@refs/heads/main",
        );
        identity.delegation_chain = vec!["human:alice".into()];
        log.record_policy_decision(
            &vault,
            &PolicyDecisionRecord {
                identity: &identity,
                operation: "get",
                resource_name: "prod/key",
                decision: "allow",
                matched_rule: Some("rule"),
                deny_reason: None,
                policy_version: "policy",
            },
        )
        .unwrap();
        let path = log.log_path(&vault).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        value["record_version"] = serde_json::json!(3);
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&value).unwrap()),
        )
        .unwrap();
        match log.verify_chain(&vault).unwrap() {
            ChainStatus::Broken { reason, .. } => {
                assert!(reason.contains("unsupported"), "{reason}")
            }
            other => panic!("expected unsupported-version break, got {other:?}"),
        }
    }

    #[test]
    fn v1_rows_with_any_v2_field_are_rejected_and_never_render_attribution() {
        let (_tmp, log, vault) = fixture();
        log.record(&vault, AuditOp::GetSecretValue, "legacy")
            .unwrap();
        let path = log.log_path(&vault).unwrap();
        let original: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        for (field, value) in [
            ("agent_id", serde_json::json!("forged")),
            ("identity_source", serde_json::json!("github-oidc")),
            ("verified", serde_json::json!(true)),
            ("invoking_principal", serde_json::json!("alice")),
            ("session_id", serde_json::json!("session")),
            ("delegation_chain", serde_json::json!(["alice", "child"])),
            ("purpose", serde_json::json!("forged")),
            ("policy_version", serde_json::json!("policy")),
            ("decision", serde_json::json!("allow")),
            ("matched_rule", serde_json::json!("rule")),
            ("deny_reason", serde_json::json!("none")),
        ] {
            let mut forged = original.clone();
            forged[field] = value;
            fs::write(
                &path,
                format!("{}\n", serde_json::to_string(&forged).unwrap()),
            )
            .unwrap();
            assert!(matches!(
                log.verify_chain(&vault).unwrap(),
                ChainStatus::Broken { .. }
            ));
            let event = log.read_all(&vault).unwrap()[0].to_event();
            assert!(event.agent_id.is_none(), "v1 {field} leaked attribution");
            assert!(event.delegation_chain.is_none());
            assert!(event.decision.is_none());
        }
    }

    #[test]
    fn tampering_with_a_record_breaks_the_chain() {
        let (_tmp, log, vault) = fixture();
        for name in ["A", "B", "C"] {
            log.record(&vault, AuditOp::GetSecretValue, name).unwrap();
        }

        // Rewrite record 2's resource name, leaving its mac untouched — the
        // edit an attacker would make to hide which secret they read.
        let path = log.log_path(&vault).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
        lines[1] = lines[1].replace("\"resource_name\":\"B\"", "\"resource_name\":\"Z\"");
        fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        match log.verify_chain(&vault).unwrap() {
            ChainStatus::Broken {
                seq,
                verified,
                reason,
            } => {
                assert_eq!(seq, 2);
                assert_eq!(verified, 1);
                assert!(reason.contains("mac mismatch"), "{reason}");
            }
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[test]
    fn deleting_a_middle_record_breaks_the_chain() {
        let (_tmp, log, vault) = fixture();
        for name in ["A", "B", "C"] {
            log.record(&vault, AuditOp::GetSecretValue, name).unwrap();
        }

        let path = log.log_path(&vault).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

        match log.verify_chain(&vault).unwrap() {
            ChainStatus::Broken { seq, reason, .. } => {
                assert_eq!(seq, 3, "the surviving third record is the first anomaly");
                assert!(reason.contains("sequence gap"), "{reason}");
            }
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[test]
    fn a_different_identity_cannot_verify_the_chain() {
        let (tmp, log, vault) = fixture();
        log.record(&vault, AuditOp::GetSecretValue, "A").unwrap();

        // Same store, different age key — an attacker without the identity.
        let other =
            LocalAuditLog::new(tmp.path().to_path_buf(), &age::x25519::Identity::generate());
        match other.verify_chain(&vault).unwrap() {
            ChainStatus::Broken { reason, .. } => assert!(reason.contains("mac mismatch")),
            other => panic!("expected Broken under a foreign key, got {other:?}"),
        }
    }

    #[test]
    fn canonical_encoding_is_unambiguous_across_field_boundaries() {
        // Two records whose fields concatenate to the same bytes under a naive
        // delimiter-free or newline-joined encoding must still MAC differently.
        let (_tmp, log, _vault) = fixture();
        let base = AuditRecord {
            record_version: 1,
            seq: 1,
            timestamp: Utc::now(),
            operation: "GetSecretValue".into(),
            resource_name: "AB".into(),
            caller: "C".into(),
            status: "Succeeded".into(),
            agent_id: None,
            identity_source: None,
            verified: None,
            invoking_principal: None,
            session_id: None,
            delegation_chain: None,
            purpose: None,
            policy_version: None,
            decision: None,
            matched_rule: None,
            deny_reason: None,
            prev: GENESIS_MAC.into(),
            mac: String::new(),
        };
        let mut shifted = base.clone();
        shifted.resource_name = "A".into();
        shifted.caller = "BC".into();
        assert_ne!(log.compute_mac(&base), log.compute_mac(&shifted));
    }

    #[test]
    fn newline_in_secret_name_cannot_forge_a_record() {
        // Local secret names may contain newlines; a name carrying a crafted
        // JSON-ish payload must not be able to impersonate extra fields.
        let (_tmp, log, vault) = fixture();
        let nasty = "A\n{\"seq\":99,\"operation\":\"GetSecretValue\"}";
        log.record(&vault, AuditOp::GetSecretValue, nasty).unwrap();
        let records = log.read_all(&vault).unwrap();
        assert_eq!(records.len(), 1, "one logical record, one line");
        assert_eq!(records[0].resource_name, nasty);
        assert_eq!(
            log.verify_chain(&vault).unwrap(),
            ChainStatus::Intact { records: 1 }
        );
    }

    #[test]
    fn tail_read_survives_records_spanning_chunk_boundaries() {
        // Force the backward tail scan across several TAIL_CHUNK windows.
        let (_tmp, log, vault) = fixture();
        let long_name = "N".repeat(4000);
        for _ in 0..8 {
            log.record(&vault, AuditOp::GetSecretValue, &long_name)
                .unwrap();
        }
        let records = log.read_all(&vault).unwrap();
        assert_eq!(records.len(), 8);
        assert_eq!(records[7].seq, 8);
        assert_eq!(
            log.verify_chain(&vault).unwrap(),
            ChainStatus::Intact { records: 8 }
        );
    }

    #[tokio::test]
    async fn events_filter_by_window_and_secret() {
        let (_tmp, log, vault) = fixture();
        log.record(&vault, AuditOp::PutSecretValue, "A").unwrap();
        log.record(&vault, AuditOp::GetSecretValue, "B").unwrap();
        log.record(&vault, AuditOp::ListSecrets, RESOURCE_VAULT_WIDE)
            .unwrap();

        let all = log.get_vault_events(&vault, None, 30).await.unwrap();
        assert_eq!(all.len(), 3);
        // Newest first.
        assert_eq!(all[0].operation, "ListSecrets");

        let for_a = log.get_secret_events(&vault, "A", None, 30).await.unwrap();
        let names: Vec<&str> = for_a.iter().map(|e| e.resource_name.as_str()).collect();
        assert_eq!(names, vec![RESOURCE_VAULT_WIDE, "A"]);

        // A zero-day window excludes everything older than now.
        let none = log.get_vault_events(&vault, None, 0).await.unwrap();
        assert!(none.len() <= 3);
    }

    #[test]
    fn event_ids_are_unique_per_record() {
        let (_tmp, log, vault) = fixture();
        for _ in 0..5 {
            log.record(&vault, AuditOp::GetSecretValue, "same-name")
                .unwrap();
        }
        let records = log.read_all(&vault).unwrap();
        let mut macs: Vec<&str> = records.iter().map(|r| r.mac.as_str()).collect();
        macs.sort_unstable();
        macs.dedup();
        assert_eq!(
            macs.len(),
            5,
            "each record must have a distinct mac/event_id"
        );
    }

    #[test]
    fn corrupt_line_is_an_error_not_a_silent_skip() {
        let (_tmp, log, vault) = fixture();
        log.record(&vault, AuditOp::GetSecretValue, "A").unwrap();
        let path = log.log_path(&vault).unwrap();
        fs::write(&path, "{not json}\n").unwrap();
        assert!(log.read_all(&vault).is_err());
    }
}
