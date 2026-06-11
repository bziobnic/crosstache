# xv P2P Secret Sharing — Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Ship `xv identity`, `xv share`, and `xv claim` so any two xv users can send encrypted secrets to each other out-of-band of their respective vaults, with origin authentication and a relay-mediated handoff.

**Architecture:** Each xv user has a long-lived identity = (age x25519 keypair + ed25519 signing keypair) stored in a passphrase-wrapped JSON file at `~/.config/xv/identity.json` (optionally cached in the OS keyring). `xv share` reads a secret from the active vault, signs+encrypts it to one or more recipient pubkeys via age, uploads the ASCII-armored blob to a relay, and prints a short claim code. `xv claim <code>` fetches, verifies the signer pubkey (TOFU-with-confirmation), decrypts, and inserts into the local vault.

**Tech Stack:** `age = 0.10` (already in `Cargo.toml`) + `armor` feature, `ed25519-dalek = 2`, `data-encoding = 2.6`, `bip39 = 2`, `secrecy = 0.8` (transitive via age), `reqwest` (already present) for the relay HTTP client, plus the existing `secrecy`, `zeroize`, `sha2`, and `serde_json` deps.

**Scope of this plan:** the CLI client only. The relay is a separate, smaller plan (see Task 13 — stub-then-spike).

**Out of scope for v1:** hardware-backed identities (YubiKey), forward secrecy via ephemeral request-keys, machine-to-machine flows, auto-rotate-after-share. All deferred to v2; design hooks called out where they matter.

**Spike basis:** `~/crosstache/spikes/` — VALIDATED 001 age-roundtrip, 002 sign-then-encrypt, 003b self-vault-storage, 004 claim-code-ux; PARTIAL 003a keyring (cache only). Verbatim copy-pasteable crypto code from those spikes is referenced throughout.

**Branch / PR workflow:** `main` is branch-protected; every task is its own PR via OMC autopilot per memory; release on `v*` tag. Two non-hermetic tests already exist (`cli::secret_ops::tests::{azure,local}_trait_vault_resolution_*`) — the same gating pattern applies to identity tests that need a real Secret Service.

---

## Task index

| # | Task | Files touched (primary) |
|---|------|-------------------------|
| 0 | Pre-flight: deps + module skeletons | `Cargo.toml`, `src/lib.rs`, `src/identity/mod.rs`, `src/share/mod.rs` |
| 1 | `identity::Identity` type + `init`/`load` | `src/identity/mod.rs`, `src/identity/store.rs` |
| 2 | Passphrase-wrap round-trip (TDD) | `src/identity/store.rs`, `tests/identity_store.rs` |
| 3 | Identity peers / trust DB | `src/identity/peers.rs` |
| 4 | Fingerprint helpers + Crockford encoding | `src/identity/fingerprint.rs` |
| 5 | `share::encrypt` core | `src/share/crypto.rs` |
| 6 | `share::decrypt` + verify | `src/share/crypto.rs` |
| 7 | Claim-code generator + parser | `src/share/claim_code.rs` |
| 8 | Relay client (trait + HTTP impl) | `src/share/relay.rs` |
| 9 | CLI: `xv identity init / show / import / trust / doctor` | `src/cli/identity_ops.rs`, `src/cli/commands.rs` |
| 10 | CLI: `xv share` | `src/cli/share_ops.rs`, `src/cli/commands.rs` |
| 11 | CLI: `xv claim` | `src/cli/share_ops.rs`, `src/cli/commands.rs` |
| 12 | OS-keyring opt-in cache (003a follow-up) | `src/identity/cache.rs` |
| 13 | Relay stub for local dev + e2e smoke test | `tests/e2e_share_claim.rs`, `scripts/dev-relay.py` |
| 14 | Docs + README updates | `README.md`, `docs/identity.md`, `docs/sharing.md` |

Each task is bite-sized (2-5 minutes of focused work for the implementer), tested, and committed independently.

---

## Task 0: Pre-flight — deps + module skeletons

**Objective:** Land the crate dependencies and empty module files so subsequent tasks compile in isolation.

**Files:**
- Modify: `Cargo.toml` (deps section)
- Create: `src/identity/mod.rs`
- Create: `src/identity/store.rs`
- Create: `src/identity/peers.rs`
- Create: `src/identity/fingerprint.rs`
- Create: `src/identity/cache.rs`
- Create: `src/share/mod.rs`
- Create: `src/share/crypto.rs`
- Create: `src/share/claim_code.rs`
- Create: `src/share/relay.rs`
- Modify: `src/lib.rs` (add `pub mod identity; pub mod share;`)

**Step 1: Add deps to `Cargo.toml`**

In the `[dependencies]` section, change `age = "0.10"` → `age = { version = "0.10", features = ["armor"] }` and add:

```toml
ed25519-dalek = { version = "2", features = ["rand_core"] }
data-encoding = "2.6"
bip39 = "2"
```

`rand`, `sha2`, `serde`, `serde_json`, `zeroize`, `reqwest`, `secrecy` (transitive), `tokio` are already present.

**Step 2: Create empty module files**

Each new file: `// stub — Task <N>` only.

**Step 3: Add modules to `src/lib.rs`**

```rust
pub mod identity;
pub mod share;
```

**Step 4: Verify build**

Run: `cargo build`
Expected: PASS, only dead-code warnings.

**Step 5: Commit**

```bash
git checkout -b feat/p2p-sharing-task-00-skeleton
git add Cargo.toml Cargo.lock src/identity src/share src/lib.rs
git commit -m "feat(sharing): module skeletons + age armor feature"
```

---

## Task 1: `identity::Identity` type + `init`/`load`

**Objective:** Define the in-memory identity type and the bare-minimum file shape. No crypto yet.

**Files:**
- Modify: `src/identity/mod.rs`
- Modify: `src/identity/store.rs`

**Step 1: Define types in `src/identity/mod.rs`**

```rust
mod store;
mod peers;
mod fingerprint;
mod cache;

pub use store::{IdentityStore, IdentityFile};
pub use peers::{PeerStore, Peer};
pub use fingerprint::Fingerprint;

use age::x25519;
use ed25519_dalek::SigningKey;

/// In-memory unlocked identity. Holds both the encryption (age x25519)
/// and signing (ed25519) keys for the local user. Keep the lifetime short.
pub struct Identity {
    pub age_identity: x25519::Identity,
    pub sign_key: SigningKey,
}

impl Identity {
    pub fn age_recipient(&self) -> x25519::Recipient { self.age_identity.to_public() }
    pub fn sign_pubkey(&self) -> ed25519_dalek::VerifyingKey { self.sign_key.verifying_key() }
}
```

**Step 2: Define the persisted shape in `src/identity/store.rs`**

```rust
use serde::{Deserialize, Serialize};

/// On-disk identity file. v1 format.
#[derive(Serialize, Deserialize)]
pub struct IdentityFile {
    pub version: u8,                  // = 1
    pub age_pubkey: String,           // "age1..."
    pub sign_pubkey_hex: String,      // 64-char ed25519 pub
    pub sealed: SealedKeys,           // passphrase-wrapped private halves
}

#[derive(Serialize, Deserialize)]
pub struct SealedKeys {
    /// age::Encryptor::with_user_passphrase ASCII-armored output.
    /// Decrypted bytes = JSON { "age_secret": "AGE-SECRET-KEY-1...", "sign_secret_hex": "..." }
    pub passphrase_wrap: String,
}

pub struct IdentityStore {
    pub path: std::path::PathBuf,
}
```

**Step 3: Build, no tests yet**

Run: `cargo build`
Expected: PASS.

**Step 4: Commit**

```bash
git add src/identity/
git commit -m "feat(identity): type skeleton + on-disk v1 format"
```

---

## Task 2: Passphrase-wrap round-trip (TDD)

**Objective:** Implement `IdentityStore::init`, `load`, and `rotate` using age passphrase wrapping. Verbatim from spike 003b.

**Files:**
- Modify: `src/identity/store.rs`
- Create: `tests/identity_store.rs`

**Step 1: Write failing test `tests/identity_store.rs`**

```rust
use crosstache::identity::{IdentityStore};
use secrecy::SecretString;
use tempfile::TempDir;

#[test]
fn init_then_load_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("identity.json");
    let pp = SecretString::new("correct horse battery staple".into());

    let store = IdentityStore { path: path.clone() };
    let id = store.init(&pp).expect("init");
    let pub_age = id.age_recipient().to_string();

    drop(id);
    let id2 = store.load(&pp).expect("load");
    assert_eq!(id2.age_recipient().to_string(), pub_age);
}

#[test]
fn wrong_passphrase_fails_loudly() {
    let tmp = TempDir::new().unwrap();
    let store = IdentityStore { path: tmp.path().join("id.json") };
    store.init(&SecretString::new("right".into())).unwrap();
    assert!(store.load(&SecretString::new("WRONG".into())).is_err());
}

#[test]
fn rotate_preserves_identity() {
    let tmp = TempDir::new().unwrap();
    let store = IdentityStore { path: tmp.path().join("id.json") };
    let pp1 = SecretString::new("pp1".into());
    let pp2 = SecretString::new("pp2".into());
    let pub1 = store.init(&pp1).unwrap().age_recipient().to_string();
    store.rotate(&pp1, &pp2).unwrap();
    let pub2 = store.load(&pp2).unwrap().age_recipient().to_string();
    assert_eq!(pub1, pub2);
    assert!(store.load(&pp1).is_err());
}
```

**Step 2: Run test to verify failure**

Run: `cargo test --test identity_store`
Expected: compile fail (`init`/`load`/`rotate` not defined).

**Step 3: Implement in `src/identity/store.rs`**

Use the code pattern verified in spike 003b at `~/crosstache/spikes/003b-self-vault-storage/src/main.rs`. Key calls:

- Generate: `age::x25519::Identity::generate()`, `SigningKey::generate(&mut OsRng)`.
- Wrap: `age::Encryptor::with_user_passphrase(pp)` over a JSON blob of the two secrets, then `ArmoredWriter` to get ASCII text.
- Unwrap: `age::Decryptor::new(&armored)?` → `Decryptor::Passphrase(d)` arm → `d.decrypt(&pp, None)?`.

API:

```rust
impl IdentityStore {
    pub fn init(&self, pp: &SecretString) -> crate::error::Result<Identity> { ... }
    pub fn load(&self, pp: &SecretString) -> crate::error::Result<Identity> { ... }
    pub fn rotate(&self, old: &SecretString, new: &SecretString)
        -> crate::error::Result<()> { ... }
    pub fn exists(&self) -> bool { self.path.exists() }
    pub fn read_pubkeys_only(&self) -> crate::error::Result<(String, String)> { ... } // no pp needed
}
```

Errors plug into existing `crate::error::Error` (extend it with `Identity(String)` and `Crypto(String)` variants if needed — see `src/error.rs`).

**Step 4: Run tests to verify pass**

Run: `cargo test --test identity_store`
Expected: 3 passed.

**Step 5: Commit**

```bash
git add src/identity/store.rs src/error.rs tests/identity_store.rs
git commit -m "feat(identity): passphrase-wrapped store with init/load/rotate"
```

---

## Task 3: Peer trust DB

**Objective:** Persist trusted peer pubkey pairs `(age_pub, sign_pub, label)` in a flat JSON file. Peers are imported, never auto-trusted.

**Files:**
- Modify: `src/identity/peers.rs`
- Create: `tests/identity_peers.rs`

**Step 1: Write failing test**

```rust
use crosstache::identity::{Peer, PeerStore};
use tempfile::TempDir;

#[test]
fn import_show_remove_peer() {
    let tmp = TempDir::new().unwrap();
    let store = PeerStore::new(tmp.path().join("peers.json"));
    let peer = Peer {
        label: "alice".into(),
        age_pubkey: "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p".into(),
        sign_pubkey_hex: "01".repeat(32),
    };
    store.import(peer.clone()).unwrap();
    let got = store.get("alice").unwrap();
    assert_eq!(got.age_pubkey, peer.age_pubkey);
    assert_eq!(store.list().unwrap().len(), 1);
    store.remove("alice").unwrap();
    assert!(store.get("alice").is_none());
}

#[test]
fn cannot_import_duplicate_label() {
    let tmp = TempDir::new().unwrap();
    let store = PeerStore::new(tmp.path().join("peers.json"));
    let p = Peer { label: "a".into(),
        age_pubkey: "age1...".into(), sign_pubkey_hex: "ff".repeat(32) };
    store.import(p.clone()).unwrap();
    assert!(store.import(p).is_err());
}
```

**Step 2: Run test to verify failure**

Run: `cargo test --test identity_peers`
Expected: compile fail.

**Step 3: Implement**

`Peer` is a plain `Serialize/Deserialize` struct. `PeerStore` is a file-backed `Vec<Peer>` with atomic-write semantics: write tempfile, fsync, rename. Use `tempfile::NamedTempFile::persist`.

**Step 4: Verify**

Run: `cargo test --test identity_peers`
Expected: 2 passed.

**Step 5: Commit**

```bash
git add src/identity/peers.rs tests/identity_peers.rs
git commit -m "feat(identity): peer trust store"
```

---

## Task 4: Fingerprint helpers + Crockford encoding

**Objective:** Provide `Fingerprint::of(age_pub, sign_pub) -> String` that returns the 16-char Crockford 4×4 fingerprint from spike 004, and a `verify_fingerprint(fp, peer) -> bool` helper.

**Files:**
- Modify: `src/identity/fingerprint.rs`

**Step 1: Write failing test (inline `#[cfg(test)]` module)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_is_stable_and_16_chars_plus_hyphens() {
        let fp = Fingerprint::of_raw(b"age-pub-32bytes-................", b"sign-pub-32bytes-...............");
        assert_eq!(fp.as_str().len(), 19); // 16 chars + 3 hyphens
        assert!(fp.as_str().chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        let again = Fingerprint::of_raw(b"age-pub-32bytes-................", b"sign-pub-32bytes-...............");
        assert_eq!(fp.as_str(), again.as_str());
    }
}
```

**Step 2: Implement**

Verbatim from spike 004 at `~/crosstache/spikes/004-claim-code-ux/src/main.rs:fingerprint16`. Wrap as a newtype:

```rust
pub struct Fingerprint(String);
impl Fingerprint {
    pub fn of_raw(age_pub: &[u8], sign_pub: &[u8]) -> Self { ... }
    pub fn of(age_pub: &age::x25519::Recipient, sign_pub: &ed25519_dalek::VerifyingKey) -> Self { ... }
    pub fn as_str(&self) -> &str { &self.0 }
}
```

Crockford alphabet: `0-9A-Z` minus `ILOU`. Sha-256 of `age_pub || sign_pub`, take first 10 bytes, encode → 16 chars, hyphenate every 4.

**Step 3: Test**

Run: `cargo test fingerprint`
Expected: 1 passed.

**Step 4: Commit**

```bash
git add src/identity/fingerprint.rs
git commit -m "feat(identity): TOFU fingerprint (Crockford 4x4 SHA-256)"
```

---

## Task 5: `share::encrypt` core

**Objective:** Pure-data API that takes `(plaintext, sender_identity, &[recipient_age_pubkey])` and returns ASCII-armored ciphertext. Sign-then-encrypt layering.

**Files:**
- Modify: `src/share/crypto.rs`

**Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use age::x25519;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn fake_identity() -> crate::identity::Identity {
        crate::identity::Identity {
            age_identity: x25519::Identity::generate(),
            sign_key: SigningKey::generate(&mut OsRng),
        }
    }

    #[test]
    fn encrypt_to_one_recipient_decrypts_for_that_recipient_only() {
        let alice = fake_identity();
        let bob = fake_identity();
        let mallory = fake_identity();

        let armored = encrypt(b"hunter2", &alice, &[bob.age_recipient()]).unwrap();

        // Bob can read it
        let opened = decrypt(armored.as_bytes(), &bob, None).unwrap();
        assert_eq!(opened.plaintext, b"hunter2");
        assert_eq!(opened.signer_pubkey, alice.sign_pubkey());

        // Mallory cannot
        assert!(decrypt(armored.as_bytes(), &mallory, None).is_err());
    }
}
```

**Step 2: Implement `encrypt`**

Verbatim from spike 002 at `~/crosstache/spikes/002-sign-then-encrypt/src/main.rs`:

```rust
const SIG_LEN: usize = 64;
const PUB_LEN: usize = 32;

pub fn encrypt(
    plaintext: &[u8],
    sender: &crate::identity::Identity,
    recipients: &[age::x25519::Recipient],
) -> crate::error::Result<String> {
    use age::Encryptor;
    use ed25519_dalek::Signer;
    use std::io::Write;

    // The signed payload includes the sender age public key so receivers can
    // compute the same TOFU fingerprint shown by `xv identity show`:
    // SHA-256(age_pub || sign_pub), Crockford 4x4.
    let sender_age_pub = sender.age_recipient().to_string();
    let mut signed_payload = Vec::new();
    signed_payload.extend_from_slice(sender_age_pub.as_bytes());
    signed_payload.push(0); // separator: age recipient strings are ASCII and contain no NUL
    signed_payload.extend_from_slice(plaintext);
    let sig = sender.sign_key.sign(&signed_payload);

    let age_len: u16 = sender_age_pub.len().try_into()
        .map_err(|_| crate::error::Error::Crypto("age pubkey too long".into()))?;
    let mut inner = Vec::with_capacity(SIG_LEN + PUB_LEN + 2 + sender_age_pub.len() + plaintext.len());
    inner.extend_from_slice(&sig.to_bytes());
    inner.extend_from_slice(sender.sign_pubkey().as_bytes());
    inner.extend_from_slice(&age_len.to_be_bytes());
    inner.extend_from_slice(sender_age_pub.as_bytes());
    inner.extend_from_slice(plaintext);

    let recip: Vec<Box<dyn age::Recipient + Send>> = recipients.iter()
        .map(|r| Box::new(r.clone()) as Box<dyn age::Recipient + Send>)
        .collect();
    let enc = Encryptor::with_recipients(recip)
        .ok_or_else(|| crate::error::Error::Crypto("no recipients".into()))?;

    let mut armored = Vec::new();
    {
        let armor_w = age::armor::ArmoredWriter::wrap_output(
            &mut armored, age::armor::Format::AsciiArmor)
            .map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
        let mut w = enc.wrap_output(armor_w)
            .map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
        w.write_all(&inner).map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
        let armor_w = w.finish().map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
        armor_w.finish().map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
    }
    Ok(String::from_utf8(armored)
        .map_err(|e| crate::error::Error::Crypto(e.to_string()))?)
}
```

**Step 3: Run test (fails — decrypt not done)**

Run: `cargo test --lib share::crypto`
Expected: compile fail.

(`decrypt` lands in Task 6 — keep this PR focused on `encrypt` and stub `decrypt`/`Opened` so the test compiles. Or fold Task 5+6 into one commit; both are fine. The plan keeps them split for review granularity.)

**Step 4: Commit**

```bash
git add src/share/crypto.rs
git commit -m "feat(share): sign-then-encrypt (age + ed25519)"
```

---

## Task 6: `share::decrypt` + verify

**Objective:** Reverse of Task 5. Returns plaintext + the signer public keys needed for signature verification and TOFU fingerprint display.

**Files:**
- Modify: `src/share/crypto.rs`

**Step 1: Implement `decrypt`**

```rust
pub struct Opened {
    pub plaintext: Vec<u8>,
    pub signer_pubkey: ed25519_dalek::VerifyingKey,
    pub signer_age_pubkey: age::x25519::Recipient,
}

pub fn decrypt(
    armored: &[u8],
    recipient: &crate::identity::Identity,
    expected_signer: Option<&ed25519_dalek::VerifyingKey>,
) -> crate::error::Result<Opened> {
    use age::Decryptor;
    use ed25519_dalek::{Signature, Verifier};
    use std::io::Read;
    use std::iter;

    let armor_r = age::armor::ArmoredReader::new(armored);
    let d = match Decryptor::new(armor_r)
        .map_err(|e| crate::error::Error::Crypto(e.to_string()))? {
        Decryptor::Recipients(d) => d,
        _ => return Err(crate::error::Error::Crypto("not recipient-style age".into())),
    };
    let mut reader = d.decrypt(iter::once(&recipient.age_identity as &dyn age::Identity))
        .map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
    let mut inner = Vec::new();
    reader.read_to_end(&mut inner)
        .map_err(|e| crate::error::Error::Crypto(e.to_string()))?;

    if inner.len() < SIG_LEN + PUB_LEN + 2 {
        return Err(crate::error::Error::Crypto("inner payload too short".into()));
    }
    let sig_bytes: [u8; SIG_LEN] = inner[..SIG_LEN].try_into().unwrap();
    let pub_bytes: [u8; PUB_LEN] = inner[SIG_LEN..SIG_LEN+PUB_LEN].try_into().unwrap();
    let age_len = u16::from_be_bytes(inner[SIG_LEN+PUB_LEN..SIG_LEN+PUB_LEN+2].try_into().unwrap()) as usize;
    let age_start = SIG_LEN + PUB_LEN + 2;
    let age_end = age_start + age_len;
    if inner.len() < age_end {
        return Err(crate::error::Error::Crypto("inner age pubkey truncated".into()));
    }
    let signer_age_str = std::str::from_utf8(&inner[age_start..age_end])
        .map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
    let signer_age_pubkey: age::x25519::Recipient = signer_age_str.parse()
        .map_err(|e| crate::error::Error::Crypto(format!("sender age pubkey: {e}")))?;
    let plaintext = inner[age_end..].to_vec();

    let sig = Signature::from_bytes(&sig_bytes);
    let signer = ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes)
        .map_err(|e| crate::error::Error::Crypto(e.to_string()))?;
    if let Some(expected) = expected_signer {
        if expected.as_bytes() != signer.as_bytes() {
            return Err(crate::error::Error::Crypto("signer pubkey mismatch".into()));
        }
    }
    let mut signed_payload = Vec::new();
    signed_payload.extend_from_slice(signer_age_str.as_bytes());
    signed_payload.push(0);
    signed_payload.extend_from_slice(&plaintext);
    signer.verify(&signed_payload, &sig)
        .map_err(|e| crate::error::Error::Crypto(format!("signature: {e}")))?;
    Ok(Opened { plaintext, signer_pubkey: signer, signer_age_pubkey })
}
```

**Step 2: Re-run Task 5 test**

Run: `cargo test --lib share::crypto`
Expected: 1 passed.

**Step 3: Add adversarial tests**

```rust
#[test]
fn tampered_ciphertext_rejected() { ... }
#[test]
fn impersonation_rejected_when_expected_signer_set() { ... }
#[test]
fn multi_recipient_both_decrypt() { ... }
```

(Mirror the assertions in `~/crosstache/spikes/002-sign-then-encrypt/src/main.rs`.)

Run: `cargo test --lib share::crypto`
Expected: 4 passed.

**Step 4: Commit**

```bash
git add src/share/crypto.rs
git commit -m "feat(share): decrypt + verify + adversarial tests"
```

---

## Task 7: Claim-code generator + parser

**Objective:** Generate 40-bit Crockford 4×2 codes (`GG6H-D5PP`), plus an equivalent 8-word BIP-39 representation for voice dictation. The parser must accept case-insensitive Crockford input with optional hyphens/whitespace **and** 8 BIP-39 words that decode back to the same 40-bit code.

**Files:**
- Modify: `src/share/claim_code.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_code_matches_expected_shape() {
        let c = ClaimCode::random();
        assert_eq!(c.as_str().len(), 9); // 4 + '-' + 4
        assert_eq!(c.as_str().chars().nth(4), Some('-'));
    }
    #[test]
    fn parses_case_insensitive_with_or_without_hyphen() {
        let c = ClaimCode::random();
        let canon = c.as_str().to_string();
        let with = canon.clone();
        let without = canon.replace('-', "");
        let lower = canon.to_lowercase();
        let p1: ClaimCode = with.parse().unwrap();
        let p2: ClaimCode = without.parse().unwrap();
        let p3: ClaimCode = lower.parse().unwrap();
        assert_eq!(p1.as_str(), c.as_str());
        assert_eq!(p2.as_str(), c.as_str());
        assert_eq!(p3.as_str(), c.as_str());
    }
    #[test]
    fn bip39_words_roundtrip_to_same_code() {
        let c = ClaimCode::random();
        let words = c.to_bip39_words();
        assert_eq!(words.split_whitespace().count(), 8);
        let parsed: ClaimCode = words.parse().unwrap();
        assert_eq!(parsed.as_str(), c.as_str());
    }

    #[test]
    fn bip39_words_accept_hyphenated_voice_output() {
        let c = ClaimCode::random();
        let words = c.to_bip39_words();
        let hyphenated = words.split_whitespace().collect::<Vec<_>>().join("-");
        let parsed: ClaimCode = hyphenated.parse().unwrap();
        assert_eq!(parsed.as_str(), c.as_str());
    }

    #[test]
    fn rejects_garbage() {
        assert!("not-a-code".parse::<ClaimCode>().is_err());
        assert!("ABCD-EF".parse::<ClaimCode>().is_err()); // wrong length
        assert!("abandon abandon abandon abandon abandon abandon abandon abandon".parse::<ClaimCode>().is_err()); // checksum/domain mismatch
    }
}
```

**Step 2: Implement**

Code = 40-bit random → Crockford base32 → 8 chars → split into 4+4 with hyphen. Store the canonical string, but keep helpers to convert to/from the underlying 5 raw bytes.

For BIP-39 voice codes, map the same 5 claim-code bytes into 8 words as:

1. `payload = raw_code_bytes` (5 bytes / 40 bits).
2. `checksum = sha256(b"xv-claim-words-v1" || payload)[0..6]` (48 bits).
3. Concatenate `payload || checksum` = 88 bits.
4. Split into eight 11-bit integers and index the English BIP-39 word list.

Decoding performs the inverse and rejects the words unless the checksum matches. This makes the 8-word form a deterministic alternate representation of the same 40-bit claim code instead of a second unrelated code space. Accept both space-separated words and the hyphen-separated form printed by `xv share --verbose-code`; hyphens are word separators for BIP-39 input, not Crockford separators, once the normalized token count is eight.

```rust
pub struct ClaimCode(String);

impl ClaimCode {
    pub fn random() -> Self { ... }   // 5 random bytes -> base32 -> uppercase -> 4-4
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn to_bytes(&self) -> [u8; 5] { ... }
    pub fn from_bytes(bytes: [u8; 5]) -> Self { ... }
    pub fn to_bip39_words(&self) -> String { ... } // 8 English words
    pub fn from_bip39_words(s: &str) -> Result<Self, crate::error::Error> { ... } // accepts space or hyphen separators; validates checksum
}

impl std::str::FromStr for ClaimCode {
    type Err = crate::error::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let word_tokens: Vec<&str> = s
            .split(|c: char| c.is_whitespace() || c == '-')
            .filter(|t| !t.is_empty())
            .collect();
        if word_tokens.len() == 8 {
            return ClaimCode::from_bip39_words(&word_tokens.join(" "));
        }
        let cleaned: String = s.chars().filter(|c| *c != '-' && !c.is_whitespace())
            .flat_map(|c| c.to_uppercase()).collect();
        if cleaned.len() != 8 { return Err(...); }
        // decode to validate it's a real Crockford string, then canonicalize
        ...
        Ok(ClaimCode(format!("{}-{}", &cleaned[..4], &cleaned[4..])))
    }
}
```

Reuse the Crockford `Encoding` builder from spike 004 and `bip39::Language::English.word_list()` for word lookup.

**Step 3: Verify**

Run: `cargo test --lib share::claim_code`
Expected: 5 passed.

**Step 4: Commit**

```bash
git add src/share/claim_code.rs
git commit -m "feat(share): claim code (Crockford 4x2, 40-bit)"
```

---

## Task 8: Relay client (trait + HTTP impl)

**Objective:** Trait `RelayClient` with one HTTP-backed implementation. Methods: `put(armored_blob, ttl) -> ClaimCode`; `get(code) -> armored_blob`; `delete(code)`. No auth in v1 — the blob is the auth.

**Files:**
- Modify: `src/share/relay.rs`

**Step 1: Define the trait**

```rust
#[async_trait::async_trait]
pub trait RelayClient: Send + Sync {
    async fn put(&self, ciphertext: &str, ttl_secs: u64)
        -> crate::error::Result<crate::share::claim_code::ClaimCode>;
    async fn get(&self, code: &crate::share::claim_code::ClaimCode)
        -> crate::error::Result<String>;
    async fn delete(&self, code: &crate::share::claim_code::ClaimCode)
        -> crate::error::Result<()>;
}
```

**Step 2: Implement `HttpRelay`**

```rust
pub struct HttpRelay {
    pub base_url: url::Url,
    pub http: reqwest::Client,
}
```

Wire format (relay API v1, to be implemented in the relay plan):
- `POST {base}/v1/blobs?ttl=<secs>` body = armored text/plain, returns `{ "code": "GG6H-D5PP" }`
- `GET  {base}/v1/blobs/{code}` returns body = armored text/plain
- `DELETE {base}/v1/blobs/{code}` returns 204

Errors: map 404 → `Error::ClaimNotFound`, 410 → `Error::ClaimExpired`, others → `Error::Relay`.

**Step 3: Mock-based unit test**

Use `mockito = "1"` (add as dev-dep) or a hand-rolled axum test server. Verify each method shape.

Run: `cargo test --lib share::relay`
Expected: 3 passed.

**Step 4: Commit**

```bash
git add src/share/relay.rs Cargo.toml
git commit -m "feat(share): relay client trait + HTTP impl"
```

---

## Task 9: CLI — `xv identity init / show / import / trust / doctor`

**Objective:** Wire the identity API into the CLI surface.

**Files:**
- Create: `src/cli/identity_ops.rs`
- Modify: `src/cli/commands.rs` (add `Identity { … }` to `Commands` enum + dispatch)
- Modify: `src/cli/mod.rs`

**Step 1: Add to `src/cli/commands.rs`**

In the `Commands` enum:

```rust
/// Manage your sharing identity (age + ed25519 keypairs).
Identity {
    #[command(subcommand)]
    action: IdentityAction,
},
```

```rust
#[derive(Subcommand)]
pub enum IdentityAction {
    /// Generate a new identity, passphrase-wrap it, write to ~/.config/xv/identity.json
    Init,
    /// Print this user's public identity (age pubkey, sign pubkey, fingerprint)
    Show {
        /// Output in a format suitable for `xv identity import` on a peer's machine
        #[arg(long)]
        export: bool,
    },
    /// Import a peer's public identity from a file or stdin
    Import {
        /// Path to peer identity (use `-` for stdin)
        path: String,
        /// Label to remember this peer under
        #[arg(long)]
        label: String,
    },
    /// List all trusted peers
    List,
    /// Remove a peer
    Untrust { label: String },
    /// Re-wrap identity under a new passphrase
    Rotate,
    /// Diagnose identity storage and optional OS-keyring cache
    Doctor,
},
```

**Step 2: Implement `src/cli/identity_ops.rs`**

One async function per subcommand. `init` calls `IdentityStore::init` (prompt passphrase twice via `rpassword`), `show` prints pubkeys + Fingerprint, etc.

The export format (Task 9, `show --export`) is the same JSON the importer reads:

```json
{
  "label": "alice",
  "age_pubkey": "age1...",
  "sign_pubkey_hex": "..."
}
```

**Step 3: Wire dispatch**

In `src/cli/commands.rs::run` (or wherever the `match commands.command` lives), add the `Identity { action }` arm dispatching into `identity_ops::*`.

**Step 4: Integration test**

Add `tests/cli_identity.rs`:

```rust
// Use `assert_cmd` (add as dev-dep) to run `xv identity init` in a tempdir
// (XDG_CONFIG_HOME=tempdir), then `xv identity show`, assert output contains "age1".
```

Run: `cargo test --test cli_identity`
Expected: passes.

**Step 5: Commit**

```bash
git add src/cli/identity_ops.rs src/cli/commands.rs src/cli/mod.rs Cargo.toml tests/cli_identity.rs
git commit -m "feat(cli): xv identity init/show/import/list/untrust/rotate/doctor"
```

---

## Task 10: CLI — `xv share`

**Objective:** Read a secret from the active vault, encrypt to one or more recipients, upload to relay, print claim code.

**Files:**
- Create: `src/cli/share_ops.rs`
- Modify: `src/cli/commands.rs`

**Step 1: Add to `Commands`**

```rust
/// Share a secret with one or more peers (out-of-band of the vault).
Share {
    /// Secret name in the active vault
    secret: String,
    /// Peer label(s) to share with. Repeat for multi-recipient.
    #[arg(long = "to", required = true)]
    to: Vec<String>,
    /// Claim-code TTL (default 24h)
    #[arg(long, default_value = "24h")]
    ttl: String,
    /// Also emit an 8-word BIP-39 code alongside the short code (for voice dictation)
    #[arg(long)]
    verbose_code: bool,
    /// Rotate the underlying secret in the vault immediately after the claim is fetched
    #[arg(long)]
    rotate_after_share: bool,
    /// Override the relay base URL (default from config)
    #[arg(long, env = "XV_RELAY_URL")]
    relay: Option<url::Url>,
},
```

**Step 2: Implement**

```rust
pub async fn share(ctx: &Context, args: ShareArgs) -> Result<()> {
    let pp = prompt_passphrase()?;
    let me = ctx.identity_store().load(&pp)?;
    let peers = ctx.peer_store();
    let recipients: Vec<age::x25519::Recipient> = args.to.iter().map(|label| {
        let p = peers.get(label)
            .ok_or_else(|| Error::UnknownPeer(label.clone()))?;
        p.age_pubkey.parse().map_err(...)
    }).collect::<Result<_>>()?;

    let secret_value = ctx.vault().get(&args.secret).await?; // existing secret API

    let armored = crate::share::crypto::encrypt(
        secret_value.as_bytes(), &me, &recipients,
    )?;

    let relay = HttpRelay::new(args.relay.unwrap_or_else(|| ctx.config().relay_url()));
    let code = relay.put(&armored, parse_ttl(&args.ttl)?).await?;

    if args.rotate_after_share {
        // record a pending rotation in vault metadata; the relay's
        // delete-on-claim hook fires the rotation. (Stub for v1 — log "rotation queued".)
    }

    println!("Share uploaded. To claim:");
    println!("  xv claim {}", code.as_str());
    if args.verbose_code {
        let words = code.to_bip39_words();
        // Hyphenated for copy/paste as one shell argument; ClaimCode::from_str
        // treats this as eight BIP-39 word tokens, not as a Crockford code.
        println!("  (or, by voice: {})", words.split_whitespace().collect::<Vec<_>>().join("-"));
    }
    Ok(())
}
```

**Step 3: Add unit test for arg parsing + relay call (mock relay)**

**Step 4: Verify**

Run: `cargo build && cargo test --test cli_share`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/cli/share_ops.rs src/cli/commands.rs tests/
git commit -m "feat(cli): xv share"
```

---

## Task 11: CLI — `xv claim`

**Objective:** Inverse of share. Fetch + verify + decrypt + insert into local vault.

**Files:**
- Modify: `src/cli/share_ops.rs`
- Modify: `src/cli/commands.rs`

**Step 1: Add to `Commands`**

```rust
/// Claim a shared secret using the code printed by `xv share`.
Claim {
    /// Claim code (e.g. GG6H-D5PP) or 8 BIP-39 words
    code: String,
    /// Name to store the secret under in the local vault (defaults to interactive prompt)
    #[arg(long)]
    as_name: Option<String>,
    /// Skip the TOFU trust prompt if the signer is already a known peer
    #[arg(long)]
    trust_existing_only: bool,
    /// Override the relay base URL
    #[arg(long, env = "XV_RELAY_URL")]
    relay: Option<url::Url>,
},
```

**Step 2: Implement**

```rust
pub async fn claim(ctx: &Context, args: ClaimArgs) -> Result<()> {
    let code: ClaimCode = args.code.parse()?;  // accepts BIP-39 too via FromStr branching
    let pp = prompt_passphrase()?;
    let me = ctx.identity_store().load(&pp)?;
    let relay = HttpRelay::new(args.relay.unwrap_or_else(|| ctx.config().relay_url()));
    let armored = relay.get(&code).await?;

    // First pass: decrypt without expected signer to discover who signed it.
    let opened = crate::share::crypto::decrypt(armored.as_bytes(), &me, None)?;

    // Look up signer in peer DB
    let peer = ctx.peer_store().find_by_sign_pubkey(&opened.signer_pubkey);
    if peer.is_none() {
        if args.trust_existing_only {
            return Err(Error::UnknownSigner);
        }
        let fp = Fingerprint::of(&opened.signer_age_pubkey, &opened.signer_pubkey);
        eprintln!("This share is signed by an UNKNOWN peer.");
        eprintln!("Fingerprint: {}", fp.as_str());
        if !dialoguer::Confirm::new()
            .with_prompt("Trust this signer for this claim only?")
            .interact()? { return Err(Error::ClaimAborted); }
    }

    let name = args.as_name.unwrap_or_else(|| prompt_name());
    ctx.vault().set(&name, &String::from_utf8(opened.plaintext)?).await?;

    // best-effort delete on relay
    let _ = relay.delete(&code).await;

    println!("Claim received and saved to '{}'", name);
    Ok(())
}
```

**Step 3: Integration test**

Use an in-process axum test server as the relay. End-to-end: identity A shares to identity B → B claims → plaintext matches.

**Step 4: Verify**

Run: `cargo test --test e2e_share_claim`
Expected: passes.

**Step 5: Commit**

```bash
git add src/cli/share_ops.rs src/cli/commands.rs tests/e2e_share_claim.rs
git commit -m "feat(cli): xv claim + e2e share/claim test"
```

---

## Task 12: OS-keyring opt-in cache

**Objective:** After a successful passphrase unwrap, optionally cache the in-memory secrets in the OS keyring under `service=crosstache-xv user=cache` with a TTL. Driven by config: `identity.cache = true`, `identity.cache_ttl = "15m"`.

**Files:**
- Modify: `src/identity/cache.rs`
- Modify: `Cargo.toml` (add keyring dep with explicit features)

**Spike-informed dep:**

```toml
keyring = { version = "3", default-features = false, features = ["sync-secret-service", "crypto-rust"] }
```

The `default-features = false` is mandatory — see spike 003a verdict: default features pick `linux-native` (kernel keyutils) instead of Secret Service on Ubuntu 26.

**Step 1: Implement**

```rust
pub struct IdentityCache;

impl IdentityCache {
    pub fn put(secrets_json: &str, _ttl: std::time::Duration) -> Result<()> {
        let e = keyring::Entry::new("crosstache-xv", "cache")?;
        e.set_password(secrets_json)?;
        // TTL is wall-clock; record expiry timestamp inside the JSON itself.
        Ok(())
    }
    pub fn try_get() -> Option<String> {
        let e = keyring::Entry::new("crosstache-xv", "cache").ok()?;
        let raw = e.get_password().ok()?;
        // Honor inline expiry.
        ...
    }
    pub fn forget() -> Result<()> { ... }
    pub fn doctor() -> CacheDoctorReport { ... }
}
```

**Step 2: Wire into `IdentityStore::load_with_cache(...)`** that consults cache before prompting.

**Step 3: Update `xv identity doctor`** to print:

```
identity file: ~/.config/xv/identity.json  [present]
peer trust db: ~/.config/xv/peers.json     [present, 3 peers]
keyring cache: [available | unreachable: <error> | not populated]
```

**Step 4: Verify**

Manual: `xv identity init` → `xv share ...` (prompts pw, populates cache) → `xv share ...` again (no prompt).
Automated: behind a `--ignored` test gate (same pattern as the existing non-hermetic vault tests).

**Step 5: Commit**

```bash
git add src/identity/cache.rs src/cli/identity_ops.rs Cargo.toml
git commit -m "feat(identity): opt-in OS keyring cache (003a follow-up)"
```

---

## Task 13: Local-dev relay stub + e2e smoke test

**Objective:** Tiny ~80-line Python relay (`scripts/dev-relay.py`) for local dev. The real relay is a separate plan.

**Files:**
- Create: `scripts/dev-relay.py`
- Create: `tests/e2e_share_claim.rs` (expanded if not yet)
- Modify: `docs/sharing.md`

**Step 1: Python dev relay**

Stdlib only — `http.server.BaseHTTPRequestHandler`, in-memory dict keyed by claim code, TTL via timestamp on insert. Echo logs to stderr.

**Step 2: e2e test**

`tests/e2e_share_claim.rs` spawns the relay as a subprocess on a random port, runs the full client roundtrip, asserts plaintext equality.

**Step 3: Verify**

Run: `cargo test --test e2e_share_claim -- --include-ignored`
Expected: passes.

**Step 4: Commit**

```bash
git add scripts/dev-relay.py tests/e2e_share_claim.rs docs/sharing.md
git commit -m "test: e2e share/claim against dev relay"
```

---

## Task 14: Docs + README

**Objective:** User-facing docs.

**Files:**
- Modify: `README.md` (add a short section linking to `docs/sharing.md`)
- Create: `docs/identity.md`
- Create: `docs/sharing.md`

**Step 1: Write `docs/identity.md`**

Cover:
- What an identity is (age + ed25519).
- Storage at `~/.config/xv/identity.json`, passphrase-wrapped.
- Optional keyring cache; how to enable; headless caveat.
- Trusting a peer (`xv identity import`).
- Fingerprint format and how to verify out-of-band.

**Step 2: Write `docs/sharing.md`**

Cover:
- The flow: share → claim code → claim.
- Multi-recipient.
- TTL.
- Relay URL config (`--relay`, `XV_RELAY_URL`, `identity.relay_url` in config).
- Security model: ciphertext-only at the relay; signer hidden from relay; TOFU prompt on first claim from unknown signer.
- What we don't (yet) do: forward secrecy, revocation (use `--rotate-after-share`), hardware keys.

**Step 3: Commit**

```bash
git add README.md docs/identity.md docs/sharing.md
git commit -m "docs: identity + sharing"
```

---

## Verification checklist (run before opening the meta-PR)

- [ ] `cargo build` — clean, no warnings outside `dead_code` on stubs
- [ ] `cargo fmt --check` — passes (project CI gate)
- [ ] `cargo clippy -- -D warnings` — passes (project CI gate)
- [ ] `cargo test` — all green
- [ ] `cargo test -- --include-ignored` — e2e against dev relay green
- [ ] `xv identity init` works from a clean `XDG_CONFIG_HOME`
- [ ] `xv identity show` prints pubkeys + fingerprint
- [ ] Two-machine smoke test or simulated via 2 XDG_CONFIG_HOMEs:
  - `xv identity init` on A
  - `xv identity init` on B
  - `xv identity show --export` on B → file
  - `xv identity import file --label bob` on A
  - `xv share <secret> --to bob` on A → prints code
  - `xv claim <code> --as-name imported` on B → secret roundtrips

## Out-of-scope follow-ups (parked as issues, not in this PR train)

- Relay server implementation (separate plan, separate crate `xv-relay`).
- Forward-secrecy via ephemeral request keys (`xv share-request`).
- YubiKey / age-plugin support.
- Machine-to-machine identities (non-interactive identity file).
- `--rotate-after-share` actually performing the rotation (currently just logs the intent).
- Concurrent-access file lock on `identity.json`.
- macOS / Windows keyring cache tests.

## Notes for the implementer

- Spike code at `~/crosstache/spikes/` is the reference. The crypto in spikes 001/002/003b is verbatim-usable in `src/share/crypto.rs` and `src/identity/store.rs`.
- Crockford encoding spec from spike 004 — copy `crockford_base32()` builder verbatim.
- Always pin `keyring`'s features explicitly (Task 12) — the default feature pick on Ubuntu 26 is wrong, and a passing test against the wrong backend is the worst failure mode.
- `age` 0.10 `Decryptor::new` returns an enum — always match the variant explicitly; never expect a specific arm or you'll silently mis-decrypt the wrong stanza type.
- Keep `SigningKey` and unwrapped `x25519::Identity` short-lived. Don't store them in long-lived structs without an explicit reason. `secrecy::SecretString` wraps where useful but the dalek types don't implement Zeroize uniformly — wrap them at the call site.
- For OMC autopilot per crosstache memory: open one PR per task. CI must be green (`cargo fmt --check` + `cargo clippy -D warnings`) before auto-merge. Bake watcher self-removal into each task's watcher prompt.
