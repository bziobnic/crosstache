# Delete Command Enhancement Checklist

## Overview
Modify the `xv file delete` command to behave like the Unix `rm` command when handling files and directories, with support for recursive deletion and safety features.

## Current State
- `xv file delete` currently handles single files only
- No directory handling logic exists
- No recursive flag support
- No safety confirmation prompts

## Proposed Behavior

### Single File Deletion (Enhanced)
```bash
xv file delete config.json                    # ✅ Works as before
xv file delete config.json -f                 # ✅ Force delete without confirmation
xv file delete config.json -v                 # ✅ Verbose output
```

### Directory Deletion - New Behavior
```bash
xv file delete docs/                          # ❌ Refuse without -r flag
xv file delete docs/ -r                       # ✅ Delete directory recursively
xv file delete docs/* -r                      # ✅ Delete contents recursively
xv file delete docs/*                         # ❌ Refuse if subdirectories exist without -r
```

### Safety Features
```bash
xv file delete important/ -r                  # ❌ Prompt for confirmation
xv file delete important/ -rf                 # ✅ Force delete without confirmation
xv file delete important/ -ri                 # ✅ Interactive mode (prompt per item)
```

## Implementation Checklist

### 1. Command Line Interface Updates
- [x] Add `-r, --recursive` flag to file delete command
- [x] Add `-f, --force` flag to skip confirmation prompts
- [x] Add `-i, --interactive` flag for per-item confirmation
- [x] Add `-v, --verbose` flag for detailed output
- [x] Add `--dry-run` flag for testing without actual deletion
- [x] Update help text to document new behavior
- [x] Update command parsing in `src/cli/commands.rs`
- [x] Create enhanced delete function placeholders

### 2. Path Analysis Logic
- [x] Create function `analyze_delete_path(path: &str) -> PathType`
- [x] Implement `PathType` enum:
  ```rust
  enum DeletePathType {
      File,
      Directory,
      GlobPattern,      // e.g., "docs/*"
      EmptyDirMarker,   // .xv_empty_dir markers
  }
  ```
- [x] Create function `requires_recursive_flag(path: &str) -> bool`
- [x] Handle glob expansion for patterns like `docs/*`
- [x] Detect empty directory markers (`.xv_empty_dir` files)

### 3. Directory Detection
- [x] Implement `is_directory_in_blob_storage(blob_manager, path) -> Result<bool>`
- [x] Implement `contains_subdirectories_in_blob_storage(blob_manager, path) -> Result<bool>`
- [x] Handle empty directory markers appropriately
- [x] Add proper error handling for inaccessible blobs

### 4. Delete Logic Updates
- [x] Modify `execute_file_delete()` in `src/cli/commands.rs`
- [x] Add validation before starting deletion:
  ```rust
  fn validate_delete_request(path: &str, recursive: bool, force: bool) -> Result<()>
  ```
- [x] Implement confirmation prompts for dangerous operations

### 5. Recursive Delete Implementation
- [x] Implement `delete_directory_recursive(blob_manager, path) -> Result<Vec<DeleteResult>>`
- [x] Implement `delete_glob_pattern(blob_manager, pattern, recursive) -> Result<Vec<DeleteResult>>`
- [x] Handle nested directory structures
- [x] Process empty directory markers correctly
- [x] Add progress reporting for multiple file deletions

### 6. Safety and Confirmation System
- [x] Create confirmation prompt system:
  ```rust
  fn prompt_for_confirmation(message: &str, force: bool, interactive: bool) -> Result<bool>
  ```
- [x] Implement different confirmation modes:
  - **Standard**: Prompt once for dangerous operations
  - **Interactive**: Prompt for each item
  - **Force**: Skip all prompts
- [x] Add special handling for important directories
- [ ] Implement "are you sure?" double-confirmation for large deletions

### 7. Error Handling
- [x] Create specific error types for delete scenarios:
  ```rust
  // Add to CrosstacheError enum in src/error.rs
  DirectoryRequiresRecursive { path: String },
  SubdirectoriesRequireRecursive { pattern: String },
  DeletionCancelled { path: String },
  PartialDeletionFailure { succeeded: usize, failed: usize },
  BlobNotFound { path: String },
  BlobPermissionDenied { path: String },
  ```
- [x] Add user-friendly error messages
- [x] Suggest using `-r` flag when appropriate
- [x] Handle partial failures gracefully

### 8. Blob Storage Integration
- [x] Update `BlobManager` to support batch deletions:
  ```rust
  pub async fn delete_files_batch(&self, names: Vec<String>) -> Result<Vec<Result<()>>>
  ```
- [x] Implement `BlobManager::delete_directory()` method:
  ```rust
  pub async fn delete_directory(&self, path: &str, recursive: bool) -> Result<DeleteSummary>
  ```
- [x] Handle blob name patterns and filtering
- [x] Add conflict detection for concurrent modifications
- [x] Implement deletion verification

### 9. Directory Structure Handling
- [x] Create function to list all blobs in directory:
  ```rust
  fn list_blobs_in_directory(blob_manager: &BlobManager, path: &str) -> Result<Vec<String>>
  ```
- [x] Handle empty directory markers:
  - Delete `.xv_empty_dir` markers when deleting empty directories
  - Recreate parent empty directory markers if needed
- [x] Maintain directory structure consistency
- [x] Handle edge case: deleting all files in a directory (should create empty marker?)

### 10. Progress Reporting and Feedback
- [x] Show progress for batch deletions:
  ```bash
  Deleting files in 'docs/'...
  Deleted 'docs/file1.txt'
  Deleted 'docs/subdir/file2.txt' 
  Deleted empty directory marker 'docs/empty_dir/.xv_empty_dir'
  
  Summary: 15 files deleted, 2 directories deleted
  ```
- [x] Implement verbose mode output
- [x] Show deletion summary with counts
- [x] Handle and report failures gracefully

### 11. Validation Rules Implementation

#### Rule 1: Directory without -r flag
```bash
xv file delete docs/  # Should fail with helpful message
```
- [x] Detect when path represents a directory (contains blobs with path prefix)
- [x] Check if recursive flag is provided
- [x] Return error: "docs/ contains files and subdirectories. Use -r to delete recursively."

#### Rule 2: Glob pattern with subdirectories
```bash
xv file delete docs/*  # Should fail if subdirs exist without -r
```
- [x] Expand glob pattern to blob list
- [x] Check if any matched blobs represent subdirectories
- [x] Return error: "Pattern matches subdirectories. Use -r to delete recursively."

#### Rule 3: Confirmation for dangerous operations
```bash
xv file delete important/ -r  # Should prompt for confirmation
```
- [x] Detect potentially dangerous operations (large deletions, important paths)
- [x] Implement confirmation prompts with clear warnings
- [x] Allow bypass with `-f` flag

### 12. Interactive Mode Implementation
- [x] Implement `-i` flag functionality:
  ```bash
  $ xv file delete docs/ -ri
  Delete directory 'docs/' and all its contents? (y/N): y
  Delete 'docs/file1.txt'? (y/N): y
  Delete 'docs/subdir/'? (y/N): n
  Delete 'docs/file2.txt'? (y/N): y
  ```
- [x] Handle user input (y/n/q for quit)
- [x] Skip remaining items if user chooses quit
- [x] Show running summary of decisions

### 13. Glob Pattern Support
- [x] Implement comprehensive glob pattern matching:
  ```rust
  fn expand_delete_pattern(pattern: &str, blob_manager: &BlobManager) -> Result<Vec<String>>
  ```
- [x] Support common patterns:
  - `docs/*` - all files in docs directory
  - `*.txt` - all .txt files in current directory
  - `**/*.tmp` - all .tmp files recursively
- [x] Handle pattern edge cases and invalid patterns
- [x] Respect recursive flag for pattern expansion

### 14. Testing Strategy

#### Unit Tests
- [ ] Test `analyze_delete_path()` with various inputs
- [ ] Test `requires_recursive_flag()` logic
- [ ] Test confirmation prompt logic
- [ ] Test glob pattern expansion
- [ ] Test error message generation

#### Integration Tests
- [ ] Test single file deletion (regression test)
- [ ] Test directory deletion without `-r` flag (should fail)
- [ ] Test directory deletion with `-r` flag (should succeed)
- [ ] Test glob pattern without subdirectories (should succeed)
- [ ] Test glob pattern with subdirectories without `-r` (should fail)
- [ ] Test glob pattern with subdirectories with `-r` (should succeed)
- [ ] Test force flag functionality
- [ ] Test interactive mode functionality
- [ ] Test mixed file/directory deletions

#### End-to-End Tests
- [ ] Create test directory structure with files and subdirectories
- [ ] Test all command variations
- [ ] Verify blob storage shows expected state after deletion
- [ ] Test progress reporting and error scenarios
- [ ] Test empty directory marker handling

### 15. Empty Directory Marker Handling
- [ ] Special handling for `.xv_empty_dir` markers:
  ```rust
  fn handle_empty_directory_marker(marker_path: &str) -> DeleteAction {
      // Convert marker deletion to directory deletion
  }
  ```
- [ ] When deleting directory containing only marker, delete the marker
- [ ] When deleting all files in directory, create empty marker if appropriate
- [ ] Update file list display to show directories correctly after deletions

### 16. Safety Features Implementation
- [ ] Implement deletion size limits:
  ```rust
  const MAX_FILES_WITHOUT_CONFIRMATION: usize = 10;
  const MAX_SIZE_WITHOUT_CONFIRMATION: u64 = 100 * 1024 * 1024; // 100MB
  ```
- [ ] Add warnings for large deletions
- [ ] Implement "dry run" mode for testing:
  ```bash
  xv file delete docs/ -r --dry-run  # Show what would be deleted
  ```
- [ ] Add ability to exclude certain file patterns from deletion

### 17. Performance Optimization
- [ ] Implement batch deletion API calls where possible
- [ ] Use concurrent deletion for multiple files (with rate limiting)
- [ ] Add progress indicators for long operations
- [ ] Optimize blob listing for large directories
- [ ] Cache directory structure information when possible

### 18. Command Line Parsing Updates
- [ ] Update `FileCommands::Delete` structure:
  ```rust
  Delete {
      /// Files or directories to delete
      #[arg(value_name = "PATH")]
      paths: Vec<String>,
      
      /// Delete directories recursively
      #[arg(short = 'r', long = "recursive")]
      recursive: bool,
      
      /// Force deletion without confirmation
      #[arg(short = 'f', long = "force")]
      force: bool,
      
      /// Interactive mode - prompt before each deletion
      #[arg(short = 'i', long = "interactive")]
      interactive: bool,
      
      /// Verbose output
      #[arg(short = 'v', long = "verbose")]
      verbose: bool,
      
      /// Show what would be deleted without actually deleting
      #[arg(long = "dry-run")]
      dry_run: bool,
  }
  ```

### 19. Documentation Updates
- [ ] Update `README.md` with new delete examples
- [ ] Update command help text with detailed examples
- [ ] Add safety warnings to documentation
- [ ] Document blob naming strategy for directories
- [ ] Update CLAUDE.md with implementation notes
- [ ] Add troubleshooting section for common deletion scenarios

### 20. Error Recovery and Logging
- [ ] Implement deletion logging for audit purposes
- [ ] Add ability to recover from partial failures
- [ ] Create deletion transaction log for complex operations
- [ ] Implement rollback capability where possible (limited by blob storage)
- [ ] Add detailed error reporting for failed deletions

## Implementation Priority
1. **Phase 1**: Basic command line interface and validation
2. **Phase 2**: Single file deletion with safety features
3. **Phase 3**: Directory detection and recursive deletion
4. **Phase 4**: Glob pattern support and interactive mode
5. **Phase 5**: Performance optimization and advanced features
6. **Phase 6**: Testing and documentation

## Files to Modify
- `src/cli/commands.rs` - Command parsing and execution
- `src/blob/manager.rs` - Delete logic and batch operations
- `src/blob/operations.rs` - Extended delete operations
- `src/blob/models.rs` - Delete request/response structures
- `src/error.rs` - New error types
- `src/utils/interactive.rs` - Confirmation prompts
- `tests/file_commands_tests.rs` - Test cases
- `README.md` - Documentation
- `CLAUDE.md` - Implementation notes

## Success Criteria
- [ ] All Unix `rm`-like behaviors work correctly
- [ ] Comprehensive safety features prevent accidental deletions
- [ ] Existing single file deletions continue to work
- [ ] Performance is acceptable for large directory deletions
- [ ] All edge cases are handled gracefully
- [ ] Complete test coverage
- [ ] Updated documentation
- [ ] Empty directory markers handled correctly

## Safety Considerations
- **Default to safe**: Require explicit flags for dangerous operations
- **Clear warnings**: Make consequences obvious before deletion
- **Confirmation prompts**: Multi-step confirmation for large deletions
- **Dry run capability**: Allow users to preview deletion actions
- **Audit logging**: Track deletion operations for security
- **Recovery guidance**: Provide clear error messages and recovery suggestions

## Edge Cases to Handle
- [ ] Deleting files that don't exist
- [ ] Concurrent modifications during deletion
- [ ] Network interruptions during batch deletions
- [ ] Very large directories (thousands of files)
- [ ] Files with special characters in names
- [ ] Nested empty directories
- [ ] Partial blob name matches
- [ ] Blob storage permission errors
- [ ] Mixed empty directory markers and regular files

## Example Command Variations

### Basic Usage
```bash
# Single file
xv file delete config.json

# Multiple files
xv file delete file1.txt file2.txt file3.txt

# With confirmation
xv file delete important.txt  # Should prompt: "Delete 'important.txt'? (y/N)"
```

### Directory Operations
```bash
# Empty directory (via marker)
xv file delete empty_dir/  # Should work without -r (just marker)

# Directory with contents (should fail without -r)
xv file delete docs/

# Recursive directory deletion
xv file delete docs/ -r

# Force recursive deletion
xv file delete docs/ -rf
```

### Interactive Mode
```bash
# Interactive deletion
xv file delete docs/ -ri

# Interactive with verbose
xv file delete docs/ -riv
```

### Glob Patterns
```bash
# All .txt files
xv file delete *.txt

# All files in directory
xv file delete docs/*

# All .tmp files recursively
xv file delete **/*.tmp -r
```

### Safety Features
```bash
# Dry run
xv file delete docs/ -r --dry-run

# Verbose output
xv file delete docs/ -rv

# Force large deletion
xv file delete huge_directory/ -rf
```

This comprehensive checklist provides a roadmap for implementing full Unix `rm`-like functionality while maintaining the blob storage paradigm and adding appropriate safety features for cloud storage operations.