# File Command Consolidation Refactoring Plan

## Status Update (Last Updated: 2025-08-27)

**Phase 1: ✅ COMPLETED** - Command structure refactoring is fully implemented
**Phase 2: ✅ COMPLETED** - All consolidation logic implemented including recursive uploads and rename validation
**Phase 3: 🔄 IN PROGRESS** - Core error handling complete (missing: BatchOperationResult struct, progress bars)
**Phase 4: ⏳ NOT STARTED** - Testing updates needed
**Phase 5: ⏳ NOT STARTED** - Documentation updates needed

**Overall Progress: ~70% Complete**

The core functionality is working! The `file batch` commands have been successfully removed and replaced with consolidated commands that handle both single and multiple files naturally.

## Executive Summary

This document outlines the refactoring plan to consolidate the file commands in the xv CLI tool, eliminating the `file batch` nested subcommand structure in favor of a simpler, more intuitive interface where `file upload`, `file download`, and `file delete` commands handle both single and multiple files.

## Current State Analysis

### Existing Command Structure
```
xv file upload <file>                    # Single file upload
xv file download <file>                  # Single file download  
xv file delete <file>                    # Single file delete
xv file batch upload <files...>          # Multiple file upload
xv file batch delete <files...>          # Multiple file delete
xv file list                             # List files
xv file info <file>                      # Get file info
xv file sync <path>                      # Sync directory
```

### Problems with Current Structure
1. **Unnecessary nesting**: `file batch upload` requires extra typing
2. **Inconsistent UX**: Users need to remember when to use `batch` subcommand
3. **Non-intuitive**: Most CLI tools handle multiple files naturally (cp, rm, mv)
4. **Poor discoverability**: Users may not realize batch operations exist

### File Locations
- **Command definitions**: `src/cli/commands.rs:446-562` (FileCommands enum)
- **Batch subcommands**: `src/cli/commands.rs:538-562` (BatchCommands enum)
- **Command execution**: `src/cli/commands.rs:720-800` (execute_file_command)
- **Batch execution**: `src/cli/commands.rs:776-777` (execute_file_batch)
- **Implementation functions**: `src/cli/commands.rs:2700-3000` (various execute_file_* functions)

## Proposed New Structure

### Consolidated Command Design
```
xv file upload <files...>                # Upload one or more files
xv file download <files...>              # Download one or more files
xv file delete <files...>                # Delete one or more files
xv file list                             # List files (unchanged)
xv file info <file>                      # Get file info (unchanged)
xv file sync <path>                      # Sync directory (unchanged)
```

### Command Definition Changes

```rust
pub enum FileCommands {
    /// Upload one or more files to blob storage
    Upload {
        /// Local file path(s) to upload (can specify multiple)
        #[arg(required = true, num_args = 1..)]
        files: Vec<String>,
        
        /// Remote name (only valid when uploading single file)
        #[arg(short, long, conflicts_with = "recursive")]
        name: Option<String>,
        
        /// Upload directory recursively
        #[arg(short = 'r', long)]
        recursive: bool,
        
        /// Groups to assign to uploaded file(s)
        #[arg(short, long)]
        group: Vec<String>,
        
        /// Metadata key-value pairs for all files
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        metadata: Vec<(String, String)>,
        
        /// Tags key-value pairs for all files
        #[arg(short, long, value_parser = parse_key_val::<String, String>)]
        tag: Vec<(String, String)>,
        
        /// Content type override (only valid for single file)
        #[arg(long)]
        content_type: Option<String>,
        
        /// Show progress during upload
        #[arg(long)]
        progress: bool,
        
        /// Continue on error when uploading multiple files
        #[arg(long)]
        continue_on_error: bool,
    },
    
    /// Download one or more files from blob storage
    Download {
        /// Remote file name(s) to download
        #[arg(required = true, num_args = 1..)]
        files: Vec<String>,
        
        /// Local output directory (defaults to current directory)
        #[arg(short, long)]
        output: Option<String>,
        
        /// Rename file (only valid for single file download)
        #[arg(long)]
        rename: Option<String>,
        
        /// Stream download for large files
        #[arg(long)]
        stream: bool,
        
        /// Force overwrite if file exists
        #[arg(short, long)]
        force: bool,
        
        /// Continue on error when downloading multiple files
        #[arg(long)]
        continue_on_error: bool,
    },
    
    /// Delete one or more files from blob storage
    #[command(alias = "rm")]
    Delete {
        /// Remote file name(s) to delete
        #[arg(required = true, num_args = 1..)]
        files: Vec<String>,
        
        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
        
        /// Continue on error when deleting multiple files
        #[arg(long)]
        continue_on_error: bool,
    },
    
    // List, Info, and Sync remain unchanged
    /// List files in blob storage
    #[command(alias = "ls")]
    List {
        /// Filter by prefix
        #[arg(short, long)]
        prefix: Option<String>,
        /// Filter by group
        #[arg(short, long)]
        group: Option<String>,
        /// Include metadata in output
        #[arg(long)]
        metadata: bool,
        /// Maximum number of results
        #[arg(long)]
        limit: Option<usize>,
    },
    
    /// Get file information
    Info {
        /// Remote file name
        file: String,
    },
    
    /// Sync files between local and remote
    Sync {
        /// Local directory path
        local_path: String,
        /// Remote prefix (optional)
        #[arg(short, long)]
        prefix: Option<String>,
        /// Direction: upload, download, or both
        #[arg(short, long, default_value = "up")]
        direction: SyncDirection,
        /// Dry run (show what would be done)
        #[arg(long)]
        dry_run: bool,
        /// Delete remote files not in local
        #[arg(long)]
        delete: bool,
    },
}
```

## Implementation Plan

### Phase 1: Command Structure Refactoring ✅ COMPLETED

#### Step 1.1: Update Command Definitions
- [x] Remove `BatchCommands` enum from `src/cli/commands.rs`
- [x] Remove `Batch` variant from `FileCommands` enum
- [x] Update `Upload` command to accept `Vec<String>` for files
- [x] Update `Download` command to accept `Vec<String>` for files
- [x] Update `Delete` command to accept `Vec<String>` for files
- [x] Add validation attributes (conflicts_with, num_args)
- [x] Add `continue_on_error` flag for batch operations

#### Step 1.2: Update Command Execution Logic
- [x] Modify `execute_file_command()` to handle the new structure
- [x] Remove `execute_file_batch()` function
- [x] Update pattern matching to handle new command variants

### Phase 2: Implementation Logic Updates ✅ COMPLETED

#### Step 2.1: Consolidate Upload Logic
- [x] Merge `execute_file_upload()` and batch upload logic
- [x] Add logic to detect single vs multiple file upload
- [x] Validate that `--name` and `--content-type` only work with single file
- [x] Implement `--recursive` flag for directory uploads
- [x] Add progress aggregation for multiple files
- [x] Implement `--continue-on-error` behavior

#### Step 2.2: Consolidate Download Logic
- [x] Create unified `execute_file_download()` function
- [x] Handle single file with optional rename
- [x] Handle multiple files to directory
- [x] Validate `--rename` only works with single file
- [x] Implement `--continue-on-error` behavior
- [x] Add progress tracking for multiple downloads

#### Step 2.3: Consolidate Delete Logic
- [x] Merge single and batch delete logic
- [x] Add confirmation prompt for multiple files without --force
- [x] Implement `--continue-on-error` behavior
- [x] Add summary report for batch deletions

### Phase 3: Error Handling and Reporting (Partially Complete)

#### Step 3.1: Batch Operation Error Handling
- [ ] Create `BatchOperationResult` struct to track successes/failures
- [x] Implement error accumulation for batch operations
- [x] Add summary reporting at end of batch operations
- [x] Respect `--continue-on-error` flag

#### Step 3.2: User Feedback
- [ ] Add progress bars for batch operations
- [x] Show current file being processed
- [x] Display success/failure count during operation
- [x] Provide detailed error report at end

### Phase 4: Testing

#### Step 4.1: Update Existing Tests
- [ ] Update unit tests to reflect new command structure
- [ ] Remove tests for batch subcommands
- [ ] Add tests for multiple file operations

#### Step 4.2: Add New Tests
- [ ] Test single file operations (unchanged behavior)
- [ ] Test multiple file operations
- [ ] Test validation (--name with multiple files should fail)
- [ ] Test --continue-on-error behavior
- [ ] Test recursive directory upload
- [ ] Test progress reporting

### Phase 5: Documentation Updates

#### Step 5.1: Update Help Text
- [ ] Update command descriptions in code
- [ ] Add examples in help text
- [ ] Document new flags and their behavior

#### Step 5.2: Update README
- [ ] Remove batch command documentation
- [ ] Update examples to show new syntax
- [ ] Add migration guide section

#### Step 5.3: Update CLAUDE.md
- [ ] Document the simplified command structure
- [ ] Update development notes

## Migration Guide

### For Users

#### Before (Old Syntax)
```bash
# Single file operations
xv file upload config.json
xv file download config.json
xv file delete config.json

# Multiple file operations
xv file batch upload file1.txt file2.txt file3.txt
xv file batch delete file1.txt file2.txt file3.txt
```

#### After (New Syntax)
```bash
# Single file operations (unchanged)
xv file upload config.json
xv file download config.json
xv file delete config.json

# Multiple file operations (simpler)
xv file upload file1.txt file2.txt file3.txt
xv file upload *.json --group production
xv file delete file1.txt file2.txt file3.txt

# New capabilities
xv file upload ./configs --recursive
xv file download *.log --output ./logs/
xv file upload *.json --continue-on-error
```

### Breaking Changes
1. `xv file batch upload` command removed - use `xv file upload` instead
2. `xv file batch delete` command removed - use `xv file delete` instead

### Deprecation Strategy
- Consider adding a deprecation warning for `batch` subcommand in v0.2.0
- Remove `batch` subcommand entirely in v0.3.0
- Or make a clean break since the tool is still in early development (v0.1.0)

## Benefits

### User Experience
1. **Simplified mental model** - one command handles both single and multiple files
2. **Less typing** - removes need for `batch` keyword
3. **Better discoverability** - users naturally try multiple files
4. **Familiar patterns** - matches Unix tool conventions

### Code Maintenance
1. **Less code duplication** - single implementation for each operation
2. **Simpler command structure** - fewer nested enums
3. **Easier to test** - unified code paths
4. **Better error handling** - consistent approach for all operations

### Future Extensibility
1. **Glob pattern support** - easier to add wildcard expansion
2. **Recursive operations** - natural extension of multiple file handling
3. **Parallel uploads** - can optimize batch operations transparently

## Implementation Timeline

- **Phase 1**: 2-3 hours - Command structure refactoring
- **Phase 2**: 3-4 hours - Implementation logic updates
- **Phase 3**: 2 hours - Error handling and reporting
- **Phase 4**: 2-3 hours - Testing
- **Phase 5**: 1 hour - Documentation updates

**Total estimated time**: 10-13 hours

## Risks and Mitigation

### Risk 1: Breaking Existing Scripts
- **Risk**: Users may have scripts using `file batch` commands
- **Mitigation**: Add deprecation period or maintain aliases temporarily

### Risk 2: Ambiguous Command Intent
- **Risk**: Not clear if user wants to upload directory or files in directory
- **Mitigation**: Require `--recursive` flag for directory operations

### Risk 3: Performance Regression
- **Risk**: Sequential processing of multiple files may be slow
- **Mitigation**: Implement parallel processing in Phase 2

## Success Criteria

1. All existing single-file operations work identically
2. Multiple file operations work without `batch` subcommand
3. Clear error messages for invalid flag combinations
4. Performance is same or better for batch operations
5. All tests pass
6. Documentation is clear and complete

## Alternative Approaches Considered

### Alternative 1: Keep Batch as Alias
- Keep `batch upload` as hidden alias to `upload` for compatibility
- **Rejected**: Adds complexity without clear benefit

### Alternative 2: Separate Commands
- Have `upload-many`, `download-many`, `delete-many` commands
- **Rejected**: Still requires users to think about single vs multiple

### Alternative 3: Interactive Mode
- Prompt user when multiple files detected
- **Rejected**: Breaks scripting and automation

## Conclusion

This refactoring will significantly improve the user experience of the xv CLI tool by removing unnecessary command nesting and making file operations more intuitive. The implementation is straightforward and can be completed without breaking changes to single-file operations.

The key principle is: **commands should do what users expect** - and users expect file commands to handle multiple files naturally, just like standard Unix tools.