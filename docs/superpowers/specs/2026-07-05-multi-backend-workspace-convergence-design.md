# Multi-Backend Workspace Convergence: Roadmap & Phase 1

**Date:** 2026-07-05
**Status:** Approved design, targeting **v0.21.0**. Phase 1 (this design's execution scope) not yet implemented.
**Depends on:** [`2026-07-04-multi-vault-workspaces-design.md`](./2026-07-04-multi-vault-workspaces-design.md)
(Phases A–C, shipped v0.20.0/v0.20.1 — `Workspace`, `WorkspaceEntry`,
`resolve_workspace`/`resolve_secret_target`, lazy `BackendRegistry`),
[`backend-trait-checklist.md`](./backend-trait-checklist.md) (Backend trait
read-surface history)
**Source planning artifacts:** `.omc/plans/ralplan-xv-multibackend-roadmap.md`
(RALPLAN-DR, consensus reached iteration 2), `.omc/specs/deep-interview-xv-multibackend-roadmap.md`
(deep interview, ambiguity 19.75%)

## Motivation

Multi-vault workspaces (Phases A–C) shipped the *workspace* abstraction, but
`xv` still carries two resolution paths side by side: the workspace path
(`resolve_workspace` → `resolve_secret_target`) for commands run inside a
configured workspace, and a legacy path (`Config::resolve_vault_name`,
`BackendRegistry::active()`/`active_arc()`, `get_azure_auth_provider`) for
everything else. The dual-path split is the active bug source today — cache-key
divergence between `active().name()` (backend kind) and the config name, and
capability gates that read `reg.active()` instead of the resolved backend are
both symptoms of the same root cause: two representations of "what backend/vault
is this command targeting" that can silently disagree.

This document sequences the remaining multi-backend completion work into three
ordered phases and records the two structural decisions (ADR-1, ADR-2) behind
them, then scopes Phase 1's execution in full (Phase 1 ships with this design;
Phases 2–3 are outlined for later execution).

## Three-phase roadmap

### Phase 1 — Workspace-of-one resolution convergence (this design's execution scope)

Eliminate the legacy no-workspace secret-resolution path. Every `xv` invocation
resolves through the single workspace path; bare/no-workspace usage becomes a
**degenerate workspace-of-one** rather than a second code path.

**Mechanism (Option B-prime, see ADR-1):**

1. **B1 — build the distinguishable degenerate workspace-of-one.** Add
   `WorkspaceSource::Degenerate` to the `WorkspaceSource` enum
   (`src/workspace/mod.rs:24`) and `Workspace::is_configured(&self) -> bool`
   (`false` for `Degenerate`, `true` for `Context`/`ProjectToml`). Extend
   `resolve_workspace`/`resolve_workspace_from` (`mod.rs:230,252`) so that when
   no `cx`/`.xv.toml` workspace is configured, it returns `Some(Workspace)`
   with a single default `WorkspaceEntry` instead of `None`. **Never-`None`
   invariant:** `resolve_workspace` returns `Some` or propagates `Err` — never
   `None` — preserving the Azure no-vault hard error (`helpers.rs:89-91`) and
   the no-vault error UX (`settings.rs:502-504`) by deriving the degenerate
   entry's vault from the same chain `resolve_vault_for_trait` uses today
   (`helpers.rs:87-102`).
2. **B2 — audit and rewrite every presence-gate.** Because B1 makes
   `resolve_workspace` always `Some`, every call site that used
   `.is_some()` to mean "a REAL user workspace is attached" (~10 sites,
   enumerated below) must switch to `ws.is_configured()`, or it silently
   breaks — most notably `xv context use` (`config_ops.rs:1097`), which must
   NOT error against a merely-degenerate workspace.
3. **B3 — route capability-gate verbs onto the unified path.** `ls --deleted`'s
   soft-delete gate (`secret_ops.rs:1936`, currently `reg.active().capabilities()`)
   and the `rotate` fallback (`secret_ops.rs:2904`, currently
   `config.resolve_vault_name(None)`) resolve their backend via the workspace
   path so the capability check targets the *resolved* backend.
4. **B4 — collapse `resolve_vault_for_trait`.** Once B1 guarantees a workspace
   always exists, fold its logic into the degenerate-workspace builder and
   remove the standalone `resolve_vault_name` dependency from the resolution
   seam.
5. **B5 — collapse the resolver seam (single-site deletion).** Delete the
   no-workspace `else` branch of `resolve_workspace_or_default`
   (`helpers.rs:155-164`) — with B1 the `if let Some(ws)` above it always
   matches, so the `else` is dead code. This is the **only** seam edit; verb
   call sites that already delegate into `resolve_workspace_or_default` (set,
   get, list, history, rollback, rotate) need no changes.
6. **B6 — tests.** `xv context use` with no configured workspace succeeds;
   single-entry degenerate-workspace `ls` matches today's no-workspace `ls` in
   output shape, exit code, AND cache-key identity; bare `xv run`/`xv inject`
   over an `xv://` URI resolves byte-identically to today.
7. **B7 — document breaks in `CHANGELOG.md`.**

**Done-bar (seam-scoped, not repo-wide):**
- The seam `else` at `helpers.rs:155-164` is deleted; `resolve_workspace_or_default`
  has a single (workspace) path.
- `resolve_workspace` never returns `None`.
- Every enumerated presence-gate tests `is_configured()`, not mere presence.
- Every *surviving* legacy resolution call site (the survivor allowlist below)
  carries a `// Phase 2` or `// Phase 3` annotation — the verification grep
  asserts only the annotated allowlist remains, no un-annotated survivor.
- `cargo test` and `cargo clippy --all-targets` green; `CHANGELOG.md` lists
  every intentional break.

A repo-wide "zero `resolve_vault_name`/`.active()` references" grep is
explicitly **not** the bar — it is unsatisfiable, since Phase 2/3 sites
legitimately keep using them until those phases retire the call sites they
belong to.

**Presence-gates rewritten to `is_configured()` (B2 audit scope):**
`config_ops.rs:1097,1767`; `mv_ops.rs:251,362`; `tui/mod.rs:49`;
`secret_ops.rs:1915,2450,4469,4517,4725`; plus the `resolve_workspace_and_registry`
tuple consumers at `secret_ops.rs:5563,5678,6192,6744,6918,6979` (post-B1 these
take the workspace branch instead of `(None, None)` — audited here, pinned by
the B6 run/inject URI parity test).

**Survivor allowlist (Phase 2/3 call sites annotated, not removed, in Phase 1):**
- `resolve_vault_name` — Phase 2 (legacy managers): `secret_ops.rs:4998,5119,5171,5244,5302,5669,6183,6644,6681,7057`;
  `scan_ops.rs:174`; `system_ops.rs:384,460,1039`; `config_ops.rs:2216,2366`.
  Phase 3 (file ops): `file_ops.rs:115,252,682,2248`.
- `.active()` — 44 non-resolution capability/name-read hits in `secret_ops.rs`,
  plus manager-construction paths (Phase 2).
- `get_azure_auth_provider` — Phase 2 (manager construction):
  `config_ops.rs:898,928`; `system_ops.rs:591,594,654,1037,1038`; definition
  at `helpers.rs:283`.

*(Line numbers verified against v0.20.1 @ `a86d3bb`; re-verify before annotating
as code drifts.)*

### Phase 2 — Full legacy manager retirement (outline only, not executed here)

Delete `SecretManager`/`VaultManager`/`BlobManager` construction from CLI paths
entirely. Introduce `VaultBackend` and `AuditBackend` sub-traits on `Backend`;
migrate share/RBAC (`vault/operations.rs`), audit, and vault-lifecycle
operations off the legacy managers. Retire `get_azure_auth_provider`
(`helpers.rs:283`) and the manager-construction chokepoints
(`config_ops.rs:898,928`; `system_ops.rs:594,1038`) once no CLI caller remains.
Implements the A4 `--vault` composition semantics (below) for `run`/`inject`/`rotate`.

**Bar:** zero manager references from `src/cli/**`.

### Phase 3 — Default-entry file-ops routing (outline only, not executed here)

Route `xv file`/blob operations through a `FileBackend` resolution against the
workspace's **default entry only** — no union file views, no alias-qualified
file addressing (`xv file get azure-prod:x`). Replace
`config.resolve_vault_name(None)` in `file_ops.rs:115,252,682,2248` (and
`file_ops_aws.rs`) with default-entry routing.

**Bar:** `xv file` resolves through the workspace default entry only.

## A4 — `--vault` composition semantics under the synthesized workspace

Post-B1, `resolve_workspace_and_registry` (`helpers.rs:177-188`) always takes
the workspace branch — it no longer returns `(None, None)` for a bare
invocation. Its callers (`run`/`inject`/`rotate`) must therefore define what an
explicit `--vault` flag means when a degenerate workspace-of-one is always
present underneath it.

**Pinned semantic:** an explicit `--vault` flag **overrides** the degenerate
default entry for `run`, `inject`, and `rotate` — it does not add a second
entry and does not error. A user passing `--vault other-vault` gets exactly
that vault, exactly as before convergence; the degenerate workspace is purely
an internal representation of "no workspace configured, use the current/default
vault" and never surfaces as workspace-shaped behavior (no ambiguity search,
no alias resolution against it) to a `--vault`-qualified call. Against a
*real* (non-degenerate) workspace, existing multi-vault-workspace semantics
are unchanged: `--vault` continues to mean what it means today for that verb
(see the 2026-07-04 design's Write semantics section).

**Implementation timing:** the semantic is pinned in this design now (Phase 1),
but the callers that must implement it (`run`/`inject`/`rotate`) still
construct legacy managers and land in **Phase 2** — Phase 1's degenerate
builder and Phase 2's caller migration must agree on this semantic without
Phase 1 needing to touch those call sites.

## ADR-1: Workspace-of-one convergence over dual-path hardening

**Decision:** Converge the legacy no-workspace resolution path into a
distinguishable **degenerate workspace-of-one** (`WorkspaceSource::Degenerate`
+ `Workspace::is_configured()`), rather than hardening the two paths (workspace
vs. legacy) to keep them in sync.

**Drivers:**
1. **Correctness surface** — the dual workspace/legacy branching is the active
   bug source (cache-key divergence between `active().name()` and the config
   name; capability gates reading `reg.active()` instead of the resolved
   backend). Collapsing it to one path is the highest-leverage safety win.
2. **Verifiability** — a grep-provable single-path invariant was selected as
   the definition of done (deep interview R7); that bar is only meaningful if
   there is truly one path left to grep for.
3. **Blast radius / reviewability** — ~131 legacy call sites exist across
   `src/cli/`; the chosen approach must keep the diff reviewable and testable
   incrementally, not one unrevertible mega-commit.

**Alternatives considered:**
- **Dual-path hardening** (keep both paths, add tests/assertions to keep them
  in sync) — rejected in the deep interview's contrarian round (R4): it
  preserves the bug *class* (two representations that can disagree) instead
  of eliminating it; every future verb addition re-risks the same divergence.
- **Option A, big-bang path removal** (flip `resolve_workspace` to always
  return `Some` and delete every legacy branch in one pass) — reaches the same
  end state, but as a large, hard-to-review diff where a subtle gap in the
  degenerate workspace's behavior surfaces everywhere simultaneously, with a
  higher risk of a `cargo test` red for many overlapping reasons at once.
- **Naive "always-`Some`" flip without a `Degenerate` marker** — rejected as
  unsound: ~10 presence-gates use `resolve_workspace().is_some()` to mean "a
  real workspace is attached" (e.g. `xv context use` at `config_ops.rs:1097`);
  a bare flip permanently breaks that guard, TUI, `mv`/`copy`, and union-`ls`
  branch selection.

**Why chosen:** the degenerate-workspace mechanism (Option B-prime) reaches the
identical single-seam end state as a big-bang removal while keeping every
intermediate `cargo test` green — each verb migrates one at a time, with
failures localized. The one explicit `Degenerate` bit preserves every "a real
workspace is attached" semantic without reintroducing a second resolution
path, and the seam collapse becomes a single, easily reviewed deletion
(`helpers.rs:155-164`) once nothing upstream can still reach `None`.

**Consequences:** `resolve_workspace` never returns `None` (`Some`-or-`Err`),
so every consumer must handle the degenerate case explicitly; all
"real workspace attached" presence-gates must be rewritten to `is_configured()`
before the seam collapses, or they silently regress; `run`/`inject` tuple
consumers take the workspace branch one phase earlier than Phase 2's manager
retirement (byte-identical by construction, pinned by the B6 parity test).

**Follow-ups:** Phase 2 (manager retirement, A4 `--vault` composition
implementation), Phase 3 (file-ops default-entry routing), re-verifying
survivor-allowlist line numbers before annotating as code drifts.

## ADR-2: Full manager retirement over partial

**Decision:** Phase 2 fully retires `SecretManager`/`VaultManager`/`BlobManager`
construction from CLI paths — including Azure-only capabilities (share/RBAC,
audit, vault lifecycle) — by extending the `Backend` trait with optional
sub-traits (`VaultBackend`, `AuditBackend`), rather than retiring only the
secret-resolution managers and leaving Azure-only features on the legacy
managers permanently.

**Drivers:**
1. Matches ADR-1's principle of single representable state: a partial
   retirement that leaves Azure-only operations on `VaultManager`/audit-log
   paths recreates exactly the dual-path problem ADR-1 eliminates for secret
   resolution, just relocated to vault-lifecycle/audit operations.
2. The existing capability-flag inconsistency (`ROADMAP.md` P3 — Azure audit
   bypasses the trait's `has_audit` flag via a legacy Activity Log path in
   `system_ops.rs`, so the flag is a lie for Azure) is a concrete, already-known
   symptom of partial retirement; full retirement closes it rather than
   perpetuating it.
3. Trait extension (adding `VaultBackend`/`AuditBackend` sub-traits) is a
   bounded, additive change to the `Backend` trait surface — the deep
   interview's constraints round (R3) confirmed extending traits as needed is
   acceptable, so there is no architectural blocker to full retirement.

**Alternatives considered:**
- **Partial retirement** (retire only secret-resolution managers; keep
  `VaultManager`/legacy audit paths for Azure-only RBAC/audit/lifecycle
  operations indefinitely) — rejected: perpetuates the exact "two
  representations of the same concept" bug class ADR-1 targets, just moved to
  a different operation family; also leaves the `has_audit` capability-flag
  lie unresolved (`ROADMAP.md` § Security hardening → Backend ecosystem).

**Why chosen:** the deep interview's constraints round (R3) explicitly settled
on full retirement — "extend trait/sub-traits as needed" — over preserving
Azure-only managers. Given ADR-1 already commits to single representable
state for secret resolution, applying a different (partial) standard to
vault-lifecycle/audit/RBAC would be an inconsistent line to draw, and the
codebase already carries a documented, live symptom (the `has_audit` flag
inconsistency) of exactly the partial-retirement failure mode.

**Consequences:** `Backend` gains two new optional sub-traits (`VaultBackend`,
`AuditBackend`); Azure's Activity-Log-based audit path is migrated onto
`AuditBackend` (closing the `has_audit` flag inconsistency as a side effect);
share/RBAC operations (`vault/operations.rs`) move behind `VaultBackend`;
manager-construction chokepoints (`config_ops.rs:898,928`; `system_ops.rs:594,1038`)
and `get_azure_auth_provider` are deleted once no caller remains. This is a
larger Phase 2 surface than a partial retirement would have been, traded for
eliminating a second known bug class instead of merely relocating it.

**Follow-ups:** the `VaultBackend`/`AuditBackend` trait shapes are Phase 2
design work, not pinned by this document; append new backend read-surface
entries to `backend-trait-checklist.md` as Phase 2 lands, per its existing
soft-commitment convention.

## Deferred non-goals (all phases)

- Multi-instance same-kind backends (e.g. two Azure tenants via a
  `NamedBackendEntry::Azure` variant) — deferred at the deep interview's Round
  0 topology gate; not needed yet.
- Union file views and alias-qualified file addressing
  (`xv file get azure-prod:x`) — Phase 3 stays default-entry-only, per the
  2026-07-04 design's existing "file ops stay single-vault as today" scope.
- Cross-vault file operations.
- Byte-for-byte legacy output/exit-code parity — the compat license is
  "break what needs breaking, document every break in `CHANGELOG.md`," not
  behavioral preservation.
- New backends (GCP Secret Manager, HashiCorp Vault, 1Password CLI bridge) —
  tracked separately under `ROADMAP.md` § Backend ecosystem, unaffected by
  this convergence.

## Verification (Phase 1)

```bash
# Seam collapsed: no `else`/active_arc in resolve_workspace_or_default
sed -n '/pub(crate) async fn resolve_workspace_or_default/,/^}/p' src/cli/helpers.rs \
  | grep -n 'else\|active_arc' && echo FAIL || echo OK

# resolve_workspace never returns None
sed -n '/pub async fn resolve_workspace(/,/^}/p' src/workspace/mod.rs \
  | grep -n 'Ok(None)\|return None' && echo FAIL || echo OK

# No un-rewritten presence-gates
grep -rn 'resolve_workspace(.*).*is_some()' src/cli/ src/tui/    # expect: none

# Survivor allowlist: every surviving legacy call site is annotated
grep -rn 'resolve_vault_name\|get_azure_auth_provider' src/cli/
grep -rBn '// Phase [23]' src/cli/*.rs | grep -E 'resolve_vault_name|get_azure_auth_provider'

cargo build && cargo test && cargo clippy --all-targets
```

See `.omc/plans/ralplan-xv-multibackend-roadmap.md` § 6 for the full
verification command set including behavioral/isolated-env checks.
