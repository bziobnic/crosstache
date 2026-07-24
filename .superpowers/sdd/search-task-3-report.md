# Search / Upload / Responsive — Task 3 Report

## Outcome

Implemented a bounded upload preflight contract and explicit, conflict-safe
upload policies for the embedded web API.

## TDD evidence

The first route-level test run was intentionally RED:

- `cargo test --features ui web::files::tests --lib`
- 5 tests failed for the expected missing behavior (405 on preflight and
  unconditional overwrite / absent policy responses).

The Local create-only concurrency test was then added RED and failed to compile
because `upload_file_if_absent` did not exist. After the minimal backend
contract and Local implementation were added, it passed with exactly one
winner.

A later regression test for backend-significant leading destination prefixes
failed with `docs/report.pdf` instead of `/docs/report.pdf`; destination joining
was corrected without normalizing away backend-specific path syntax.

## Implemented contract

- Added bounded `UploadCandidate` and `UploadPreflightResult` request/response
  models.
- Added authenticated `POST /api/files/preflight` with a 512 KiB body limit,
  1–1000 candidates, bounded string fields, deny-unknown JSON, and the stable
  100 MiB per-file maximum.
- Preflight uses the exact stateless alias/backend/vault triple, verifies the
  selected backend's file-storage capability, and exercises the backend's
  metadata lookup/name validation boundary.
- Conflict results include the exact existing name and a deterministic,
  currently unreserved `name (2).ext` suggestion.
- `POST /api/files` now accepts only explicit `skip`, `replace`, or `rename`
  policies (with `target` restricted to rename).
- A conflict without policy returns `409 xv-file-conflict` and leaves the
  existing bytes unchanged.
- Skip returns a stable `{ "status": "skipped", "name": ... }` outcome.
- Replace is the only path that authorizes overwriting.
- Rename writes only the explicitly requested target and never overwrites it.
- Added `FileBackend::upload_file_if_absent`; unsupported providers fail closed
  rather than using a racy check-then-write fallback.
- Local create-only uploads perform destination checking and commit while
  holding the exclusive vault lock. The web test backend provides the same
  mutex-atomic contract for route concurrency tests.
- Conflict and validation errors use stable safe envelopes and do not echo
  rejected request fields or file bytes.

## Verification

- `cargo test --features ui web::files::tests --lib` — 9 passed
- `cargo test --features ui backend::local::files::tests --lib` — 10 passed
- `cargo test --features ui web:: --lib` — 152 passed
- Hermetic `cargo test --features ui --lib` — 1058 passed, 1 ignored
- `cargo clippy --all-targets --features ui -- -D warnings` — passed
- `cargo check --all-targets --all-features` — passed
- `cargo fmt --all -- --check` — passed
- `git diff --check` — passed

No tests were weakened or removed. No workspace-global routing state was
introduced, and no remote branch was pushed.

## Independent review remediation

The initial implementation received a FAIL/FAIL review. Every finding was
remediated test-first:

- Local create-only and replace uploads now use private staged ciphertext and
  metadata, durable old-pair backups, an atomically published journal, ordered
  file/directory syncs, exclusive vault locking, and journal-removal as the
  commit point.
- Recovery runs before every Local file read, list, delete, and upload. Before
  commit it restores the exact old ciphertext/metadata pair (or removes a
  partial create); after commit it preserves the exact new pair and cleans
  residue.
- Crash injection covers pre-journal staging, published journal, ciphertext
  activation, metadata activation, and post-commit cleanup. Restart tests prove
  exact bytes plus groups/metadata/tags, successful create-only retry, and
  recovery through every entrypoint.
- Transaction directories/files are owner-only and symlinked transaction roots
  or artifacts fail closed. The journal contains no raw file name; the existing
  encoded file stem is reused, so no extra plaintext path is introduced.
- Added truthful `has_atomic_file_create` plus an implementation predicate.
  Local advertises and implements it; AWS/Azure do not. Effective UI context
  exposes the combined truth, including the advertised-but-unimplemented test.
- Unsupported preflight returns per-file `unsupported` before metadata calls.
  Default/Skip/Rename uploads reject safely without mutation; explicit Replace
  remains available on ordinary file-storage backends.
- The 100 MiB file maximum is unchanged. Multipart has a tightly bounded
  64 KiB envelope allowance, a Content-Length fast rejection, and the normal
  streaming/body limit for unknown lengths. Boundary tests cover exact max,
  max + 1, and route envelope max + 1 without allocating a giant body.
- Preflight caches duplicate metadata checks, caps each suggestion search at
  100 attempts, and enforces a request-wide 2,000-lookup budget. Duplicate and
  adversarial call-count tests lock the bounds.
- Candidate and multipart content types are parsed as MIME. Empty, malformed,
  control-containing, and oversized values receive field-specific redacted
  errors.

### Final verification after remediation

- `cargo test --features ui web::files::tests --lib` — 16 passed
- `cargo test --features ui backend::local::files::tests --lib` — 15 passed
- `cargo test --features ui web:: --lib` — 160 passed
- Hermetic `cargo test --features ui --lib` — 1072 passed, 1 ignored
- `cargo clippy --all-targets --features ui -- -D warnings` — passed
- `cargo check --all-targets --all-features` — passed
- `cargo fmt --all -- --check` — passed
- `git diff --check` — passed

## Second review remediation

A follow-up durability and platform-boundary review was also addressed
test-first:

- Newly created Local `files`, `.transactions`, and per-file transaction
  directories are synced together with their parent links before the first
  active ciphertext or metadata rename. An ordered event test starts from a
  fresh files directory and proves every required sync precedes mutation.
- Existing transaction roots and children must be real directories owned by
  the current user. Overly permissive modes are repaired to `0700` and synced;
  wrong ownership fails closed where ownership metadata is available. Existing
  symlink rejection remains covered.
- Local logical file keys now have a truthful platform-safe limit of 255 UTF-8
  bytes. Boundary tests cover ASCII 255/256 and multibyte 254/256.
- Percent-encoded names that would exceed the component limit use a fixed
  SHA-256 storage stem. Existing names whose encoded components already fit
  retain their previous stems, while a valid 255-byte name round-trips through
  upload and download.
- Invalid Local names fail before transaction artifacts are created. The web
  preflight and multipart routes validate the complete destination name before
  metadata lookup or file buffering and return stable, redacted,
  field-specific errors.
- Name validation is a backend contract with a permissive default, so providers
  do not accidentally inherit Local filesystem constraints. AWS continues to
  apply its existing provider-specific validation.

### Final verification after second remediation

- `cargo test --features ui web::files::tests --lib` — 17 passed
- `cargo test --features ui backend::local::files::tests --lib` — 19 passed
- `cargo test --features ui web:: --lib` — 162 passed
- Hermetic `cargo test --features ui --lib` — 1077 passed, 1 ignored
- `cargo clippy --all-targets --features ui -- -D warnings` — passed
- `cargo check --all-targets --all-features` — passed
- `cargo fmt --all -- --check` — passed
- `git diff --check` — passed
