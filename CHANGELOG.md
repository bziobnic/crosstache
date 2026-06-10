# Changelog

## Unreleased

### Fixed

- **Local backend: soft-delete trash collisions (P2, data loss)** — Trash entries are now keyed by `<encoded_name>@<unix-millis>` instead of name alone, so `xv delete <X>`, recreate, delete again no longer clobbers previously trashed material. A same-name+same-timestamp collision is rejected with a clear error instead of overwriting. Recover restores the most recent trash entry; legacy un-suffixed trash entries from older versions remain listable and recoverable; purge removes all trash snapshots for a name.
- **Env export escaping** — `xv vault export --format env` now emits POSIX single-quoted values (`KEY='val'`, embedded single quotes escaped as `'\''`), so values containing newlines, `#`, `$`, quotes, spaces, or backslashes survive shell `source`/`eval` byte-for-byte. Secrets whose derived env name is not a valid shell identifier are skipped with a warning on stderr.
- **`--stdin` now preserves secret bytes exactly** (`xv set --stdin`, `xv update --stdin`): values read from stdin are stored byte-for-byte as piped — trailing newlines and leading/trailing whitespace are no longer stripped. Previously values were silently whitespace-trimmed, corrupting secrets where exact bytes matter (e.g. PEM keys, values whose consumers expect a trailing `\n`). Pass the new `--trim` flag (requires `--stdin`) to restore the old behavior of stripping leading/trailing whitespace. Empty stdin input is still rejected. (ROADMAP P3 — "`--stdin` trims whitespace")

---

## v0.11.1 — Security fixes (2026-05-28 security review)

All 10 findings from `docs/security-review-2026-05-28.md` resolved in **#232**.

### Security

- **Critical** — xfunction: a vault without a `CreatedByID` tag is now refused (403) instead of proceeding to Owner/Key Vault Administrator role assignment.
- **High** — `xv upgrade` refuses to install a release that has no `.minisig` signature asset (previously warn-and-continue). All releases since v0.11.0 are signed in CI.
- **High** — `install.sh` / `install.ps1` abort on every checksum-verification failure path (missing/empty checksum file, no checksum utility, download failure).
- **High** — xfunction: storage RBAC discovery no longer falls back to *all* storage accounts in the resource group; accounts without an explicit `AssociatedVault` tag or naming-convention match are skipped.
- **High** — xfunction: `EXPECTED_AUDIENCE` and issuer configuration are required for JWT validation; tokens are never validated without audience+issuer checks. `setup-app-registration.ps1` now sets `EXPECTED_AUDIENCE`.
- **Medium** — Recursive blob download routes through `safe_join`, rejecting absolute blob names that previously escaped the output directory.
- **Medium** — `xv run` only resolves `xv://` references from parent environment variables when `--inherit-env` is active, closing an `env_clear` isolation bypass.
- **Medium** — Local age key files are opened with `O_NOFOLLOW`, group/world-accessible key files are rejected (with a `chmod 600` hint), the stat→read TOCTOU window is closed, and key material is read into a `Zeroizing` buffer.
- **Medium** — `setup-app-registration.ps1` no longer prints the client secret to the console.
- **Low** — Table and plain output visibly escape control characters (C0/DEL/C1) in untrusted content (blob names, metadata, tags); JSON/YAML/CSV output remains raw for scripts.

### Breaking / behavioral notes

- Pre-existing local-backend key files with permissions looser than 0600 are now rejected at load; run `chmod 600 <key-file>` to fix.
- xfunction deployments must set `EXPECTED_AUDIENCE`; untagged vaults no longer receive role assignments.

---

## v0.11.0 — Security hardening + dependency triage

### Security (P2 items from GPT-5.5 review)

- **#222** — Local file metadata now written with 0600 permissions via `write_private`; permissions asserted in tests.
- **#223** — Traversal guard added to single-file blob download; multi-download `--output` collision check enforced via shared containment helper.
- **#224** — Scanner `SecretRef.value` wrapped in `Zeroizing<String>` end-to-end; engine dropped promptly after use.
- **#225** — Every segment in ARM resource ID construction is URL-encoded; wrong-path addressing via malformed names is prevented.

### Dependencies

- `ratatui` bumped `0.28` → `0.30`; transitively updates `lru` `0.12.5` → `0.16.4` (clears Dependabot alert #2).
- 4 remaining Dependabot alerts triaged: #17, #8, #9 are dev-only (`aws-sdk-secretsmanager` `test-util` feature, not in shipped binary); #11 (`rand 0.7.3`) is pinned by `azure_core 0.21` and not exploitable without a custom logger.

---

## v0.10.0 — AWS Secrets Manager backend

_Release candidate: v0.10.0-rc.1 (rc soak in progress)_

### Added

- **AWS Secrets Manager backend** (`xv --backend aws ...`) behind the `aws` Cargo feature flag.
  - `[aws]` config block: `region`, `profile`, `endpoint_url`, `default_vault`.
  - `[named_backends.*]` map for multi-region setups (e.g., `aws-east`, `aws-west`).
  - Prefix-based virtual vaults via `<vault>/.xv-vault` marker secrets.
  - Full secrets CRUD: create, get, list, update, delete (soft), purge (force), restore.
  - Version history: list versions, get by version ID, rollback.
  - Group, folder, note, expiry, content-type — all preserved via tags.
- **`--aws-profile` and `--region` global CLI flags** (override config file per invocation).
- **`xv init` wizard** now offers "AWS Secrets Manager" as a backend option.
- **`xv migrate` hardening** (marquee feature):
  - `--on-conflict skip|replace|fail` — conflict resolution strategy (replaces deprecated `--overwrite`).
  - `--concurrency N` — bounded parallel transfers (default 8).
  - `--force-replace` — ignore idempotency tags, always overwrite.
  - Pre-flight diff and summary table before any writes.
  - Idempotent re-runs via `xv:migrated_from` / `xv:migrated_at` tags.
  - Exponential backoff on throttling (`BackendError::RateLimited`).
- **Documentation**: `docs/migration.md` — full cross-cloud migration guide.
- **Test coverage**: hermetic mock tests (aws-smithy-mocks), LocalStack-gated integration tests, migration round-trip tests, CLI dry-run smoke test.

### Changed

- `--overwrite` on `xv migrate` is deprecated; use `--on-conflict replace` instead. The flag still works with a deprecation warning for one minor version.

### Capabilities matrix (AWS backend)

| Feature | Status |
|---|---|
| Secrets CRUD | ✅ |
| Versioning (list, get, rollback) | ✅ |
| Soft-delete + restore + purge | ✅ |
| Vaults (prefix-based) | ✅ |
| Groups, folders, notes, expiry | ✅ (via tags) |
| `xv share` (RBAC) | ❌ Use AWS IAM directly |
| `xv audit` | ❌ Use AWS CloudTrail |
| Native rotation | ❌ `xv rotate` writes new versions |
| File storage (S3) | ❌ Deferred to future phase |

### Performance notes

- Binary size with `--features aws`: ~19 MB (stripped, LTO). Default binary (no AWS): ~11 MB.
- 100-secret cross-cloud migration completes in <60 s on a warm credential cache at `--concurrency 8`.

### Upgrade notes

- Existing Azure or local users: **no action required**. Default behavior is unchanged.
- New AWS users: run `xv init` and pick "AWS Secrets Manager", or set `backend = "aws"` in `~/.config/xv/xv.conf`.
