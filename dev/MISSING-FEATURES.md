# Missing Features & Technical Debt

> Last reviewed: 2026-03-22 | Codebase version: **v0.5.2**

Comprehensive audit of missing features, stubs, bugs, and technical debt across the crosstache codebase. Line numbers drift as the code changes — treat them as approximate.

**Recent fixes (v0.5.0–v0.5.1):** CLI decomposition complete (`commands.rs` reduced from ~9,656 to ~1,301 lines); `xv upgrade` self-update command added; secret rename empty value fixed; `parse_iso_datetime` unreachable branch fixed; `created_timestamp` hardcoded-to-0 fixed; `xv parse` bad format exit code fixed; vault import skip-on-missing-value fixed; `--progress`/`--stream`/`--metadata` flags removed from CLI. Re-validate line references below when editing code.

---

## P0 — Bugs (Incorrect Behavior)

### ~~1. Secret rename silently writes empty value~~ — fixed (v0.5.0)

### ~~2. `xv run` — masking path buffers full output in memory~~ — fixed (v0.5.1)
**File:** `src/cli/secret_ops.rs` (`execute_secret_run`, `stream_and_mask`)
The masking path now spawns the child with `Stdio::piped()` and streams stdout/stderr line-by-line via two `BufReader` threads, masking secrets per line before printing. `Command::output()` is no longer used. Fixed in commit `b96e046`.

### ~~3. `parse_iso_datetime` — unreachable branch~~ — fixed (v0.5.0)

### ~~4. `created_timestamp` hardcoded to 0~~ — fixed (v0.5.0)

### ~~5. `xv parse` — bad format returns exit code 0~~ — fixed (v0.5.0)

---

## P1 — High Priority (User-Facing Feature Gaps)

### 6. ~~File sync (`xv file sync`)~~ — implemented
**Files:** `src/cli/commands.rs` (`execute_file_sync`), `src/blob/sync.rs` (change detection helpers).

### ~~7. Blob metadata and tags silently dropped on upload~~ — fixed
**File:** `src/blob/manager.rs` (`upload_file`)
Metadata is now built as an `azure_core::request_options::Metadata` object and tags as `azure_storage_blobs::prelude::Tags` (via `From<HashMap<String,String>>`). Both are passed to `put_block_blob` via `.metadata()` and `.tags()` chaining in a single API call. The `tracing::warn!` stubs are gone.

### ~~8. Blob tags never retrieved in list/get operations~~ — fixed
**File:** `src/blob/manager.rs` (`list_files`, `list_files_hierarchical`, `get_file_info`)
`list_files` and `list_files_hierarchical` now pass `.include_tags(true)` to the list builder so tags are returned inline with each blob (no extra round-trips). `get_file_info` calls `blob_client.get_tags().await` since `get_properties` does not return tags. All three extract tags into `HashMap<String, String>` via `Tags::into()`.

### ~~9. `--progress` flag on file upload~~ — removed (v0.5.0, flags removed from CLI)

### ~~10. `--stream` flag on file download~~ — removed (v0.5.0, flags removed from CLI)

### ~~11. `--metadata` flag on file list~~ — removed (v0.5.0, flags removed from CLI)

### ~~12. Large file upload is a stub~~ — fixed (v0.5.2)
**File:** `src/blob/manager.rs` (`upload_large_file`)
Implemented real Azure block blob upload: reads the file in `chunk_size_mb`-sized chunks, stages each with `put_block`, commits the full list with `put_block_list`, then fetches blob properties for an accurate `FileInfo`. `BlobManager` now carries `chunk_size_mb` and `max_concurrent_uploads` (populated from `BlobConfig` via the `with_blob_config` builder) and a `tokio::sync::Semaphore` bounds concurrency to `max_concurrent_uploads` in-flight requests. Unit tests for the `generate_block_id` and `read_chunk` helpers are in `src/blob/manager.rs`.

### ~~13. `--open` flag on file download is a no-op~~ — fixed (v0.5.1)
Added `opener = "0.7"` dependency and replaced the no-op path-print with `opener::open(&path)` in `src/cli/file_ops.rs`.

### ~~14. Vault list missing pagination~~ — fixed (v0.5.1)
**File:** `src/vault/operations.rs`
`list_vaults` now follows Azure ARM `nextLink` pagination for large subscriptions, matching the pattern used by `list_secrets` in `secret/manager.rs`.

### ~~15. Template output format (`--format template`) not implemented~~ — fixed
**File:** `src/utils/format.rs`
Implemented `{{field_name}}` substitution (case-insensitive, whitespace-tolerant) via `format_as_template`. Requires `--template` flag with a format string, e.g. `--format template --template 'export {{Name}}={{Value}}'`. Unknown placeholders are left as-is. Full unit test coverage added.

### ~~16. `--resource-group` flag missing from audit command~~ — fixed
The `Audit` command now exposes `--resource-group` (see `Commands::Audit` in `src/cli/commands.rs`). Remove this entry after the next full audit pass.

### ~~17. Managed Identity credential priority is a no-op~~ — fixed
**File:** `src/auth/provider.rs`
`AzureCredentialType::ManagedIdentity` now uses `AppServiceManagedIdentityCredential` (when `IDENTITY_ENDPOINT` is set) or `VirtualMachineManagedIdentityCredential` (IMDS) instead of `DefaultAzureCredential`. No CLI or environment-variable fallback — fails fast if not running in an Azure-managed environment.

---

## P2 — Medium Priority (Quality & Robustness)

### ~~18. `get_secret_info` — `recovery_level` and `version_count` always `None`~~ — fixed
**File:** `src/secret/manager.rs`
`version_count` now calls `get_secret_versions().await.map(|v| v.len())`. `recovery_level` was already extracted from `attributes.recoveryLevel`.

### ~~19. Vault restore returns fabricated properties~~ — fixed
**File:** `src/vault/operations.rs`
`restore_vault` now parses the Azure ARM response body (200 OK returns vault JSON). Falls back to a best-effort struct only if the body is empty.

### ~~20. `parse_access_policy` — `user_email` never resolved~~ — fixed
**File:** `src/vault/operations.rs`
`get_vault` now calls `resolve_principal_ids` after parsing access policies, populating `user_email` for each policy from the Graph API response.

### ~~21. Vault share commands don't check RBAC mode~~ — fixed
**File:** `src/cli/vault_ops.rs`
Added `check_vault_rbac_mode` helper; called at the start of Grant, Revoke, and List arms. Returns a clear error if the vault uses access policy authorization mode.

### ~~22. Config init shells out to Azure CLI~~ — fixed
**File:** `src/config/init.rs`
All three `az` subprocess calls now handle `ErrorKind::NotFound` with a helpful message directing users to install Azure CLI or create the storage account manually.

### ~~23. `config load_from_file` silently discards TOML parse errors~~ — fixed
**File:** `src/config/settings.rs`
TOML error is now captured; when both TOML and JSON parsing fail, the TOML error is surfaced in the returned `CrosstacheError::config` message.

### ~~24. Vault import doesn't support `txt` format~~ — fixed
**File:** `src/cli/vault_ops.rs`
Added `"txt"` branch to `execute_vault_import` parsing `KEY=VALUE` or `KEY: VALUE` lines, consistent with the txt export format.

### ~~25. `resolve_and_filter_roles` silently swallows errors~~ — fixed
**Files:** `src/vault/manager.rs`, `src/cli/vault_ops.rs`, `src/cli/secret_ops.rs`
Method signature changed to `async fn ... -> crate::error::Result<()>`. All callers now use `.await?` to propagate errors.

### ~~26. Env pull only supports `dotenv` format~~ — fixed
**File:** `src/cli/config_ops.rs`
`execute_env_pull` now accepts `OutputFormat` enum and emits JSON array, YAML array, CSV, or dotenv output depending on the selected format.

### ~~27. `VaultShareCommands::List` format is still a loose `String`~~ — fixed
**File:** `src/cli/commands.rs`, `src/cli/vault_ops.rs`
`--format` field changed from `String` to `OutputFormat` enum. `execute_vault_share` List arm uses the enum directly with no ad hoc string matching.

### ~~28. Hardcoded emojis bypass `--no-color`/pipe detection~~ — fixed (v0.5.0–v0.5.2)
Emoji output was removed during prior CLI decomposition; no bare `println!` emoji calls remain in source.

### ~~29. `main.rs` error handler catch-all is too broad~~ — fixed
**File:** `src/main.rs`
Added specific `match` arms for `IoError`, `JsonError`, `HttpError`, `UuidError`, and `RegexError` with user-friendly messages and actionable hints.

### ~~30. Azure Identity error conversion not implemented~~ — fixed
**File:** `src/error.rs`
`azure_identity` v0.21 errors surface as `azure_core::Error`, which is already handled by the existing `From<azure_core::Error>` impl. TODO replaced with an explanatory comment.

### ~~31. Missing `secret_not_found()` constructor helper~~ — fixed (was already present)
**File:** `src/error.rs`
`secret_not_found()` constructor already existed at line 92. Marked resolved.

---

## P3 — Low Priority (Dead Code, Polish, Tech Debt)

### ~~32. Paginated blob listing always returns empty~~ — fixed
**File:** `src/blob/operations.rs` (deleted)
`list_files_paginated` and `upload_file_with_progress` (and all other dead extension methods) removed along with `src/blob/operations.rs`. `pub mod operations` removed from `blob/mod.rs`.

### ~~33. `upload_file_with_progress` — fake progress callback~~ — fixed (see #32)

### ~~34. `Config.function_app_url` — never used~~ — fixed
Removed field from `Config` struct, `Default` impl, `load_from_env`, `config_ops.rs` (display, set handler, help string), and `init.rs` struct literal.

### ~~35. `Config.cache_ttl` — never used~~ — outdated
**File:** `src/config/settings.rs` (`cache_ttl_secs`)
TTL is read from config/env and passed into `CacheManager::from_config` (`src/cache/manager.rs`). Remove this entry after the next full audit pass.

### ~~36. `_collect_files_recursive` — dead code~~ — already removed
Already removed during CLI decomposition; not present in codebase.

### ~~37. `EnvironmentProfileManager::current_profile()` — dead code~~ — already removed
The dead `current_profile()` method was already removed during CLI decomposition; `current_profile` is now a field used throughout `config_ops.rs`.

### ~~38. `VaultStatus` enum — dead code~~ — already removed
Not present in `vault/models.rs`; removed in a prior cleanup pass.

### ~~39. Dead model scaffolding~~ — fixed
- `RoleDefinition`, `RolePermission`, `RoleAssignmentRequest` removed from `vault/models.rs`
- `SecretGroup` struct and `get_secrets_by_group` method removed from `secret/manager.rs`
- `NameMapping`/`NameMappingStats` and `SecretManagerBuilder` were already absent

### ~~40. Dead auth infrastructure~~ — fixed
`ClientSecretProvider` and `AuthProviderFactory` removed from `auth/provider.rs`. `sign_out()` and `get_client_id()` removed from the `AzureAuthProvider` trait and `DefaultAzureCredentialProvider`. Unused imports (`ClientSecretCredential`, `HashMap`) cleaned up.

### ~~41. Dead blob infrastructure~~ — fixed
- `create_context_aware_blob_manager` removed from `blob/manager.rs`
- `output_path` and `stream` removed from `FileDownloadRequest`; `recursive` removed from `FileListRequest` in `blob/models.rs`; all struct literals in `file_ops.rs` updated accordingly

### ~~42. `resolve_user_to_object_id` duplicated verbatim~~ — fixed (see #40)
`ClientSecretProvider` removed; only one implementation remains in `DefaultAzureCredentialProvider`.

### ~~43. Regex compiled without caching~~ — fixed
Added `static UUID_REGEX: std::sync::LazyLock<regex::Regex>` at module level; `resolve_user_to_object_id` now uses the cached static.

### ~~44. `is_valid_file_name` — trivially incomplete~~ — fixed
Removed `is_valid_file_name` from `src/utils/resource_detector.rs` and its tests.

### ~~45. `resource_group` creation timing gap in init~~ — fixed
In `run_interactive_setup`, the resource group is now created immediately after `configure_location` returns (before blob storage and vault steps), ensuring it exists even if vault creation is skipped.

---

## P4 — Backlog (Potential Panic Sites)

### ~~All P4 items~~ — fixed

| Location | Risk | Fix |
|----------|------|-----|
| `cli/commands.rs` (was 6738) | Medium | `ContextManager::new_global().unwrap()` — already using `?` after CLI decomposition (`config_ops.rs:515`) |
| `cli/commands.rs` (was 6858) | Low | `.current_vault().unwrap()` — already removed by CLI decomposition |
| `cli/commands.rs` (was 5698, 5715) | Low | `.unwrap()` on `.tags`/`.groups` — already removed by CLI decomposition |
| `config/context.rs` (was 120, 144) | Medium | `context_file.as_ref().unwrap()` — already using `if let` in current code |
| `secret/manager.rs` (was 382) | Low | `as_object().unwrap()` → `.as_object().expect("json!({}) always produces an Object")` |
| `secret/manager.rs` (was multiple) | Low | `"application/json".parse().unwrap()` → `HeaderValue::from_static(...)` |

---

## Summary

| Priority | Count | Open | Description |
|----------|-------|------|-------------|
| **P0** | 5 | 0 | Bugs — incorrect behavior, data loss, wrong exit codes |
| **P1** | ~11 | 0 | User-facing feature gaps — stubs, no-op flags |
| **P2** | 14 | 0 | Quality & robustness — error handling, silent failures, missing checks |
| **P3** | ~13 | 0 | Dead code, polish, tech debt |
| **P4** | 5 | 0 | Potential panic sites in production code |
| **Total** | ~48 | 0 | All items fixed or confirmed removed as of v0.5.3 |

> **Note:** Items marked ~~fixed~~ or ~~removed~~ should be deleted on the next full audit pass. File paths updated for CLI decomposition (commands moved to `secret_ops.rs`, `system_ops.rs`, `vault_ops.rs`, etc.).
es, missing checks |
| **P3** | ~13 | ~12 | Dead code, polish, tech debt |
| **P4** | 5 | 5 | Potential panic sites in production code |
| **Total** | ~48 | ~17 | Strikethrough items fixed or removed in v0.5.0–v0.5.2 |

> **Note:** Items marked ~~fixed~~ or ~~removed~~ should be deleted on the next full audit pass. File paths updated for CLI decomposition (commands moved to `secret_ops.rs`, `system_ops.rs`, `vault_ops.rs`, etc.).
