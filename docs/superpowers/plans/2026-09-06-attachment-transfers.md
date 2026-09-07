# Attachment transfers implementation plan

Spec: `docs/superpowers/specs/2026-09-06-attachment-transfer-design.md`.

## Global constraints

- Three PRs, sequential implementation and merge; inspect actual Bugbot contents on
  the final head, as well as passing Linux/Windows CI, before each merge.
- Preserve source data until the destination is independently verified. Fail closed
  on errors, collisions, drift, unsupported capabilities and ambiguous storage aliases.
- Manifest contains no secret values, private keys or decrypted attachments. Progress
  alone cannot authorize deletion. Preview performs no planned provisioning or data
  mutations; existing read auditing, locks and transaction recovery remain enabled.
  Apply is offline.
- Preserve custody guards and policy checks. No reserved-record generic transfer.
- Reuse strict snapshot/authentication helpers and private atomic file helpers.
- Use regression tests first, independent task review and whole-branch review.

## Task 1: transfer foundation

Implement a shared backend attachment-presence probe, including persisted local
attachments in builds without file operations, and wrapper forwarding that preserves
policy boundaries. Detect exact prefix membership and propagate failures. Add guards
to copy/move, workspace cross-vault mv, same-vault rename before folder changes,
update --rename before metadata changes, and migration before the first secret write.
Do not convert listing failures into absence. Destination attachments also block a
generic copy/overwrite. Dry runs must report the same unsupported attached transfers.

Add a read-only transfer planner and bounded, versioned private recovery manifest
codec with endpoint/name binding, strict object mappings, ciphertext generation
fingerprints and progress state. No execution in this PR. Tests cover attached source,
orphaned destination, sibling prefixes, errors, force/idempotency bypasses, bulk
preflight ordering, feature-disabled local storage, manifest round-trip and corruption.

## Task 2: attachment-aware rename (after Task 1 merges)

Implement offline same-vault transfer execution using the foundation. Preserve exact
ciphertext/metadata. Require safe create, strict verification, durable progress,
source drift detection and retry after each interruption boundary. Wire CLI and web
preview/apply/recovery with clear stopped-writers acknowledgement. Keep ordinary
atomic rename for unattached secrets. Test readback/write failures, source/destination
changes, partial cleanup, restart and metadata preservation.

## Task 3: cross-vault/backend transfers (after Task 2 merges)

Extend execution with explicit healthy destination key binding and authenticated
reencryption, preserving source keys. Integrate copy, move, workspace mv and migrate;
preflight the whole selected batch before writes. Detect shared physical blob namespaces
and refuse overlapping transfers. Verify destination readability and retries; copies
retain source, moves clean up only after verification. Test independent local vaults
and provider transport contracts as applicable, policy denial, destination key drift,
collisions and interrupted retries. Document unsupported combinations honestly.

## Verification and release

Use shared Cargo target at `.worktrees/attachment-key-p0/target`. For each frozen PR:
workspace all-features tests, all-targets/all-features Clippy with warnings denied,
format, diff check, and minimal-feature check when unconditional APIs change. Commit,
pull --rebase origin main, push and create a regular PR. Review every Bugbot comment
and review thread, fix findings, wait for checks on the final head, and squash merge
with the expected head SHA. Record merged PRs and validation below.

## Progress

- Task 1: merged PR #436 at 78369d4; 4,282 tests passed, 47 ignored; Linux/Windows CI and Bugbot clean.
- Task 2: merged PR #437 at acfe750; 4,352 tests passed, 47 ignored; 283 JS tests; Linux/Windows CI and actual Bugbot clean.
- Task 3: in progress from acfe750; cross-vault transfers and explicit destination key initialization.
