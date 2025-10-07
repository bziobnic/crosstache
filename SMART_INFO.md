# Smart Info Command Implementation Checklist

## Overview
Convert the root `info` command from a simple vault info alias into an intelligent dispatcher that automatically determines resource type (vault, secret, or file) based on context and patterns.

## Phase 1: Command Structure Updates

### 1.1 Update CLI Command Definition
- [x] Modify the `Info` variant in `Commands` enum in `src/cli/commands.rs`
  - [x] Change from current structure to new smart detection structure
  - [x] Add `resource: String` field (replaces `vault_name`)
  - [x] Add `resource_type: Option<ResourceType>` field for explicit type override
  - [x] Keep `resource_group` and `subscription` as optional fields (needed for vault operations)
  - [x] Add `--type` flag with short alias `-t`

### 1.2 Create ResourceType Enum
- [x] Define `ResourceType` enum in `src/cli/commands.rs`
  ```rust
  #[derive(Debug, Clone, Copy, ValueEnum)]
  pub enum ResourceType {
      Vault,
      Secret,
      File,
  }
  ```
- [x] Implement `Display` trait for user-friendly output
- [x] Add documentation comments for each variant

### 1.3 Update Command Documentation
- [x] Update help text for `Info` command to reflect new smart detection capability
- [x] Add examples showing different usage patterns (via help text)
- [x] Document the detection precedence order (in comments)

## Phase 2: Secret Info Implementation

### 2.1 Create Secret Info Data Structure
- [x] Add `SecretInfo` struct in `src/secret/models.rs` (create file if needed)
  - [x] Include fields: name, version, enabled, created, updated, expires
  - [x] Include metadata fields: tags, content_type, recovery_level
  - [x] Implement `Serialize` for JSON output

### 2.2 Implement Secret Info Method
- [x] Add `get_secret_info` method to `SecretManager` in `src/secret/manager.rs`
  - [x] Make REST API call to get secret properties (without value)
  - [x] Parse response into `SecretInfo` struct
  - [x] Handle authentication and error cases
  - [x] Return formatted info or raw struct based on output format

### 2.3 Add Secret Info Display
- [x] Implement `Display` trait for `SecretInfo` for formatted console output
- [x] Format timestamps in human-readable format
- [x] Show groups, folders, and notes from tags
- [x] Display version information
- [x] Show enabled/disabled status prominently

## Phase 3: Detection Logic Implementation

### 3.1 Create Resource Detector Module
- [x] Create new file `src/utils/resource_detector.rs`
- [x] Implement `ResourceDetector` struct with methods:
  - [x] `detect_resource_type(resource: &str, hint: Option<ResourceType>) -> ResourceType`
  - [x] `is_valid_vault_name(name: &str) -> bool`
  - [x] `is_valid_secret_name(name: &str) -> bool`
  - [x] `is_valid_file_name(name: &str) -> bool`

### 3.2 Implement Detection Algorithm
- [x] Priority 1: Check explicit `--type` flag if provided
- [x] Priority 2: Check if resource_group is provided (implies vault)
- [x] Priority 3: Try pattern matching:
  - [x] File patterns: Contains extension (`.txt`, `.json`, etc.)
  - [x] Vault patterns: Matches Azure vault naming rules (3-24 chars, alphanumeric and hyphens)
  - [x] Secret patterns: Default fallback for Key Vault naming rules
- [ ] Priority 4: Attempt discovery (try each type until one succeeds) - Not implemented yet

### 3.3 Add Discovery Fallback
- [ ] Implement `try_discover_resource` function - Future enhancement
  - [ ] Try vault lookup first (if resource_group available)
  - [ ] Try secret lookup second
  - [ ] Try file lookup third
  - [ ] Return specific error if none found

## Phase 4: Execute Info Command Refactor

### 4.1 Refactor execute_info_command
- [x] ~~Rename current `execute_info_command` to `execute_info_command_legacy`~~ - Not needed
- [x] Create new `execute_info_command` with smart detection logic
- [x] Parse resource type using detector
- [x] Route to appropriate handler based on detected/specified type

### 4.2 Create Type-Specific Handlers
- [x] Extract current vault info logic into `execute_vault_info_from_root`
- [x] Create `execute_secret_info_from_root` function
  - [x] Create secret manager
  - [x] Call `get_secret_info`
  - [x] Format output based on config (JSON or table)
- [x] Create `execute_file_info_from_root` function
  - [x] Create blob manager
  - [x] Call existing file info logic
  - [x] Handle output formatting

### 4.3 Implement Error Handling
- [x] Create specific error messages for each resource type not found
- [x] Add helpful suggestions when detection fails
- [x] Provide clear error when type is ambiguous
- [x] Suggest using `--type` flag for disambiguation

## Phase 5: Integration and Testing

### 5.1 Update Existing Commands
- [ ] Ensure `vault info` subcommand still works
- [ ] Ensure `file info` subcommand still works
- [ ] Add deprecation notice for direct subcommands (optional)
- [ ] Update command aliases if needed

### 5.2 Write Unit Tests
- [ ] Test resource type detection logic
  - [ ] Test with explicit type flag
  - [ ] Test pattern matching for files (extensions)
  - [ ] Test pattern matching for vaults
  - [ ] Test fallback to secrets
- [ ] Test error cases
  - [ ] Resource not found
  - [ ] Ambiguous resource type
  - [ ] Missing required parameters

### 5.3 Write Integration Tests
- [ ] Create test file `tests/info_command_tests.rs`
- [ ] Test vault info with smart detection
- [ ] Test secret info with smart detection
- [ ] Test file info with smart detection
- [ ] Test with explicit `--type` override
- [ ] Test error scenarios

### 5.4 Update Documentation
- [ ] Update README.md with new info command usage
- [ ] Add examples for each resource type
- [ ] Document the smart detection behavior
- [ ] Update help text in the CLI

## Phase 6: Optimization and Polish

### 6.1 Performance Optimization
- [ ] Implement caching for recently queried resources
- [ ] Add parallel lookup option for discovery mode
- [ ] Optimize API calls to minimize round trips

### 6.2 User Experience Improvements
- [ ] Add progress indicators for discovery mode
- [ ] Implement colored output for better readability
- [ ] Add `--verbose` flag for detailed information
- [ ] Add shortcuts for common patterns

### 6.3 Edge Cases
- [ ] Handle resources with same name across types
- [ ] Handle special characters in resource names
- [ ] Handle very long resource names
- [ ] Handle network timeout during discovery
- [ ] Handle partial permissions (can read secret but not vault)

## Phase 7: Final Validation

### 7.1 Manual Testing Checklist
- [ ] Test: `xv info my-vault` (vault detection)
- [ ] Test: `xv info my-secret` (secret detection)
- [ ] Test: `xv info my-file.txt` (file detection via extension)
- [ ] Test: `xv info my-resource --type secret` (explicit type)
- [ ] Test: `xv info nonexistent` (error handling)
- [ ] Test: JSON output with `--format json`
- [ ] Test: Table output (default)

### 7.2 Backward Compatibility
- [ ] Verify existing scripts using `xv info <vault>` still work
- [ ] Verify `vault info` subcommand still functions
- [ ] Verify `file info` subcommand still functions
- [ ] Check that existing error codes are preserved

### 7.3 Code Review Checklist
- [ ] Code follows Rust best practices
- [ ] Error messages are helpful and actionable
- [ ] All new public APIs are documented
- [ ] No unwrap() calls in production code
- [ ] Proper use of async/await
- [ ] Consistent error handling with CrosstacheError

## Implementation Notes

### Design Decisions
1. **Detection Order**: Explicit type > Context clues > Pattern matching > Discovery
2. **Default Behavior**: When ambiguous, prefer secrets (most common use case)
3. **Performance**: Cache detection results within same command execution
4. **Error Recovery**: Always suggest using `--type` flag when detection fails

### Technical Considerations
- Resource detection should be fast (<100ms)
- Discovery mode may take longer (up to 3 API calls)
- Consider implementing timeout for discovery mode
- Ensure proper cleanup of resources on error

### Future Enhancements
- Add support for regex patterns in resource names
- Implement fuzzy matching for typos
- Add resource history/recent resources cache
- Support for batch info queries
- Integration with shell completion for resource names

## Completion Criteria
- [ ] All unit tests pass
- [ ] All integration tests pass
- [ ] Manual testing validates all scenarios
- [ ] Documentation is updated
- [ ] Code review completed
- [ ] Performance benchmarks meet targets (<100ms for detection)
- [ ] No regression in existing commands