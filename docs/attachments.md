# Secret file attachments

Client-side age encryption for files associated with a secret — and for
standalone confidential uploads — on every backend that has file storage
(Azure Blob, AWS S3, local).

Storage-layer access alone is not enough to read plaintext: the age identity
lives in the vault's secret store as the reserved secret `xv-attachment-key`,
so attachment access is gated by vault permissions.

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
| Key | One age x25519 identity per vault, stored as secret `xv-attachment-key`. Created automatically on first attach / `--encrypt` upload; concurrent creates re-read the stored value so races converge on one key. |
| Ciphertext location | Ordinary file storage under `attachments/<secret-name>/<filename>`. Association is the naming convention — no secret tags are consumed. |
| Metadata flag | Uploaded blobs carry `xv_encrypted=age`. Underscore is required: Azure Blob rejects hyphenated metadata keys (`xv-encrypted` fails with 400). |
| Listings | `xv list` / `xv ls` hide `xv-attachment-key`. Treat it as infrastructure, not a user secret. |
| Download | `xv attachments --get` and `xv file download` decrypt when the blob is under `attachments/` or carries the metadata flag. |

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

Deleting `xv-attachment-key` itself prompts that **all** attachments in the
vault become permanently unreadable. Group deletes refuse the reserved key
silently skipping it with a warning — use the single-secret form if you
really mean to destroy the key.

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

## Web UI

With `--features ui`, the secret detail drawer lists attachments as download
links (`GET /api/secrets/{name}/attachments`). File downloads go through the
same decrypt path as `xv file download`. See [`web-ui.md`](web-ui.md).

## Common pitfalls

| Symptom | Cause / fix |
|---------|-------------|
| `attachment key not found in vault '…'` | No attachments were ever created, or `xv-attachment-key` was deleted. Re-attach / re-upload with `--encrypt` to mint a new key — old ciphertext stays unreadable. |
| `wrong or rotated attachment key` | Ciphertext was encrypted under a different identity than the one currently in the vault. Restoring the original key secret is the only recovery. |
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
