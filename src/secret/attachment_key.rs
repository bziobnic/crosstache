//! Attachment key identity, references, and reserved crypto-metadata
//! contracts for the race-free attachment-key lifecycle (PR 1 integrity
//! foundation).
//!
//! See `docs/attachments.md` and the design at
//! `2026-09-03-xv-race-free-attachment-key-lifecycle-design.md`.
//!
//! This module holds the pure, backend-independent data contracts:
//!
//! - [`AttachmentKeyId`] — portable `ak1-<64 hex>` identifier derived from a
//!   public age recipient (design §7.1, invariant I6).
//!
//! Private key material never lives here; see `AttachmentKeyMaterial` for the
//! non-`Debug`, non-`Serialize` custody type.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Domain-separation preimage prefix for attachment key-ID derivation.
///
/// The trailing NUL is significant: it separates the fixed domain tag from the
/// variable recipient bytes so no recipient string can collide with the tag.
const KEY_ID_DOMAIN: &[u8] = b"xv-attachment-key-id-v1\0";

/// Portable, backend-independent identifier for one committed attachment key
/// generation.
///
/// ```text
/// key_id = "ak1-" + lowercase_hex(SHA-256(KEY_ID_DOMAIN || canonical_recipient))
/// ```
///
/// Derived only from the *public* age recipient, so it carries no secret
/// material and is safe to log, serialize, and place in blob metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttachmentKeyId(String);

impl AttachmentKeyId {
    /// Derive the deterministic key ID from a canonical age recipient string
    /// (its bech32 `age1…` form).
    pub fn derive(canonical_recipient: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(KEY_ID_DOMAIN);
        hasher.update(canonical_recipient.as_bytes());
        let digest = hasher.finalize();
        AttachmentKeyId(format!("ak1-{}", hex::encode(digest)))
    }

    /// The full `ak1-<64 hex>` string form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse and validate a stored key-ID string. Accepts only the strict
    /// `ak1-` prefix followed by exactly 64 lowercase hex characters; every
    /// other input (wrong prefix, wrong length, uppercase, non-hex) is
    /// rejected without a fallback.
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix("ak1-")?;
        if hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Some(AttachmentKeyId(s.to_string()))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Reserved record names and the active pointer (design §7.2)
// ---------------------------------------------------------------------------

/// Exact name of the reserved active-pointer / V1-identity secret.
// Consumed by the V2-init and reserved-guard slices (PR 1).
#[allow(dead_code)]
pub const ACTIVE_POINTER_SECRET: &str = "xv-attachment-key";

/// Value prefix marking a V2 key-ring active pointer.
// Consumed by the V2-init and reserved-guard slices (PR 1).
#[allow(dead_code)]
pub const POINTER_V2_PREFIX: &str = "xv-attachment-key-pointer/v2:";

/// Prefix of a retained per-key record name.
pub const RETAINED_RECORD_PREFIX: &str = "xv-attachment-key-";

/// Reserved content type marking a secret as an actual attachment key-custody
/// record (design §8). Generic callers cannot set or remove this marker; the
/// custody path uses it to tell a real key record from an unmarked user secret
/// that merely collides with a strict-format name.
pub const KEY_RECORD_CONTENT_TYPE: &str = "application/x-xv-attachment-key-record";

/// Advisory lifecycle tag; never gates historical decryption.
pub const KEY_RETIRED_TAG: &str = "xv_attachment_key_retired";

/// True if a secret's content type marks it as a managed key-custody record.
pub fn is_marked_key_record(content_type: &str) -> bool {
    content_type == KEY_RECORD_CONTENT_TYPE
}

/// HRP prefix of a raw age x25519 secret identity (uppercase bech32).
const AGE_IDENTITY_PREFIX: &str = "AGE-SECRET-KEY-1";

/// Deterministic name of the immutable retained record for `key_id`.
pub fn retained_record_name(key_id: &AttachmentKeyId) -> String {
    format!("{RETAINED_RECORD_PREFIX}{}", key_id.as_str())
}

/// Serialize a V2 active pointer value.
// Consumed by the V2-init/rotation slices (PR 1).
#[allow(dead_code)]
pub fn format_v2_pointer(active: &AttachmentKeyId, legacy: Option<&AttachmentKeyId>) -> String {
    match legacy {
        Some(l) => format!(
            "{POINTER_V2_PREFIX}{}:legacy={}",
            active.as_str(),
            l.as_str()
        ),
        None => format!("{POINTER_V2_PREFIX}{}", active.as_str()),
    }
}

/// Classification of the reserved active-pointer secret's stored value
/// (design §10.1). Missing/denied states are handled by the caller; this
/// classifies a value that was successfully read.
// Consumed by the resolve_active / V2-init slices (PR 1).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerKind {
    /// A raw age identity — the vault is in V1 legacy mode.
    V1RawIdentity,
    /// A V2 key-ring pointer with an active key and optional permanent
    /// V1-fallback key ID.
    V2 {
        active: AttachmentKeyId,
        legacy: Option<AttachmentKeyId>,
    },
}

/// Classify the active-pointer value. Returns `None` for a malformed value
/// that must fail without replacement (never falls back to a current key).
// Consumed by the resolve_active / V2-init slices (PR 1).
#[allow(dead_code)]
pub fn parse_pointer_value(value: &str) -> Option<PointerKind> {
    let value = value.trim();
    if value.starts_with(AGE_IDENTITY_PREFIX) {
        return Some(PointerKind::V1RawIdentity);
    }
    let rest = value.strip_prefix(POINTER_V2_PREFIX)?;
    let (active_str, legacy) = match rest.split_once(":legacy=") {
        Some((a, l)) => (a, Some(AttachmentKeyId::parse(l)?)),
        None => (rest, None),
    };
    let active = AttachmentKeyId::parse(active_str)?;
    Some(PointerKind::V2 { active, legacy })
}

// ---------------------------------------------------------------------------
// Reserved crypto metadata (design §7.3)
// ---------------------------------------------------------------------------

/// Metadata key flagging client-side-encrypted content. Underscore (not
/// hyphen) so it survives Azure Blob metadata's C#-identifier constraint.
pub const META_ENCRYPTED: &str = "xv_encrypted";
/// Value of [`META_ENCRYPTED`] for age encryption.
pub const ENC_VALUE_AGE: &str = "age";
/// Metadata key naming the crypto envelope schema version.
pub const META_CRYPTO_SCHEMA: &str = "xv_crypto_schema";
/// Current crypto schema version.
pub const CRYPTO_SCHEMA_V1: &str = "1";
/// Metadata key carrying the portable [`AttachmentKeyId`].
pub const META_KEY_ID: &str = "xv_key_id";
/// Metadata key carrying the exact, opaque provider version of the key record.
pub const META_KEY_VERSION: &str = "xv_key_version";
/// Metadata key carrying the [`KeySlot`] (`legacy` | `retained`).
pub const META_KEY_SLOT: &str = "xv_key_slot";

/// The five reserved crypto-metadata keys. The encryption path always writes
/// these from the committed key material; caller-supplied values are
/// overwritten (design §7.3, "caller metadata cannot override them").
// Consumed by apply_crypto_metadata and the reserved-guard slice (PR 1).
#[allow(dead_code)]
pub const RESERVED_CRYPTO_METADATA_KEYS: [&str; 5] = [
    META_ENCRYPTED,
    META_CRYPTO_SCHEMA,
    META_KEY_ID,
    META_KEY_VERSION,
    META_KEY_SLOT,
];

/// True if `key` is one of the reserved crypto-metadata keys that the
/// encryption path owns exclusively.
// Consumed by the reserved-guard slice (PR 1).
#[allow(dead_code)]
pub fn is_reserved_crypto_metadata_key(key: &str) -> bool {
    RESERVED_CRYPTO_METADATA_KEYS.contains(&key)
}

/// Which key record a blob's key reference resolves through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySlot {
    /// Versioned lookup of the fixed V1 `xv-attachment-key` record.
    Legacy,
    /// Lookup of the immutable `xv-attachment-key-<key-id>` record.
    Retained,
}

impl KeySlot {
    /// Stable metadata string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            KeySlot::Legacy => "legacy",
            KeySlot::Retained => "retained",
        }
    }

    /// Parse the metadata string form; unknown values yield `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "legacy" => Some(KeySlot::Legacy),
            "retained" => Some(KeySlot::Retained),
            _ => None,
        }
    }
}

/// An opaque provider version token for a secret record. Never parsed for
/// ordering (design §7.3); it is only compared and re-read exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretVersion(String);

impl SecretVersion {
    /// Wrap a provider-returned version token.
    pub fn new(version: impl Into<String>) -> Self {
        SecretVersion(version.into())
    }

    /// The raw version string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A blob's binding to the exact key generation that encrypted it (design
/// §7.4): portable key ID, slot, and exact provider version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentKeyRef {
    /// Portable key identifier.
    pub key_id: AttachmentKeyId,
    /// Which record family resolves the key.
    pub slot: KeySlot,
    /// Exact provider version of the key record.
    pub provider_version: SecretVersion,
}

/// Write all five reserved crypto-metadata keys from `key_ref`, unconditionally
/// overwriting any caller-supplied values for those keys. Non-reserved keys are
/// left untouched.
pub fn apply_crypto_metadata(metadata: &mut HashMap<String, String>, key_ref: &AttachmentKeyRef) {
    metadata.insert(META_ENCRYPTED.to_string(), ENC_VALUE_AGE.to_string());
    metadata.insert(META_CRYPTO_SCHEMA.to_string(), CRYPTO_SCHEMA_V1.to_string());
    metadata.insert(META_KEY_ID.to_string(), key_ref.key_id.as_str().to_string());
    metadata.insert(
        META_KEY_VERSION.to_string(),
        key_ref.provider_version.as_str().to_string(),
    );
    metadata.insert(META_KEY_SLOT.to_string(), key_ref.slot.as_str().to_string());
}

// ---------------------------------------------------------------------------
// Structural reserved-resource classification (design §8 / task §E)
// ---------------------------------------------------------------------------

/// How the generic secret facade must treat a name (design §8 / task §E).
///
/// Classification operates on a name that has already been mapped through the
/// provider-canonical form (Azure sanitizer + case-fold + hyphen normalization,
/// AWS logical/provider encoding, Local canonical stem). Case variants,
/// underscore aliases, and repeated-hyphen aliases are resolved *before* this
/// call so they cannot bypass the guard.
// Consumed by the generic-facade reserved-guard slice (PR 1).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedClass {
    /// An ordinary user secret — every generic operation is permitted. This
    /// includes broad-prefix names like `xv-attachment-key-notes` that are NOT
    /// the strict generated format (the broad prefix is deliberately NOT
    /// reserved).
    Ordinary,
    /// The exact active pointer `xv-attachment-key`: hidden from ordinary reads
    /// and blocked from every ordinary mutation.
    ActivePointer,
    /// A strict-format retained record `xv-attachment-key-ak1-<64 hex>`: every
    /// generic mutation is blocked (whether the record is marked, unmarked, or
    /// absent — closing the metadata-then-mutate TOCTOU). An unmarked
    /// collision here remains readable/listable/exportable; a marked record is
    /// additionally hidden from reads by the marker check at a higher layer.
    StrictRetainedRecord,
}

/// True if `canonical_name` is the exact reserved active pointer.
// Consumed by the generic-facade reserved-guard slice (PR 1).
#[allow(dead_code)]
pub fn is_active_pointer_name(canonical_name: &str) -> bool {
    canonical_name == ACTIVE_POINTER_SECRET
}

/// True if `canonical_name` is a strict-format retained record name,
/// `xv-attachment-key-ak1-<64 lowercase hex>`.
// Consumed by the generic-facade reserved-guard slice (PR 1).
#[allow(dead_code)]
pub fn is_strict_retained_record_name(canonical_name: &str) -> bool {
    canonical_name
        .strip_prefix(RETAINED_RECORD_PREFIX)
        .and_then(AttachmentKeyId::parse)
        .is_some()
}

/// Classify a canonical secret name for the generic facade.
// Consumed by the generic-facade reserved-guard slice (PR 1).
#[allow(dead_code)]
pub fn classify_reserved_name(canonical_name: &str) -> ReservedClass {
    if is_active_pointer_name(canonical_name) {
        ReservedClass::ActivePointer
    } else if is_strict_retained_record_name(canonical_name) {
        ReservedClass::StrictRetainedRecord
    } else {
        ReservedClass::Ordinary
    }
}

/// A secret name paired with the provider-canonical identity that the eventual
/// backend request will actually address (design §8).
///
/// Reserved-name matching MUST use `provider_identity`, never raw CLI/API
/// input: Azure Key Vault is case-insensitive and the shared sanitizer folds
/// underscores and repeated hyphens onto `-`, so several distinct logical
/// inputs address one provider secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSecretName {
    /// The caller-supplied logical name, unchanged.
    pub logical: String,
    /// The name the provider request will address.
    pub provider_identity: String,
}

/// Convert `logical` into the provider-canonical identity for `kind`.
///
/// Returns `None` when canonicalization is ambiguous or fails; the caller MUST
/// fail closed before any provider mutation rather than fall back to the raw
/// input (design §8).
pub fn canonicalize_secret_name(
    logical: &str,
    kind: crate::backend::BackendKind,
) -> Option<CanonicalSecretName> {
    use crate::backend::BackendKind;

    // The shared sanitizer performs the invalid-char -> '-' mapping, hyphen
    // collapsing, and trimming that every provider request already applies.
    let sanitized = crate::utils::sanitizer::sanitize_secret_name(logical).ok()?;
    if sanitized.is_empty() {
        return None;
    }

    let provider_identity = match kind {
        // Key Vault names are case-insensitive; fold so case variants cannot
        // address a reserved record without matching it.
        BackendKind::Azure => sanitized.to_ascii_lowercase(),
        // Local uses the same canonical logical/stem mapping as the sanitizer.
        BackendKind::Local => sanitized,
        // AWS names are case-sensitive, so the sanitized form is the identity.
        BackendKind::Aws => sanitized,
    };

    Some(CanonicalSecretName {
        logical: logical.to_string(),
        provider_identity,
    })
}

/// Every provider-canonical identity `logical` could address.
///
/// CLI and Web guard call sites do not know which backend will serve the
/// request, so the guard must consider every provider mapping and fail closed
/// if *any* of them lands on a protected name.
fn canonical_identities(logical: &str) -> Vec<String> {
    use crate::backend::BackendKind;

    let mut out = vec![logical.to_string()];
    for kind in [BackendKind::Azure, BackendKind::Local, BackendKind::Aws] {
        if let Some(c) = canonicalize_secret_name(logical, kind) {
            if !out.contains(&c.provider_identity) {
                out.push(c.provider_identity);
            }
        }
    }
    out
}

/// Provider-agnostic [`generic_mutation_blocked`]: blocks when the raw name or
/// any provider canonicalization of it is a protected custody resource
/// (design §8).
pub fn generic_mutation_blocked_canonical(logical: &str) -> bool {
    canonical_identities(logical)
        .iter()
        .any(|n| generic_mutation_blocked(n))
}

/// Provider-agnostic [`hidden_from_generic_listing`].
pub fn hidden_from_generic_listing_canonical(logical: &str, content_type: &str) -> bool {
    canonical_identities(logical)
        .iter()
        .any(|n| hidden_from_generic_listing(n, content_type))
}

/// True if a *generic* (non-custody) mutation of `canonical_name` must be
/// blocked before any provider I/O: the exact pointer and every strict-format
/// retained record are immutable through ordinary paths (design §8, task §E).
/// True if `name` must be hidden from ordinary list/read/export paths given its
/// `content_type` (design §8 / task §E): the exact active pointer is always
/// hidden, and a strict-format retained record is hidden only when it is a
/// *marked* key-custody record. An unmarked strict-format user collision stays
/// listable, readable, and exportable until offline migration.
pub fn hidden_from_generic_listing(name: &str, content_type: &str) -> bool {
    is_active_pointer_name(name)
        || (is_strict_retained_record_name(name) && is_marked_key_record(content_type))
}

// Consumed by the generic-facade reserved-guard slice (PR 1).
pub fn generic_mutation_blocked(canonical_name: &str) -> bool {
    !matches!(
        classify_reserved_name(canonical_name),
        ReservedClass::Ordinary
    )
}

/// Blob-namespace prefix under which crosstache stores managed attachments.
pub const ATTACHMENTS_NAMESPACE: &str = "attachments/";

/// True if `name` is in the reserved managed-attachment namespace.
pub fn is_in_attachments_namespace(name: &str) -> bool {
    name.starts_with(ATTACHMENTS_NAMESPACE)
}

/// Extract a complete schema-1 key reference from blob metadata, or `None` if
/// any component is missing or malformed. Never guesses: a partial or invalid
/// reference yields `None` (invariant I7).
pub fn parse_key_ref_from_metadata(metadata: &HashMap<String, String>) -> Option<AttachmentKeyRef> {
    let key_id = AttachmentKeyId::parse(metadata.get(META_KEY_ID)?)?;
    let slot = KeySlot::parse(metadata.get(META_KEY_SLOT)?)?;
    let version = metadata.get(META_KEY_VERSION)?;
    if version.is_empty() {
        return None;
    }
    Some(AttachmentKeyRef {
        key_id,
        slot,
        provider_version: SecretVersion::new(version.clone()),
    })
}

/// The decision produced by classifying a downloaded object generation
/// (design §11 / task §D, steps 1–7). Pure: the caller maps each variant onto
/// concrete key resolution, decryption, errors, or passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadPlan {
    /// Not managed ciphertext — return bytes unchanged (steps 3, 4).
    Passthrough,
    /// Managed state is declared but the bytes are not age ciphertext — fail
    /// closed; never return plaintext (step 2).
    FailClosedNonCiphertext,
    /// Managed age blob with no schema-1 envelope — legacy/V1 resolution,
    /// which is pointer-aware at the caller (step 6: V1 raw, converted-V2
    /// legacy fallback, or fail-closed for direct-V2). This variant means "no
    /// schema-1 key reference is present".
    LegacyNoSchema,
    /// Managed age blob with a complete, valid schema-1 key reference (step 6).
    Schema1 { key_ref: AttachmentKeyRef },
    /// Managed age blob declaring schema 1 but whose key reference is missing,
    /// malformed, or of an unknown schema — fail; never fall back to a current
    /// key or scan keys (step 7, invariant I7).
    ReferenceInvalid,
}

/// Classify a downloaded object generation using only the stable-snapshot
/// inputs: the blob name, its user metadata, and whether its bytes are age
/// ciphertext. Provider object *tags* are deliberately excluded (design §11).
pub fn classify_download(
    name: &str,
    metadata: &HashMap<String, String>,
    is_age_ciphertext: bool,
) -> DownloadPlan {
    let managed = is_in_attachments_namespace(name)
        || metadata.get(META_ENCRYPTED).map(String::as_str) == Some(ENC_VALUE_AGE);

    if !is_age_ciphertext {
        // Step 2: managed but not ciphertext → fail closed. Step 3: ordinary
        // unmarked non-age → passthrough.
        return if managed {
            DownloadPlan::FailClosedNonCiphertext
        } else {
            DownloadPlan::Passthrough
        };
    }

    // Bytes are age ciphertext.
    if !managed {
        // Step 4: foreign unmarked age file — not ours to decrypt.
        return DownloadPlan::Passthrough;
    }

    match metadata.get(META_CRYPTO_SCHEMA).map(String::as_str) {
        // Step 6: no schema envelope → legacy resolution (pointer-aware).
        None => DownloadPlan::LegacyNoSchema,
        // Step 6/7: schema 1 requires a complete, valid reference.
        Some(CRYPTO_SCHEMA_V1) => match parse_key_ref_from_metadata(metadata) {
            Some(key_ref) => DownloadPlan::Schema1 { key_ref },
            None => DownloadPlan::ReferenceInvalid,
        },
        // Unknown schema version → never fall back.
        Some(_) => DownloadPlan::ReferenceInvalid,
    }
}

// ---------------------------------------------------------------------------
// Key material custody type (design §7.4, invariants I6/I9)
// ---------------------------------------------------------------------------

/// A committed attachment key generation *with* its private material.
///
/// Deliberately implements neither `Debug` nor any `serde` trait: the raw age
/// identity must never reach logs, errors, or serialized output (invariant
/// I9). The identity is held in [`Zeroizing`] storage. Callers reach the
/// secret only through [`AttachmentKeyMaterial::expose_identity`].
pub struct AttachmentKeyMaterial {
    reference: AttachmentKeyRef,
    identity: Zeroizing<String>,
    recipient: age::x25519::Recipient,
}

impl AttachmentKeyMaterial {
    /// Build key material from a raw age identity string plus the slot and the
    /// exact provider version of the record it was read from. Derives the
    /// recipient and key ID from the parsed identity (never trusts an external
    /// key ID). Returns `None` if the string is not a valid age identity.
    pub fn from_identity(
        slot: KeySlot,
        provider_version: SecretVersion,
        identity: Zeroizing<String>,
    ) -> Option<Self> {
        let parsed = identity.trim().parse::<age::x25519::Identity>().ok()?;
        let recipient = parsed.to_public();
        let key_id = AttachmentKeyId::derive(&recipient.to_string());
        Some(AttachmentKeyMaterial {
            reference: AttachmentKeyRef {
                key_id,
                slot,
                provider_version,
            },
            identity,
            recipient,
        })
    }

    /// The public reference (key ID, slot, exact version). Safe to log.
    pub fn reference(&self) -> &AttachmentKeyRef {
        &self.reference
    }

    /// The public age recipient for encryption.
    pub fn recipient(&self) -> &age::x25519::Recipient {
        &self.recipient
    }

    /// Guarded access to the raw identity string for decryption. Callers must
    /// not log or persist the returned value.
    pub fn expose_identity(&self) -> &str {
        &self.identity
    }

    /// Verify this material's derived key ID matches an expected ID (invariant
    /// I6: verify, do not trust references).
    pub fn verify_id(&self, expected: &AttachmentKeyId) -> bool {
        &self.reference.key_id == expected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendKind;

    /// A fixed canonical recipient string, used to pin the exact derivation.
    const RECIPIENT: &str = "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p";

    #[test]
    fn key_id_is_deterministic_and_well_formed() {
        let a = AttachmentKeyId::derive(RECIPIENT);
        let b = AttachmentKeyId::derive(RECIPIENT);
        assert_eq!(a, b, "derivation must be deterministic");

        let s = a.as_str();
        assert!(s.starts_with("ak1-"), "expected ak1- prefix, got {s}");
        let hex = &s[4..];
        assert_eq!(hex.len(), 64, "expected 64 hex chars, got {}", hex.len());
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex, got {hex}"
        );
    }

    #[test]
    fn key_id_uses_domain_separated_sha256() {
        // Independently recompute the preimage to pin the exact domain string
        // and byte ordering (a separate code path from `derive`).
        let mut hasher = Sha256::new();
        hasher.update(b"xv-attachment-key-id-v1\0");
        hasher.update(RECIPIENT.as_bytes());
        let expected = format!("ak1-{}", hex::encode(hasher.finalize()));
        assert_eq!(AttachmentKeyId::derive(RECIPIENT).as_str(), expected);
    }

    #[test]
    fn distinct_recipients_derive_distinct_ids() {
        let a = AttachmentKeyId::derive(RECIPIENT);
        let b = AttachmentKeyId::derive(
            "age1differentrecipientstringxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        );
        assert_ne!(a, b);
    }

    fn sample_ref(slot: KeySlot, version: &str) -> AttachmentKeyRef {
        AttachmentKeyRef {
            key_id: AttachmentKeyId::derive(RECIPIENT),
            slot,
            provider_version: SecretVersion::new(version),
        }
    }

    #[test]
    fn apply_crypto_metadata_sets_all_reserved_keys() {
        let key_ref = sample_ref(KeySlot::Retained, "v-abc-123");
        let mut md = HashMap::new();
        md.insert("user_tag".to_string(), "keep".to_string());
        apply_crypto_metadata(&mut md, &key_ref);
        assert_eq!(md.get(META_ENCRYPTED).unwrap(), "age");
        assert_eq!(md.get(META_CRYPTO_SCHEMA).unwrap(), "1");
        assert_eq!(md.get(META_KEY_ID).unwrap(), key_ref.key_id.as_str());
        assert_eq!(md.get(META_KEY_VERSION).unwrap(), "v-abc-123");
        assert_eq!(md.get(META_KEY_SLOT).unwrap(), "retained");
        // Non-reserved metadata is left untouched.
        assert_eq!(md.get("user_tag").unwrap(), "keep");
    }

    #[test]
    fn apply_crypto_metadata_overwrites_caller_supplied_reserved_values() {
        let key_ref = sample_ref(KeySlot::Legacy, "real-version");
        let mut md = HashMap::new();
        for k in RESERVED_CRYPTO_METADATA_KEYS {
            md.insert(k.to_string(), "attacker-controlled".to_string());
        }
        apply_crypto_metadata(&mut md, &key_ref);
        assert_eq!(md.get(META_ENCRYPTED).unwrap(), "age");
        assert_eq!(md.get(META_CRYPTO_SCHEMA).unwrap(), "1");
        assert_eq!(md.get(META_KEY_ID).unwrap(), key_ref.key_id.as_str());
        assert_eq!(md.get(META_KEY_VERSION).unwrap(), "real-version");
        assert_eq!(md.get(META_KEY_SLOT).unwrap(), "legacy");
        assert!(
            !md.values().any(|v| v == "attacker-controlled"),
            "no reserved key may retain a caller-supplied value"
        );
    }

    #[test]
    fn key_slot_string_round_trip() {
        assert_eq!(KeySlot::Legacy.as_str(), "legacy");
        assert_eq!(KeySlot::Retained.as_str(), "retained");
        assert_eq!(KeySlot::parse("legacy"), Some(KeySlot::Legacy));
        assert_eq!(KeySlot::parse("retained"), Some(KeySlot::Retained));
        assert_eq!(KeySlot::parse("bogus"), None);
        assert_eq!(KeySlot::parse(""), None);
    }

    #[test]
    fn reserved_crypto_metadata_key_predicate() {
        for k in RESERVED_CRYPTO_METADATA_KEYS {
            assert!(is_reserved_crypto_metadata_key(k), "{k} must be reserved");
        }
        assert!(!is_reserved_crypto_metadata_key("user_tag"));
        assert!(!is_reserved_crypto_metadata_key("xv_random"));
        assert!(!is_reserved_crypto_metadata_key("xv_encrypte")); // near-miss
    }

    #[test]
    fn key_id_parse_accepts_valid_and_rejects_malformed() {
        let good = AttachmentKeyId::derive(RECIPIENT);
        assert_eq!(AttachmentKeyId::parse(good.as_str()), Some(good.clone()));
        let hex64 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(AttachmentKeyId::parse(&format!("ak1-{hex64}")).is_some());
        // too short / too long
        assert_eq!(
            AttachmentKeyId::parse(&format!("ak1-{}", &hex64[..63])),
            None
        );
        assert_eq!(AttachmentKeyId::parse(&format!("ak1-{hex64}0")), None);
        // uppercase hex rejected
        assert_eq!(
            AttachmentKeyId::parse(
                "ak1-0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            None
        );
        // non-hex char
        assert_eq!(
            AttachmentKeyId::parse(
                "ak1-g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde"
            ),
            None
        );
        // wrong / missing prefix
        assert_eq!(AttachmentKeyId::parse(&format!("ak2-{hex64}")), None);
        assert_eq!(AttachmentKeyId::parse(hex64), None);
        assert_eq!(AttachmentKeyId::parse(""), None);
    }

    #[test]
    fn pointer_v1_raw_identity_is_classified() {
        use age::secrecy::ExposeSecret;
        let identity = age::x25519::Identity::generate();
        let raw = identity.to_string().expose_secret().to_string();
        assert!(raw.starts_with("AGE-SECRET-KEY-1"), "{raw}");
        assert_eq!(parse_pointer_value(&raw), Some(PointerKind::V1RawIdentity));
    }

    #[test]
    fn pointer_v2_without_legacy_round_trips() {
        let active = AttachmentKeyId::derive(RECIPIENT);
        let value = format_v2_pointer(&active, None);
        assert_eq!(
            parse_pointer_value(&value),
            Some(PointerKind::V2 {
                active,
                legacy: None
            })
        );
    }

    #[test]
    fn pointer_v2_with_legacy_round_trips() {
        let active = AttachmentKeyId::derive(RECIPIENT);
        let legacy = AttachmentKeyId::derive(
            "age1legacyrecipientxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        );
        let value = format_v2_pointer(&active, Some(&legacy));
        assert_eq!(
            parse_pointer_value(&value),
            Some(PointerKind::V2 {
                active,
                legacy: Some(legacy)
            })
        );
    }

    #[test]
    fn pointer_malformed_values_are_rejected() {
        assert_eq!(parse_pointer_value("garbage"), None);
        assert_eq!(parse_pointer_value(""), None);
        assert_eq!(parse_pointer_value("xv-attachment-key-pointer/v2:"), None);
        assert_eq!(
            parse_pointer_value("xv-attachment-key-pointer/v2:not-an-id"),
            None
        );
        assert_eq!(
            parse_pointer_value(
                "xv-attachment-key-pointer/v3:ak1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            None
        );
        // valid active but malformed legacy => whole pointer rejected
        assert_eq!(
            parse_pointer_value(
                "xv-attachment-key-pointer/v2:ak1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:legacy=bad"
            ),
            None
        );
    }

    #[test]
    fn retained_record_name_is_strict() {
        let id = AttachmentKeyId::derive(RECIPIENT);
        assert_eq!(
            retained_record_name(&id),
            format!("xv-attachment-key-{}", id.as_str())
        );
        assert!(retained_record_name(&id).starts_with("xv-attachment-key-ak1-"));
    }

    #[test]
    fn material_derives_id_from_identity_not_from_input() {
        use age::secrecy::ExposeSecret;
        let identity = age::x25519::Identity::generate();
        let raw = identity.to_string().expose_secret().to_string();
        let expected_id = AttachmentKeyId::derive(&identity.to_public().to_string());

        let material = AttachmentKeyMaterial::from_identity(
            KeySlot::Retained,
            SecretVersion::new("v1"),
            Zeroizing::new(raw.clone()),
        )
        .expect("valid identity");

        assert_eq!(material.reference().key_id, expected_id);
        assert_eq!(material.reference().slot, KeySlot::Retained);
        assert_eq!(material.reference().provider_version.as_str(), "v1");
        assert!(material.verify_id(&expected_id));
        assert!(!material.verify_id(&AttachmentKeyId::derive("age1other")));
        assert_eq!(material.expose_identity(), raw.trim());
    }

    #[test]
    fn material_rejects_non_identity() {
        assert!(AttachmentKeyMaterial::from_identity(
            KeySlot::Retained,
            SecretVersion::new("v1"),
            Zeroizing::new("not-an-age-key".to_string()),
        )
        .is_none());
    }

    fn schema1_metadata(key_ref: &AttachmentKeyRef) -> HashMap<String, String> {
        let mut md = HashMap::new();
        apply_crypto_metadata(&mut md, key_ref);
        md
    }

    const HEX64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn reserved_class_exact_pointer() {
        assert_eq!(
            classify_reserved_name("xv-attachment-key"),
            ReservedClass::ActivePointer
        );
        assert!(generic_mutation_blocked("xv-attachment-key"));
    }

    #[test]
    fn reserved_class_strict_retained_record() {
        let name = format!("xv-attachment-key-ak1-{HEX64}");
        assert_eq!(
            classify_reserved_name(&name),
            ReservedClass::StrictRetainedRecord
        );
        assert!(generic_mutation_blocked(&name));
        assert!(is_strict_retained_record_name(&name));
    }

    #[test]
    fn hidden_from_listing_hides_pointer_and_marked_records_only() {
        let strict = format!("xv-attachment-key-ak1-{HEX64}");
        // Exact pointer is always hidden, regardless of content type.
        assert!(hidden_from_generic_listing("xv-attachment-key", ""));
        assert!(hidden_from_generic_listing("xv-attachment-key", "anything"));
        // Strict record hidden only when marked.
        assert!(hidden_from_generic_listing(
            &strict,
            KEY_RECORD_CONTENT_TYPE
        ));
        assert!(!hidden_from_generic_listing(&strict, ""));
        assert!(!hidden_from_generic_listing(
            &strict,
            "application/x-age-identity"
        ));
        // Broad-prefix / ordinary names are never hidden.
        assert!(!hidden_from_generic_listing("xv-attachment-key-notes", ""));
        assert!(!hidden_from_generic_listing(
            "xv-attachment-key-notes",
            KEY_RECORD_CONTENT_TYPE
        ));
        assert!(!hidden_from_generic_listing("my-secret", ""));
    }

    #[test]
    fn reserved_class_broad_prefix_is_ordinary() {
        // Broad prefix but NOT strict format — ordinary user secrets.
        for name in [
            "xv-attachment-key-notes",
            "xv-attachment-key-",
            "xv-attachment-key-ak1-tooshort",
            &format!("xv-attachment-key-ak1-{}0", HEX64), // 65 hex
            &format!("xv-attachment-key-ak2-{HEX64}"),    // wrong tag
            &format!("xv-attachment-key-AK1-{HEX64}"),    // uppercase tag → not stripped
            &format!("xv-attachment-key-ak1-{}", HEX64.to_uppercase()), // uppercase hex
            "my-secret",
            "database-password",
        ] {
            assert_eq!(
                classify_reserved_name(name),
                ReservedClass::Ordinary,
                "{name} must be ordinary"
            );
            assert!(!generic_mutation_blocked(name), "{name} must be mutable");
        }
    }

    #[test]
    fn classify_passthrough_for_ordinary_and_foreign_files() {
        let empty = HashMap::new();
        // Ordinary non-age file, not managed.
        assert_eq!(
            classify_download("docs/readme.md", &empty, false),
            DownloadPlan::Passthrough
        );
        // Foreign age file, not managed (no namespace, no flag).
        assert_eq!(
            classify_download("secrets.age", &empty, true),
            DownloadPlan::Passthrough
        );
    }

    #[test]
    fn classify_managed_non_ciphertext_fails_closed() {
        let empty = HashMap::new();
        // Managed by namespace but bytes are not age ciphertext.
        assert_eq!(
            classify_download("attachments/db/cert.pem", &empty, false),
            DownloadPlan::FailClosedNonCiphertext
        );
        // Managed by flag but bytes are not age ciphertext.
        let mut flagged = HashMap::new();
        flagged.insert(META_ENCRYPTED.to_string(), ENC_VALUE_AGE.to_string());
        assert_eq!(
            classify_download("some/where.bin", &flagged, false),
            DownloadPlan::FailClosedNonCiphertext
        );
    }

    #[test]
    fn classify_managed_age_without_schema_is_legacy() {
        let empty = HashMap::new();
        assert_eq!(
            classify_download("attachments/db/cert.pem", &empty, true),
            DownloadPlan::LegacyNoSchema
        );
    }

    #[test]
    fn classify_schema1_with_complete_ref_decrypts() {
        let key_ref = sample_ref(KeySlot::Retained, "v-xyz");
        let md = schema1_metadata(&key_ref);
        assert_eq!(
            classify_download("attachments/db/cert.pem", &md, true),
            DownloadPlan::Schema1 { key_ref }
        );
    }

    #[test]
    fn classify_schema1_with_broken_ref_never_falls_back() {
        let key_ref = sample_ref(KeySlot::Retained, "v-xyz");
        // Missing key id.
        let mut md = schema1_metadata(&key_ref);
        md.remove(META_KEY_ID);
        assert_eq!(
            classify_download("attachments/db/cert.pem", &md, true),
            DownloadPlan::ReferenceInvalid
        );
        // Malformed key id.
        let mut md = schema1_metadata(&key_ref);
        md.insert(META_KEY_ID.to_string(), "ak1-nothex".to_string());
        assert_eq!(
            classify_download("attachments/db/cert.pem", &md, true),
            DownloadPlan::ReferenceInvalid
        );
        // Missing version.
        let mut md = schema1_metadata(&key_ref);
        md.insert(META_KEY_VERSION.to_string(), String::new());
        assert_eq!(
            classify_download("attachments/db/cert.pem", &md, true),
            DownloadPlan::ReferenceInvalid
        );
        // Bad slot.
        let mut md = schema1_metadata(&key_ref);
        md.insert(META_KEY_SLOT.to_string(), "bogus".to_string());
        assert_eq!(
            classify_download("attachments/db/cert.pem", &md, true),
            DownloadPlan::ReferenceInvalid
        );
    }

    #[test]
    fn classify_unknown_schema_never_falls_back() {
        let key_ref = sample_ref(KeySlot::Retained, "v-xyz");
        let mut md = schema1_metadata(&key_ref);
        md.insert(META_CRYPTO_SCHEMA.to_string(), "2".to_string());
        assert_eq!(
            classify_download("attachments/db/cert.pem", &md, true),
            DownloadPlan::ReferenceInvalid
        );
    }

    #[test]
    fn material_never_leaks_identity_through_reference_debug() {
        use age::secrecy::ExposeSecret;
        let identity = age::x25519::Identity::generate();
        let raw = identity.to_string().expose_secret().to_string();
        let material = AttachmentKeyMaterial::from_identity(
            KeySlot::Retained,
            SecretVersion::new("v1"),
            Zeroizing::new(raw.clone()),
        )
        .unwrap();
        // The public reference is the only Debug-able view; it must not carry
        // the raw identity. (AttachmentKeyMaterial itself has no Debug impl.)
        let dbg = format!("{:?}", material.reference());
        assert!(!dbg.contains(raw.trim()), "reference Debug leaked identity");
        assert!(
            !dbg.contains("AGE-SECRET-KEY"),
            "reference Debug leaked identity"
        );
    }

    #[test]
    fn alias_inputs_must_not_bypass_the_reserved_guard() {
        // Azure Key Vault is case-insensitive and the shared sanitizer maps
        // underscores and repeated hyphens onto '-'. Each of these therefore
        // addresses the SAME provider secret as the exact reserved pointer,
        // so the guard must classify them identically.
        for alias in [
            "xv-attachment-key",
            "XV-ATTACHMENT-KEY",
            "Xv-Attachment-Key",
            "xv_attachment_key",
            "xv--attachment--key",
        ] {
            let canon = canonicalize_secret_name(alias, BackendKind::Azure)
                .unwrap_or_else(|| panic!("canonicalization failed for {alias}"));
            assert_eq!(
                canon.provider_identity, ACTIVE_POINTER_SECRET,
                "{alias} must canonicalize onto the reserved pointer"
            );
            assert!(
                generic_mutation_blocked(&canon.provider_identity),
                "{alias} must be mutation-blocked"
            );
            assert!(
                hidden_from_generic_listing(&canon.provider_identity, ""),
                "{alias} must be hidden"
            );
        }
    }

    #[test]
    fn guard_entrypoints_block_alias_spellings_without_a_backend_kind() {
        // CLI/Web guard call sites do not know the target provider, so the
        // guard must fail closed across every provider canonicalization.
        for alias in [
            "xv-attachment-key",
            "XV-ATTACHMENT-KEY",
            "xv_attachment_key",
            "xv--attachment--key",
            "  xv-attachment-key  ",
            "xv attachment key",
        ] {
            assert!(
                generic_mutation_blocked_canonical(alias),
                "{alias} must be mutation-blocked"
            );
            assert!(
                hidden_from_generic_listing_canonical(alias, ""),
                "{alias} must be hidden"
            );
        }
        // Ordinary names, and unmarked strict-format user collisions, are
        // unaffected.
        assert!(!generic_mutation_blocked_canonical("my-secret"));
        assert!(!generic_mutation_blocked_canonical(
            "xv-attachment-key-notes"
        ));
        assert!(!hidden_from_generic_listing_canonical("my-secret", ""));
        let strict = format!("xv-attachment-key-ak1-{HEX64}");
        assert!(!hidden_from_generic_listing_canonical(&strict, ""));
        assert!(hidden_from_generic_listing_canonical(
            &strict,
            KEY_RECORD_CONTENT_TYPE
        ));
    }
}
