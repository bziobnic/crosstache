# Crosstache Roadmap

> Updated: 2026-02-20 | Current version: **v0.4.1**

---

## ✅ Completed

Features shipped and verified in the codebase.

### Core Secret Management (v0.1–v0.3)
- Secret CRUD, bulk set (`xv set K1=v1 K2=v2`), groups, folders, notes, tags, expiry
- Secret injection (`xv run`) with output masking and env var cleanup after child spawn
- Template injection (`xv inject`) with cross-vault secret references
- Secret history, rollback, rotation (`xv history`, `xv rollback`, `xv rotate`)
- Vault CRUD, import/export
- Environment profiles (`.env` pull/push, `xv env`)
- Cross-vault copy/move (`xv copy`, `xv move`)
- Shell completion (`xv completion bash|zsh|fish|powershell`)
- `xv whoami` — authenticated identity and active context
- `xv audit` — access/change history from Azure Activity Log
- Secret expiration/TTL (`--expires`, `--expiring`)

### File Operations (v0.2–v0.3)
- File upload/download (recursive, with directory structure preservation)
- Directory-style file listing
- Recursive file delete with `-r`, `-f`, `-i`, `--dry-run`, `--verbose`, glob patterns

### Security Hardening (v0.3.0–v0.3.1)
- Zeroize secret values in memory after use
- Clipboard auto-clear after 30 seconds
- Sensitive files written with 0600 permissions (config, exports, template output)
- Generator script ownership + world-writable checks
- Path traversal protection in recursive download
- Env vars dropped after child process spawn

### v0.4.0 — Sharing, Diff, Pagination
- `xv vault update` — update vault properties (tags, purge protection, retention, deployment flags)
- `xv vault share grant/revoke/list` — RBAC-based vault access management
- `xv share grant/revoke/list` — secret-level sharing
- `xv diff <vault1> <vault2>` — compare secrets across vaults (shows added/removed/changed; values hidden by default, `--show-values` to reveal)
- Pagination for secret listing (follows Azure `nextLink`)
- Configurable select page size

### v0.4.1
- Bug fixes and release cleanup

---

## 🔜 Open — High Priority

### 1. File Sync (`xv file sync`)
**Impact:** Listed in `xv file --help` but prints "not yet implemented."
**Current state:** Fully stubbed — no sync logic exists.
**Effort:** Medium-High
**Scope:** Start with one-way `sync up` and `sync down` before bidirectional.

### 2. Configurable Clipboard Timeout
**Impact:** 30-second auto-clear is hardcoded. Users want longer/shorter or to disable it.
**Current state:** Hardcoded `Duration::from_secs(30)` in `commands.rs`.
**Effort:** Low — read from config, pass to thread.

### 3. Progress Indicators for File Operations
**Impact:** Large file uploads/downloads give no feedback until completion.
**Current state:** No progress bars or per-file status during recursive ops.
**Effort:** Low-Medium (`indicatif` crate or similar).

### 4. Large File Chunked Upload
**Impact:** Files over ~100MB may fail. Block-based upload with resume needed.
**Current state:** Stubbed in `src/blob/manager.rs` and `src/blob/operations.rs` (paginated listing also stubbed).
**Effort:** Medium

### 5. Output Format Support (JSON, YAML, CSV, Template)
**Impact:** `--format json|yaml|csv|template` flags exist but all return "not yet implemented."
**Current state:** Stubbed in `src/utils/format.rs`.
**Effort:** Low-Medium

---

## 🔜 Open — Medium Priority

### 6. Secret References / URI Scheme
**Impact:** Stable `xv://vault/secret` URIs for templates, env vars, and config files. Partially supported in `xv inject`.
**Effort:** Medium — formalize the scheme, integrate with `xv run`.

### 7. Interactive TUI (`xv tui`)
**Impact:** Browse vaults → secrets → values with keyboard navigation, fuzzy search, quick copy.
**Current state:** Not started.
**Effort:** High (`ratatui` or similar).

### 8. Blob Metadata & Tags
**Impact:** File metadata and tag setting stubbed but not functional (Azure SDK limitation noted).
**Current state:** `src/blob/manager.rs` logs warnings — "not yet implemented for Azure SDK v0.21."
**Effort:** Medium — depends on Azure SDK support.

---

## 🔜 Open — Low Priority / Nice to Have

### 9. Self-Update (`xv update`)
**Effort:** Low (`self_update` crate). Convenient but not essential.

### 10. Plugin / Extension System
**Effort:** High. Premature for current user base.

### 11. Webhook / Event Notifications
**Effort:** Medium. Niche use case — notify external systems on secret changes.

### 12. AWS Secrets Manager Backend
**Impact:** Opens tool to AWS ecosystem. Architecture supports it (manager/provider patterns).
**Effort:** High — new backend module, credential handling, feature mapping.
**Prerequisite:** Abstract the backend trait boundary first.

### 13. HashiCorp Vault Backend
**Impact:** Popular in self-hosted/hybrid environments.
**Effort:** High

---

## 🔧 Technical Debt & Security

### Security (from audit)

| # | Issue | Status | Priority |
|---|-------|--------|----------|
| 15 | Bearer tokens not zeroized (`format!("Bearer {}", token)`) | ❌ Open | Medium |
| 5 | Secrets visible in process env (`xv run`) | ⚠️ By design | Document it |
| 10 | 105 `unwrap()` calls in non-test code | ❌ Open | Low (robustness) |
| 12 | Secret names visible in process arguments | ⚠️ Inherent to CLI | Document it |

### Code Quality
- `unwrap()` cleanup — gradual replacement with proper error handling
- Blob operations: paginated listing stub in `src/blob/operations.rs` needs real implementation
- Azure Management API integration stub in `src/config/init.rs`

---

## 🎨 UX Improvements (Backlog)

These are quality-of-life ideas — not committed, but worth considering:

- **Interactive setup** (`xv init`) — guided wizard with Azure CLI auto-detection
- **Smart vault context** — per-directory `.xv` config, `xv vault use` context switching
- **Better error messages** — context-aware suggestions (e.g., "Vault not found → try `xv vault list`")
- **Command aliases** — `xv ls`, `xv get`, `xv set` as shortcuts
- **Fuzzy search** — `xv find "database"` across all secrets
- **Tree view** — `xv tree` for folder/group visualization

---

## Delete Command Status

The `xv file delete` enhancement is **largely complete**. Implemented:

- ✅ `-r`, `-f`, `-i`, `--verbose`, `--dry-run` flags
- ✅ Path analysis (file/directory/glob/empty-dir-marker)
- ✅ Recursive delete with confirmation prompts
- ✅ Glob pattern expansion
- ✅ Interactive mode (per-item prompts)
- ✅ Batch deletion via `BlobManager`
- ✅ Progress reporting and summaries

Still open:
- ❌ Double-confirmation for large deletions (>10 files or >100MB)
- ❌ Unit and integration tests
- ❌ Empty directory marker edge cases (cleanup/recreation)
- ❌ Performance optimization (concurrent deletion, caching)
- ❌ Documentation updates (README examples)
