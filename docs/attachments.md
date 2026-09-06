# Secret file attachments

Client-side age encryption for files associated with a secret — and for
standalone confidential uploads — on every backend that has file storage
(Azure Blob, AWS S3, local).

Storage-layer access alone is not enough to read plaintext: the age identity
lives in reserved key records in the vault's secret store, so attachment access
is gated by vault permissions and, when enabled, agent policy.

Requires the `file-ops` feature (on by default in release builds).

## Quick reference

```bash
# Attach a local file to an existing secret (encrypt + upload)
xv attach db-cert ./cert.pem
xv attach db-cert ./cert.pem --name leaf.pem   # override the stored name

# List / download (decrypted)
xv attachments db-cert
xv attachments db-cert --get leaf.pem
xv attachments db-cert --get leaf.pem -o ./out.pem

# Remove one attachment
xv detach db-cert leaf.pem
xv detach db-cert leaf.pem --force

# Standalone confidential file (no secret association)
xv file upload ./license.key --encrypt
xv file download license.key                  # decrypts transparently
```

The secret must already exist (`xv set` / `xv gen --save`). Attaching to a
missing name fails early rather than creating an orphan blob.

## How it works

| Piece | Behavior |
|-------|----------|
| Key | An age x25519 identity per vault. A new vault initializes a **V2 key ring** on first attach / `--encrypt`: the private identity lives in an immutable, marked record `xv-attachment-key-ak1-<hash>`, and the reserved `xv-attachment-key` secret holds a non-secret pointer to the active key. Existing vaults created before V2 keep a raw identity directly in `xv-attachment-key` (V1) and continue to work. |
| Ciphertext location | Ordinary file storage under `attachments/<secret-name>/<filename>`. Association is the naming convention — no secret tags are consumed. |
| Metadata | Uploaded blobs carry the reserved crypto metadata `xv_encrypted=age`, `xv_crypto_schema=1`, `xv_key_id=ak1-<hash>`, `xv_key_version=<exact provider version>`, and `xv_key_slot=legacy` (V1) or `retained` (V2). Underscore keys are required: Azure Blob rejects hyphenated metadata keys. These five keys are owned by the encryption path — any caller-supplied value for them is overwritten. |
| Key ID | `ak1-` + SHA-256 of a domain tag and the public age recipient. Portable and backend-independent; identifies the exact key that encrypted a blob. |
| Version pinning | Each blob records the **exact provider version** of the key record it was encrypted with. Downloads read that exact version and re-derive its key ID to verify it before decrypting, so replacing or rotating the current attachment key does **not** make existing attachments unreadable. |
| Listings | Generic listings hide `xv-attachment-key` and marked retained key records. Unmarked strict-name collisions remain visible as user secrets. |
| Download | `xv attachments --get` and `xv file download` classify each object before returning bytes: a managed attachment (under `attachments/` or flagged `xv_encrypted=age`) whose bytes are **not** age ciphertext fails closed rather than returning plaintext; a schema-1 blob is decrypted only through its pinned key version after ID verification, and a missing/malformed key reference is an error — never a silent fall back to another key. Unflagged / foreign `.age` files pass through untouched. |

Attachment classification reads file bytes and crypto metadata from the same
committed generation. Local storage holds its file lock across both reads;
S3 supplies both in one GetObject response; Azure conditions every download
chunk on the original ETag. If an Azure blob changes during download, the
operation fails instead of mixing generations. Cloud snapshots enforce the
5 GiB transfer cap and reject incomplete responses. Third-party backends must
implement consistent snapshots; there is no fallback to separate reads.

New retained keys are committed before the active pointer is published. Local
and AWS create the retained record only if its name is absent; a concurrent
collision is retried without overwriting that record. Azure Set returns a new
provider version. Initialization reads back the exact committed version and
verifies its key ID before using it for encryption.

Local key records and pointer updates journal their encrypted value/metadata
pair. An interrupted write is recovered under the vault lock before a read or
mutation proceeds. Existing unexplained half-pairs are refused; recovery does
not guess which half is authoritative. Retained versions remain available for
attachments that already reference them.

On the local backend, files are already age-encrypted at rest; attachments
still use the vault key so the same CLI and web paths work on every backend.

## Commands

### `xv attach <secret> <file> [--name <name>]`

Encrypts the file and uploads it as `attachments/<secret>/<name>`. Default
`<name>` is the local basename. Names must be a single path component (no
`/` or `\`).

Workspace writes follow the usual rule: an unqualified secret targets the
workspace **default** vault; use `alias:secret` to attach elsewhere.

### `xv attachments <secret> [--get <name>] [-o/--output <path>]`

Without `--get`, lists attachment names with ciphertext size and last
modified time. With `--get`, downloads and decrypts to `--output` (default:
the attachment name in the current directory). Refuses to overwrite an
existing path.

### `xv detach <secret> <name> [--force]`

Deletes one attachment blob. Confirms unless `--force`.

### `xv file upload --encrypt`

Same encryption and metadata as attachments, without the
`attachments/<secret>/` prefix. **Single-file only** — combining `--encrypt`
with `--recursive` or multiple files is rejected.

Quick aliases `xv upload` / `xv download` do not expose `--encrypt`; use
`xv file upload --encrypt`.

## Lifecycle interactions

### Delete cascade

`xv delete <secret>` lists the secret's attachment prefix first. The
confirmation prompt includes the count (`Delete secret 'X' and its N
attachment(s)?`), then removes those blobs after the secret delete commits.

Generic secret commands refuse mutations of `xv-attachment-key` and every
strict-format retained key-record name, including provider-equivalent alias
spellings. `--force` cannot override this protection. The same boundary applies
to Web edits and folder moves, imports, and migration targets. Generic opaque
backup restore is disabled because its destination cannot be checked before
provider mutation. Dedicated pointer recovery and encrypted key backup/restore
are described below, along with offline rotation, rewrap, and logical retirement.

### Sync skips ciphertext

`xv file sync` never transfers encrypted attachment blobs (reserved
`attachments/` prefix or `xv_encrypted=age`). Syncing them as plaintext would
decrypt on download or clobber ciphertext on upload. Expect a skip summary;
use `xv attach` / `xv attachments --get` / `xv file upload --encrypt` instead.

### Rename and move

Attachment association is the blob path `attachments/<old-name>/…`.
The web UI and generic CLI rename, copy and move commands refuse attached sources
or destination prefixes before changing secrets. `xv update --rename` and `xv mv`
also check before applying accompanying metadata or folder changes. Folder-only
moves keep the same secret name and attachment prefix.

Inspect a proposed transfer without changing data:

```bash
xv transfer cert --from work --to work --new-name certificate --move
xv transfer cert --from work --to stage --to-key-id DESTINATION_ACTIVE_ID
```

The JSON preview lists endpoints, attachment counts/bytes and verified key bindings,
without secret values or file contents. Cross-vault attachments require a healthy
destination V2 key ring and its explicit active key ID. This release provides the
preview and encrypted recovery-manifest foundation; applying attached transfers is
not yet enabled.

### Migration

`xv migrate` copies secrets, not file blobs. It preflights attachment prefixes for
the selected batch and refuses attached transfers before writing secrets, including
with `--force-replace`. An unavailable inventory is an error, not evidence of an
empty prefix. Key custody records remain excluded from generic migration.

## Agent policy

With agent enforcement enabled, attachment key reads and writes use the same
policy and decision logging as secret operations. Policies must authorize
`get` on the pointer and referenced retained records; first use also needs
`set`. Reads that return key material require `raw_disclosure = true`, including
internal reads for encryption/decryption. No raw provider handle bypasses those
checks. Decision records contain resource names and outcomes, never private
keys or file contents. File operations themselves are not yet covered by the
secret policy; see [agent identity and policy](agent-identity.md).

## Web UI

With `--features ui`, the secret detail drawer lists attachments as download
links (`GET /api/secrets/{name}/attachments`). File downloads go through the
same decrypt path as `xv file download`. See [`web-ui.md`](web-ui.md).

## Common pitfalls

| Symptom | Cause / fix |
|---------|-------------|
| `attachment key not found in vault '…'` | No attachments were ever created, or `xv-attachment-key` was deleted. Re-attach / re-upload with `--encrypt` to mint a new key — old ciphertext stays unreadable. |
| `…key generation for 'ak1-…' is missing …` | The exact key version a schema-1 blob pins no longer exists (the key record/version was deleted). Restoring that key record/version is the only recovery — the pinned reference is never silently replaced with the current key. |
| `…is a managed attachment but its bytes are not age ciphertext` | A file under `attachments/` (or flagged `xv_encrypted=age`) is not valid ciphertext. Download fails closed rather than leak it as plaintext; investigate how an unencrypted object landed in the managed namespace. |
| `…key reference is missing or malformed …` | A schema-1 blob's `xv_key_*` metadata is incomplete or invalid. Fix the metadata to the exact committed reference; the download will not fall back to another key. |
| `wrong or rotated attachment key` | A pre-schema (legacy) attachment cannot be decrypted by the current key. New uploads pin their exact key version and are immune; legacy blobs predate that binding. |
| Azure upload 400 / InvalidMetadata | Must use metadata key `xv_encrypted` (underscore). Fixed in v0.27.1; older docs or scripts saying `xv-encrypted` are wrong. |
| `--encrypt currently supports single-file uploads only` | Drop `--recursive` / extra paths; encrypt one file at a time. |
| File storage unsupported | Backend/config has no file store (e.g. AWS without `[aws].s3_bucket`). Configure storage, or use a backend that has it. |
| Sync “skipped N encrypted attachment blob(s)” | Expected. Use attach/download commands for those objects. |
| Attachments missing after rename | Path still under the old secret name. Re-attach or use the web UI rename guard. |
| Secret name with `/` rejected for attachments | Path separators would break prefix isolation; rename the secret first. |

## Related

- Design: [`superpowers/specs/2026-07-21-secret-file-attachments-design.md`](superpowers/specs/2026-07-21-secret-file-attachments-design.md)
- File storage overview: [`FEATURES.md`](FEATURES.md#file-storage)
- Cross-cloud secret migration (secrets only): [`migration.md`](migration.md)

## Structured attachment errors

Attachment integrity failures have stable codes in CLI JSON/YAML errors and
Web error responses. They retain CLI exit status `2` and Web HTTP `400` for
compatibility. Messages and recovery hints contain no private keys, ciphertext,
or raw provider responses. Authentication, permission, and network failures keep
their existing error codes.

| Code | Meaning |
|------|---------|
| `xv-attachment-key-missing` | The required key record or exact provider version is absent. |
| `xv-attachment-key-invalid` | The stored key has no value or is not an age identity. |
| `xv-attachment-pointer-invalid` | The active pointer is empty or malformed. |
| `xv-attachment-key-mismatch` | The stored identity does not derive the expected key ID. |
| `xv-attachment-key-version-invalid` | A key commit returned no version or exact-version verification returned a different version. |
| `xv-attachment-commit-unconfirmed` | The active pointer could not be confirmed after publication. |
| `xv-attachment-initialization-conflict` | Bounded initialization attempts exhausted conflicting retained names. |
| `xv-attachment-reference-invalid` | The blob reference is incomplete, malformed, or uses an unsupported schema. |
| `xv-attachment-not-ciphertext` | A managed attachment contains non-age bytes; plaintext output was refused. |
| `xv-attachment-decryption-failed` | The required key could not decrypt the attachment. |
| `xv-attachment-snapshot-unsupported` | The backend cannot read consistent file bytes and metadata. |

Restore original attachment/key data from a trusted backup when custody or
integrity is broken. Generating a replacement key cannot decrypt old attachments.
CLI TTY output and Web responses provide guidance specific to the error code.
See [exit codes](exit-codes.md) for the CLI error envelope.

## Inspecting key status and file references

```sh
xv attachment-key status
xv attachment-key inventory --format json
xv attachment-key status --vault production --format yaml
```

Both commands use the same current backend/vault resolution as file commands,
including the workspace default entry. An explicit `--vault` selects an attached
workspace alias when it matches one, otherwise a literal vault on the effective
backend. Reports include the backend registry
name and vault so named backends remain distinguishable. JSON and YAML reports
contain a `report` object with `schema_version: 1`; human output uses the same
nested structure. CSV and templates are not supported.

`status` reads the active pointer and validates the active age identity and
derived key ID. It reports `absent`, `v1`, `v2`, or `invalid`, together with
public key IDs, the active provider version, and a safe `problem_code` when
integrity is broken. It never initializes or rotates an attachment key and
does not require file storage. A completed diagnosis exits successfully even
when it reports an absent or broken key; scripts should inspect
`report.mode` and `report.problem_code`. Provider access failures still fail
the command with their usual error codes. Inspecting status requires
permission to read the pointer and active key material, although neither is
printed.

`inventory` lists files and reads each file's metadata, including encrypted
files outside `attachments/`. Entries are sorted by name and classified as
`schema1`, `legacy_unversioned`, `invalid_reference`, or `unmanaged`.
Only valid schema-1 references include a key ID, slot, and provider version.
It requests no private key material or blob downloads and emits no report if a
listing or per-file metadata request fails.

These commands request no custody mutations. The local backend's existing
read-time crash recovery still applies: it may restore ciphertext files or
finish pending journal operations before returning a read. They are not a
forensic mode that guarantees zero filesystem writes.

The inventory is explicitly a `metadata_only` observation. It does not verify
ciphertext, test decryption, enumerate unreferenced retained keys, or establish
that any key can be retired. It observes the files returned by the provider;
listing and metadata reads are not one atomic snapshot and may race concurrent
uploads or deletes. Cloud inventory uses the existing file-info API for each listed file: on
AWS/Azure that means a properties/metadata request and a separate best-effort
tag request, in addition to listing requests. Tags are not used for reference
classification, so an optional tag-read failure does not invalidate it. S3
listings alone omit user metadata.
Stop concurrent writers and use the verified lifecycle commands below before
making custody changes; inventory alone is not a retirement check.

## Retained keys and offline lifecycle operations

```sh
xv attachment-key keys --format json
xv attachment-key upgrade --vault production --format json
# After stopping writers and upgrading all clients:
xv attachment-key upgrade --vault production --apply --offline
```

`keys` lists the current marked retained records visible to the caller, sorted
by canonical name. Its report contains `schema_version: 1`,
`observation: visible_retained_records`, and a `keys` array with `name`,
`key_id`, and `enabled`. It reads no private values and excludes unmarked
user-secret collisions, the active pointer, and ordinary secrets. It does not
enumerate all historical provider versions or verify the listed identities.
Agent enforcement checks list scope before provider access and filters each
record by policy; an empty list is not proof that the vault has no retained keys.

`upgrade` previews a V1-to-V2 conversion by default. It retains the **same**
identity under its deterministic marked key name, verifies the exact retained
version, then publishes a V2 pointer with that key as both active and permanent
legacy fallback. It verifies that the original V1 provider version remains
readable. Pre-schema attachments use the explicit legacy fallback; schema-1
legacy attachments continue to use their original pinned versions; new uploads
use the retained record. No blobs are rewritten and no new identity is generated.
A valid V2 ring returns `outcome: unchanged`.

Both upgrade and recovery require `--apply --offline` to write. `--offline`
is an acknowledgment that all writers and old clients are stopped; it does not
acquire a distributed lock. These operations recheck the pointer before writing
and confirm it afterward, but the providers do not offer one portable atomic
compare-and-swap. Keep writers stopped throughout the operation, and upgrade
all clients before resuming. Preview and apply independently validate current
state; a preview is not a saved transaction.

If upgrade stops after retaining the key but before publishing the pointer,
retry it while writers remain stopped. It reuses and verifies the marked
retained record. Committed keys are kept on every failure. An unmarked record
at the required name is refused and never overwritten.

### Repair a missing or broken active pointer

First inspect `status`, `keys`, and the file-reference `inventory`. Select
existing retained key IDs based on trusted records of the original ring.
Recovery requires an explicit legacy choice:

```sh
# ACTIVE_KEY_ID and LEGACY_KEY_ID are IDs chosen from your trusted ring records.
xv attachment-key recover --key-id "$ACTIVE_KEY_ID" --legacy-key-id "$LEGACY_KEY_ID"
# Apply only with writers stopped:
xv attachment-key recover --key-id "$ACTIVE_KEY_ID" --legacy-key-id "$LEGACY_KEY_ID" --apply --offline
# For a ring originally created directly in V2 with no legacy attachments:
xv attachment-key recover --key-id "$ACTIVE_KEY_ID" --no-legacy
```

Recovery verifies the selected marked identities and their exact versions
before publishing a pointer. It refuses to replace a V1 identity (use upgrade),
rotate a healthy V2 pointer to another active key, or change a known V2 legacy
binding. Recovering an already matching valid pointer is a no-op. When the
pointer is missing or malformed, the original legacy binding cannot be inferred:
`--no-legacy` deliberately disables pre-schema decryption, even if the selected
active key could decrypt those files.

Lifecycle reports use `operation` (`upgrade` or `recover`), `outcome`
(`ready`, `applied`, or `unchanged`), public active/legacy IDs, and the
verified `retained_version` (null in an upgrade preview when the record is not
yet created). JSON/YAML and `--vault` alias/literal selection follow the
observation commands. No private key is printed.

This recovery repairs the pointer using keys already present. It cannot restore
deleted key material, recreate a missing provider version, recover attachments
already unreadable before upgrade, or restore a ring into another vault.
Encrypted key backup/restore, rotation, rewrap, and logical retirement are available
as described below.
Provider soft-deleted records may need restoration through the provider's
recovery tooling before these commands can access or write them. Existing
Local read-time journal recovery applies to these commands as well.

### Encrypted key backups and offline restore

`attachment-key export` creates an age-encrypted bundle containing verified
attachment identities, active/legacy bindings, and a manifest of visible current
encrypted files. Supply an independent X25519 age recovery recipient and keep
its private identity separately from the vault. The bundle contains **no file
payloads or ordinary secrets**. Back up ciphertext and its metadata separately.

Stop all writers and older clients for the source vault, then export:

```sh
xv attachment-key export --vault SOURCE --recipient age1... --output keys.age --offline
```

Export authenticates managed ciphertext and checks exact key versions. Missing
keys, invalid references, or observed source changes fail the operation. It
includes unreferenced visible retained keys, but cannot export records hidden by
agent policy. The output path must not already exist; no plaintext key file is
created. A healthy source pointer is required.

To recover into the same or another vault, first restore the separately backed-up
ciphertext **and metadata** at their original names. Every manifest file must be
present, unchanged; additional managed destination files cause a conflict.
An age identity file may contain comments and exactly one X25519 private key.

Restore updates files in the destination's configured storage in place. Azure
file storage is scoped to a blob container, not to the vault name: selecting
another vault on the same Azure backend still selects the same files. For a
separate cross-vault recovery copy, configure a destination backend with a
different container or storage account before restoring the file backup there.

```sh
xv attachment-key restore --vault DEST --input keys.age --identity-file recovery.agekey --format json
# After reviewing the preview, keep all destination writers stopped:
xv attachment-key restore --vault DEST --input keys.age --identity-file recovery.agekey --apply --offline
```

Preview reads and authenticates files but writes nothing. Apply imports missing
retained keys, verifies their actual destination versions, and updates file key
references while preserving ciphertext bytes and user metadata. It verifies
normal decryption before publishing the active/legacy pointer last. Source
provider version IDs are never fabricated at the destination. Repeating the
command resumes interrupted work and reuses verified keys and rebound files.

If the destination pointer is malformed, add `--repair-pointer` to both preview
and apply. This explicitly permits replacing that malformed value after the
bundle's keys and files verify; it cannot replace a valid V1 pointer or conflicting
V2 bindings. A V1 destination must first use `attachment-key upgrade`. Unmarked
or invalid retained-record collisions are never overwritten. Soft-deleted cloud
records may still require provider recovery before import can proceed.

Cloud restore requires permission to read object tags and to write the preserved
tags. A tag-read failure stops preflight; it is never treated as an empty tag set.
Configured agent policy is checked for planned key writes and their exact-value
readback before mutation. Provider permissions are also checked at each actual
operation, so a later provider failure can leave completed steps to retry.

These commands cover the bundle's visible **current files**, not historical blob
versions. Keep writers stopped throughout restore: drift checks detect observed
changes but there is no portable atomic transaction across providers. On failure,
completed writes remain available for retry; no automatic rollback or key deletion
occurs. Re-encryption under another identity and retirement are separate operations.

### Offline key rotation

`attachment-key rotate` changes the key used for new encrypted uploads while
retaining every existing identity and provider version. Existing ciphertext is
unchanged and remains readable. Start with a healthy V2 ring (use `upgrade` for a
V1 vault), stop all writers, and take an encrypted key backup first.

```sh
xv attachment-key status --format json
xv attachment-key rotate --from-key-id OLD_ACTIVE_ID --format json
xv attachment-key rotate --from-key-id OLD_ACTIVE_ID --apply --offline
```

Copy the active key ID from status into `OLD_ACTIVE_ID`. Preview validates the
ring without generating or writing a key. Apply creates and exact-verifies a
fresh independent identity before publishing the pointer; the explicit legacy
binding stays unchanged. If the active ID no longer matches, the command refuses
before creating another key. Thus repeating a successfully completed invocation
cannot silently rotate again.

Failure before pointer publication can leave an unreferenced retained candidate;
retrying may create a fresh candidate. Do not delete these records. After an
unconfirmed publication, inspect `status` and `keys`: retrying with the old expected
ID refuses if publication succeeded. Drift checks detect observed changes but do
not make this a provider-portable atomic transaction; writers must stay stopped.

### Rewrap current attachments to the active key

After rotation, `attachment-key rewrap` re-encrypts current managed files under
the active V2 identity. Stop all writers, keep an encrypted key backup and a
separate backup of ciphertext and metadata, then use the active ID from `status`:

```sh
xv attachment-key rewrap --to-key-id ACTIVE_KEY_ID --format json
xv attachment-key rewrap --to-key-id ACTIVE_KEY_ID --apply --offline
```

Preview authenticates every visible managed current file without writing. Apply
also verifies the whole inventory before the first replacement, then re-encrypts
each old file and records the exact retained target-key version. It preserves
user metadata, tags, groups, content type, and upload bookkeeping. Plaintext stays
in memory. Already-target files are authenticated and skipped.

Schema-1 files use their exact source key versions. Older files without a schema
require the ring's explicit legacy binding. Missing keys, invalid crypto metadata,
tampered ciphertext, or an unexpected active ID stop the operation. Complete
metadata access is required; tag-read errors are not treated as empty tags.

If interrupted, completed replacements remain readable and a retry skips them.
Keep writers stopped until verification completes: drift checks detect observed
changes but do not provide an atomic transaction across the vault. Rewrap changes
no keys or pointer bindings and never deletes retained identities. Historical
blob versions, external copies, and backups retain their original key dependency;
rewrapping current files does not make old keys safe to delete.

### Logical retirement without deleting keys

`attachment-key retire` marks a retained key as retired after verifying that the
healthy V2 ring and visible current managed files no longer depend on it:

```sh
xv attachment-key retire --key-id OLD_KEY_ID --format json
xv attachment-key retire --key-id OLD_KEY_ID --apply --offline
xv attachment-key keys --format json
```

Preview verifies custody and authenticates all managed current files without
writing. Apply requires stopped writers and adds only the
`xv_attachment_key_retired=true` metadata tag. `keys` exposes a `retired` boolean.
Other tags, identity material, enabled state, and provider versions are preserved;
no ciphertext or pointer is changed. Repeating the command verifies the same
conditions before returning an already-retired result.

The active key and explicit legacy binding cannot be retired. The legacy binding
remains protected even after current files are rewrapped, because historical
pre-schema ciphertext may still depend on it. Any current managed reference to
the candidate, including another provider version or legacy slot, also blocks
retirement. Invalid metadata, unreadable keys, tampering, or observed drift stop
the operation.

Use full vault visibility and permissions. Retirement is refused through an agent
policy context because a restricted view cannot establish that a key is unused.
The marker is advisory: it does not revoke access, block explicit pointer recovery,
or authorize deletion. Historical blob versions, external ciphertext, and backups
may still require the key; their normal exact-version reads remain supported.
