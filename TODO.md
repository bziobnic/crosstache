# TODO - Outstanding Development Tasks

This document consolidates all outstanding development tasks for crosstache from various project documents.

## High Priority - Core Functionality Missing

### Storage Account Management

**Status**: Mixed implementation - some functionality exists in different modules

**✅ ALREADY IMPLEMENTED:**
- [x] `create_storage_account()` - **IMPLEMENTED** in `src/config/init.rs` (lines 347-468)
  - Uses Azure CLI commands with proper timeout handling
  - Includes comprehensive error handling and verification
- [x] `create_blob_container()` - **IMPLEMENTED** in `src/config/init.rs` (lines 471-554)
  - Handles both new and existing storage accounts
  - Includes container existence verification

**❌ STILL MISSING (in `src/blob/storage.rs`):**
- [ ] Implement `delete_storage_account()` - Delete storage accounts
- [ ] Implement `delete_container()` - Delete blob containers  
- [ ] Implement `list_storage_accounts()` - List available storage accounts

**Note**: The `src/blob/storage.rs` file was deleted during cleanup as it only contained "not yet implemented" stubs. The working storage account creation functionality already exists in the interactive setup system.

### Secret Management - Advanced Operations

**Status**: Advanced secret operations are stubbed out in `src/secret/manager.rs`

- [ ] Implement `list_deleted_secrets()` - List soft-deleted secrets (line 917-920)
- [ ] Implement `get_secret_versions()` - Get all versions of a secret (line 939-942)  
- [ ] Implement `backup_secret()` - Create secret backups (line 949-952)
- [ ] Implement `restore_secret_from_backup()` - Restore from backup (line 962-965)

### CLI Commands - Missing Features

**Status**: Several commands print TODO messages instead of implementing functionality

- [ ] Implement `execute_info_command()` - Show vault information (line 1225)
- [ ] Implement vault sharing commands:
  - [ ] Share grant command (line 1951-1953)
  - [ ] Share revoke command (line 1957-1959) 
  - [ ] Share list command (line 1963-1965)
- [ ] Implement `execute_vault_update()` - Update vault properties (line 2419)
- [ ] Implement file sync functionality (line 3027-3028)

## Medium Priority - Enhanced Features

### Blob Management Enhancements

**Status**: Basic blob operations work but missing advanced features

- [ ] Implement metadata setting for Azure SDK v0.21 (line 80-86 in `src/blob/manager.rs`)
- [ ] Implement tag setting for Azure SDK v0.21 (line 88-94)
- [ ] Implement tags retrieval strategy (line 180, 325)
- [ ] Implement chunked upload for large files (line 403)
- [ ] Implement progress callback for file operations (line 2738)
- [ ] Implement streaming download (line 2792)

### Pagination Support

**Status**: Pagination is not implemented for large result sets

- [ ] Implement `list_files_paginated()` in `src/blob/operations.rs` (line 95-97)
- [ ] Implement pagination support for secret operations in `src/secret/manager.rs` (line 680-681)

### Output Formatting

**Status**: Basic JSON/YAML work, but advanced formats missing

- [ ] Implement template engine (line 73-74 in `src/utils/format.rs`)
- [ ] Implement template output (line 200-202)
- [ ] Implement CSV output (line 86-87, 188)

## Low Priority - Nice-to-Have Features

### Enhanced Retry Logic

**Status**: Basic retry works but missing advanced features

- [ ] Add context-aware cancellation support (line 66-68 in `src/utils/retry.rs`)
- [ ] Add configurable retry conditions
- [ ] Add retry policy abstraction

### Configuration Improvements

**Status**: Basic config works but could be enhanced

- [ ] Replace Azure CLI dependency with proper Azure Management API integration (line 357 in `src/config/init.rs`)

### Interactive Setup Implementation

**Status**: `xv init` command needs comprehensive interactive setup

#### Phase 1: Analysis & Architecture
- [ ] Analyze current configuration system (`/src/config/settings.rs`, `/src/config/context.rs`)
- [ ] Document environment variable handling patterns  
- [ ] Review CLI command patterns in `/src/cli/commands.rs`
- [ ] Design interactive setup architecture with stages: welcome → detection → subscription → resource group → vault → test vault → summary

#### Phase 2: Core Implementation
- [ ] Create `/src/cli/setup/detection.rs` module
- [ ] Implement `AzureEnvironment` struct with Azure CLI detection
- [ ] Implement `detect_azure_environment() -> Result<AzureEnvironment>`
- [ ] Implement `verify_azure_cli_login() -> Result<bool>`
- [ ] Create `/src/cli/setup/prompts.rs` module  
- [ ] Implement interactive prompting system (`YesNoPrompt`, `SelectPrompt`, `TextPrompt`)

#### Phase 3: Configuration & Setup Logic
- [ ] Create `/src/cli/setup/config_builder.rs` module
- [ ] Implement `SetupConfigBuilder` with smart defaults
- [ ] Create `/src/cli/setup/vault_creation.rs` module
- [ ] Implement test vault creation workflow

#### Phase 4: Integration & Polish
- [ ] Create `/src/cli/commands/init.rs` module
- [ ] Implement `execute_init_command(args: InitArgs) -> Result<()>`
- [ ] Add comprehensive testing for init command flow
- [ ] Add documentation and help text

### File Support Implementation

**Status**: Comprehensive file support using Azure Blob Storage (planned feature)

#### Phase 1: Foundation and Storage Setup
- [ ] Update `Cargo.toml` with Azure Blob Storage dependencies (`azure_storage_blobs`, `azure_mgmt_storage`, `tempfile`, `mime_guess`)
- [ ] Extend configuration structure in `src/config/settings.rs` with `BlobConfig`
- [ ] Add environment variable support for blob configuration
- [ ] Extend `ConfigInitializer` in `src/config/init.rs` for storage account creation

#### Phase 2: Core File Operations Module  
- [ ] Create module structure: `src/blob/mod.rs`, `manager.rs`, `models.rs`, `operations.rs`
- [ ] Implement file data models (`FileInfo`, `FileUploadRequest`, `FileDownloadRequest`, `FileListRequest`)
- [ ] Implement `BlobManager` with core operations (upload, download, list, delete, get_file_info)
- [ ] Add large file support with block-based chunking

#### Phase 3: CLI Commands Integration
- [ ] Add file commands to `src/cli/commands.rs`
- [ ] Implement `FileCommands` enum (Upload, Download, List, Delete, Info, Edit, Sync)
- [ ] Create quick upload/download aliases (`xv upload`, `xv download`)
- [ ] Implement file edit functionality (download, edit, upload)

#### Phase 4: Advanced Features
- [ ] Implement file synchronization (`sync_up`, `sync_down`, bidirectional sync)
- [ ] Add metadata and tagging support for files
- [ ] Create progress indicators for file operations
- [ ] Implement streaming for large file downloads

#### Phase 5: Testing and Documentation
- [ ] Write comprehensive unit and integration tests
- [ ] Update README.md with file operations documentation  
- [ ] Add help text and usage examples for all file commands

## Test Coverage Gaps

### Missing Test Coverage

- [ ] Integration tests for actual file upload/download operations in `tests/file_commands_tests.rs`
- [ ] Tests for error conditions and Azure API integration
- [ ] Complete incomplete test in `tests/auth_tests.rs` (line 30)
- [ ] Verify `create_test_config()` usage in `tests/file_commands_tests.rs` (lines 15-29)

## Code Cleanup Tasks

### Placeholder Method Cleanup

- [ ] Remove or implement `list_files_paginated` method from `src/blob/operations.rs` (lines 88-98)
- [ ] Remove or implement `upload_large_file` method from `src/blob/manager.rs` (lines 395-425)
- [ ] Replace TODO print statements in CLI commands with proper error messages or implementations

### Documentation Consolidation

- [ ] Review documentation overlap between:
  - `FILE-OPS.md` - Azure Blob Storage implementation checklist
  - `FILES.md` - File support implementation plan  
  - `UNDONE.md` - Stubbed-out features catalog
  - `CLEANUP_CHECKLIST.md` - Code cleanup tasks
- [ ] Consolidate overlapping documentation
- [ ] Remove redundant documentation files

## Implementation Status Summary

| Module | Core Features | Advanced Features | Status |
|--------|---------------|-------------------|---------|
| Authentication | ✅ Implemented | ✅ Graph API | 95% Complete |
| Storage Management | ✅ Basic Create | ❌ Delete/List | 40% Complete |
| Secret Backup/Restore | ❌ Missing | ❌ Missing | 0% Complete |
| Vault Sharing | ❌ Missing | ❌ Missing | 0% Complete |
| File Operations | ✅ Basic | ❌ Advanced | 70% Complete |
| File Sync | ❌ Missing | N/A | 0% Complete |
| Blob Metadata/Tags | ❌ Partial | ❌ Missing | 20% Complete |
| Pagination | ❌ Missing | ❌ Missing | 10% Complete |
| Output Formatting | ✅ Basic | ❌ Advanced | 75% Complete |
| Secret Operations | ✅ Basic | ❌ Advanced | 80% Complete |
| Interactive Setup | ❌ Missing | ❌ Missing | 0% Complete |

## Success Criteria

- [ ] New users can complete setup in under 2 minutes with `xv init`
- [ ] All stubbed operations either implemented or properly marked as unimplemented
- [ ] File operations work seamlessly with existing secret operations
- [ ] All tests pass with >80% coverage for new features
- [ ] Documentation is complete and accurate
- [ ] No placeholder TODO print statements remain in production code

## Notes

**Recent Corrections Made:**
- Authentication system is actually 95% complete (not missing as originally documented)
- Microsoft Graph API integration is functional with user/service principal lookup
- JSON/YAML output formatting is implemented and working
- Azure Identity error handling is comprehensive and well-implemented

**Current Priority Focus:**
1. Advanced secret operations (backup/restore/versioning) - highest priority
2. Vault sharing/permissions system 
3. File sync functionality and advanced file operations
4. Storage account delete/list operations (basic create already works)
5. Interactive setup system enhancements (`xv init` command has basic functionality)

Last Updated: 2024-07-23