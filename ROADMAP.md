# Crosstache Roadmap

> **Last reviewed:** 2026-06-15 · **Latest released version:** `v0.15.0` · **Branch protection:** `main` (all changes via PR)

Single source of truth for **unimplemented** ideas, deferred work, and known
limitations worth fixing. Anything already shipped lives in [`CHANGELOG.md`](./CHANGELOG.md).
Implementation history for individual features lives in the dated specs under
`docs/superpowers/specs/` — each one is tagged with the version that shipped it.

Severity legend (mirrors the UX/code reviews):
- **P0** — blocks core flows / data-loss / security
- **P1** — high user-pain, ships next minor
- **P2** — medium friction
- **P3** — polish / nice-to-have

---

## In flight

No active release-soak lane. Implemented work is tracked in
[`CHANGELOG.md`](./CHANGELOG.md); this roadmap only tracks open work.

---

## Security hardening

Sourced from `docs/code-review-gpt55.md` (GPT-5.5 code review, 2026-05-09).
Each item names the source file at review time — verify line numbers before
fixing as code drifts.

All four P2 items from this review shipped on 2026-06-11 (#242 rename
recoverability, #243 blob download streaming, #244 per-call file vault
resolution, #245 Azure deleted/backup/restore REST paths). Several P3
hardening items shipped in v0.14.0; see [Shipped history](#shipped-history).
Remaining items are P3 and below.

### P3 — Age identity files not zeroized
`src/backend/local/crypto.rs:138,139`. Load into `Zeroizing<String>`;
open with no-follow and read from the file handle to close the TOCTOU
window.

### P3 — CSV output manually assembled
`src/utils/format.rs:174`. Use the `csv` crate.

### P3 — Missing serialization guards for value-like fields
`src/error.rs:637`. Extend the existing error-variant guard to cover
cache entries, scan findings, structured output, logs, tracing.

### P4 — Code-quality polish
Deduplicate Azure secret response parsing
(`src/secret/manager.rs:493`); update stale "placeholder" comments in
`src/blob/manager.rs:6`; refresh Azure SDK version comments
(`src/secret/manager.rs:382`); make `path_to_blob_name` return
`Result` instead of silently normalizing
(`src/cli/file_ops.rs:814`); replace `.expect(...)` with `is_some_and`
(`src/secret/manager.rs:418`); skip `xv://` env scan when `inherit_env`
is false (`src/secret/manager.rs:2020`); keep TUI clipboard state
`Zeroizing` (`src/tui/update.rs:142`); add safety comment to the
SIGPIPE `unsafe` block (`src/main.rs:170`); surface corrupted version
listings (`src/backend/local/secrets.rs:651`); add adversarial tests
for traversal/symlink/rollback/duplicate-trash
(`src/backend/local/secrets.rs:861`); cover single-file and sync
download with traversal tests (`src/cli/file_ops.rs:1203`); replace
regex-only entropy fallback with real entropy or label as
low-confidence (`src/scan/patterns.rs:62`).

---

## Backend ecosystem

### P1 — AWS capability matrix gaps (deferred from v0.10.0)
Source: `CHANGELOG.md` § AWS capabilities matrix.

All four gaps shipped in **v0.12.0** (2026-06-12, #248–#251). Retained here
as history; current AWS capability state lives in `CHANGELOG.md`.

| Feature           | AWS status                                             | Shipped                                                                       |
| ----------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------- |
| `xv share` (RBAC) | ✅ Capability-aware hint with `aws secretsmanager put-resource-policy` example | v0.12.0 (#248) |
| `xv audit`        | ✅ Reads CloudTrail `LookupEvents`, mirrors Azure Activity Log UX | v0.12.0 (#249) |
| Native rotation   | ✅ `xv rotate --native` invokes Secrets Manager `RotateSecret` (Lambda) | v0.12.0 (#250) |
| File storage (S3) | ✅ `xv file` on S3, vault-prefixed, streaming + containment | v0.12.0 (#251) |

### P3 — `has_audit` capability flag is inconsistent across audit backends
Surfaced during the v0.12.0 audit work (#249). AWS audit dispatches through
the `AuditBackend` trait (`registry.active().audit()`), so AWS correctly sets
`has_audit: true`. Azure audit still uses a **legacy Activity Log path** in
`src/cli/system_ops.rs` that bypasses the capability system entirely, so
`xv audit` works on Azure while the Azure backend reports `has_audit: false`.
Harmless today (the CLI tries the trait first, then falls through to the Azure
path), but the flag is a lie for Azure. Fix: either migrate Azure audit onto
the trait and flip `has_audit: true`, or document the flag as "trait-dispatch
only" so capability introspection isn't misleading.

### P3 — Additional backends
Open ground from `2026-04-29-strategic-improvements-phase-1-design.md`:
- GCP Secret Manager
- HashiCorp Vault (KV v2)
- 1Password CLI bridge

Each new backend appends to `docs/superpowers/specs/backend-trait-checklist.md`.

---

## Shipped history

- **Local secret names disclosed via filenames** — closed in v0.15.0 by
  opaque local-backend filenames in #276. The retained design plan is
  [`docs/plans/2026-06-19-local-secret-filename-opaquing.md`](./docs/plans/2026-06-19-local-secret-filename-opaquing.md);
  release notes live in [`CHANGELOG.md`](./CHANGELOG.md) under `v0.15.0`.

---

## UX & docs polish

From `docs/UX-REVIEW.md` (2026-05-16 AWS-backend baseline).

The full P2 lane and P3-1..4 shipped post-v0.12.0 (#254 §P2-1/§P2-5,
#255 §P2-2, #256 §P2-3/§P2-4, #257 §P3-4, #258 §P3-1..3). They are
recorded in [`CHANGELOG.md`](./CHANGELOG.md) under `v0.13.0`. §P3-5 is
also addressed in unreleased CLI output by inline hints on
`config show --resolved`, `context show`, and `context envs` that explain
env profile vs vault context vs global config precedence where users see the
resolved values.

No substantive UX review items remain open.

---

## Discarded / superseded

These ideas are *not* on the roadmap; recording for traceability:

- **`bd`/`beads` issue tracking** — per `AGENTS.md`, out-of-band, do not reintroduce.
- **`--progress` / `--stream` / `--metadata` flags on file ops** — removed in v0.5.0; functionality replaced by built-in progress indicators (v0.7.3) and streaming defaults.
- **`Config.cache_ttl` and `Config.function_app_url`** — never used, removed during cleanup.
- **`bd` integration plans, output-consistency redesign, README audit, e2e test fixes, list-pagination plan, output-consistency design** — all shipped; plans removed in the 2026-05-23 docs sweep.

---

## How to read this file

- Items here are **not yet implemented**. If you find one that's actually shipped, file a PR moving it to `CHANGELOG.md` and updating the matching spec banner under `docs/superpowers/specs/`.
- Severity is a rough triage signal, not a deadline. Re-rank as priorities shift.
- New feature ideas go here first (a one-paragraph sketch is fine). Promote to a full spec under `docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md` once design is converging.
- When a spec ships, add a banner at the top:
  `> **Status:** ✅ Implemented in **vX.Y.Z** (YYYY-MM-DD). Retained as design history.`
