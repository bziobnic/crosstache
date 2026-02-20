# Crosstache Roadmap — Prioritized by User Impact

> Updated: 2026-02-20

---

## Tier 1 — High Impact, Noticeable Gaps

These are features users will hit quickly and wonder why they don't work.

### 1. Recursive File Delete (`xv file delete -r`)
**Impact:** Users can upload directory trees but can't delete them without naming each file individually.
**Current state:** `xv file delete` only accepts explicit file names. No `-r`, `--dry-run`, or glob support.
**Effort:** Medium
**See:** `dev/DELETE_ROADMAP.md`

### 2. File Sync (`xv file sync`)
**Impact:** Sync is listed in `xv file --help` but prints "not yet implemented." Users expect it to work.
**Current state:** Fully stubbed.
**Effort:** Medium-High (need diffing logic, conflict resolution)
**Scope:** Start with one-way `sync up` and `sync down` before tackling bidirectional.

### 3. `xv vault update`
**Impact:** Command exists, accepts flags, but does nothing. Users trying to update vault properties (tags, purge protection, retention) get a silent no-op.
**Current state:** Parses all args, builds the request struct, then prints "not yet implemented."
**Effort:** Low-Medium (the request struct is ready, just needs the REST API call)

### 4. Vault & Secret Sharing (`xv vault share`, `xv share`)
**Impact:** RBAC commands exist in `--help` but all print TODO. Users setting up team access hit a wall.
**Current state:** CLI parsing done, execution prints "not yet implemented."
**Effort:** Medium (Azure RBAC API integration)

### 5. Progress Indicators for File Operations
**Impact:** Uploading/downloading large files or directories gives no feedback until completion. Feels broken on slow connections.
**Current state:** No progress bars, no per-file status during recursive ops.
**Effort:** Low-Medium (indicatif crate or similar)

---

## Tier 2 — Quality of Life Improvements

Features that make the daily experience significantly better.

### 6. Pagination for Large Vaults
**Impact:** Vaults with hundreds of secrets may only show the first page. Warning is printed but no way to get more.
**Current state:** `src/secret/manager.rs` prints "Pagination not yet implemented."
**Effort:** Low-Medium

### 7. Configurable Clipboard Timeout
**Impact:** 30-second auto-clear is hardcoded. Some users want longer/shorter, or to disable it.
**Current state:** Hardcoded `Duration::from_secs(30)`.
**Effort:** Low (read from config, pass to thread)

### 8. `xv diff` — Compare Secrets Across Vaults
**Impact:** Common need when promoting secrets between environments (dev → staging → prod).
**Current state:** Not implemented. Users manually `xv list` both vaults and compare.
**Effort:** Medium
**Scope:** Show added/removed/changed keys. Never show values by default.

### 9. Large File Chunked Upload
**Impact:** Files over ~100MB may fail or hang. Block-based upload with resume would fix this.
**Current state:** Stubbed in `src/blob/manager.rs`.
**Effort:** Medium

### 10. Interactive TUI (`xv tui`)
**Impact:** Browse vaults → secrets → values with keyboard navigation. Fuzzy search, quick copy. Very appealing for exploration.
**Current state:** Not started.
**Effort:** High (ratatui or similar)

---

## Tier 3 — Polish & Power Features

### 11. `xv info` for Vaults
**Impact:** `xv info <vault>` is listed but stubbed. Minor since `xv vault info` works.
**Effort:** Low (route to vault info)

### 12. Secret References / URI Scheme
**Impact:** Stable `xv://vault/secret` URIs usable in templates, env vars, and config files. Already partially supported in `xv inject`.
**Effort:** Medium (formalize the scheme, add to `xv run`)

### 13. Self-Update (`xv update`)
**Impact:** Convenient but not essential — users can re-download.
**Effort:** Low (self_update crate)

### 14. Plugin/Extension System
**Impact:** Community extensions without bloating core. Nice long-term but premature now.
**Effort:** High

### 15. Webhook Notifications
**Impact:** Notify external systems on secret changes. Niche use case.
**Effort:** Medium

---

## Tier 4 — Backend & Architecture

### 16. AWS Secrets Manager Backend
**Impact:** Opens the tool to the AWS ecosystem. Significant market expansion.
**Current state:** Architecture uses manager/provider patterns that could support this.
**Effort:** High (new backend module, credential handling, feature mapping)
**Prerequisite:** Abstract the backend trait boundary first.

### 17. HashiCorp Vault Backend
**Impact:** Popular in self-hosted/hybrid environments.
**Effort:** High

---

## Completed (v0.3.1)

✅ Secret CRUD, bulk set, groups, folders, notes, tags, expiry
✅ Secret injection (`xv run`) with output masking
✅ Template injection (`xv inject`) with cross-vault refs
✅ Secret history, rollback, rotation
✅ Vault CRUD, import/export
✅ Environment profiles, `.env` pull/push
✅ Cross-vault copy/move
✅ File upload/download (recursive, with structure preservation)
✅ Directory-style file listing
✅ Shell completion, whoami, audit
✅ Zeroize, clipboard auto-clear, file permissions, path traversal protection
