# Structured attachment errors

Continue the approved integrity foundation from merged PR #427 on feat/attachment-errors.

## Contract

Represent attachment domain failures using `CrosstacheError::Attachment(AttachmentError)`, with a public, data-free `AttachmentError` enum. Each kind has a stable `xv-attachment-*` code and a safe static message and recovery hint. No identity, ciphertext, provider payload, or untrusted metadata is retained in these errors. Preserve exit 2 and Web HTTP 400 for attachment domain failures, replacing previous generic invalid-argument codes; preserve other underlying provider/auth/network errors unchanged.

Kinds: KeyMissing, KeyInvalid, PointerInvalid, KeyMismatch, KeyVersionInvalid, CommitUnconfirmed, InitializationConflict, ReferenceInvalid, NotCiphertext, DecryptionFailed, SnapshotUnsupported.

## Tasks

- [x] Prove real attachment error paths currently return generic codes; add typed errors and migrate domain paths, preserving fail-closed behavior and provider failures.
- [x] Connect CLI hints, Web safe responses, JSON/CLI regression coverage; exercise Azure missing exact-version handling as typed missing-key input.
- [x] Validate focused/full workspace, formatting, Clippy; independent review; reconcile roadmap and public error docs; commit and push.

Lifecycle/rotation/rewrap commands remain outside this chunk. Missing referenced provider versions must remain distinguishable from authentication/transport failures. Existing local journal and custody permission errors retain their backend classifications.

## Progress

- Missing-key generic code, Web generic HTTP500 fallback, missing CLI hint, and Azure exact-version 404 mapping were each demonstrated by failing regressions before fixes.
- Review found two missing-record race classifications during commit verification/confirmation; failing regression observed and typed mappings added.
- Attachment-focused suite passed 82 tests before the final race cases and snapshot-error coverage.
- Real CLI regressions exposed upload/download progress contaminating JSON errors; both failed before routing single-file progress to stderr and passed afterward. Independent review found no blockers.
- Final validation: full all-features workspace passed 4,018 tests (47 ignored); Clippy all targets with warnings denied, formatting/diff checks, minimal-feature build, and 270 Web unit tests passed. Cloud behavior was tested through hermetic seams, not live provider accounts.
- The existing file roundtrip assertion was updated to check progress on stderr; the complete roundtrip and both new CLI regressions pass.
