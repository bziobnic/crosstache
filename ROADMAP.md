# Crosstache Roadmap

> **Baseline:** `v0.39.0` · **Scope:** open work only

This is the canonical backlog for work not yet shipped. Released behavior and
completed work belong in [`CHANGELOG.md`](./CHANGELOG.md); retained designs under
`docs/superpowers/` are implementation history, not evidence that a feature is
still open.

Priority is a risk/order signal, not a release commitment:

- **P0** — possible data loss, unrecoverable data, or security boundary failure
- **P1** — correctness or architecture needed before broadening the product
- **P2** — important product/capability gap
- **P3** — useful expansion or polish

## Safety and correctness

### P1 — Finish attachment-aware transfers on remaining cloud routes

Recoverable same-vault rename and the supported cross-vault routes shipped in
v0.39.0 (`xv transfer`, generic `--with-attachments` on copy/move/mv/migrate).
Remaining provider and surface gaps:

- Azure destinations: Key Vault cannot atomically create destination secrets, so
  any→Azure attached transfers are refused.
- AWS and Azure source moves: those providers cannot conditionally delete source
  secrets. Copy is supported when snapshots and exact ownership verify.
- `xv update --rename` still refuses attached secrets; use `xv transfer --move`
  or the local web rename flow.
- Web attached rename is local-only; other backends keep the rename guard.
- Azure source copy still refuses ambiguous sanitized/case-insensitive attachment
  ownership and missing snapshots.

Operator matrix: [`docs/attachments.md`](./docs/attachments.md#rename-and-move).
Design history: [`2026-09-06-attachment-transfer-design.md`](./docs/superpowers/specs/2026-09-06-attachment-transfer-design.md).

### P2 — Distinguish scheduled-rotation failure categories

`DueRotationFailureCategory::classify` keys off the `CrosstacheError` variant on
purpose, but the rotation path it classifies (`execute_secret_rotate` in
`src/cli/secret_ops.rs`) wraps most provider failures in `CrosstacheError::config`,
so a denied write, an unreachable backend and a generator that failed all reach
the scheduler as `rotate-failed`. The categories exist and are now persisted —
`last-run.json`'s `diagnostic.code` carries the classification through to
`xv schedule status` — but they still collapse to `rotate-failed` because
`execute_secret_rotate` re-wraps every provider error before the schedule
runner's due-rotation service ever sees the typed variant. Fix it in the
rotation path — propagate the typed backend error instead of re-wrapping —
rather than by matching on error text.

### P3 — Scheduled-rotation manifest/status follow-ups

Smaller gaps identified while shipping the target manifest, drift refusal and
outcome/status reporting (`docs/superpowers/specs/2026-09-09-scheduled-target-manifest-design.md`,
now shipped):

- The default `rotate.log` path is derived by walking up from the state root
  rather than a single documented rule; an ancestor that does not exist yet
  can make the derivation surprising. Document or simplify the walk.
- Windows unit rendering does not quote `schtasks /TR` against every
  metacharacter (e.g. `&`, `^`) a manifest or log path could contain.
- `xv schedule status` on Windows issues five separate scheduler calls to
  assemble one report; consolidate where `schtasks` output allows it.
- A systemd install that is deregistered mid-way (timer removed, service left,
  or vice versa) is currently reported as `foreign` rather than as a specific
  half-deleted state.
- Real (non-fake) native coverage is partial and differs by platform.
  `.github/workflows/schedule-native.yml` lints the *rendered* launchd plist
  and systemd units with `plutil`/`systemd-analyze`, but on Windows it creates a
  **synthetic harmless task** (`/TR "cmd /c exit 0"`, triggered once in 2099)
  purely to check that the `schtasks /Query` XML and LIST shapes our parsers
  read are the shapes Windows prints — it never registers the rendered `/TR`,
  whose quoting is covered only by the unit snapshot. Everything else runs
  through the `XV_SCHEDULE_RUNNER=fake` path; there is no unit-level test
  against the real `launchctl`/`systemctl`/`schtasks` binaries.
- `manifest.json` stayed `schema_version: 1` when `target.vault_selection` was
  added on top of the PR 2 shape (a pre-`vault_selection` manifest is now
  refused as unreadable rather than versioned); a future incompatible manifest
  change should get its own `schema_version` rather than repeating that
  pattern.

### P1 — Split secret-domain types from provider/legacy manager types

The module split has shipped: request/property/summary/metadata models now
live in `secret::domain`, separate from the Azure-era `secret::manager`
implementation. Plaintext is wrapped in a dedicated `SecretValue` type with no
serde impls, a redacted `Debug`, and read access only through
`expose_secret()`; web metadata responses return a value-free `SecretMetadata`
body. The backend traits have also shipped their split: `SecretBackend`/
`SecretOperations` lost their single boolean disclosure flag in favor of
separate metadata-only and value-returning getters (`get_secret_metadata`/
`get_secret_version_metadata` return `SecretMetadata`; `get_secret`/
`get_secret_version` return `Secret`; `get_secret_snapshot` takes
`SnapshotValue::{Omit, Include}`), and the old combined value/metadata struct
is gone. What remains (PR 3): introduce explicit disclosure DTOs
(`DisclosedSecret`) for
the handful of export routes that legitimately return plaintext, with a
both-direction canary suite across CLI, web, cache, and errors proving every
other route stays value-free.

### P3 — Vault-list cache is Azure-only on the read side

`xv vault list` on the trait path (local, AWS, and named backends) neither
reads nor writes the `vaults` cache entry; only the Azure dispatch does. The
removal-side invalidation shipped for all backends, so wiring the read side
is a small follow-up: read/write `CacheKey::VaultList` in the trait-path
`List` arm of `src/cli/vault_ops.rs` the way `execute_vault_list` does.

## Product and platform work

### P2 — Workspace-wide UI views and TOTP surface parity

- The web/desktop UI can switch among resolved workspace entries across
  backends and routes each request to the selected `(alias, backend, vault)`.
  Remaining scope is an optional CLI-style union `ls`/`find` view across every
  attached entry; per-entry switching is complete.
- Bring the shipped CLI `xv totp` flow to the web/desktop UI and TUI with the same
  encrypted-field-only, no-accidental-stdout, clipboard-expiry, and redaction
  guarantees. Live/watch output, QR enrollment, and seed provisioning remain
  separate decisions.

### P2 — Provider compare-and-swap primitives

Define portable conditional secret mutation semantics (create-if-absent and
update-if-version/etag-matches) across Azure, AWS, and local. Use provider-native
preconditions where available and a fail-closed local implementation. Attachment
key custody already implements these semantics per provider (create-only writes
on Local/AWS, versioned Set with exact-version verification on Azure) behind its
own interface; lift that into a shared backend contract so rotation and other
concurrent workflows reuse it instead of inventing command-specific locks.

### P2 — Rotation workflow and rollout coordination

`xv rotate` can replace a stored value, run a generator, or delegate AWS native
rotation, but it does not provide a transactional external-system change,
validation, staged rollout, restart/redeploy, or rollback workflow. Design an
explicit hook/workflow model with failure states and idempotency before adding
surface; do not imply that updating the vault also updates consumers.

### P2 — Off-box audit durability

The local-backend audit chain and agent policy decision chain are tamper-evident,
not tamper-proof: a key holder can rewrite them and a writer can truncate them.
Add an append-only off-box sink (for example
syslog, an authenticated HTTP collector, or WORM/object-lock storage) with
backpressure, retry, and fail-open/fail-closed policy made explicit. A manually
pushed local Git remote is not a complete audit sink.

### P2 — AWS file-operation parity

Implement `xv file sync` for S3 and restore streaming upload/download plus atomic
local download replacement on the unified backend path. Preserve containment,
size limits, progress, metadata, and interruption safety rather than achieving
parity with an in-memory shortcut.

### P2 — Managed named backends

`xv backend add|rm|ls` manages one canonical instance per provider while advanced
`named_backends` entries still require hand-edited config (and Azure lacks a
managed multi-instance path). Add lifecycle commands for named instances,
including validation, workspace-reference safeguards, reconfiguration, and safe
removal. Keep backend configuration distinct from `xv cx` workspace attachment.

### P2 — Agent broker, bounded sessions, and approvals

Build on the identity and deny-by-default secret-policy foundation shipped in
PR #422:

- Add an authenticated local channel (Unix socket on macOS/Linux, named pipe on
  Windows) with peer identity verification and workload attestation.
- Issue opaque, one-shot or usage-limited handles instead of reusable plaintext.
- Make the parsed `max_duration` and `approval_tier` policy fields enforceable;
  bind approvals to agent, purpose, target, operation, duration, and policy
  version.
- Add locked/zeroized in-memory key custody, idle timeout, session-wide
  revocation, and an immediate kill switch.
- Add verified AWS-role and SPIFFE identity resolvers.
- Extend enforcement to vault/file and currently refused maintenance operations
  only where every affected resource can be bound to policy fail-closed.

Keep caches disabled during enforcement unless broker-managed cache-key custody
is designed explicitly. Dynamic provider credentials and off-box audit durability
remain separate follow-on capabilities.

### P3 — First-party CI integrations

The GitHub Action is first-party; GitLab and CircleCI currently use documented
install steps. Add a GitLab component with OIDC token acquisition and a CircleCI
orb, plus release/install verification and representative hosted-runner tests.
Do not claim provider OIDC parity until each path is exercised end to end.

### P3 — Additional backends

Candidate providers remain GCP Secret Manager, HashiCorp Vault KV v2, and a
1Password CLI bridge. Each implementation must satisfy and extend
[`backend-trait-checklist.md`](./docs/superpowers/specs/backend-trait-checklist.md),
state unsupported capabilities honestly, and include hermetic contract tests.

### P3 — P2P secret sharing (design-ready, unshipped)

The retained plan at
[`2026-05-27-p2p-secret-sharing.md`](./docs/plans/2026-05-27-p2p-secret-sharing.md)
covers identities, authenticated age encryption, trust, claim codes, and a relay;
validated spikes are recorded there. It is design-ready but no client, relay, or
public command has shipped. Revalidate dependencies, relay abuse controls,
identity recovery, and operational ownership before implementation.

## Explicitly discarded decisions

These are deliberate non-goals, not deferred backlog:

- **No cloud Git mirroring.** Do not copy Azure/AWS secret values into Git
  history; that creates a second, durable secret store with a different custody
  and deletion model. Git-native versioning remains local-backend-only.
- **No continuous replication.** Workspaces compose explicitly selected
  backends/vaults, and `xv migrate` is an operator-invoked transfer. Do not add a
  background bidirectional sync loop with ambiguous conflict ownership or silent
  propagation of deletes/rotations.

## Maintaining this file

- Add only unshipped work. Move completed behavior to `CHANGELOG.md` and mark its
  design/spec as implemented.
- Link an issue or design when one exists, but keep this file understandable on
  its own.
- Re-check assumptions against current source before changing priority or calling
  an item complete.
