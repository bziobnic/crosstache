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
  - `format.rs`: Output formatting (JSON, table, plain text)
  - `azure_detect.rs`: Azure environment detection
  - `interactive.rs`: Interactive prompting utilities
- `cli/`: Command parsing using `clap` with derive macros
  - `commands.rs`: All CLI command definitions and execution logic (3000+ lines)

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
- `FUNCTION_APP_URL`: Function app URL for extended functionality
- `CACHE_TTL`: Cache time-to-live in seconds
- `DEBUG`: Enable debug logging (true/1)

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
- Uses custom `build.rs` that auto-increments build numbers stored in `build_number.txt`
- Embeds git commit hash, branch, and build timestamp
- Creates version strings like `0.1.0.123+abc1234`
- Build metadata available via environment variables: `BUILD_NUMBER`, `GIT_HASH`, `BUILD_TIME`, `GIT_BRANCH`

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
Major features partially implemented or TODO:
- **File Sync** (`xv file sync`): Command structure exists but not implemented (see `execute_file_sync` at line 3071 in commands.rs)
- **Vault Sharing**: Commands defined but not implemented (grant/revoke/list)
- **Secret Backup/Restore**: Methods stubbed in `secret/manager.rs`
- **Advanced Secret Operations**: List deleted secrets, get versions, backup/restore
- **Pagination**: Not implemented for large result sets
- **Template Output**: Format option exists but not implemented

For detailed task list, see TODO.md