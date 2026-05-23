# Crosstache Roadmap

> **Last reviewed:** 2026-05-23 · **Current version:** `v0.10.0-rc.2` · **Branch protection:** `main` (all changes via PR)

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

### AWS backend → GA (v0.10.0)
`v0.10.0-rc.2` is currently soaking. Once soak completes, cut `v0.10.0`. No
new feature work in this lane — only blockers found during rc soak.

---

## Near-term (v0.11.x)

### P0 — UX: three "environment" concepts collide
Source: `docs/UX-REVIEW.md` §P0-1. `xv env` (legacy `.env` push/pull),
`.xv.toml` project envs, and `vault context` all read as "environments." Pick a
single user-facing word and rename/alias the other two. Until then, every
onboarding doc has to over-explain.

### P0 — AWS auth failures surface as network timeouts
Source: `docs/UX-REVIEW.md` §P0-2. Missing/invalid AWS credentials produce
generic `reqwest` timeouts instead of a "credentials not configured" message
with the standard remediation pointers (`aws configure`, `AWS_PROFILE`, IAM
role chain). Partially addressed in `#206` but needs verification across all
AWS code paths (list, get, migrate).

### P0 — Default build silently cannot use AWS
Source: `docs/UX-REVIEW.md` §P0-3. A binary built without `--features aws`
accepts `--backend aws` flags and only fails deep in the call stack with an
opaque error. Detect feature absence early; print a one-line "rebuild with
`--features aws`" hint (partially addressed in `#206`; verify).

### P1 — `.xv.toml` activation is invisible
Source: `docs/UX-REVIEW.md` §P1-1, §P1-2. Read-only discovery commands fail
before showing which `.xv.toml` profile is active, and there's no
`xv env activate` command. Add an "active profile" line to `xv whoami` /
`xv context show`, and either an activation subcommand or a documented
walk-up resolution flow surfaced in `--help`.

### P1 — Backend selection diagnostics
Source: `docs/UX-REVIEW.md` §P1-3. Backend can come from global config,
`.xv.toml`, env vars, or `--backend`. Add `xv config show --resolved` (or
similar) that prints which source won, with precedence rules.

### P1 — `xv context init` cannot create AWS or local profiles
Source: `docs/UX-REVIEW.md` §P1-4. Wizard is still Azure-only despite AWS
being shipped. Add backend selection to the init flow.

---

## Security hardening

Sourced from `docs/code-review-gpt55.md` (GPT-5.5 code review, 2026-05-09).
Each item names the source file at review time — verify line numbers before
fixing as code drifts.

### P1 — Path-traversal & URL-injection via unvalidated names
- **Azure vault names** interpolated into Key Vault URLs without
  validation (`src/secret/manager.rs:273,383,476,430`). A vault name
  containing URL authority/path delimiters can redirect a bearer token to
  an attacker-controlled host. Introduce `ValidatedVaultName`
  (`^[a-zA-Z][a-zA-Z0-9-]{1,22}[a-zA-Z0-9]$`); build URLs with `url::Url`.
- **Local vault & secret names** used as raw filesystem path components
  (`src/backend/local/vaults.rs`, `secrets.rs`, `files.rs`). Names like
  `../../outside` escape the store. Validate component-only or
  encode + canonicalize + assert containment.
- **Cache keys** use raw `vault_name` (`src/cache/models.rs`,
  `cache/manager.rs`, `cache/refresh.rs`). Encode or hash; reject separators.

### P1 — `xv upgrade` signature verification
Source: `docs/superpowers/specs/2026-05-04-upgrade-signature-verification.md`.
Binary + checksum currently come from the same GitHub Releases endpoint
(integrity, not authenticity). Embed a minisign public key, sign release
archives in CI, verify before swap. Spec is design-approved but
implementation has not landed.

### P1 — Local secret writes not transactional
`src/backend/local/secrets.rs:300,571`. Archive-then-write loses the active
secret on encryption/metadata failure. Write to temp + fsync + atomic
rename; archive only after the replacement is durable.

### P1 — Symlink-following on sensitive writes
`src/utils/helpers.rs:21`, `src/backend/local/crypto.rs:38`. Use
`O_NOFOLLOW | O_CLOEXEC` + `create_new`, write through temp files in
trusted directories, verify `symlink_metadata` before writes.

### P2 — Local file metadata uses world-readable defaults
`src/backend/local/files.rs:57`. Switch to `write_private`; assert
permissions in tests.

### P2 — Single-file blob download lacks traversal guard
`src/cli/file_ops.rs:428`. Multi-download `--output` collisions
(`src/cli/file_ops.rs:1162`). Share the recursive containment helper;
require `--output` to be a directory for multi-downloads.

### P2 — Secret rename is non-atomic create + delete
`src/secret/manager.rs:1959`. Surface recovery plan on partial failure or
introduce a backend-level rename where APIs allow.

### P2 — ARM resource IDs not URL-encoded
`src/vault/operations.rs:133`. Lower-impact than the Azure URL issue
(fixed host), but still allows wrong-path addressing. URL-encode every
segment.

### P2 — Scanner secret values held as plain `String`
`src/scan/engine.rs:17`, `src/scan/orchestrator.rs:61`. Defeats the
project's `Zeroizing` posture. Store fetched values in
`Zeroizing<String>` end-to-end; drop the engine promptly.

### P2 — Blob downloads buffer entire blob in memory
`src/blob/manager.rs:393,568,654`. Stream to writer; bound chunk
concurrency; enforce configurable max file size.

### P2 — Local file backend captures vault at construction
`src/backend/local/files.rs:100`. Inconsistent with `SecretBackend`.
Either accept vault per call, or rebuild per operation from resolved vault.

### P2 — Azure backend exposes stub list_deleted / backup / restore_from_backup
`src/backend/azure/secrets.rs:204`. Either align capabilities with what's
implemented or finish the REST paths.

### P2 — Local soft-delete trash collisions
`src/backend/local/secrets.rs:514`. `xv delete <X>`, recreate, delete
again clobbers prior deleted material. Suffix trash entries with
deletion timestamp; reject on collision.

### P3 — `--stdin` trims whitespace
`src/cli/secret_ops.rs:51,724`. Corrupts secrets where trailing newlines
matter. Preserve bytes exactly by default; add explicit `--trim`.

### P3 — No tri-state expiry update
`src/cli/secret_ops.rs:723`, `src/backend/local/secrets.rs:611`. Can't
distinguish "leave unchanged" from "clear." Model as
`Unchanged | Set(T) | Clear` for expiry, not-before, note, folder.

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

### P3 — Env export emits unescaped `KEY=value`
`src/cli/vault_ops.rs:644`. POSIX single-quote escaping or
dotenv-quoted output; add tests for newlines, `#`, `$`, quotes.

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

From `docs/UX-REVIEW.md` (2026-05-16, v0.10.0-rc.2 baseline):

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
