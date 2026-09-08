# Attachment provider commits and local recovery

> **Status:** ✅ Shipped in **v0.39.0**. Create-only Local/AWS commits, Azure
> versioned Set plus exact-version verification, and local journaled key pairs.

User-authorized continuation from the completed snapshot chunk (b16b12a).

## Contract

Retained key creation is a dedicated custody operation. Local and AWS use atomic create-only commits; a racing collision is reclassified and never overwritten. Azure uses its versioned Set response, followed by exact-version verification. Only after verifying the committed key can initialization publish the active pointer and return encryption material. Policy and audit checks cover the new operation.

Local key writes must durably commit ciphertext and metadata together, including first creation and active-pointer replacement. Reuse the local transaction lock/recovery machinery; recover interrupted staging or activation before any reader observes the pair. Preserve existing retained versions and refuse unexplained half-pairs. Test failures at publication, each activation, and commit/cleanup boundaries, plus interrupted recovery.

## Tasks

- [x] Add dedicated retained-key commit to custody, provider dispatch, collision retry and policy enforcement; prove create-only behavior and fail-closed verification.
- [x] Implement durable Local key pair creation/replacement and recovery with fault injection and concurrent create tests.
- [x] Exercise AWS create-only and Azure versioned request/response paths with hermetic provider seams, including concurrent initializers and exact versions.
- [x] Independent review, workspace tests, lint, formatting; update documentation, commit and sync branch.

## Decisions

- Use existing isolated worktree feat/attachment-key-p0 and its just-verified baseline.
- Keep generic secret CRUD semantics unchanged where possible; Local durability may reuse shared journal helpers.
- The lifecycle design named by ROADMAP is absent from the available project files. Implement the explicit recorded invariants and protocol already in source and roadmap; no new lifecycle commands in this chunk.

## Progress

- Custody create-only regression observed RED, then passed with provider dispatch.
- Wrong exact-version response regression observed RED; initialization now refuses it before pointer publication.
- AWS SDK transport tests and Azure HTTP interleaving test passed.
- Local staging/half-pair regression tests observed RED; 99 Local secret tests passed, including 168 write-interruption cases and recovery restarts.
- Review identified legacy-layout orphan pointer refusal; reproduced and fixed. Explicit v2-create journals also prevent malformed legacy journals from deleting active keys. Scoped re-review clear.

## Final validation

- Workspace/all-feature tests: 4,000 passed, 47 ignored, zero failures.
- Web unit tests: 270 passed.
- Workspace/all-target/all-feature Clippy with warnings denied: passed after a test-loop style fix; the affected Azure test was rerun and passed.
- No-default-feature check: passed with the existing 36 dead-code warnings.
- Formatting and whitespace checks: passed.
- Independent review and scoped re-review: no remaining blockers.
- Provider tests use hermetic SDK/HTTP transports; live cloud tests were not run.

Structured attachment error variants remain the next PR 1 task; lifecycle commands remain outside this chunk.
