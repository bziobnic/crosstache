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

### P0 — Race-free attachment-key lifecycle

Design: `2026-09-03-xv-race-free-attachment-key-lifecycle-design.md`
(immutable per-key records + non-secret active pointer + exact-version blob
binding). Staged as PR 1 (integrity foundation) → PR 2 (lifecycle) → PR 3
(rewrap/retirement).

**PR 1 — implemented on this branch (not yet merged):**

- `src/secret/attachment_key.rs`: portable `ak1-` key IDs derived from the
  public recipient (§7.1); reserved schema-1 crypto metadata that overwrites
  caller-supplied reserved keys (§7.3); V1/V2 active-pointer model and strict
  `ak1-` parsing with no fallback on malformed values (§10.1); non-`Debug`,
  non-`Serialize` `AttachmentKeyMaterial` that derives its ID from the parsed
  identity and verifies it (§7.4, I6/I9); the pure download-classification
  decision table (§11 / §D order 1–7); and the structural reserved-name
  classifier (exact pointer vs. strict record vs. ordinary — the broad prefix
  stays unreserved) (§8 / §E).
- `src/secret/attachments.rs`: uploads dispatch on the vault's mode — an empty
  vault **initializes directly to V2** (generate candidate → inspect the exact
  strict record name → commit a *marked* immutable retained record → re-read the
  exact version and verify the derived key ID → publish and confirm the
  non-secret V2 pointer), an existing raw V1 vault stays V1 (legacy slot), and a
  valid V2 pointer resolves the active retained record. Uploads bind the key
  identity **and** exact provider version from one response and stamp schema-1
  metadata; an unmarked user secret colliding with a strict record name is never
  modified. Downloads route through the classifier — managed-namespace
  non-ciphertext fails closed, foreign/ordinary bytes pass through, and a
  schema-1 blob is decrypted through its **pinned exact version** with derived-ID
  verification, so replacing the active key no longer orphans existing blobs (the
  §4.1 loss is closed on the read path). Broken/unknown references never fall
  back (I7). Two-initializer concurrency (§10.3) is proven: distinct generations
  each yield a decryptable blob regardless of which pointer wins.
- Structural reserved-guard adoption across generic surfaces (§8/§E): the exact
  active pointer **and** every strict-format retained record (`xv-attachment-key-
  ak1-<hash>`) are now hard-blocked (no `--force`) from generic mutation — CLI
  `set`/`mv`/rename/rollback/update/rotate/copy/`delete`/bulk-set, web
  PUT/PATCH/DELETE, `.env` import, and vault import. List/display hides the
  pointer and *marked* key records while keeping unmarked strict-format user
  collisions visible; generic migration skips marked custody records. The broad
  `xv-attachment-key-*` prefix stays fully usable for ordinary secrets.

- Provider-canonical name mapping (`CanonicalSecretName`, `canonicalize_secret_name`)
  runs before reserved classification, so alias spellings — `xv_attachment_key`,
  `xv--attachment--key`, `XV-ATTACHMENT-KEY` — that address the same provider
  secret cannot bypass the guard; the CLI/Web guard sites use the
  `*_canonical` variants (§8).
- The generic facade is mandatory at registry construction: eager/default,
  lazy/named, cloned, and cross-backend factory handles all expose guarded
  secret operations. Generic CLI/Web/import/migration callers cannot obtain
  the raw provider handle through the registry. Folder-only Web moves of
  reserved records are refused too; ordinary CRUD remains available.
- Attachment encryption uses a separate `AttachmentKeyStore` with only
  canonical custody reads, exact-version reads, and writes. It exposes no raw
  backend handle. The agent wrapper applies policy, raw-disclosure checks, and
  redacted decision/audit context before provider access, including when policy
  and guard wrappers are nested in either order. The unused legacy first-use
  upsert helper has been removed; existing legacy attachment reads remain.

**PR 1 — still open:**

- Single-generation `download_file_snapshot` for Local/AWS/Azure (§11, I4) — the
  download path still reads content and metadata separately.
- Durable journaled Local key-pair commit + crash recovery/fault injection (§12,
  I5).
- Provider-specific generation commit (§13): the V2 protocol is implemented and
  verified against the `SecretBackend` trait (in-memory), but Local/AWS still
  need create-only records and Azure needs its versioned Set path exercised
  against real provider request/response seams, with barrier-based concurrency
  tests per provider.
- Structured error variants (§20).

**PR 2 / PR 3:** status/inventory, offline V1→V2 upgrade, encrypted
export/import/recovery, rotation, rewrap, and logical retirement — none started.

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
