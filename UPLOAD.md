# Upload Command Enhancement Checklist

## Overview
Modify the `xv upload` command to behave like the Unix `cp` command when handling directories and recursive operations.

## Current State
- `xv upload` currently handles single files only
- No directory handling logic exists
- No recursive flag support

## Proposed Behavior

### Single File Upload (No Change)
```bash
xv upload config.json                    # ✅ Works as before
xv file upload config.json              # ✅ Works as before
```

### Directory Upload - New Behavior
```bash
xv upload docs/                          # ❌ Refuse without -r flag
xv upload docs/ -r                       # ✅ Upload directory recursively
xv upload docs/* -r                      # ✅ Upload contents recursively
xv upload docs/*                         # ❌ Refuse if subdirectories exist without -r
```

## Implementation Checklist

### 1. Command Line Interface Updates
- [x] Add `-r, --recursive` flag to upload command
- [x] Add `-r, --recursive` flag to file upload subcommand
- [x] Update help text to document recursive behavior
- [x] Update command parsing in `src/cli/commands.rs`

### 2. Path Analysis Logic
- [x] Create function `analyze_upload_path(path: &str) -> PathType`
- [x] Implement `PathType` enum:
  ```rust
  enum PathType {
      File,
      Directory,
      GlobPattern,  // e.g., "docs/*"
  }
  ```
- [x] Create function `requires_recursive_flag(path: &str) -> bool`
- [x] Handle glob expansion for patterns like `docs/*`

### 3. Directory Detection
- [x] Implement `is_directory(path: &Path) -> bool`
- [x] Implement `contains_subdirectories(path: &Path) -> Result<bool>`
- [x] Handle symbolic links appropriately
- [x] Add proper error handling for inaccessible paths

### 4. Upload Logic Updates
- [x] Modify `execute_upload_command()` in `src/cli/commands.rs`
- [x] Modify `execute_file_upload()` in `src/cli/commands.rs`
- [x] Add validation before starting upload:
  ```rust
  fn validate_upload_request(path: &str, recursive: bool) -> Result<()>
  ```

### 5. Recursive Upload Implementation
- [x] Implement `upload_directory_recursive(path: &Path) -> Result<Vec<UploadResult>>`
- [x] Implement `upload_glob_pattern(pattern: &str, recursive: bool) -> Result<Vec<UploadResult>>`
- [x] Handle nested directory structures
- [x] Preserve directory structure in blob storage (**Decision**: Maintain full directory hierarchy)
- [x] Add progress reporting for multiple file uploads

### 6. Error Handling
- [x] Create specific error types for directory upload scenarios:
  ```rust
  // Added to CrosstacheError enum in src/error.rs
  DirectoryRequiresRecursive { path: String },
  SubdirectoriesRequireRecursive { pattern: String },
  InvalidGlobPattern { pattern: String },
  PathNotFound { path: String },
  PathPermissionDenied { path: String },
  ```
- [x] Add user-friendly error messages
- [x] Suggest using `-r` flag when appropriate

### 6B. File List Display Enhancement (ls-like behavior)

#### Overview
Update `xv file list` to behave like Unix `ls` with directory-aware display and navigation.

#### Current State
- Shows flat list: Name, Size, Content-Type, Modified, Groups
- No directory structure awareness
- No path-based filtering

#### Proposed Behavior
```bash
xv file list                    # Show root level: dirs first, then files
xv file list docs/              # Show contents of docs/ directory
xv file list docs/subdir/       # Show contents of docs/subdir/
```

#### Display Format Changes
- **From**: Name, Size, Content-Type, Modified, Groups  
- **To**: Name, Path, Size, Modified, Groups
- **Sort**: Directories first (alpha), then files (alpha)

#### Implementation Checklist

##### 1. Data Structure Design
- [x] Create `DirectoryListing` struct:
  ```rust
  #[derive(Debug, Clone)]
  pub struct DirectoryListing {
      pub directories: Vec<DirectoryEntry>,
      pub files: Vec<FileEntry>,
  }
  ```
- [x] Create `DirectoryEntry` struct:
  ```rust
  #[derive(Debug, Clone)]
  pub struct DirectoryEntry {
      pub name: String,        // "docs", "subdir"
      pub file_count: usize,   // Number of files in directory
  }
  ```
- [x] Create `FileEntry` struct:
  ```rust
  #[derive(Debug, Clone)]
  pub struct FileEntry {
      pub name: String,        // "config.json"
      pub path: String,        // "docs/config.json" 
      pub size: u64,
      pub modified: DateTime<Utc>,
      pub groups: Vec<String>,
  }
  ```

##### 2. Path Parsing Logic
- [x] Create `parse_blob_paths()` function:
  ```rust
  fn parse_blob_paths(files: Vec<FileInfo>, prefix: Option<&str>) -> DirectoryListing
  ```
- [x] Implement path segmentation using `std::path::Path`
- [x] Handle root-level files vs. files in directories
- [x] Filter files by prefix if provided (e.g., "docs/" shows docs contents)
- [x] Extract unique directory names at the current level

##### 3. Directory Detection Logic
- [ ] Create `extract_directories_at_level()` function:
  ```rust
  fn extract_directories_at_level(files: &[FileInfo], prefix: &str) -> Vec<String>
  ```
- [ ] Parse blob names to identify directories at current path level
- [ ] Handle edge cases: empty directories, nested paths
- [ ] Count files per directory for display

##### 4. Sorting and Display Logic
- [x] Implement directory-first sorting:
  ```rust
  fn sort_directory_listing(listing: &mut DirectoryListing)
  ```
- [x] Sort directories alphabetically
- [x] Sort files alphabetically within each category
- [x] Handle case-insensitive sorting

##### 5. Update File List Display
- [x] Modify `execute_file_list()` function
- [x] Replace current `FileItem` struct with new display format
- [x] Update table columns: Name, Path, Size, Modified, Groups
- [x] Add directory indicators (e.g., "/" suffix or folder icon)
- [x] Show file count for directories

##### 6. Path Navigation Support
- [x] Update `FileCommands::List` to accept path parameter:
  ```rust
  List {
      path: Option<String>,     // New: path to list (e.g., "docs/")
      group: Option<String>,    // Existing
      metadata: bool,           // Existing  
      limit: Option<usize>,     // Existing
  }
  ```
- [x] Parse path parameter in command handler
- [x] Use path as prefix filter for blob listing
- [ ] Validate path format and existence

##### 7. Display Formatting
- [x] Create directory-aware table formatter:
  ```rust
  #[derive(Tabled)]
  struct DisplayItem {
      #[tabled(rename = "Name")]
      name: String,           // "docs/" or "file.txt"
      
      #[tabled(rename = "Path")]  
      path: String,           // Full path or "<DIR>"
      
      #[tabled(rename = "Size")]
      size: String,           // "5 files" for dirs, "1.2KB" for files
      
      #[tabled(rename = "Modified")]
      modified: String,       // Latest modified time for dirs
      
      #[tabled(rename = "Groups")]
      groups: String,         // Aggregated groups for dirs
  }
  ```
- [x] Add visual indicators for directories
- [x] Handle mixed directory/file display
- [x] Show directory summary information

##### 8. Error Handling
- [x] Handle invalid path parameters
- [x] Show helpful messages for empty directories
- [x] Handle blob storage access errors
- [ ] Provide suggestions for navigation

##### 9. Command Line Integration  
- [x] Update help text for `file list` command
- [x] Add examples: `xv file list docs/`
- [x] Update command parsing logic
- [x] Maintain backward compatibility

##### 10. Testing
- [ ] Unit tests for path parsing logic
- [ ] Test directory detection with various blob structures
- [ ] Test sorting behavior (directories first)
- [ ] Test path navigation (filtering by prefix)
- [ ] Integration tests with actual blob data
- [ ] Test edge cases: empty dirs, deep nesting, special characters

#### Example Output Format
```
$ xv file list
Name          Path          Size      Modified            Groups
docs/         <DIR>         3 files   2024-08-28 10:30   app,config  
config.json   config.json   1.2KB     2024-08-28 09:15   root
README.md     README.md     2.5KB     2024-08-28 08:45   docs

$ xv file list docs/
Name          Path              Size      Modified            Groups
subdir/       <DIR>             2 files   2024-08-28 10:25   app
app.json      docs/app.json     856B      2024-08-28 10:30   app,config
settings.yml  docs/settings.yml 445B      2024-08-28 10:28   config
```

##### 11. Implementation Notes
- [ ] Use `indexmap::IndexMap` for ordered directory structures if needed
- [ ] Leverage existing `tabled` crate for display formatting
- [ ] Consider performance with large numbers of blobs
- [ ] Handle Unicode characters in file/directory names
- [ ] Add caching for directory structure if needed

### 7. Blob Storage Integration
- [x] Update `BlobManager::upload_file()` to handle multiple files
- [x] Implement `BlobManager::upload_directory()` method
- [x] **Directory structure preservation**: Maintain full directory structure in blob storage (`docs/file.txt` → `docs/file.txt`)
- [x] Handle blob name conflicts
- [x] Add batch upload optimization
- [x] Update file list display format:
  - Change columns from: Name, Size, Content-Type, Modified, Groups
  - To: Name, Path, Size, Modified, Groups
- [x] Update default sort order: Alpha ascending by folder, then file name

### 8. Progress Reporting
- [ ] Show progress for single file uploads (existing)
- [ ] Show progress for multiple file uploads (new)
- [ ] Display current file being uploaded
- [ ] Show overall progress: "Uploading file 3 of 15..."
- [ ] Handle upload failures gracefully (continue with remaining files)

### 9. Validation Rules Implementation

#### Rule 1: Directory without -r flag
```bash
xv upload docs/  # Should fail with helpful message
```
- [ ] Detect when path is a directory
- [ ] Check if recursive flag is provided
- [ ] Return error: "docs/ is a directory. Use -r to upload recursively."

#### Rule 2: Glob pattern with subdirectories
```bash
xv upload docs/*  # Should fail if subdirs exist without -r
```
- [ ] Expand glob pattern to file list
- [ ] Check if any expanded paths are directories
- [ ] Return error: "Pattern includes subdirectories. Use -r to upload recursively."

#### Rule 3: Successful recursive uploads
```bash
xv upload docs/ -r     # Should upload all files in docs/ recursively
xv upload docs/* -r    # Should upload all contents of docs/ recursively
```

### 10. Testing

#### Unit Tests
- [ ] Test `analyze_upload_path()` with various inputs
- [ ] Test `requires_recursive_flag()` logic
- [ ] Test `contains_subdirectories()` detection
- [ ] Test glob pattern expansion
- [ ] Test error message generation

#### Integration Tests
- [ ] Test single file upload (regression test)
- [ ] Test directory upload without `-r` flag (should fail)
- [ ] Test directory upload with `-r` flag (should succeed)
- [ ] Test glob pattern without subdirectories (should succeed)
- [ ] Test glob pattern with subdirectories without `-r` (should fail)
- [ ] Test glob pattern with subdirectories with `-r` (should succeed)
- [ ] Test mixed file/directory uploads
- [ ] Test permission errors
- [ ] Test non-existent paths

#### End-to-End Tests
- [ ] Create test directory structure with files and subdirectories
- [ ] Test all command variations
- [ ] Verify blob storage contains expected files
- [ ] Test progress reporting
- [ ] Test error scenarios

### 11. Documentation Updates
- [ ] Update `README.md` with new upload examples
- [ ] Update command help text
- [ ] Add examples to `--help` output
- [ ] Document blob naming strategy for directories
- [ ] Update CLAUDE.md with implementation notes

### 12. Edge Cases to Handle
- [ ] Empty directories
- [ ] Directories with only hidden files (`.gitignore`, `.DS_Store`)
- [ ] Very deep directory structures
- [ ] Files with special characters in names
- [ ] Symbolic links (follow or ignore?)
- [ ] Very large files in directories
- [ ] Network interruptions during batch uploads
- [ ] Duplicate file names in different subdirectories

## Implementation Priority
1. **Phase 1**: Basic directory detection and validation
2. **Phase 2**: Recursive flag and simple directory uploads  
3. **Phase 3**: Glob pattern support
4. **Phase 4**: Progress reporting and error handling
5. **Phase 5**: Testing and documentation

## Files to Modify
- `src/cli/commands.rs` - Command parsing and execution
- `src/blob/manager.rs` - Upload logic
- `src/blob/models.rs` - Data structures (if needed)
- `src/error.rs` - New error types
- `tests/file_commands_tests.rs` - Test cases
- `README.md` - Documentation
- `CLAUDE.md` - Implementation notes

## Success Criteria
- [ ] All `cp`-like behaviors work correctly
- [ ] Comprehensive error messages guide users
- [ ] Existing single file uploads continue to work
- [ ] Performance is acceptable for large directories
- [ ] All edge cases are handled gracefully
- [ ] Complete test coverage
- [ ] Updated documentation