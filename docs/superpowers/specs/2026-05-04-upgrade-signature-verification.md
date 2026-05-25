# `xv upgrade` Signature Verification Design

> **Status:** ✅ Implemented in **v0.11.0** (2026-05-24). Retained as design history. | **Date:** 2026-05-04 | **Author:** Hermes + Scott
> Originally tracked in `ROADMAP.md` → "Security hardening".

---

## Problem

`xv upgrade` currently downloads a binary and `.sha256` checksum from the same GitHub Releases endpoint. Both are fetched over TLS from the same host. This provides **integrity** (detect corruption in transit) but not **authenticity** (detect a compromised release). If an attacker gains write access to the GitHub repo or release assets, they can replace both the binary and the checksum.

The code review classified this as P2 — it's not exploitable without repo compromise, but for a secrets manager the trust chain should be stronger.

## Options Evaluated

### Option A: minisign (Recommended)

**What:** Embed a minisign public key in the `xv` binary. Sign each release archive with a minisign secret key during CI. `xv upgrade` verifies the detached signature before replacing the binary.

**Why minisign:**
- Purpose-built for signing release artifacts (created by the author of libsodium)
- Single static binary, trivial to add to CI (`cargo install minisign-verify` or the C tool)
- Tiny signatures (one line), tiny public key (one line)
- Ed25519-based, no certificate chains, no expiration to manage
- Rust crate `minisign-verify` (verify-only, ~50 LOC, no openssl dep) — exactly what we need in the client
- Used by: Zig, Wireguard, OpenBSD signify (same scheme), age

**Alternatives considered:**

| Tool | Pros | Cons |
|------|------|------|
| **cosign** (Sigstore) | Keyless, transparency log | Heavy dependency, requires OIDC flow in CI, overkill for a CLI tool |
| **age** | Already a dependency | Not designed for signing — encryption only, no detached signatures |
| **GPG** | Widely understood | Large dependency, key management nightmare, expired keys break verification |
| **minisign** | Tiny, purpose-built, Rust crate exists | Requires managing a secret key (but that's one secret, which is what KMS is for) |

## Proposed Design

### Key Management

1. **Generate a minisign keypair** (one-time, offline):
   ```
   minisign -G -p xv-release.pub -s xv-release.key -c "crosstache release signing key"
   ```
2. **Embed the public key** in `src/cli/upgrade_ops.rs` as a constant:
   ```rust
   const RELEASE_SIGNING_KEY: &str = "untrusted comment: crosstache release signing key\nRW...";
   ```
3. **Store the secret key** as a GitHub Actions secret (`MINISIGN_SECRET_KEY`).
4. **Optionally** also publish the public key in the README and at a well-known URL for out-of-band verification.

### CI Changes (release.yml)

After building each platform binary and computing SHA256:

```yaml
- name: Sign release archive
  run: |
    echo "${{ secrets.MINISIGN_SECRET_KEY }}" > /tmp/minisign.key
    minisign -S -s /tmp/minisign.key -m ${{ matrix.archive_name }}
    rm -f /tmp/minisign.key
  # Produces: {archive_name}.minisig
```

Upload `.minisig` alongside the archive and `.sha256`.

### Client Changes (upgrade_ops.rs)

```rust
use minisign_verify::{PublicKey, Signature};

fn verify_signature(archive_bytes: &[u8], signature_bytes: &[u8]) -> Result<()> {
    let pk = PublicKey::from_base64(RELEASE_SIGNING_KEY)
        .map_err(|e| CrosstacheError::upgrade(format!("Invalid signing key: {e}")))?;
    let sig = Signature::decode(&String::from_utf8_lossy(signature_bytes))
        .map_err(|e| CrosstacheError::upgrade(format!("Invalid signature: {e}")))?;
    pk.verify(archive_bytes, &sig, false)
        .map_err(|_| CrosstacheError::upgrade(
            "Signature verification FAILED. The release may have been tampered with. \
             Do NOT install this binary. Report this at https://github.com/bziobnic/crosstache/issues"
        ))?;
    Ok(())
}
```

### Upgrade Flow (updated)

1. Fetch latest release metadata from GitHub API
2. Download archive, `.sha256`, and `.minisig`
3. **Verify signature** (minisign public key embedded in binary) — fail hard if invalid
4. Verify SHA256 checksum — fail if mismatch
5. Extract and replace binary

### Backward Compatibility

- Old binaries (pre-signature) won't verify signatures — they don't know about them
- New binaries upgrading to a release without a `.minisig` file: **warn but allow** (with `--force`), since the transition period will have unsigned older releases
- After a few releases, make signature mandatory (remove the `--force` escape hatch)

### Dependency Addition

```toml
[dependencies]
minisign-verify = "0.2"  # verify-only, no signing capability, minimal deps
```

## Implementation Plan

1. **PR A:** Add `minisign-verify` dep, embed public key, implement `verify_signature()`, wire into upgrade flow with warn-on-missing
2. **PR B:** Update `release.yml` to sign archives with minisign
3. **PR C (later):** Make signature mandatory, remove warn-on-missing fallback

## Open Questions

1. Should we also sign the `.sha256` file, or is signing the archive sufficient? (Archive signature covers the same bytes the checksum covers — signing the checksum is redundant.)
2. Key rotation strategy? (Embed multiple public keys with a `key_id` field? Or just ship a new binary with the new key?)
3. Should we publish the public key to a separate domain (e.g., `keys.waffle.monster`) for true out-of-band verification?
