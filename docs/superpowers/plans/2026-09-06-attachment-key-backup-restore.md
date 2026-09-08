# Attachment key backup and restore implementation plan

> **Status:** ✅ Shipped in **v0.39.0** (PR #432). Operator guide:
> [`docs/attachments.md`](../../attachments.md#encrypted-key-backups-and-offline-restore).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Export encrypted key recovery bundles and restore verified keys and current file references offline.

**Architecture:** A bounded, private age codec validates the recovery manifest; collection reads custody and coherent file snapshots; restore preflights everything before committing identities, rebinding ciphertext metadata, and publishing the pointer last. CLI file I/O never writes plaintext key material.

**Tech Stack:** Rust, existing age/serde/zeroize/sha2/tempfile, async custody/file traits.

**Spec:** docs/superpowers/specs/2026-09-06-attachment-key-backup-design.md

## Global constraints

- 16 MiB ciphertext and plaintext bundle limits; 10,000 identities; 100,000 files.
- No normal-download fallback, raw provider access, unencrypted output, key deletion, or rotation.
- Preview has zero provider writes; apply requires stopped writers and --apply --offline.
- Explicit --repair-pointer permits only malformed-pointer repair, never conflicting parsed bindings.
- Scope is visible current files; payload and historical-object backups remain separate.
- Use generic safe errors for untrusted bundle/identity parsing; no private Debug implementations.

## Shared interfaces

All new modules are gated by file-ops. The parent owns src/secret/mod.rs registration.

```rust
// attachment_backup_codec.rs: fields pub(crate), no Debug on private material.
#[derive(Serialize, Deserialize)] // deny_unknown_fields on all DTOs
pub(crate) struct Bundle {
    pub format: String, // "xv-attachment-key-backup"
    pub schema_version: u32, // 1
    pub source_backend: String,
    pub source_vault: String,
    pub created_at: String,
    pub active_key_id: String,
    pub legacy_key_id: Option<String>,
    pub identities: Vec<IdentityRecord>,
    pub references: Vec<SourceRef>,
    pub files: Vec<ManifestFile>,
}
pub(crate) struct IdentityRecord {
    pub key_id: String,
    pub identity: Zeroizing<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceRef {
    pub key_id: String,
    pub slot: String, // legacy or retained
    pub provider_version: String,
}
pub(crate) struct ManifestFile {
    pub name: String,
    pub ciphertext_sha256: String,
    pub key_id: String,
    pub source_ref: Option<SourceRef>, // None only for explicit pre-schema legacy
}
pub(crate) const MAX_BUNDLE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) fn validate(bundle: &Bundle) -> Result<()>;
pub(crate) fn encrypt(bundle: &Bundle, recipient: &age::x25519::Recipient) -> Result<Vec<u8>>;
pub(crate) fn decrypt(bytes: &[u8], identity: &age::x25519::Identity) -> Result<Bundle>;
pub(crate) async fn collect(keys: &dyn AttachmentKeyStore, files: &dyn FileBackend,
    backend: &str, vault: &str) -> Result<Bundle>; // attachment_backup.rs
pub(crate) async fn restore(keys: &dyn AttachmentKeyStore, files: &dyn FileBackend,
    vault: &str, bundle: &Bundle, apply: bool, repair_pointer: bool) -> Result<RestoreReport>;
```

### Task 1: Bounded encrypted bundle codec

**Files:** Create src/secret/attachment_backup_codec.rs with inline tests.
**Produces:** Bundle DTOs and validate/encrypt/decrypt above. Zeroizing serde field can use custom serde conversion to avoid dependency feature changes. Every DTO also derives Serialize/Deserialize; SourceRef derives the stated extra traits.

- [x] Write round-trip and rejection tests before implementation. Example assertion:
  `assert!(decrypt(&ciphertext, &age::x25519::Identity::generate()).is_err());`
- [x] Observe RED using `cargo test --lib attachment_backup_codec`.
- [x] Implement bounded recipient-only age codec. Reject duplicate JSON object fields through serde structs, unknown fields, extra bytes, unknown schemas, ID/identity mismatch, duplicates, dangling refs/bindings, unsafe file names, malformed SHA-256/version strings. For opaque versions reuse metadata reference validation. Require each source reference key in identities and each manifest reference in references; pre-schema key equals explicit legacy binding. Reject legacy pointer version bindings with different IDs; retained versions are scoped by the key-ID-derived record name. Bound decryption via Read::take(MAX+1); reject passphrase age before work-factor processing. Never expose parser errors containing source data.
- [x] Test authenticated truncation/tampering, empty/oversized input and plaintext, duplicate and inconsistent manifests, same encrypted bytes round trip without plaintext leakage; run focused tests and report.

### Task 2: Offline restore and retry

**Files:** Create src/secret/attachment_restore.rs and src/secret/attachment_restore_tests.rs. Own only these files.
**Consumes:** Codec Bundle interface above, existing custody/FileBackend traits.
**Produces:** restore signature above and public safe Serialize RestoreReport with outcome, key IDs/version mappings and per-file outcomes (no private values).

- [x] Write Local cross-vault test with different version tokens before implementation. Copy ciphertext/metadata separately; assert preview creates nothing; apply preserves ciphertext and ordinary download returns original bytes.
- [x] Observe RED with `cargo test --lib attachment_restore`.
- [x] Implement full spec preflight (bundle validate before provider access; all target names; all keys/files/pointer before writes). Reject extra managed files; verify ciphertext hash plus authenticated decrypt, source metadata exact ref or already rebound exact destination identity. Refuse V1 or conflicting parsed V2 and allow malformed only with repair flag. Reject missing value/disabled records and empty/mismatched returned versions.
- [x] Commit missing retained keys, exact-verify conflict winners, derive destination refs, reread file drift before upload, preserve all metadata/tags/groups/content type and ciphertext. Confirm snapshots and normal decryption. Pointer compare version+value and publish last, idempotently. No portable CAS claims.
- [x] Cover missing keys+malformed pointer, collisions zero writes, disabled keys, wrong ciphertext, changed refs, different legacy, interrupted/repeated restore, pointer/file drift, readback failure, and metadata preservation. Use fault-injecting trait wrappers where necessary.

### Task 3: Export collection, CLI, and user documentation

**Files:** Create src/secret/attachment_backup.rs; modify src/cli/attachment_key_ops.rs, src/secret/mod.rs, docs/attachments.md, README.md, CHANGELOG.md, ROADMAP.md; extend tests/e2e_local_file_ops.rs.
**Consumes:** Codec and restore interfaces above.

- [x] Add collection and CLI tests proving V1/V2, historical exact refs, distinct active/legacy, encryption round-trip, no clobber and required offline flags.
- [x] Implement collect with enabled/current/exact custody checks, managed snapshot authentication, source-ref deduplication, SHA-256 manifest, and final set/pointer/snapshot drift checks.
- [x] Add export/restore CLI resolving existing workspace/backend policy wrappers. Parse recipient/recovery key safely before provider I/O, read input with a size cap, zeroize identity file contents, create no-clobber ciphertext files with tempfile and flush. No key data in reports. Keep human/JSON/YAML existing render behavior.
- [x] Document key bundle vs separate payload backup, stopped writers, repair flag, current-visible scope, and no rotation. Run CLI round-trip on Local.

### Task 4: Integration review and verification

- [x] Implement review-required strict file restore adapters: FileBackend get_file_restore_info/restore_file default Unsupported; Local preserves its existing exact request metadata; AWS/Azure strict info propagates tag failures and restore upload preserves supplied bookkeeping/tags. Add injected SDK tests. Custody preflight_set_secret checks configured Set policy for every planned write before mutation; actual writes retain authorization checks. Refresh destination listing metadata and retain only per-file hashes/info across restore passes.
- [x] Review codec, restore, and collector separately for spec compliance and quality, then whole branch for interactions. Fix verified findings.
- [x] Run `cargo fmt --all --check`, `cargo clippy --all-features --workspace --all-targets -- -D warnings`, `cargo test --all-features --workspace`, and `cargo check --no-default-features`.
- [x] Record actual verification results and limits. Publish the verified implementation through the session completion workflow; never merge automatically.


## Verification and review results — 2026-09-06

- Full all-features workspace suite: 4,149 passed, 0 failed, 47 ignored.
- All-features workspace/all-targets Clippy with `-D warnings`: passed.
- No-default-features build: passed (existing feature-gated dead-code warnings).
- Formatting and whitespace checks: passed.
- Local CLI cross-vault restore, historical V1/current V2 file recovery, and
  injected Azure/AWS transport preservation tests passed.
- Independent task and final reviews completed; verified findings fixed:
  record-scoped version validation, metadata/tag preservation, refreshed listing
  metadata, Set/raw-Get policy preflight, malformed V1-prefix repair, and identity
  whitespace compatibility. Exported provider identities are canonicalized;
  ordinary schema-1 downloads now trim as custody validation already does.
- Source/destination drift tests and interrupted apply/readback retries passed.
- Cloud verification used injected SDK HTTP transports, not live customer vaults.
  Offline operation and separate ciphertext backups remain required. Azure
  separate-copy recovery needs a separately configured storage container/account.
