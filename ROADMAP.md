# Crosstache Roadmap

> **Last reviewed:** 2026-06-11 · **Latest released version:** `v0.11.1` · **Branch protection:** `main` (all changes via PR)

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

No active release-soak lane. Implemented-but-unreleased fixes are tracked in
[`CHANGELOG.md`](./CHANGELOG.md) under `Unreleased`; this roadmap only tracks
open work.

---

## Security hardening

Sourced from `docs/code-review-gpt55.md` (GPT-5.5 code review, 2026-05-09).
Each item names the source file at review time — verify line numbers before
fixing as code drifts.

All four P2 items from this review shipped on 2026-06-11 (#242 rename
recoverability, #243 blob download streaming, #244 per-call file vault
resolution, #245 Azure deleted/backup/restore REST paths). Remaining items
are P3 and below.

### P3 — Context files not written via private writer
`src/config/context.rs:193,348`. Treat as user-private config; share the
sensitive-file helper.

### P3 — `az` subprocess calls have no timeout
`src/backend/azure/auth.rs:155,366`. Centralize `az` execution with a
timeout and stderr cap.

### P3 — JWT payloads decoded without signature validation
`src/backend/azure/auth.rs:300`. Document boundary; prefer SDK metadata;
validate claim shapes.

### P3 — Age identity files not zeroized
`src/backend/local/crypto.rs:138,139`. Load into `Zeroizing<String>`;
open with no-follow and read from the file handle to close the TOCTOU
window.

### P3 — Cache lock TOCTOU
`src/cache/refresh.rs:77`. `OpenOptions::create_new(true)` with PID +
timestamp metadata for stale-lock diagnostics.

### P3 — Scanner reads whole files into RAM
`src/scan/orchestrator.rs:19`. Stream; enforce max file size; report
skipped files; fail-loud in CI/hook mode.

### P3 — `list_secrets` does N+1 `get_secret` for tags
`src/secret/manager.rs:802`. Use list-response metadata when sufficient;
batch with bounded concurrency + retry.

### P3 — `stream_and_mask` unbounded line buffer
`src/cli/secret_ops.rs:2220`. Bounded chunked masking with overlap =
longest secret length.

### P3 — CSV output manually assembled
`src/utils/format.rs:174`. Use the `csv` crate.

### P3 — Local metadata plaintext disclosure
`src/backend/local/secrets.rs:146`. Document the limitation in `init`
and docs; opt-in encrypted metadata index.

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

| Feature           | AWS status today                                       | Next step                                                                    |
| ----------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------- |
| `xv share` (RBAC) | ❌ Use AWS IAM directly                                | Add capability-aware hint with `aws iam` example; longer term, wrapper UX.   |
| `xv audit`        | ❌ Use AWS CloudTrail                                  | Read recent CloudTrail events for the vault; mirror Azure Activity Log UX.   |
| Native rotation   | ❌ `xv rotate` writes new versions                     | Optional integration with Secrets Manager rotation Lambdas.                  |
| File storage (S3) | ❌ Deferred                                            | Mirror Azure Blob backend; same containment fixes as local file backend.    |

### P2 — Local backend metadata encryption (opt-in)
Source: `docs/reviews/2026-05-03-ux-review.md` §3 (since absorbed) and
code-review P3 item above. Provide an opt-in encrypted index mode or, at
minimum, a clear warning in `init` + docs.

### P3 — Additional backends
Open ground from `2026-04-29-strategic-improvements-phase-1-design.md`:
- GCP Secret Manager
- HashiCorp Vault (KV v2)
- 1Password CLI bridge

Each new backend appends to `docs/superpowers/specs/backend-trait-checklist.md`.

---

## UX & docs polish

From `docs/UX-REVIEW.md` (2026-05-16 AWS-backend baseline):

### P2
- **§P2-1 Top-level framing still says Azure-only.** README hero + `xv --help` intro need an AWS mention.
- **§P2-2 AWS flags appear on commands where they do nothing.** Hide `--aws-profile` / `--region` from commands that ignore them.
- **§P2-3 `.xv.toml` and `xv.conf` overlapping backend fields** have inconsistent naming. Pick one canonical key per concept.
- **§P2-4 `context envs` does not show the effective profile.** Include resolved backend + vault.
- **§P2-5 Backend-unsupported operations framed in Azure terms.** Use neutral language; surface the active backend in the error.

### P3
- **§P3-1 Help hides global options by default**, including the `.xv.toml` activation flag. Promote critical globals.
- **§P3-2 `xv env create` uses `--group`** where adjacent commands say `resource_group`. Align.
- **§P3-3 Generic AWS inherited flags are visually louder** than command-specific flags in `--help`. Reorder.
- **§P3-4 Build warnings on first source-install.** Sweep clippy/build warnings periodically.
- **§P3-5 The CLI doesn't surface the env-vs-profile-vs-context distinction at the moment of confusion** — only docs do. Add inline hints.

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
