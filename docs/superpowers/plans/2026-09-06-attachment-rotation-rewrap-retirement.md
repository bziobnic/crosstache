# Offline attachment lifecycle: rotation, rewrap, retirement

> Use superpowers:subagent-driven-development for implementation and independent
> task review. User authorized three sequential PRs, clean reviews/CI, and merge
> of each before implementing the next. Do not merge a failing/unreviewed head.

## Contract

Build on merged encrypted backup/restore (#432). All commands preview by default;
apply requires --apply --offline with stopped writers. Normal exact-version reads
remain strict. No deletion, disabling, or loss of historical key material.

### Task 1: rotation PR

Create `src/secret/attachment_rotation.rs` and tests; register under file-ops.
Expose `rotate(keys: &dyn AttachmentKeyStore, vault: &str,
expected: &AttachmentKeyId, apply: bool) -> Result<RotationReport>`.

CLI: `attachment-key rotate --from-key-id OLD [--apply --offline]`.
Require a healthy V2 pointer matching OLD and verified marked/enabled exact active
and optional legacy identities. Validate policy for pointer write/readback before
any mutation; generate a random independent age identity only on apply, preflight
its retained write/readback, commit and exact-verify it, recheck original pointer
version/value plus current active/legacy refs, publish new V2 active preserving
legacy, confirm exact publication and old key readability. Never overwrite an
existing candidate name. Preview does not generate a key or mutate providers.

Retries using OLD after a successful rotation refuse before creating another key.
An interruption before publication may leave an unreferenced retained candidate;
retry may create a fresh candidate, but never deletes keys or rotates twice after
publication. State that limitation plainly; do not claim transactional idempotency.
Reports contain old/new IDs, legacy binding, outcome and destination version,
never private identities. Tests first: old/new upload readability, preview zero
writes, V1 refusal, expected-ID mismatch/repeated apply, invalid/disabled records,
policy denial, commit/readback failure, pointer/ref drift and interrupted retry.
Reuse existing strict custody semantics, no generic backend bypass.

Parent owns CLI/docs; subagent owns rotation module/tests. Run focused tests then
full all-features workspace tests/Clippy/format. Create regular PR, monitor current
head CI and Bugbot plus comments/review threads, fix findings and recheck, merge
with head SHA pinned only once clean.

### Task 2: rewrap PR (after Task 1 merge)

CLI: `attachment-key rewrap --to-key-id ACTIVE [--apply --offline]`.
Require expected healthy V2 active identity; authenticate all visible managed
current files before any write using strict exact source refs or explicit legacy
binding. Refresh listing metadata; require strict complete restore metadata APIs.
Keep only hashes/info between files. Re-read coherent snapshots and pointer/target
refs to detect drift. Re-encrypt old ciphertext with the active identity, stamp
retained exact target refs, and preserve metadata/tags/groups/content type through
restore_file. No plaintext disk artifacts. Verify ciphertext readback and normal
decryption. Already-target files verify then skip; interrupted retries resume.
Verify final file set and all target references; no key/pointer mutation or deletion.
Partial failure retains completed writes and remaining readable original files.
Test mixed V1/pre-schema/V2 sources, all metadata preservation, expected target
refusal, tampering, sparse listings, drift, interrupted upload/readback, and retries.
Review, CI, merge as Task 1 before proceeding.

### Task 3: logical retirement PR (after Task 2 merge)

CLI: `attachment-key retire --key-id OLD [--apply --offline]`.
Verify a healthy V2 ring and candidate exact identity, refuse active/legacy-bound
IDs, authenticate the full visible current managed-file inventory and refuse any
candidate reference. Require complete vault visibility; filtered agent enumeration
cannot establish unused status. Mark only a reserved metadata retirement flag
through a narrow custody method with policy enforcement and readback. Preserve
identity, provider historical versions, enabled state, and normal decryption.
Expose retirement marker in key enumeration; repeat is a verified no-op. Retirement
is an advisory lifecycle state, not permission to delete: historical blob versions,
external backups, and ciphertext outside visibility remain out of scope. Tests
cover active/legacy/current-reference refusal, unknown/invalid files, incomplete
visibility, no mutation in preview, metadata-only mark/retry and historical reads.
Review, CI, merge as Task 1. Report all three PRs and validation evidence.

## Progress

- Task 1: merged as PR #433 (0528326) after clean independent review, Bugbot,
  and Linux/Windows CI. Local validation: 4,172 passed, 47 ignored; Clippy/fmt clean.
- Task 2: implemented and reviewed, including a partial-envelope regression fix.
  Local validation: 4,191 passed, 47 ignored; all-features/all-targets workspace
  Clippy with warnings denied and formatting checks passed. PR checks/merge pending.
- Task 3: pending Task 2 merge.
