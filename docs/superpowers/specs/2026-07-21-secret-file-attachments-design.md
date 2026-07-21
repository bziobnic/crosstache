# Secret File Attachments — Design

> **Status:** 📋 Approved design, not yet implemented (2026-07-21).

## Problem

crosstache's file storage (`xv file`) is not confidential on cloud backends:
Azure Blob / S3 server-side encryption protects data at rest, but anyone with
storage-account access reads plaintext. The local backend already age-encrypts
every file. We want:

1. **Confidential files** on every backend — content unreadable without vault
   (secret-store) access, regardless of storage-layer access.
2. **Attachments** — files associated with a specific secret (e.g. the cert
   that goes with `db-cert`).
3. **Arbitrary sizes** — up to the existing 5 GiB file cap, so content cannot
   live in Key Vault secret values (25 KB cap).

## Decision

**age client-side encryption on every backend, with key custody in the vault's
secret store.** The objection to age-on-Azure is key placement, not the
cipher: store the age identity as a vault secret and file access becomes gated
by Key Vault RBAC (audited), not storage RBAC. One scheme, one code path,
reuses the existing `age` dependency. Works identically on Azure (identity in
Key Vault, ciphertext in Blob), AWS (Secrets Manager + S3), and local.

Rejected alternatives:

- **Azure-native CEK wrapping** (client-side encryption with Key Vault
  *keys*): not implemented in Rust SDK v0.21, requires a resource type we
  don't manage, Azure-only — three code paths instead of one.
- **SSE + storage RBAC only:** fails the confidentiality requirement.
- **Content in Key Vault secret values:** 25 KB cap fails the size
  requirement.

## Design

### 1. Key management

- One reserved secret per vault: **`xv-attachment-key`**, value is an age
  x25519 identity string (`AGE-SECRET-KEY-1…`). Recipient is derived from it.
- Auto-generated on first attach / encrypted upload. After writing, the client
  **re-reads the stored value and uses that**, so a concurrent-create race
  converges on a single key.
- Filtered out of `xv list` output. `xv delete xv-attachment-key` warns that
  all attachments in the vault become unreadable.
- No local-backend special case: the identity is a vault secret on every
  backend. Local storage double-encrypts (store key over attachment key);
  harmless, and uniformity beats a second code path.
- Skipped (retrofit later if needed): per-file keys, key rotation command.

### 2. Encryption layer

- New small module `src/secret/attachments.rs`: fetch identity via the
  existing `SecretBackend`, age-encrypt the buffer, call the existing
  `FileBackend::upload_file`; reverse for download. **No changes to the
  `FileBackend` trait or any backend implementation.**
- Encrypted files carry file metadata **`xv-encrypted: age`**. `xv file
  download` auto-decrypts when the flag is present.
- Buffered encryption, matching the existing `FileUploadRequest { content:
  Vec<u8> }` path. Streaming age is the upgrade path if large attachments
  become common.

### 3. Attachment association

- Pure naming convention in existing file storage:
  **`attachments/<secret-name>/<filename>`**.
- Listing a secret's attachments = prefix listing via the existing
  `FileBackend::list_files_hierarchical`. No metadata schema; no secret-tag
  budget consumed.
- `xv delete <secret>` checks the prefix; the existing confirmation prompt
  includes "N attachment(s) will also be deleted", then cascades.

### 4. CLI

- `xv attach <secret> <path>` — encrypt + upload as attachment.
- `xv attachments <secret>` — list; `--get <name>` downloads + decrypts.
- `xv detach <secret> <name>` — delete the attachment blob.
- `xv file upload --encrypt` — standalone confidential file (same key, same
  metadata flag, no secret association).
- On a backend/config without file storage, attach fails fast with the
  existing `has_file_storage` capability error.

### 5. Error handling

- Missing key on decrypt → `attachment key not found in vault '<v>'`.
- age decryption failure → friendly "wrong or rotated attachment key" message.
- All errors flow through the existing `CrosstacheError` / `BackendError`
  mapping.

### 6. Testing

- Round-trip unit test of the encrypt/upload/download/decrypt helper against
  stub backends (in-memory secret + file stubs, following the existing
  `backend/file.rs` test patterns).
- Key auto-generation test: first attach creates `xv-attachment-key`; second
  attach reuses it.
- Delete-cascade test: deleting a secret removes its `attachments/<name>/`
  prefix.
- `xv list` filtering test: reserved key never appears.
