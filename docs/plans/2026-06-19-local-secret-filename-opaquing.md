# Design: Opaque on-disk filenames for the local secret backend

**Status:** Proposed (design only — no implementation in this PR)
**ROADMAP item:** P3 — "Local secret NAMES disclosed via filenames"
**Author:** Hermes Agent
**Date:** 2026-06-19

## Problem

The local backend (`src/backend/local/secrets.rs`) stores each secret as two
files inside `<store>/vaults/<vault>/secrets/`:

```
<encoded_name>.age         — age-encrypted secret VALUE
<encoded_name>.meta.json   — metadata (content encrypted at rest since v0.13.0)
```

where `<encoded_name>` is the **URL-encoding of the secret name** (`encode_name`,
RFC 3986 percent-encoding). v0.13.0 added opt-in encryption of metadata
*content*, but the **filename still embeds the secret name verbatim** (modulo a
trivially reversible percent-encoding). Anyone who can list the directory learns:

- **Identity** — `decode_name("DB%2DPASSWORD.age") = "DB-PASSWORD"`. The name is
  recovered with a standard URL-decode; no key required.
- **Existence** — whether a specific named secret is present.
- **Count** — how many secrets exist in a vault (file count / 2).
- **Activity** — mtimes reveal when each named secret was last written.

This is a confidentiality leak that survives metadata-content encryption: the
directory listing is an unencrypted index. The vault directory may be synced
(Dropbox/iCloud), backed up, or sit on a shared filesystem, widening exposure.

Threat model: an attacker with **read access to the store directory but not the
age identity**. They cannot read values or (encrypted) metadata content, but the
filenames hand them the full catalog of secret names for free.

## Goal

Make on-disk filenames **opaque**: a directory listing must not reveal secret
names, existence-by-name, or count beyond an upper bound, to anyone lacking the
age identity. Preserve O(1) get/set/delete by name and the existing version
archive layout.

## Non-goals

- Hiding the *number* of secrets entirely (a count bound from file count is
  acceptable; padding/decoys are out of scope for v1).
- Changing the AWS or Azure backends (their object naming is a separate concern).
- Re-encrypting secret *values* — already age-encrypted.

## Design options

### Option A — Keyed hash of the name as the filename (recommended)

Derive the filename from a **keyed** hash of the secret name:

```
file_stem = base32_nopad( HMAC-SHA256(index_key, normalized_name) )[..26]   // 128 bits
<file_stem>.age
<file_stem>.meta.json
```

- `index_key` is derived from the age identity (e.g. HKDF-SHA256 over the
  identity's secret scalar with a fixed `info = "xv-local-filename-index/v1"`),
  so it is available exactly when the backend can already decrypt, and an
  attacker without the identity cannot compute or invert the mapping.
- Keyed (HMAC), not a bare SHA-256, so an attacker cannot confirm a guessed name
  by hashing it themselves (a bare digest of a low-entropy name like `aws-key`
  is brute-forceable from a dictionary). **This is the crux: a plain hash does
  NOT fix the leak for guessable names.**
- Base32 (no padding, lowercase) keeps stems case-insensitive-FS-safe and
  filename-legal without percent-encoding.
- Reverse lookup (needed by `list_secrets`, which must return real names) is
  served by an **encrypted index file** — see "The index" below.

**Pros:** O(1) name→file; filenames reveal nothing without the identity; no
plaintext name on disk anywhere. **Cons:** `list_secrets` now needs the index
(can't derive names from filenames); a migration is required.

### Option B — Encrypt the name into the filename (reversible without a side index)

`file_stem = base32( age_encrypt(index_key, name) )`. Self-describing (no
separate index needed for listing — decrypt each stem). **Cons:** age/AEAD
ciphertext + nonce inflates names; filename length limits (255 bytes) constrain
long secret names; nonce handling per-rename is fiddly. Rejected in favor of A's
fixed-width stems + one index.

### Option C — Single encrypted "pack" file per vault

Store all secrets in one encrypted blob/SQLite-in-an-age-envelope. Strongest
metadata hiding (even count). **Cons:** large blast radius, loses the simple
one-file-per-secret model, complicates concurrent access and the version
archive, hurts partial sync. Over-scoped for a P3; revisit only if count-hiding
becomes a hard requirement.

## Recommended: Option A + encrypted index

### The index

`<store>/vaults/<vault>/secrets/.index.age` — an age-encrypted JSON map:

```json
{ "<file_stem>": { "name": "<original_name>", "v": 1 }, ... }
```

- Encrypted to the same recipients as secrets; only the identity holder can read
  it. Written via the existing `write_private` (0600, O_NOFOLLOW) +
  `encrypt_bytes` path already used for `.meta.json`.
- `list_secrets` reads + decrypts the index instead of scanning filenames.
- `get/set/delete` compute `file_stem` directly from the name (no index needed
  on the hot path); `set`/`delete` additionally update the index entry.
- Index updates happen under the existing `fs2` file lock (already used in this
  module) to stay consistent with concurrent writers.

### Name normalization

HMAC the **NFC-normalized, original** name bytes (not the percent-encoded form)
so two encodings of the same name map to one stem. Document that names are
case-sensitive (consistent with current behavior).

## Migration

This changes the on-disk layout, so it must be explicit and reversible:

1. New store format version (bump the store's `format`/schema marker).
2. `xv local migrate` (or auto-migrate on first write when an old layout is
   detected): for each `<encoded_name>.{age,meta.json}`, compute the new
   `file_stem`, rename both files, append to `.index.age`, archive versions
   under the new stem. Idempotent; safe to re-run.
3. Keep a one-release back-compat read path that falls back to the old
   `encode_name` filename if the hashed stem is absent, so a half-migrated or
   un-migrated store still reads. Remove in the following release.
4. `--dry-run` prints the rename plan without touching disk.

## Security analysis

- **Without the identity:** filenames are 128-bit keyed-hash stems → no name
  recovery, no dictionary confirmation, no per-name existence oracle. Count is
  still bounded by file count (accepted non-goal). The index is age-encrypted →
  opaque.
- **With the identity:** full functionality; names live only inside the
  encrypted index and encrypted metadata, never as a plaintext filename.
- **Collision risk:** 128-bit HMAC stems → negligible collision probability for
  realistic secret counts; `set` should still detect a stem collision with a
  different name (via the index) and error rather than overwrite.

## Test plan

- `encode`/lookup round-trip: name → stem → index → name.
- Directory listing of a populated vault contains **no** substring of any secret
  name (property test over random names incl. unicode/percent-y chars).
- Dictionary-guess resistance: a known name's stem is not reproducible without
  `index_key` (compute with a wrong key → different stem).
- Migration: old-layout fixture → migrate → all secrets readable, index correct,
  re-running migrate is a no-op; back-compat read path serves an un-migrated
  secret.
- Concurrent `set` of two names with index updates stays consistent under the
  `fs2` lock.

## Rollout

1. Land this design (this PR).
2. Implement Option A behind the existing local-metadata-encryption opt-in (or a
   new `[local].opaque_filenames` flag) so existing stores are unaffected until
   opted in / migrated.
3. Ship `xv local migrate` + `--dry-run`.
4. After one release with the back-compat read path, make opaque filenames the
   default for new stores and drop the legacy fallback.

## Open questions

- Should `index_key` derive from the age identity (zero new key material, but
  rotating the identity rotates every stem → full re-migration) or be a separate
  stored-encrypted key (survives identity rotation, but one more secret to
  manage)? **Leaning identity-derived for v1** (simpler; rotation is rare and the
  migration tool already handles bulk rename), revisit if rotation UX demands it.
- Count-hiding via padding/decoy files: explicitly deferred; note it here so the
  non-goal is a conscious decision, not an oversight.
