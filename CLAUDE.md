# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

crosstache is a cross-platform secrets manager CLI written in Rust. The binary is named `xv`. Currently backed by Azure Key Vault, with the architecture intended to support additional backends (AWS Secrets Manager, etc.) in the future. Core features include secret CRUD, group organization, secret injection (`xv run`), template rendering (`xv inject`), and optional blob file storage.

## Key Architecture Details

### Hybrid Azure SDK + REST API Approach
- Uses Azure SDK v0.21 for authentication and credential management
- Uses direct REST API calls to Azure Key Vault API v7.4 for secret operations
- This hybrid approach works around SDK limitations with tag support which is essential for group management
- Authentication: `azure_identity` crate with DefaultAzureCredential
- Secret operations: Direct `reqwest` HTTP calls with bearer tokens

### Module Structure
- `auth/`: Azure authentication using DefaultAzureCredential pattern
  - `provider.rs`: Core Azure authentication implementation with Graph API integration
- `vault/`: Vault management operations (create, delete, list, restore)
  - `manager.rs`: Core vault operations and lifecycle management
  - `models.rs`: Vault-related data structures
  - `operations.rs`: Specific vault operations (RBAC, access control)
- `secret/`: Secret CRUD operations with group and metadata support
  - `manager.rs`: Core secret operations with REST API integration
  - `models.rs`: Secret-related data structures (SecretInfo, etc.)
  - `name_manager.rs`: Name sanitization and validation logic
- `blob/`: Azure Blob Storage operations for file management
  - `manager.rs`: Core blob operations (upload, download, list, delete)
  - `models.rs`: File-related data structures
  - `operations.rs`: Batch and sync operations
- `config/`: Configuration management with hierarchy (CLI → env vars → config file → defaults)
  - `settings.rs`: Configuration structure and loading
  - `context.rs`: Runtime context management
  - `init.rs`: Interactive setup and storage account creation
- `utils/`: Sanitization, formatting, retry logic, and helper functions
  - `sanitizer.rs`: Azure Key Vault name sanitization with hashing for long names
  - `network.rs`: HTTP client configuration with proper timeouts and error classification
  - `retry.rs`: Retry logic for Azure API calls
  - `format.rs`: Output formatting (JSON, YAML, CSV, table, plain text)
  - `azure_detect.rs`: Azure environment detection
  - `resource_detector.rs`: Azure resource detection utilities
  - `interactive.rs`: Interactive prompting utilities
  - `helpers.rs`: General helper functions
  - `output.rs`: User-friendly output and formatting
  - `datetime.rs`: Date/time parsing and formatting utilities
- `cli/`: Command parsing using `clap` with derive macros
  - `commands.rs`: All CLI command definitions and execution logic (~8,300+ lines)

### Critical Implementation Details
- **Group Management**: Groups stored as comma-separated values in single "groups" tag
- **Name Sanitization**: Client-side sanitization with original names preserved in "original_name" tag; names >127 chars are SHA256 hashed
- **Error Handling**: Custom `CrosstacheError` enum with `thiserror` for structured errors with network error classification
- **Async**: Full `tokio` async runtime throughout
- **REST API Integration**: Uses `reqwest` with bearer tokens from Azure SDK for secret operations

## Development Commands

### Building and Running
```bash
# Build in debug mode
cargo build

# Build release version
cargo build --release

# Build with a feature (tui, aws, ui — see Cargo.toml [features])
cargo build --features ui

# Run the CLI tool
cargo run -- [COMMAND]

# Install locally
cargo install --path .
```

### Testing
```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test file
cargo test --test auth_tests
cargo test --test vault_tests
cargo test --test file_commands_tests

# Run tests in single thread (useful for Azure API tests)
cargo test -- --test-threads=1

# Run unit tests only (exclude integration tests)
cargo test --lib

# Run a specific test function
cargo test test_function_name
```

### Code Quality
```bash
# Format code
cargo fmt

# Run clippy linter
cargo clippy

# Run clippy with all targets (including tests)
cargo clippy --all-targets

# Check without building
cargo check
```

### Linting and Type Checking
```bash
# Run clippy with stricter checks
cargo clippy -- -W clippy::all -W clippy::pedantic

# Check for unsafe code
cargo geiger

# Check dependencies for vulnerabilities
cargo audit
```

## Configuration System

crosstache uses hierarchical configuration with this priority order:
1. Command-line flags (highest)
2. Environment variables  
3. Config file (`$XDG_CONFIG_HOME/xv/xv.conf` or `$HOME/.config/xv/xv.conf`)
4. Default values (lowest)

Key environment variables:
- `AZURE_SUBSCRIPTION_ID`: Default Azure subscription
- `AZURE_TENANT_ID`: Azure tenant ID
- `AZURE_CREDENTIAL_PRIORITY`: Credential type priority (cli, managed_identity, environment, default)
- `DEFAULT_VAULT`: Default vault name
- `DEFAULT_RESOURCE_GROUP`: Default resource group
- `DEFAULT_LOCATION`: Default Azure location (e.g., eastus)
- `FUNCTION_APP_URL`: Function app URL for extended functionality
- `CACHE_TTL`: Cache time-to-live in seconds
- `DEBUG`: Enable debug logging (true/1)
- `AZURE_STORAGE_ACCOUNT`: Azure storage account name (for blob/file operations)
- `AZURE_STORAGE_CONTAINER`: Azure storage container name
- `AZURE_STORAGE_ENDPOINT`: Custom Azure storage endpoint
- `BLOB_CHUNK_SIZE_MB`: Chunk size in MB for blob uploads
- `BLOB_MAX_CONCURRENT_UPLOADS`: Max concurrent blob uploads

## Important Implementation Notes

### Authentication Flow
The tool relies on Azure's DefaultAzureCredential which tries these methods in order:
1. Environment variables (`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`)
2. Managed Identity
3. Azure CLI
4. Visual Studio Code
5. Azure PowerShell

**Credential Priority Configuration**: The authentication order can be customized using the `azure_credential_priority` setting:
- CLI flag: `--credential-type cli` (highest priority)
- Environment variable: `AZURE_CREDENTIAL_PRIORITY=cli`
- Config file: `azure_credential_priority = "cli"`
- Supported values: `cli`, `managed_identity`, `environment`, `default`
- Implementation: `DefaultAzureCredentialProvider::with_credential_priority()` in `src/auth/provider.rs`

### Tag-Based Features
Azure Key Vault secrets are limited to 15 tags total. crosstache uses:
- `groups`: Comma-separated group names
- `original_name`: Preserves user-friendly names before sanitization
- `created_by`: Tracks creation metadata
- `note`: Optional note/description
- `folder`: Folder-based organization
- User can add additional tags up to the 15-tag limit

### Build System
- Uses `built` crate (v0.7 with `git2` and `chrono` features) via `build.rs`
- Automatically embeds git commit hash, branch, build timestamp, and other metadata
- Generated build info is included at compile time from `OUT_DIR/built.rs`
- Release profile: `strip = true`, `lto = true`, `codegen-units = 1`, `panic = "abort"`

### Network Configuration
- HTTP client configured with 30s connect timeout, 120s request timeout
- Comprehensive network error classification for user-friendly error messages
- Handles DNS resolution errors, connection timeouts, SSL/TLS errors
- User-agent header includes version information

### Error Handling Architecture
- Structured error types in `CrosstacheError` enum with specific variants for:
  - Authentication failures
  - Azure API errors
  - Network issues (DNS, timeout, SSL)
  - Secret/vault not found
  - Permission denied
  - Configuration errors
- Network errors are classified for better user experience
- All errors implement `thiserror::Error` for consistent error formatting

### Testing Strategy
- Integration tests in `tests/` directory for auth, vault, and file operations
- Unit tests embedded in modules using `#[cfg(test)]`
- Mock support via `mockall` crate for Azure API testing
- Tests require Azure credentials for integration testing

### Current Implementation Status
As of `v0.14.0` plus current `main`:

- **Output Formats**: JSON, YAML, CSV, plain, raw, and `template` (with field substitution, shipped v0.5.2) all working.
- **Pagination**: Secret listing follows Azure `nextLink` for large result sets; list-style pagination across `xv list` / `vault list` / `file list` / `share` shipped v0.6.0-rc.2.
- **Configurable Clipboard Timeout**: `clipboard_timeout` config key (default 30s, 0 to disable).
- **Config Editing**: `xv config edit` opens the resolved config in `$VISUAL`, then `$EDITOR`, then a platform default; missing configs are seeded with valid defaults.
- **Secret Write Metadata**: `xv set` and `xv gen --save` share write-time flags through `SecretWriteArgs` (`--group`, `--note`, `--folder`, `--expires`, `--not-before`).
- **File Sync** (`xv file sync`): Implemented (`--direction` up/down/both, `--dry-run`, `--delete`); see `src/blob/sync.rs` and `execute_file_sync` in `src/cli/file_ops.rs`.
- **Vault Sharing**: Implemented via Azure RBAC (`xv share grant|revoke|list`).
- **Backends**: Azure Key Vault (default), AWS Secrets Manager (`--features aws`, shipped v0.10.0), Local (age-encrypted on disk). AWS now includes share-policy hints, CloudTrail audit, native rotation, and S3 file storage; `xv file sync` remains unsupported on AWS.
- **Local Backend Hardening**: `[local].encrypt_metadata` encrypts metadata content with `xv local encrypt-metadata`; `[local].opaque_filenames` stores active secrets, versions, and trash under keyed-hash stems with `xv local migrate`.
- **v0.14 Hardening**: context files use private 0600 writes, `xv run` masking is bounded streaming, Azure `az` auth subprocesses are bounded and JWT claim shapes validated, scanner reads are bounded/fail-loud, cache locks use atomic create, and secret-list detail fetches use bounded concurrency.
- **TUI**: Read-only browser (`xv tui`), shipped v0.7.0-rc.2.
- **Web UI**: Embedded localhost browser UI (`xv ui`, `--features ui`) — secret CRUD, folder/group metadata, rename/move, file upload/download, vault switching; loopback-only with a per-session bearer token. See `docs/web-ui.md`.
- **Leak Scanner**: `xv scan` pre-commit scanner, shipped v0.7.0-rc.1.
- **Self-update**: `xv upgrade`, shipped v0.5.1.
- **Secret File Attachments**: `xv attach`/`xv attachments`/`xv detach` plus `xv file upload --encrypt` — client-side age encryption with per-vault key custody in the vault's secret store (`xv-attachment-key`); see `docs/superpowers/specs/2026-07-21-secret-file-attachments-design.md`.
- **Rotation policies (all backends)**: `xv:rotate_every` + `xv:rotated_at` tags, `xv update --rotate-every`, `xv rotate --every/--due/--check` (exit 51 `xv-rotation-due`). AWS `--native` is still the only *server-side* rotation. See `src/secret/rotation.rs`, `docs/rotation.md`.
- **Automatic rotation scheduling**: `xv schedule install|status|uninstall` manages a per-user job in the OS scheduler (launchd / systemd user timer / Task Scheduler) running `xv rotate --due --force`. No daemon, nothing system-wide. `--print` renders without installing. Units carry no credentials; `HOME`/`XDG_CONFIG_HOME` are pinned so the scheduled run resolves the same config. Lifecycle logic is tested against a fake `CommandRunner` — no test registers a real job. See `src/schedule/mod.rs`, `src/cli/schedule_ops.rs`.
- **Local audit trail** (`[local].audit`): hash-chained append-only JSONL, `xv audit --verify` (exit 52 `xv-audit-chain-broken`). Fail-closed appends; `has_audit` reflects the flag. Tamper-*evident* only — the age-identity holder can rewrite it. Records **failures as well as successes**, with status tokens from a closed set keyed off the error variant (`DecryptionFailed`, `NotFound`, …) — never from error messages. `BackendError::Decryption` exists to make failed decryption its own status. See `src/backend/local/audit.rs`, `docs/git-versioning.md`.
- **Git-native versioning** (`[local].git`, local backend only): store is a real git repo, auto-commit per mutation, `xv git init/log/status/diff/push/pull`. Age identity protected by a managed `.gitignore` **and** a pre-commit staged-path refusal. Azure/AWS excluded by design (would create a permanent second copy of every cloud secret). See `src/backend/local/git.rs`.
- **Keeper JSON import/export**: `xv vault import|export --fmt keeper` reads/writes the Keeper Security import format. Keeper logins become typed `login` records (`f.username`/`f.url` tags, password in the envelope); `$oneTimeCode` becomes the `one-time-code` envelope field, never a tag. Folder nesting maps `\` ↔ `/`. Per-record refusals (nothing storable, sanitized-name collision, unusable folder path, backend tag-cap overflow) are reported with reasons and exit non-zero; Keeper shared-folder ACLs have no xv equivalent and are reported, not applied. Pure conversion lives in `src/records/keeper.rs`; CLI wiring in `src/cli/vault_ops.rs`. See `docs/keeper.md`.
- **`vault export`/`vault import` are backend-agnostic**: both need only `SecretBackend`, so `secrets_only_verb` in `src/cli/vault_ops.rs` routes them past the vault-trait shim that used to refuse them on local/AWS.
- **Backend lifecycle** (`xv backend ls|add|rm`): configure and remove backends without disturbing which one is active — `xv init` still bootstraps and switches to a single backend, but now preserves any others already configured instead of discarding them. One instance per type (`local`/`azure`/`aws`); `named_backends` remains the separate multi-instance mechanism. `rm` is config-only by default (drops the backend's block and any workspace entries pointing at it) and refuses in four cases: backend not configured, `--purge` on a non-local backend, removing the active backend while others remain, or stranding the workspace's default vault. `--purge` (local only) additionally deletes the store, age key, and recipients file — unrecoverable, since the key is gone too — guarded by store/key shape checks and a non-TTY refusal without `--yes`; it also refuses inside a directory governed by an active `.xv.toml` `[env.X].vaults` overlay, same as `xv cx rm`. New `[azure]` config block (`Config.azure`) with the top-level Azure fields kept as the legacy fallback, resolved via `azure_settings()`. `Config::validate()` resolves through `azure_settings()` too, so an `[azure]`-only config is a supported hand-authored form (in v0.37.0 it passed `xv backend ls` but failed `xv list`, because validate read the top-level fields directly). Precedence is **whole-block**: a present `[azure]` block shadows the top-level fields entirely, so a partial block that omits `subscription_id`/`tenant_id` is rejected at validation rather than resolving to `None` at the call site. `xv`-generated configs always mirror both fields to the top level. See `docs/backends.md`, `src/cli/backend_ops.rs`, `src/config/backend_ops.rs`.
- **First-party CI/CD**: root `action.yml` composite GitHub Action (per-OS release archive, fail-closed SHA-256, tool cache, masked secret export to `GITHUB_ENV`) plus OIDC-native Azure auth (`AZURE_CREDENTIAL_PRIORITY=oidc`) federating a GitHub OIDC token as a `client_assertion` — no stored secret, no `azure/login`. See `src/backend/azure/oidc.rs`, `docs/ci-cd.md`, `.github/workflows/action-test.yml`.

Known partial / known limitations (tracked in `ROADMAP.md`):

- **Azure Secret Backup/Restore**: stub on Azure backend (`src/backend/azure/secrets.rs`).
- **AWS capability gap**: `xv file sync` is not supported on AWS S3 storage yet.
- **Local audit trail has no off-box sink** (a git remote is the current answer); the chain is tamper-evident, not tamper-proof.
- **Rotation has no external-system hook** (`--generator` or AWS `--native` are the escape hatches) and no rollout coordination — a rotated credential still needs the consumer to re-read it.
- **Scheduler lifecycle is untested on a real runner** (rendering + command sequencing are tested against a fake runner; macOS was verified manually). systemd-less Linux gets a diagnostic error with the cron line, not an auto-installed fallback.
- **CI/CD is GitHub-only** as a first-party integration; GitLab/CircleCI use a documented plain install step.
- **`xv rotate --check --format json` emits two JSON documents on stdout** when something is due (rows + the error envelope) — same shape `xv scan --format json` has always had; the framework's error path owns this.

For open work items, see `ROADMAP.md` at the repo root. Implementation history is in `CHANGELOG.md`; shipped designs live under `docs/superpowers/specs/` with version banners.
