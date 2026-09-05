# Consistent file-download snapshots

User-authorized continuation of the attachment integrity work; execute inline with the existing TDD and verification workflow.

## Design

Add `FileDownloadSnapshot { content, metadata }` and `FileBackend::download_file_snapshot`. The default refuses unsupported snapshots; it must never synthesize a snapshot using independent download/info calls. Attachment classification and decryption consume only this snapshot. Snapshot values are neither Debug nor Serialize.

- Local: recover pending transactions, then read metadata and ciphertext through the retained directory handles under the same existing vault/file lock. Decrypt local at-rest encryption before releasing the lock.
- AWS: use one GetObject response for metadata, declared length and streamed body. Enforce the existing download cap before and during consumption; reject missing/negative length and truncated/oversized bodies.
- Azure: obtain properties and metadata once, then pin every ranged GET to that ETag. Enforce size limits and exact total length, rejecting provider/precondition failures. An empty object is already a coherent snapshot from its properties response and requires no invalid zero-length range request.
- Do not fetch mutable object tags: attachment classification needs only generation-bound object metadata.

## Tasks

- [x] Demonstrate that attachment download still uses split reads, then switch it to the snapshot contract. Add a default-refusal test.
- [x] Implement local locked snapshots and test consistency during concurrent replacement plus missing/corrupt metadata and path containment.
- [x] Implement single-response AWS snapshots and test through the SDK HTTP seam, including metadata/body pairing, size bounds and truncation.
- [x] Implement Azure ETag-pinned snapshots and test actual SDK requests through an injected HTTP transport, including concurrent replacement, multiple pages and empty files.
- [x] Update test doubles to honor the snapshot contract, run focused then workspace tests, Clippy and formatting; obtain independent code review.
- [x] Update roadmap/changelog/operator documentation; commit, pull --rebase, and push the branch.

Provider key commit protocols and local key-pair crash recovery remain separate tasks.

## Validation

- Full workspace/all-feature tests: 3,970 passed, 47 ignored, zero failures.
- Web unit tests: 270 passed.
- Workspace/all-target/all-feature Clippy with warnings denied: passed.
- Formatting and diff whitespace checks: passed.
- No-default-feature check: passed with dead-code warnings.
- Independent review: corrected Azure HEAD 404 error mapping after a failing regression test; no remaining blockers.
- Cloud validation uses injected SDK HTTP transports; live cloud tests were not run.
