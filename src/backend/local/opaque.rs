//! Opaque on-disk filenames for the local secret backend.
//!
//! When the `[local].opaque_filenames` option is enabled, a secret's on-disk
//! files are named by a *keyed* hash of the secret name rather than by a
//! reversible URL-encoding of the name. A directory listing then reveals no
//! secret names, existence-by-name, or count beyond an upper bound to anyone
//! lacking the age identity.
//!
//! - **Filename stem:** `base32_nopad_lowercase(HMAC_SHA256(index_key,
//!   name.as_bytes()))[..26]` (128 bits). HMAC (keyed) — not a bare digest — so
//!   an attacker cannot confirm a guessed name by hashing it themselves.
//! - **`index_key`:** `HKDF-SHA256` over the age identity's serialized secret
//!   scalar with `info = "xv-local-filename-index/v1"`. Available exactly when
//!   the backend can already decrypt; un-derivable without the identity.
//! - **Reverse lookup:** `list_secrets` needs real names, so an age-encrypted
//!   `.index.age` maps each stem back to its name.
//!
//! See `docs/plans/2026-06-19-local-secret-filename-opaquing.md`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use age::secrecy::ExposeSecret;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::backend::error::BackendError;
use crate::utils::helpers::write_private;

use super::crypto;

type HmacSha256 = Hmac<Sha256>;

/// Length of an opaque filename stem in base32 characters. 26 chars * 5 bits =
/// 130 bits of HMAC output (≈128-bit security), well below any realistic
/// collision concern for per-vault secret counts.
pub const STEM_LEN: usize = 26;

/// HKDF `info` string binding derived index keys to this scheme + version.
const HKDF_INFO: &[u8] = b"xv-local-filename-index/v1";

/// Name of the age-encrypted reverse index inside a vault's `secrets/` dir.
pub const INDEX_FILE: &str = ".index.age";

// ---------------------------------------------------------------------------
// Key derivation + stem computation
// ---------------------------------------------------------------------------

/// Derive the per-store `index_key` from the age identity.
///
/// IKM is the identity's serialized secret (the `AGE-SECRET-KEY-1…` bech32
/// string, which encodes the x25519 secret scalar) — available only to the
/// identity holder, so an attacker with mere directory read access cannot
/// reproduce any stem. Rotating the identity rotates every stem (a full
/// re-migration), which is the accepted v1 trade-off.
pub fn derive_index_key(identity: &age::x25519::Identity) -> [u8; 32] {
    let secret = identity.to_string();
    let hk = Hkdf::<Sha256>::new(None, secret.expose_secret().as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    okm
}

/// Compute the opaque filename stem for `name` under `index_key`.
///
/// HMACs the **raw UTF-8 name bytes** (`name.as_bytes()`) — not percent-encoded
/// and not Unicode-normalized — so byte-distinct names (e.g. NFC vs NFD) map to
/// distinct stems, preserving the backend's byte-exact name identity.
pub fn opaque_stem(index_key: &[u8; 32], name: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(index_key).expect("HMAC-SHA256 accepts any key length");
    mac.update(name.as_bytes());
    let tag = mac.finalize().into_bytes();
    let encoded = data_encoding::BASE32_NOPAD
        .encode(&tag)
        .to_ascii_lowercase();
    // base32 output is ASCII, so a byte slice on a char boundary is safe.
    encoded[..STEM_LEN].to_string()
}

/// Whether `stem` matches the fixed opaque pattern `^[a-z2-7]{26}$`.
///
/// Used as a cheap on-disk shape heuristic only. Legacy `encode_name` stems
/// usually contain `%`, but a 26-character secret name using only `a-z` and
/// `2-7` can match this pattern without being the HMAC stem — migration and
/// layout code must compare against [`is_canonical_stem`] (or [`opaque_stem`])
/// after resolving the real name from metadata, not rely on this alone.
#[allow(dead_code)] // public shape heuristic; production paths use is_canonical_stem
pub fn is_opaque_stem(stem: &str) -> bool {
    stem.len() == STEM_LEN && stem.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}

/// Whether `stem` is the canonical HMAC stem for `name` under `index_key`.
pub fn is_canonical_stem(index_key: &[u8; 32], stem: &str, name: &str) -> bool {
    stem == opaque_stem(index_key, name)
}

// ---------------------------------------------------------------------------
// Encrypted reverse index
// ---------------------------------------------------------------------------

/// One entry in the encrypted reverse index: stem → original name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// The secret's original (byte-exact) name.
    pub name: String,
    /// Index entry schema version.
    #[serde(default = "default_entry_version")]
    pub v: u8,
}

fn default_entry_version() -> u8 {
    1
}

/// In-memory reverse index: `file_stem` → entry. `BTreeMap` for deterministic
/// on-disk ordering (no name leak via ordering, and stable diffs).
pub type Index = BTreeMap<String, IndexEntry>;

/// Path to the encrypted index inside a vault's `secrets/` directory.
pub fn index_path(secrets_dir: &Path) -> PathBuf {
    secrets_dir.join(INDEX_FILE)
}

/// Load and decrypt the reverse index. Returns an empty index when the file is
/// absent (un-migrated or empty store).
pub fn load_index(
    secrets_dir: &Path,
    identity: &age::x25519::Identity,
) -> Result<Index, BackendError> {
    let path = index_path(secrets_dir);
    if !path.exists() {
        return Ok(Index::new());
    }
    let raw = fs::read(&path)
        .map_err(|e| BackendError::Internal(format!("read index {}: {e}", path.display())))?;
    let json = if crypto::is_age_encrypted(&raw) {
        crypto::decrypt_bytes(&raw, identity)
            .map_err(|e| BackendError::Internal(format!("decrypt index: {e}")))?
    } else {
        // Defensive: an un-encrypted index should never occur, but parse it
        // rather than silently dropping every mapping.
        zeroize::Zeroizing::new(raw)
    };
    serde_json::from_slice(&json)
        .map_err(|e| BackendError::Internal(format!("parse index {}: {e}", path.display())))
}

/// Encrypt and atomically write the reverse index to `<secrets_dir>/.index.age`.
///
/// Reuses the backend's `write_private` (0600, O_NOFOLLOW) + `encrypt_bytes`
/// path; the temp-file + rename keeps the index intact if a write is
/// interrupted. Callers must already hold the vault `fs2` lock.
pub fn save_index(
    secrets_dir: &Path,
    index: &Index,
    recipients: &[age::x25519::Recipient],
) -> Result<(), BackendError> {
    let path = index_path(secrets_dir);
    let json = serde_json::to_vec(index)
        .map_err(|e| BackendError::Internal(format!("serialize index: {e}")))?;
    let ciphertext = crypto::encrypt_bytes(&json, recipients)
        .map_err(|e| BackendError::Internal(format!("encrypt index: {e}")))?;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_file_name(format!(".{INDEX_FILE}.tmp.{}.{ts}", std::process::id()));
    write_private(&tmp, &ciphertext)
        .map_err(|e| BackendError::Internal(format!("write temp index {}: {e}", tmp.display())))?;
    fs::rename(&tmp, &path)
        .map_err(|e| BackendError::Internal(format!("activate index {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> age::x25519::Identity {
        age::x25519::Identity::generate()
    }

    #[test]
    fn stem_is_deterministic_and_well_formed() {
        let id = test_identity();
        let key = derive_index_key(&id);
        let a = opaque_stem(&key, "DB-PASSWORD");
        let b = opaque_stem(&key, "DB-PASSWORD");
        assert_eq!(a, b, "stem must be deterministic for a fixed key + name");
        assert_eq!(a.len(), STEM_LEN);
        assert!(is_opaque_stem(&a), "stem must match the opaque pattern");
    }

    #[test]
    fn wrong_key_yields_different_stem() {
        let key1 = derive_index_key(&test_identity());
        let key2 = derive_index_key(&test_identity());
        assert_ne!(
            opaque_stem(&key1, "aws-key"),
            opaque_stem(&key2, "aws-key"),
            "a different identity (key) must produce a different stem"
        );
    }

    #[test]
    fn distinct_names_yield_distinct_stems() {
        let key = derive_index_key(&test_identity());
        assert_ne!(opaque_stem(&key, "a"), opaque_stem(&key, "b"));
    }

    #[test]
    fn legacy_stems_are_not_opaque() {
        // Percent-encoded legacy stems contain '%' and so fail the pattern.
        assert!(!is_opaque_stem("DB%2DPASSWORD"));
        assert!(!is_opaque_stem("short"));
        assert!(!is_opaque_stem(&"a".repeat(27)));
        // Uppercase / digits outside 2-7 are rejected.
        assert!(!is_opaque_stem(&"A".repeat(26)));
        assert!(!is_opaque_stem(&"1".repeat(26)));
    }

    #[test]
    fn opaque_looking_legacy_name_is_not_canonical() {
        let id = test_identity();
        let key = derive_index_key(&id);
        let name = "abcdefghijklmnopqrstuvwxyz";
        assert!(is_opaque_stem(name));
        assert!(!is_canonical_stem(&key, name, name));
        assert!(is_canonical_stem(&key, &opaque_stem(&key, name), name));
    }
}
