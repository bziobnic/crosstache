# File Sync Command Implementation Checklist

## Overview
Complete the implementation of the `xv file sync` command that currently exists but is not yet functional. The command structure is already defined in `src/cli/commands.rs` with basic parameters.

## Current State
The `file sync` command is already defined in the CLI with the following structure:
- **Command**: `xv file sync <local_path> [OPTIONS]`
- **Current Options**:
  - `local_path`: Local directory path (required positional argument)
  - `--prefix, -p`: Remote prefix (optional)
  - `--direction, -d`: Direction - `up`, `down`, or `both` (default: `up`)
  - `--dry-run`: Show what would be done without making changes
  - `--delete`: Delete remote files not in local (for upload direction)
- **Implementation Status**: Skeleton only - prints parameters but doesn't perform sync

## Phase 1: Complete Basic Implementation

### Fix Current Implementation
- [x] Command structure already exists in `FileCommands::Sync`
- [x] `SyncDirection` enum already defined (Up, Down, Both)
- [ ] Replace TODO placeholder in `execute_file_sync()` with actual implementation
- [ ] Connect to existing blob storage infrastructure (already used for file upload/download)

## Phase 2: Core Sync Implementation

### Implement Sync Logic in `execute_file_sync()`
- [ ] Implement upload sync (`SyncDirection::Up`):
  - [ ] Walk local directory tree
  - [ ] Calculate file checksums
  - [ ] Compare with remote blob storage
  - [ ] Upload new/changed files
  - [ ] Handle `--delete` flag for remote cleanup
- [ ] Implement download sync (`SyncDirection::Down`):
  - [ ] List remote blobs with optional prefix
  - [ ] Compare with local files
  - [ ] Download new/changed files
  - [ ] Preserve file permissions and timestamps
- [ ] Implement bidirectional sync (`SyncDirection::Both`):
  - [ ] Detect conflicts (files changed on both sides)
  - [ ] Apply simple conflict resolution (newer wins by default)
  - [ ] Report conflicts to user
- [ ] Implement dry-run mode:
  - [ ] Show planned operations without executing
  - [ ] Display file count and size statistics
  - [ ] Color-coded output for different operations

### Create Supporting Modules

#### Create `src/blob/sync.rs`
- [ ] Define `SyncOperation` enum:
  - [ ] `Upload(path, size)`
  - [ ] `Download(name, size)`
  - [ ] `Delete(name)`
  - [ ] `Skip(reason)`
  - [ ] `Conflict(path, resolution)`
- [ ] Define `SyncReport` struct:
  - [ ] Files uploaded/downloaded/deleted
  - [ ] Total bytes transferred
  - [ ] Conflicts encountered
  - [ ] Errors list
- [ ] Implement sync helpers:
  - [ ] `compare_files()` - Compare local vs remote using checksums
  - [ ] `calculate_checksum()` - SHA256 for file content
  - [ ] `should_sync()` - Determine if file needs syncing
  - [ ] `resolve_conflict()` - Basic conflict resolution

#### Enhance `src/blob/manager.rs`
- [ ] Add batch operations for efficiency:
  - [ ] `list_blobs_with_metadata()` - Get all blobs with checksums
  - [ ] `upload_batch()` - Upload multiple files in parallel
  - [ ] `download_batch()` - Download multiple files in parallel
- [ ] Add checksum support:
  - [ ] Store MD5/SHA256 in blob metadata
  - [ ] Validate checksums after transfer
- [ ] Add prefix-based operations:
  - [ ] List blobs by prefix efficiently
  - [ ] Delete blobs by prefix

## Phase 3: Enhanced Features

### Add Pattern Matching and Filtering
- [ ] Implement glob pattern support:
  - [ ] Use `glob` crate for pattern matching
  - [ ] Support patterns like `*.env`, `**/*.json`
  - [ ] Handle exclusion patterns
- [ ] Add `.xvignore` file support:
  - [ ] Parse ignore file similar to `.gitignore`
  - [ ] Apply ignore rules during sync
  - [ ] Support comments and negation patterns
- [ ] Add group-based filtering:
  - [ ] Filter by blob metadata groups
  - [ ] Support multiple group selection

### Add Progress and Reporting
- [ ] Implement progress bars:
  - [ ] Use `indicatif` crate for progress display
  - [ ] Show file count and bytes transferred
  - [ ] Display current file being processed
- [ ] Create detailed sync reports:
  - [ ] Summary of operations performed
  - [ ] List of errors and warnings
  - [ ] Time taken and transfer rates
- [ ] Add verbose mode output:
  - [ ] Show detailed operations
  - [ ] Display checksums and metadata
  - [ ] Include skip reasons

## Phase 4: Testing

### Unit Tests
- [ ] Test sync operation logic:
  - [ ] File comparison algorithms
  - [ ] Checksum calculation
  - [ ] Conflict detection
  - [ ] Path normalization
- [ ] Test pattern matching:
  - [ ] Glob pattern evaluation
  - [ ] Ignore file parsing
  - [ ] Exclusion rules
- [ ] Test error handling:
  - [ ] Network failures
  - [ ] Permission errors
  - [ ] Invalid paths

### Integration Tests (`tests/blob_sync_tests.rs`)
- [ ] Test upload sync:
  - [ ] Single file upload
  - [ ] Directory upload
  - [ ] Upload with prefix
  - [ ] Upload with delete flag
- [ ] Test download sync:
  - [ ] Single file download
  - [ ] Directory download
  - [ ] Download with prefix
  - [ ] Overwrite handling
- [ ] Test bidirectional sync:
  - [ ] No conflicts scenario
  - [ ] Conflict detection
  - [ ] Conflict resolution
- [ ] Test dry-run mode:
  - [ ] Verify no changes made
  - [ ] Verify correct operations reported
- [ ] Test edge cases:
  - [ ] Empty directories
  - [ ] Large files (>10MB)
  - [ ] Special characters in filenames
  - [ ] Symlinks and special files

## Phase 5: Documentation and Examples

### Update Documentation
- [ ] Update README.md:
  - [ ] Add sync command documentation
  - [ ] Include usage examples
  - [ ] Document sync directions
  - [ ] Explain prefix usage
- [ ] Add inline documentation:
  - [ ] Document sync functions
  - [ ] Add examples in doc comments
  - [ ] Explain algorithms used
- [ ] Create user guide:
  - [ ] Common sync scenarios
  - [ ] Best practices
  - [ ] Troubleshooting guide

### Create Example Scripts
- [ ] Basic sync examples:
  ```bash
  # Upload local directory to blob storage
  xv file sync ./data --direction up
  
  # Download all files with prefix
  xv file sync ./backup --prefix "2024/" --direction down
  
  # Bidirectional sync with dry run
  xv file sync ./config --direction both --dry-run
  
  # Upload and clean up deleted files
  xv file sync ./docs --direction up --delete
  ```

## Phase 6: Performance and Polish

### Optimize Performance
- [ ] Implement parallel uploads/downloads:
  - [ ] Use tokio tasks for concurrent operations
  - [ ] Limit concurrent operations (default: 5)
  - [ ] Add `--parallel` flag to control concurrency
- [ ] Add chunked transfer for large files:
  - [ ] Use existing chunk size configuration
  - [ ] Show progress for individual large files
- [ ] Implement incremental sync:
  - [ ] Cache last sync timestamps
  - [ ] Skip unchanged files based on metadata
  - [ ] Add `--force` flag to ignore cache

### Error Handling and Resilience
- [ ] Implement retry logic:
  - [ ] Retry failed operations with exponential backoff
  - [ ] Maximum retry count (default: 3)
  - [ ] Log retry attempts
- [ ] Add partial sync recovery:
  - [ ] Track successful operations
  - [ ] Allow resume of interrupted sync
  - [ ] Report partial success
- [ ] Improve error messages:
  - [ ] Clear descriptions of failures
  - [ ] Actionable recovery suggestions
  - [ ] Differentiate transient vs permanent errors

## Implementation Priority

### MVP (Minimum Viable Product)
1. **Complete basic sync implementation** in `execute_file_sync()`:
   - Upload sync (local → blob storage)
   - Download sync (blob storage → local)
   - Dry-run mode showing planned operations
   - Basic error handling

2. **Core functionality**:
   - Directory traversal with `walkdir`
   - File checksum calculation with `sha2`
   - Prefix-based filtering
   - Delete flag support for upload

3. **Testing**:
   - Basic integration tests
   - Error case handling

### Phase 2 Enhancements
1. **Bidirectional sync**:
   - Conflict detection
   - Simple resolution (newer wins)
   - Conflict reporting

2. **Progress and reporting**:
   - Progress bars with `indicatif`
   - Detailed sync reports
   - Verbose mode

3. **Pattern matching**:
   - Glob patterns with `glob` crate
   - `.xvignore` file support

### Phase 3 Advanced Features
1. **Performance optimizations**:
   - Parallel transfers
   - Incremental sync with caching
   - Large file handling

2. **Additional features**:
   - Group-based filtering
   - Advanced conflict resolution
   - Resume interrupted syncs

## Dependencies to Add

```toml
# Add to Cargo.toml for sync functionality
walkdir = "2.3"        # Directory traversal
sha2 = "0.10"          # SHA256 checksums
glob = "0.3"           # Pattern matching (Phase 2)
indicatif = "0.17"     # Progress bars (Phase 2)
```

## File Location Reference

- **Main sync execution**: `src/cli/commands.rs:3071` - `execute_file_sync()` function
- **CLI command definition**: `src/cli/commands.rs:520-535` - `FileCommands::Sync`
- **Blob manager**: `src/blob/manager.rs` - Existing blob storage interface
- **New sync module**: `src/blob/sync.rs` - To be created

## Success Criteria

### Phase 1 (MVP)
- [x] Command structure exists and is properly integrated
- [ ] Basic upload sync works for directories
- [ ] Basic download sync works with prefixes
- [ ] Dry-run mode shows accurate operations
- [ ] Basic error handling prevents data loss
- [ ] Integration tests pass

### Phase 2
- [ ] Bidirectional sync detects conflicts
- [ ] Progress bars show during long operations
- [ ] Pattern matching filters files correctly
- [ ] Sync reports provide useful information

### Phase 3
- [ ] Performance acceptable for >100 files
- [ ] Parallel transfers improve speed
- [ ] Advanced features work reliably
- [ ] Documentation is complete

## Next Steps

1. **Start with Phase 1**: Focus on implementing the basic sync logic in the existing `execute_file_sync()` function
2. **Use existing infrastructure**: Leverage the existing `BlobManager` for storage operations
3. **Test incrementally**: Add tests as each feature is implemented
4. **Document as you go**: Update help text and README with examples