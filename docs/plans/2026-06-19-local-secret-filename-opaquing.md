# Design: Opaque on-disk filenames for the local secret backend

**Status:** Implemented — Option A (keyed-hash stems + age-encrypted index),
behind the opt-in `[local].opaque_filenames` flag, with `xv local migrate`
(+ `--dry-run`) and a one-release back-compat read path. See
`src/backend/local/opaque.rs` and `src/backend/local/secrets.rs`.
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

Soft-deleted secrets use the same reversible encoding under
`<store>/vaults/<vault>/.trash/`:

```
.trash/<encoded_name>@<deleted_at_millis>/
  <encoded_name>.age
  <encoded_name>.meta.json
  .deleted.json                 — includes plaintext "original_name"
```

Deleting a secret therefore **preserves the name leak in trash** until purge.
Archived versions under `secrets/.versions/<encoded_name>/` have the same
problem. Metadata-content encryption does not help: the directory listing is
still an unencrypted index. The vault directory may be synced (Dropbox/iCloud),
backed up, or sit on a shared filesystem, widening exposure.

Threat model: an attacker with **read access to the store directory but not the
age identity**. They cannot read values or (encrypted) metadata content, but the
filenames hand them the full catalog of secret names for free.

## Goal

Make on-disk filenames **opaque** across active secrets, version archives, and
trash: a directory listing must not reveal secret names, existence-by-name, or
count beyond an upper bound, to anyone lacking the age identity. Preserve O(1)
get/set/delete by name and the existing version/trash semantics.

## Non-goals

- Hiding the *number* of secrets entirely (a count bound from file count is
  acceptable; padding/decoys are out of scope for v1).
- Changing the AWS or Azure backends (their object naming is a separate concern).
- Re-encrypting secret *values* — already age-encrypted.

## Design options

### Option A — Keyed hash of the name as the filename (recommended)

Derive the filename from a **keyed** hash of the secret name:

```
file_stem = base32_nopad( HMAC-SHA256(index_key, name.as_bytes()) )[..26]   // 128 bits
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
  During the back-compat window (see Migration), it also reconciles on-disk
  pairs that are **missing from the index** so a half-migrated or crash-interrupted
  store never reports a secret via `get` that is missing from `list_secrets`:
  - **Legacy scan:** any `encode_name`-named `.age`/`.meta.json` pair not yet
    in the index (unmigrated secrets).
  - **Orphan opaque scan:** any opaque-stem `.age`/`.meta.json` pair whose stem
    is absent from the index — recover the name by decrypting `.meta.json`
    (same as today's metadata-driven listing). Classify stems as opaque when
    they match the fixed base32 pattern (26 chars `[a-z2-7]+`); legacy
    `encode_name` stems contain `%` or other URL-encoding characters and are
    handled by the legacy scan instead. This covers the failure mode where a
    rename completed but the index update did not: legacy files are gone so the
    legacy scan cannot help, yet `get` still finds the stem via HMAC(name).
    The orphan scan is **read-only for listing**; persisting missing index
    entries is handled by the migration recovery pass (below), by any write
    mutator on that name (see Legacy cleanup on write paths), or by re-running
    `xv local migrate` — all rebuild from decrypted
    metadata, not from reversible filenames.
  Skip pairs already represented in the index (by stem) to avoid double-counting
  when index-first migration left both legacy and opaque files briefly present.
  Drop both scans once the back-compat read path is removed.
- All mutators compute `file_stem` directly from the name (no index needed on the
  hot path). **Active index lifecycle** (must stay consistent with `get` /
  `list_secrets`):
  - **Add or refresh entry** (`{ file_stem → name }`): `set`, `update_secret`,
    `rollback`, `restore_secret`, and the shared `ensure_opaque_layout` helper
    when it re-activates an active pair.
  - **Remove entry:** soft `delete` (secret moves to trash; not listed as active).
  - **No index change:** `get`, `list_secrets`, `list_versions`,
    `list_deleted_secrets`, hard `purge_secret` (trash-only; entry already absent).
  Soft `delete` + `restore_secret` are a matched pair: delete drops the active
  entry; restore **must** re-add it before returning, or the secret is readable
  via `get` but missing from `list_secrets`.
- Index updates happen under the existing `fs2` file lock (already used in this
  module) to stay consistent with concurrent writers.

### Trash and version archives

Opaque stems apply to **every** secret-related path, not only active
`secrets/` pairs:

- **Version archive:** `secrets/.versions/<file_stem>/` (replacing
  `.versions/<encode_name>/`). Same stem as the active pair.
- **Trash entry:** `.trash/<file_stem>@<deleted_at_millis>/` with inner files
  `<file_stem>.age` and `<file_stem>.meta.json`. The `@<millis>` suffix stays
  (it cannot appear in a base32 stem) so repeated delete/recreate/delete cycles
  remain collision-free.
- **`.deleted.json`:** store only `{ "deleted_at": "<RFC3339>" }` (or encrypt
  the whole file). Do **not** write plaintext `original_name`; restore and
  `list_deleted_secrets` recover the name from the (encrypted) `.meta.json`
  content, same as today but without dirname assistance.

`trash_entries_for` (used by `restore_secret` and `purge_secret`) derives
`<file_stem>` from the secret name and scans `.trash/` for `{file_stem}@*` dirs
(plus a back-compat window for legacy `{encode_name}@*` / unsuffixed
`{encode_name}` dirs during migration). `list_deleted_secrets` already reads
metadata inside each trash entry rather than parsing directory names; opaque
trash dirs do not change that flow beyond requiring decrypted metadata.
`restore_secret` is a write mutator: it moves the chosen trash pair back to
`<file_stem>.{age,meta.json}` and **re-adds the active index entry** (see Active
index lifecycle). `purge_secret` only removes trash/version dirs; the active
index entry is already absent after soft delete.

### Legacy cleanup on write paths

The back-compat **read** fallback (`get` tries the hashed stem first, then the
legacy `encode_name` path) must not leave reversible filenames on disk after a
write. If only some mutators upgraded layout while others still wrote legacy
paths, a metadata-only `update_secret` on an unmigrated secret would refresh
`<encode_name>.meta.json` in place and **still disclose the name** in directory
listings — defeating the goal even though reads and the index looked correct.

Therefore, whenever opaque filenames are active, **every write mutator upgrades
the on-disk layout for that secret** (under the same `fs2` lock as index
updates). Implement this once as a shared helper (e.g. `ensure_opaque_layout`)
invoked after the mutator's primary work; do not duplicate cleanup logic across
call sites.

**Shared upgrade steps** (idempotent — no-op when legacy paths are already
absent):

1. Ensure active files live at `<file_stem>.{age,meta.json}` (rename or rewrite
   from legacy paths when the read fallback was used).
2. Ensure the index contains `{ file_stem → name }` for active secrets.
3. **Remove** legacy `<encode_name>.age`, `<encode_name>.meta.json`, and rename
   or merge `.versions/<encode_name>/` into `.versions/<file_stem>/` when
   present.
4. **Remove or rename** legacy trash dirs (`.trash/<encode_name>@*`, unsuffixed
   `.trash/<encode_name>/`) for that secret when applicable.

Mutator-specific behavior on top of the shared helper:

- **`set`:** write/update `<file_stem>.{age,meta.json}` and the index entry;
  then run the shared upgrade (covers recreate-over-legacy and version merge).
- **`update_secret`:** resolve the active pair via the read fallback if needed,
  apply the metadata/value mutation to `<file_stem>.*` (archiving prior
  snapshots under `.versions/<file_stem>/`), update the index entry, then run the
  shared upgrade. **Metadata-only updates must upgrade too** — they are a
  common path for long-lived legacy files to stay on disk if cleanup is limited
  to `set`/`delete`.
- **`delete` (soft):** move the hashed-stem pair into
  `.trash/<file_stem>@<deleted_at_millis>/` (not `{encode_name}@…`), drop the
  active index entry, then run the shared upgrade.
- **`delete` (hard / purge):** same stem-based trash and version paths; purge
  all `{file_stem}@*` trash entries and legacy `{encode_name}@*` leftovers.
- **`rollback`:** read the target version from `.versions/<file_stem>/` (read
  fallback: `.versions/<encode_name>/`), archive the current active pair under
  the opaque version path, write the rolled-back active pair at
  `<file_stem>.*`, refresh the index entry, then run the shared upgrade.
- **`restore_secret`:** restore from the newest matching trash entry (opaque or
  legacy dirname) into `<file_stem>.{age,meta.json}`, re-add the index entry,
  then run the shared upgrade so no legacy active or trash paths remain.
- **`get`:** read-only fallback to legacy paths is allowed during the back-compat
  window; it must **not** create or retain legacy files.

`list_secrets` reconciles on-disk pairs missing from the index via the legacy
and orphan-opaque scans (see The index). After any write mutator upgrades a
secret, its legacy pair is gone and the index entry is authoritative — scans
must not double-count stems already in the index.

### Name byte identity

HMAC the **raw UTF-8 name bytes** — the same bytes `encode_name` uses via
`name.as_bytes()` — not the percent-encoded form and **not** Unicode-normalized.
The current backend treats names as byte-exact: two canonically equivalent forms
(e.g. NFC vs NFD `"é"`) are distinct secrets with separate on-disk pairs. Opaque
stems must preserve that 1:1 mapping; normalizing to NFC would collapse them to
one stem during migration and cause overwrites or collision errors.

Names remain case-sensitive (consistent with current behavior). During migration,
derive the name from decrypted metadata (`original_name`) or by decoding the
legacy filename; compute the stem from those exact bytes.

## Migration

**Ordering invariant:** migration must never leave opaque stems on disk without a
matching index entry when legacy filenames are already gone. A rename-then-append
sequence loses the only recoverable name source (legacy dirname) while `get`
still resolves the secret by HMAC(name); listing and idempotent re-migration
then fail unless metadata-driven orphan recovery exists. The steps below enforce
index-before-rename and a metadata-based recovery pass.

This changes the on-disk layout, so it must be explicit and reversible:

1. New store format version (bump the store's `format`/schema marker).
2. `xv local migrate` (or auto-migrate on first write when an old layout is
   detected), **per secret under the vault `fs2` lock** (same lock as runtime
   index updates):
   - Read the exact name from metadata (or decode the legacy filename) and
     compute `file_stem` from its raw UTF-8 bytes.
   - **Append/update the index entry first**, then rename active
     `<encoded_name>.{age,meta.json}` → `<file_stem>.*`, then rename
     `.versions/<encode_name>/` → `.versions/<file_stem>/`. Index-before-rename
     ensures a crash after the index write still lists the secret (from the
     index); a crash before rename leaves legacy files in place so the legacy
     scan still lists it. **Never rename away legacy filenames before the index
     records the stem** — otherwise `list_secrets` loses the only recoverable
     name source and idempotent re-migration cannot rebuild the index.
   - **Also** walk `.trash/`: for each legacy `<encode_name>@<millis>/` (and
     unsuffixed `<encode_name>/`), read the name from inner metadata (or decode
     the dirname), compute `file_stem`, rename the directory to
     `<file_stem>@<millis>/`, rename inner `{encode_name}.{age,meta.json}` to
     `{file_stem}.*`, and strip plaintext `original_name` from `.deleted.json`
     when present. Trash is not in the active index (`delete` already dropped the
     entry); `list_deleted_secrets` reads metadata inside each trash dir.
   - **Recovery pass (idempotent):** scan `secrets/` for opaque-stem
     `.age`/`.meta.json` pairs whose stem is not in the index; decrypt
     `.meta.json` and **persist** the missing index entries (handles stores
     already stuck in rename-before-index state from an interrupted run or from
     a buggy rename-then-append ordering). Without this pass (or the orphan scan
     + a subsequent write/migrate), legacy names are unrecoverable once renamed
     away, yet `get` would still resolve the stem — listing and re-migration
     would omit the secret. Then drop any duplicate legacy pair for the same
     name if both layouts coexist.
   Safe to re-run.
3. Keep a one-release back-compat **read** path: `get` falls back to the old
   `encode_name` filename if the hashed stem is absent, so a half-migrated or
   un-migrated store still reads. **Write mutators do not use this fallback for
   their output paths** — they always target hashed stems and remove matching
   legacy pairs (see Legacy cleanup on write paths), so any mutation upgrades
   that secret and clears the name leak. While the read fallback is active,
   `list_secrets`
   also reconciles legacy-named and orphan opaque pairs not yet in the index
   (see The index) so listing stays consistent with `get`. Remove the read
   fallback and both reconciliation scans in the following release.
4. `--dry-run` prints the rename plan without touching disk.

## Security analysis

- **Without the identity:** filenames are 128-bit keyed-hash stems → no name
  recovery, no dictionary confirmation, no per-name existence oracle. Count is
  still bounded by file count (accepted non-goal). The index is age-encrypted →
  opaque.
- **With the identity:** full functionality; names live only inside the
  encrypted index and encrypted metadata — not in any filename under `secrets/`,
  `.versions/`, or `.trash/`, and not in plaintext `.deleted.json`. All write
  mutators (`set`, `update_secret`, `delete`, `rollback`, `restore_secret`,
  `purge_secret`) must purge legacy `encode_name` paths (active, version
  archive, and trash) so any mutation cannot leave reversible directory names
  alongside opaque stems.
- **Collision risk:** 128-bit HMAC stems → negligible collision probability for
  realistic secret counts; `set` should still detect a stem collision with a
  different name (via the index) and error rather than overwrite.

## Test plan

- `encode`/lookup round-trip: name → stem → index → name.
- Directory listing of a populated vault (including `.trash/` after deletes)
  contains **no** substring of any secret name (property test over random names
  incl. unicode/percent-y chars).
- Dictionary-guess resistance: a known name's stem is not reproducible without
  `index_key` (compute with a wrong key → different stem).
- Migration: old-layout fixture → migrate → all secrets readable, index correct,
  re-running migrate is a no-op; back-compat read path serves an un-migrated
  secret.
- Migration ordering: interrupt after index write but before rename →
  `list_secrets` includes the secret via the index; `get` still reads (legacy
  fallback or stem once renamed).
- Migration crash recovery: fixture with opaque stems on disk but empty/partial
  index (simulating rename-before-index) → re-run migrate or `list_secrets`
  orphan scan → index rebuilt from metadata, listing matches `get`.
- Unicode byte identity: a vault with both NFC and NFD forms of the same
  grapheme as separate legacy secrets migrates to two distinct stems; neither
  overwrites the other and both remain readable by their original names.
- Upgrade-on-write (`set`): fixture with legacy-named files only → `set` same
  name → hashed-stem files exist, legacy pair and `.versions/<encode_name>/`
  removed (or merged), index updated; directory listing contains no URL-encoded
  secret name substring.
- Upgrade-on-write (`update_secret`, metadata-only): legacy active pair only →
  `update_secret` changes tags/note/enabled with no value change → hashed-stem
  files exist, legacy pair removed, index updated; directory listing still
  contains no URL-encoded name substring.
- Upgrade-on-write (`update_secret`, value): legacy pair with
  `.versions/<encode_name>/` → value update archives under
  `.versions/<file_stem>/`, legacy paths removed.
- Upgrade-on-write (`rollback` / `restore_secret`): legacy layout (active and/or
  trash) → mutator succeeds → opaque stems only; index consistent with `get`.
- Index lifecycle (`restore_secret`): soft-delete removes active index entry →
  `restore_secret` → secret readable via `get` **and** present in `list_secrets`
  (entry re-added).
- Delete of unmigrated secret: legacy active pair removed; trash entry uses
  opaque stem; no orphaned `encode_name` files under `secrets/` or `.trash/`.
- Soft-delete then list `.trash/`: directory names contain no URL-encoded secret
  name substring; `list_deleted_secrets` still returns correct names via metadata.
- Migration with pre-existing trash: legacy `.trash/<encode_name>@*/` entries
  renamed to opaque stems; `.deleted.json` no longer carries plaintext
  `original_name`.
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
