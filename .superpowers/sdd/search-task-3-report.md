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
