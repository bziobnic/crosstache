# Recursive Upload with Directory Structure Preservation

## Executive Summary

This document outlines the implementation plan for enhancing the recursive upload feature to preserve directory structure in Azure Blob Storage. Currently, recursive uploads flatten all files into the blob container root. The enhanced implementation will maintain the original directory hierarchy using blob name prefixes.

## Problem Statement

### Current Behavior
- `xv file upload ./docs --recursive` uploads all files to the container root
- Files like `docs/api/v1/users.md` become just `users.md` in blob storage
- Directory structure is lost, making it impossible to reconstruct the original organization
- Name conflicts occur when files with same names exist in different directories

### Desired Behavior
- Files should maintain their relative path structure in blob names
- `docs/api/v1/users.md` should be stored as `docs/api/v1/users.md` in blob storage
- Support for different prefix strategies (absolute, relative, custom)
- Ability to download and reconstruct the original directory structure

## Implementation Checklist

### Phase 1: Analysis and Design

#### 1.1 Current State Analysis
- [ ] Review current `execute_file_upload_recursive()` implementation in `src/cli/commands.rs`
- [ ] Analyze how `collect_files_recursive()` gathers files
- [ ] Document current blob naming strategy (just filename)
- [ ] Identify Azure Blob Storage path separator conventions (typically `/`)
- [ ] Review how Azure Portal and Storage Explorer handle "folders"

#### 1.2 Design Decisions
- [ ] Choose path preservation strategy:
  - [ ] Option A: Preserve full relative path from command execution point
  - [ ] Option B: Preserve path relative to specified base directory
  - [ ] Option C: Allow user to specify custom prefix strategy
- [ ] Define path separator standardization (convert `\` to `/` on Windows)
- [ ] Determine handling of absolute vs relative paths
- [ ] Design CLI flag interface for controlling behavior

#### 1.3 Edge Cases Documentation
- [ ] Files with same name in different directories
- [ ] Very deep directory structures (path length limits)
- [ ] Special characters in directory names
- [ ] Hidden directories and files (`.git`, `.env`, etc.)
- [ ] Symbolic links and directory loops
- [ ] Empty directories (blob storage doesn't support empty "folders")

### Phase 2: Core Implementation

#### 2.1 Update Data Structures
```rust
// Add to execute_file_upload_recursive or create new struct
struct FileUploadInfo {
    local_path: PathBuf,      // Full local file path
    relative_path: String,    // Relative path for blob name
    blob_name: String,        // Final blob name with prefix
}
```

- [ ] Create `FileUploadInfo` struct to track path mappings
- [ ] Add `base_path` parameter to track the root directory
- [ ] Add `preserve_structure` flag to control behavior
- [ ] Add `path_prefix` option for custom prefixes

#### 2.2 Modify collect_files_recursive Function
- [ ] Accept base path parameter to calculate relative paths
- [ ] Return Vec<FileUploadInfo> instead of Vec<PathBuf>
- [ ] Calculate relative path for each file:
  ```rust
  let relative = file_path.strip_prefix(&base_path)?;
  let blob_name = relative.to_slash_lossy(); // Convert to forward slashes
  ```
- [ ] Handle path separator normalization (Windows `\` → `/`)

#### 2.3 Update execute_file_upload_recursive Function
- [ ] Process FileUploadInfo structs instead of simple paths
- [ ] Pass blob_name (with path) to blob storage instead of just filename
- [ ] Update progress messages to show relative paths
- [ ] Implement path prefix options:
  ```rust
  let final_blob_name = match path_prefix {
      Some(prefix) => format!("{}/{}", prefix, relative_path),
      None => relative_path,
  };
  ```

#### 2.4 Blob Storage Integration
- [ ] Update `BlobManager::upload_file` calls to use full blob paths
- [ ] Ensure blob names are properly URL-encoded if needed
- [ ] Verify Azure SDK handles "folder" paths correctly
- [ ] Test that Azure Portal shows proper folder structure

### Phase 3: CLI Interface Updates

#### 3.1 Add New Command Flags
```rust
/// Upload one or more files to blob storage
Upload {
    // ... existing fields ...
    
    /// Preserve directory structure when uploading recursively
    #[arg(long, requires = "recursive")]
    preserve_structure: bool,
    
    /// Base directory for relative path calculation (defaults to current directory)
    #[arg(long, requires = "recursive")]
    base_dir: Option<String>,
    
    /// Prefix to add to all uploaded blob names
    #[arg(long)]
    prefix: Option<String>,
    
    /// Exclude patterns (gitignore style)
    #[arg(long, value_name = "PATTERN")]
    exclude: Vec<String>,
}
```

- [ ] Add `--preserve-structure` flag (or make it default behavior)
- [ ] Add `--base-dir` flag for custom base directory
- [ ] Add `--prefix` flag for blob name prefixes
- [ ] Add `--exclude` patterns for filtering files
- [ ] Add `--flatten` flag to maintain backward compatibility

#### 3.2 Update Command Validation
- [ ] Validate `--preserve-structure` only works with `--recursive`
- [ ] Validate `--base-dir` path exists and is a directory
- [ ] Ensure `--prefix` doesn't conflict with `--name`
- [ ] Validate exclude patterns syntax

### Phase 4: Recursive Download Support

#### 4.1 Download Pattern Detection
- [ ] Detect when a download target is a prefix/directory (no file extension or trailing `/`)
- [ ] Support both explicit patterns: `docs/*` and implicit: `docs` 
- [ ] Auto-detect recursive intent from blob prefix patterns
- [ ] Handle single file vs directory prefix disambiguation

#### 4.2 Implement Directory Recreation on Download
- [ ] Add `--recursive` flag to Download command (optional, auto-detect from pattern)
- [ ] Create local directories as needed during download
- [ ] Preserve blob "folder" structure locally
- [ ] Handle path separator conversion (`/` → platform-specific)
- [ ] Support `--flatten` flag to download without preserving structure

#### 4.3 Batch Download with Structure
```rust
async fn execute_file_download_recursive(
    blob_manager: &BlobManager,
    prefix: String,
    output_dir: String,
    force: bool,
    preserve_structure: bool,
    continue_on_error: bool,
) -> Result<()>
```

- [ ] List all blobs with given prefix using `BlobManager::list_files()`
- [ ] Filter results to match the prefix pattern
- [ ] Create directory structure locally before downloading files
- [ ] Download files maintaining relative paths from prefix
- [ ] Show progress with directory creation and file count
- [ ] Handle name conflicts with force/skip/rename options

#### 4.4 Smart Pattern Matching
- [ ] Support exact prefix: `xv file download docs` → downloads all blobs starting with `docs/`
- [ ] Support wildcard: `xv file download "docs/*.md"` → downloads all .md files under docs/
- [ ] Support recursive wildcard: `xv file download "docs/**/*.md"` → all .md files in any subdirectory
- [ ] Support multiple patterns: `xv file download docs images` → downloads both prefixes
- [ ] Auto-detect when pattern represents directory vs file

### Phase 5: Enhanced Features

#### 5.1 Filtering and Exclusions
- [ ] Implement gitignore-style exclusion patterns
- [ ] Add default exclusions (`.git`, `node_modules`, etc.)
- [ ] Support for `.xvignore` file for project-specific exclusions
- [ ] Add `--include` patterns for selective uploads

#### 5.2 Progress and Reporting
- [ ] Show directory traversal progress
- [ ] Display tree structure of files to be uploaded
- [ ] Add `--dry-run` mode to preview operations
- [ ] Generate upload manifest file

#### 5.3 Sync Functionality
- [ ] Update `file sync` command to preserve structure
- [ ] Implement directory-aware diff algorithm
- [ ] Support bidirectional sync with structure preservation
- [ ] Handle moved/renamed files intelligently

### Phase 6: Testing

#### 6.1 Unit Tests
- [ ] Test `collect_files_recursive` with structure preservation
- [ ] Test relative path calculation
- [ ] Test path separator normalization
- [ ] Test exclusion pattern matching
- [ ] Test very long path handling

#### 6.2 Integration Tests
- [ ] Test uploading nested directory structure
- [ ] Test downloading with structure recreation
- [ ] Test Windows/Unix path compatibility
- [ ] Test with symbolic links
- [ ] Test with special characters in paths

#### 6.3 End-to-End Tests
- [ ] Upload directory → verify in Azure Portal → download → compare
- [ ] Test round-trip preservation of structure
- [ ] Test with real-world directory structures
- [ ] Performance testing with large directory trees

### Phase 7: Documentation

#### 7.1 Update Help Text
- [ ] Document new flags in command help
- [ ] Add examples for common scenarios
- [ ] Explain path preservation behavior
- [ ] Document exclusion patterns syntax

#### 7.2 README Updates
- [ ] Add recursive upload section with examples
- [ ] Document directory structure preservation
- [ ] Show before/after blob naming examples
- [ ] Include migration guide for existing users

#### 7.3 CLAUDE.md Updates
- [ ] Document implementation approach
- [ ] Note Azure Blob Storage "folder" conventions
- [ ] Explain path mapping strategy
- [ ] Document any limitations

## Usage Examples

### Basic Recursive Upload with Structure
```bash
# Upload entire docs directory, preserving structure
xv file upload ./docs --recursive --preserve-structure

# Result in blob storage:
# docs/README.md
# docs/api/v1/users.md
# docs/api/v2/users.md
# docs/guides/quickstart.md
```

### Custom Prefix
```bash
# Upload with custom prefix
xv file upload ./src --recursive --preserve-structure --prefix "backup/2024-01-15"

# Result in blob storage:
# backup/2024-01-15/src/main.rs
# backup/2024-01-15/src/utils/helpers.rs
```

### Excluding Patterns
```bash
# Upload excluding certain patterns
xv file upload . --recursive --preserve-structure --exclude "*.log" --exclude "node_modules"
```

### Recursive Downloads

#### Simple Prefix Download
```bash
# Download all files under "docs" prefix (auto-detects recursive need)
xv file download docs

# Result locally (preserves structure):
# ./docs/README.md
# ./docs/api/v1/users.md
# ./docs/api/v2/users.md
# ./docs/guides/quickstart.md
```

#### Download to Custom Directory
```bash
# Download all files under "docs/" prefix to custom location
xv file download docs --output ./restored-docs

# Result locally:
# ./restored-docs/docs/README.md
# ./restored-docs/docs/api/v1/users.md
# ./restored-docs/docs/api/v2/users.md
```

#### Download with Flatten
```bash
# Download all files without preserving directory structure
xv file download docs --flatten

# Result locally (all files in current directory):
# ./README.md
# ./users.md (warning: name conflict if multiple users.md exist)
# ./quickstart.md
```

#### Pattern-Based Downloads
```bash
# Download only markdown files from docs
xv file download "docs/**/*.md"

# Download from multiple prefixes
xv file download docs images configs

# Download specific subdirectory
xv file download "docs/api/v2"
```

## Implementation Priority

1. **High Priority (Core Functionality)**
   - Basic structure preservation for upload
   - Path separator normalization
   - Relative path calculation

2. **Medium Priority (Enhanced UX)**
   - Exclusion patterns
   - Custom prefixes
   - Download with structure recreation

3. **Low Priority (Nice-to-Have)**
   - Dry-run mode
   - Upload manifest
   - Smart sync with structure

## Technical Considerations

### Recursive Download Auto-Detection
When a user runs `xv file download docs`, the system should:
1. Check if "docs" matches any exact blob name → single file download
2. If no exact match, check if any blobs have "docs/" prefix → recursive download
3. List all blobs with "docs/" prefix
4. Download each blob preserving the path structure after "docs/"
5. Create local directories as needed

This allows intuitive commands like:
- `xv file download docs` → downloads entire docs directory
- `xv file download config.json` → downloads single file
- `xv file download "*.json"` → downloads all JSON files (with --recursive if in subdirs)

### Azure Blob Storage "Folders"
- Blob storage is flat; "folders" are just name prefixes
- Use `/` as path separator (Azure convention)
- Empty "folders" cannot exist without files
- Azure Portal/Explorer interpret `/` as folder separator

### Path Length Limits
- Azure Blob names: max 1024 characters
- Consider truncation strategy for very deep paths
- Warning when approaching limits

### Performance
- Batch upload API calls where possible
- Parallel uploads for better performance
- Progress indication for large directory trees

### Cross-Platform Compatibility
- Handle Windows `\` vs Unix `/` paths
- Use `path_slash` crate or similar for conversion
- Test on Windows, macOS, and Linux

## Success Criteria

- [ ] Can upload directory tree and see proper "folder" structure in Azure Portal
- [ ] Can download with `--recursive` and recreate original structure
- [ ] Path preservation works identically on Windows/Mac/Linux
- [ ] No regression in single-file upload functionality
- [ ] Clear documentation and examples
- [ ] Performance acceptable for directories with 1000+ files

## Alternative Approaches Considered

### Alternative 1: Tar/Zip Archive
- Upload directory as compressed archive
- **Rejected**: Loses individual file access in blob storage

### Alternative 2: Metadata-Based Structure
- Store structure in blob metadata
- **Rejected**: Incompatible with Azure Portal folder view

### Alternative 3: Separate Manifest File
- Upload manifest describing directory structure
- **Rejected**: Adds complexity, not standard practice

## Conclusion

Implementing directory structure preservation will significantly improve the usability of recursive uploads, making the tool more suitable for backing up or migrating entire directory trees while maintaining their organization in Azure Blob Storage.