# Crosstache Development Roadmap

> Consolidated from previous TODO.md and ROADMAP.md. Updated 2026-02-20.

---

## High Priority — Core Gaps

### Secret Management — Stubbed Operations
These exist in `src/secret/manager.rs` but return "not implemented":
- [ ] `list_deleted_secrets()` — List soft-deleted secrets
- [ ] `get_secret_versions()` — Get all versions of a secret (needed by `xv history`)
- [ ] `backup_secret()` / `restore_secret_from_backup()` — Secret backup/restore

### CLI Commands — Stubbed or Incomplete
- [ ] `execute_info_command()` — `xv info` for vault details (line ~1225)
- [ ] `execute_vault_update()` — `xv vault update` properties (line ~2419)
- [ ] Vault sharing commands (`xv vault share grant/revoke/list`) — print TODO
- [ ] File sync (`xv file sync`) — prints TODO

### Storage Account Management
Working: `create_storage_account()`, `create_blob_container()` (in `src/config/init.rs`)
Missing:
- [ ] `delete_storage_account()`
- [ ] `delete_container()`
- [ ] `list_storage_accounts()`

---

## Medium Priority — Security & Quality

### Security Audit Follow-ups (see `docs/SECURITY_AUDIT.md`)
- [ ] Remove secret fallback printing to stderr on clipboard failure
- [ ] Restrict config/export file permissions to 0600
- [ ] Auto-clear clipboard after timeout (30s default)
- [ ] Validate generator scripts for `xv rotate --generator`
- [ ] Restrict template output file permissions

### File Delete Enhancement (see `docs/DELETE_ROADMAP.md`)
- [ ] Add `-r, --recursive` flag for directory deletion
- [ ] Add `-i, --interactive` and `--dry-run` modes
- [ ] Glob pattern support

### Testing
- [ ] Integration tests for file operations
- [ ] Complete `tests/auth_tests.rs` (incomplete test at line 30)
- [ ] Windows path compatibility testing

---

## Low Priority — Enhancements

### Blob Operations
- [ ] Chunked upload for large files (>100MB)
- [ ] Progress callbacks for file operations
- [ ] Blob metadata/tag support (Azure SDK v0.21)
- [ ] Pagination for large result sets

### Output & Formatting
- [ ] Template engine for custom output
- [ ] CSV export

### Configuration
- [ ] Replace Azure CLI dependency with Azure Management API for init

---

## Completed (for reference)

- ✅ Zeroize secret values in memory (v0.3.0, PR #26)
- ✅ `file-ops` compilation flag (2026-02-16)
- ✅ Directory structure preservation for uploads (v0.2.0)
- ✅ Recursive download with structure preservation
- ✅ Directory-style file listing (hierarchical by default)
- ✅ Secret injection (`xv run`)
- ✅ Template injection (`xv inject`)
- ✅ Secret versioning (`xv history`, `xv rollback`)
- ✅ Secret rotation (`xv rotate`)
- ✅ Shell completion (`xv completion`)
- ✅ `xv whoami`
- ✅ Environment profiles (`xv env`)
- ✅ Audit log (`xv audit`)
- ✅ Bulk set (`xv set KEY1=val1 KEY2=val2`)
- ✅ Cross-vault copy/move (`xv copy`, `xv move`)
- ✅ `.env` file sync (`xv env pull/push`)
- ✅ Secret expiration (`--expires`, `--expiring`, `--expired`)
