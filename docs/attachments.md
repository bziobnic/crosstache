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
provider mutation. Key recovery and retirement commands remain future work.

### Sync skips ciphertext

`xv file sync` never transfers encrypted attachment blobs (reserved
`attachments/` prefix or `xv_encrypted=age`). Syncing them as plaintext would
decrypt on download or clobber ciphertext on upload. Expect a skip summary;
use `xv attach` / `xv attachments --get` / `xv file upload --encrypt` instead.

### Rename and move

Attachment association is the blob path `attachments/<old-name>/…`. Renaming
or moving the secret does **not** rewrite those paths.

- **Web UI** refuses rename when attachments exist (`xv-attachments-block-rename`).
- **CLI** `xv update --rename` / `xv mv` can leave ciphertext under the old
  prefix. Detach (or re-attach under the new name) before renaming if you need
  the association to stay intact.

### Migration

`xv migrate` copies **secrets**, not file blobs. It will not move attachment
ciphertext between backends. If the target vault already has its own
`xv-attachment-key`, migrate **preserves** that key rather than overwriting it
(even under `--force-replace`) — overwriting would brick existing attachments
on the target.

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
