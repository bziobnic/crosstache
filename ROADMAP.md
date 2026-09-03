# Crosstache Roadmap

> **Baseline:** `v0.38.0` · **Scope:** open work only

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

### P0 — Make attachment-key creation race-free

`get_or_create_identity` currently performs get → unconditional set → re-read.
Two first attachments can therefore encrypt with different keys while the
reserved `xv-attachment-key` secret is being overwritten; re-reading makes a
client converge for subsequent work but cannot repair ciphertext already written
with the losing key. Add an atomic create-if-absent/conditional-write path, cover
all providers, and test the real two-writer outcome before treating first use as
safe.

### P1 — Make rename and migration attachment-aware

Attachments are associated by `attachments/<secret-name>/<filename>`. Rename
currently cannot portably move that prefix (the web UI refuses the operation),
and `xv migrate` copies secrets but not attachment ciphertext or key custody.
Design and implement recoverable rename/move semantics plus migration that
preserves readability, handles target-key conflicts explicitly, and is safe to
resume after partial failure. Do not silently leave or orphan blobs.

### P1 — Persist a scheduled target manifest

`xv schedule` embeds an explicit `--vault` when supplied and otherwise pins the
current non-empty global `default_vault`; only a schedule with neither resolves
its vault name at execution time. It still does not pin the full target identity:
backend or named-backend instance, project environment/workspace alias, config
path, and working directory can differ in the scheduler session, so a same-named
vault may resolve against the wrong provider or fail. Installation should persist
and validate an explicit resolved target manifest. Define upgrade, missing-target,
drift-reporting, and uninstall behavior and exercise the manifest on real
scheduler runners where practical.

### P1 — Finish cache invalidation on vault removal

Vault deletion and purge currently invalidate the vault list but leave that
vault's cached secret/file listings behind. Wire the existing
`cache::invalidation::on_vault_removed` seam into successful removal paths and
cover both built-in and named backends.

### P1 — Split secret-domain types from provider/legacy manager types

Backend-neutral traits still exchange request/property models owned by
`secret::manager`, which preserves Azure-era coupling in otherwise generic code.
Move value-bearing requests, summaries, properties, updates, and deleted/version
models into a dedicated secret-domain module with explicit redaction/zeroization
contracts. Keep provider adapters responsible for translation and avoid another
flag-day rewrite.

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
key initialization is the first required consumer; rotation and other concurrent
workflows should reuse the same contract rather than inventing command-specific
locks.

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
