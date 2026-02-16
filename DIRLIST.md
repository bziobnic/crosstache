# Directory-Style File Listing Implementation Plan

## Executive Summary

This document outlines the plan to change the default `xv file list` behavior from showing all files recursively to displaying only immediate children (files and "directories") in the current prefix level, with directories shown with a trailing `/` character. This provides a more intuitive, filesystem-like browsing experience.

## Problem Statement

### Current Behavior
- `xv file list` shows ALL files in the container, regardless of depth
- `xv file list --prefix docs` shows ALL files under `docs/`, including deeply nested files
- No visual distinction between files and "directories" (blob prefixes)
- Output can be overwhelming for large directory structures

### Desired Behavior
- Show only immediate children at the current prefix level
- Display "directories" (common prefixes) with trailing `/`
- Sort directories first, then files (both alphabetically)
- Maintain backward compatibility with a flag for recursive listing

## Implementation Checklist

### Phase 1: Analysis and Design ✅

#### 1.1 Azure Blob Storage API Research
- [x] Review Azure SDK's delimiter-based listing capabilities
- [x] Understand how `list_blobs()` with delimiter returns `BlobPrefix` items
- [x] Identify the correct delimiter character (`/` for Azure)
- [x] Document how to extract common prefixes from API response

**Key Findings:**
- Azure SDK's `list_blobs()` supports a `delimiter` parameter
- When delimiter is set to `/`, the API returns:
  - `blobs`: Files at the current prefix level only
  - `prefixes` (or `BlobPrefix`): Common "directory" prefixes
- This is exactly what we need for directory-style listing

#### 1.2 Design Decisions
- [x] **Default Behavior**: Non-recursive (show immediate children only)
- [x] **Backward Compatibility**: Add `--recursive` or `--all` flag for old behavior
- [x] **Sort Order**: Directories first (with `/`), then files (both alphabetical)
- [x] **Prefix Handling**: When user specifies prefix, show children of that prefix
- [x] **Edge Cases**: Root listing, empty directories, no trailing slash in input

#### 1.3 Data Model Updates
```rust
// New enum to represent list items (file or directory)
pub enum BlobListItem {
    File(FileInfo),
    Directory(String),  // Directory prefix with trailing /
}
```

- [x] Define new enum to distinguish files from directories
- [x] Update return types to support mixed file/directory results
- [x] Consider serialization for JSON output format

### Phase 2: Core Implementation

#### 2.1 Update Data Models (`src/blob/models.rs`)

**Task 2.1.1**: Add `BlobListItem` enum
```rust
/// Represents either a file or a directory prefix in blob listing
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BlobListItem {
    #[serde(rename = "file")]
    File(FileInfo),
    #[serde(rename = "directory")]
    Directory {
        name: String,  // Includes trailing /
        full_path: String,
    },
}
```

**Task 2.1.2**: Update `FileListRequest`
```rust
#[derive(Debug, Clone)]
pub struct FileListRequest {
    pub prefix: Option<String>,
    pub groups: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub delimiter: Option<String>,  // NEW: For hierarchical listing
    pub recursive: bool,             // NEW: Control listing depth
}
```

#### 2.2 Implement New Listing Method (`src/blob/manager.rs`)

**Task 2.2.1**: Create `list_files_hierarchical()` method
```rust
/// List files and directories at a specific prefix level
pub async fn list_files_hierarchical(
    &self,
    request: FileListRequest
) -> Result<Vec<BlobListItem>> {
    // Use delimiter for non-recursive listing
    // Extract both files and prefixes from response
    // Convert to BlobListItem enum instances
}
```

**Implementation Steps:**
1. Create BlobServiceClient and get container_client (similar to existing `list_files`)
2. Build list_blobs request with `delimiter("/")`
3. Apply prefix filter if provided (normalize to ensure proper format)
4. Collect results from stream:
   - Extract `blobs` → convert to `BlobListItem::File`
   - Extract `blob_prefixes` → convert to `BlobListItem::Directory`
5. Apply group filtering to files only
6. Sort results: directories first, then files (alphabetically)
7. Apply limit if specified
8. Return `Vec<BlobListItem>`

**Task 2.2.2**: Update existing `list_files()` method
- Keep existing behavior for backward compatibility
- Document that it returns all files recursively
- Consider deprecation notice for future versions

**Task 2.2.3**: Handle prefix normalization
```rust
fn normalize_prefix(prefix: Option<String>) -> Option<String> {
    prefix.map(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            None
        } else if trimmed.ends_with('/') {
            Some(trimmed.to_string())
        } else {
            Some(format!("{}/", trimmed))
        }
    }).flatten()
}
```

#### 2.3 Update CLI Commands (`src/cli/commands.rs`)

**Task 2.3.1**: Add `--recursive` flag to List command
```rust
/// List files in blob storage
List {
    /// Optional prefix to filter files
    #[arg(short, long)]
    prefix: Option<String>,

    /// Filter by group
    #[arg(short, long)]
    group: Option<String>,

    /// Include file metadata in output
    #[arg(short, long)]
    metadata: bool,

    /// Maximum number of results to return
    #[arg(short, long)]
    limit: Option<usize>,

    /// List all files recursively (old behavior)
    #[arg(short, long)]
    recursive: bool,  // NEW FLAG
},
```

**Task 2.3.2**: Update `execute_file_list()` function signature
```rust
async fn execute_file_list(
    blob_manager: &BlobManager,
    prefix: Option<String>,
    group: Option<String>,
    include_metadata: bool,
    limit: Option<usize>,
    recursive: bool,        // NEW PARAMETER
    config: &Config,
) -> Result<()>
```

**Task 2.3.3**: Implement new listing logic
```rust
// Create list request
let list_request = FileListRequest {
    prefix,
    groups: group.map(|g| vec![g]),
    limit,
    delimiter: if recursive { None } else { Some("/".to_string()) },
    recursive,
};

// Call appropriate method based on recursive flag
let items = if recursive {
    // Old behavior: flat list of all files
    let files = blob_manager.list_files(list_request).await?;
    files.into_iter().map(BlobListItem::File).collect()
} else {
    // New behavior: hierarchical listing
    blob_manager.list_files_hierarchical(list_request).await?
};
```

**Task 2.3.4**: Update display logic for mixed items
```rust
#[derive(Tabled)]
struct ListItem {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Size")]
    size: String,
    #[tabled(rename = "Content-Type")]
    content_type: String,
    #[tabled(rename = "Modified")]
    modified: String,
    #[tabled(rename = "Groups")]
    groups: String,
}

let items: Vec<ListItem> = blob_items
    .iter()
    .map(|item| match item {
        BlobListItem::Directory { name, .. } => ListItem {
            name: name.clone(),  // Already has trailing /
            size: "<DIR>".to_string(),
            content_type: "-".to_string(),
            modified: "-".to_string(),
            groups: "-".to_string(),
        },
        BlobListItem::File(file) => ListItem {
            name: file.name.clone(),
            size: format_size(file.size),
            content_type: file.content_type.clone(),
            modified: file.last_modified.format("%Y-%m-%d %H:%M:%S").to_string(),
            groups: file.groups.join(", "),
        },
    })
    .collect();
```

**Task 2.3.5**: Update command invocation
```rust
// In the main command match statement
FileCommand::List {
    prefix,
    group,
    metadata,
    limit,
    recursive,  // NEW
} => {
    execute_file_list(
        &blob_manager,
        prefix,
        group,
        *metadata,
        *limit,
        *recursive,  // NEW
        config,
    )
    .await?;
}
```

### Phase 3: Enhanced Features

#### 3.1 Sorting Logic

**Task 3.1.1**: Implement custom sort function
```rust
fn sort_blob_items(items: &mut Vec<BlobListItem>) {
    items.sort_by(|a, b| {
        match (a, b) {
            // Directories before files
            (BlobListItem::Directory { .. }, BlobListItem::File(_)) => std::cmp::Ordering::Less,
            (BlobListItem::File(_), BlobListItem::Directory { .. }) => std::cmp::Ordering::Greater,

            // Both directories: alphabetical
            (BlobListItem::Directory { name: n1, .. }, BlobListItem::Directory { name: n2, .. }) => {
                n1.to_lowercase().cmp(&n2.to_lowercase())
            },

            // Both files: alphabetical
            (BlobListItem::File(f1), BlobListItem::File(f2)) => {
                f1.name.to_lowercase().cmp(&f2.name.to_lowercase())
            },
        }
    });
}
```

#### 3.2 JSON Output Formatting

**Task 3.2.1**: Implement JSON serialization for mixed items
```rust
if config.output_json {
    let json_output = serde_json::to_string_pretty(&items).map_err(|e| {
        CrosstacheError::serialization(format!("Failed to serialize list items: {e}"))
    })?;
    println!("{json_output}");
} else {
    // Table output...
}
```

**Expected JSON format:**
```json
[
  {
    "type": "directory",
    "name": "api/",
    "full_path": "docs/api/"
  },
  {
    "type": "directory",
    "name": "guides/",
    "full_path": "docs/guides/"
  },
  {
    "type": "file",
    "name": "README.md",
    "size": 1024,
    "content_type": "text/markdown",
    "last_modified": "2024-01-15T10:30:00Z",
    "groups": ["public"],
    "metadata": {},
    "tags": {}
  }
]
```

#### 3.3 Size Formatting Helper

**Task 3.3.1**: Add human-readable size formatter
```rust
fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];

    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}
```

### Phase 4: Testing

#### 4.1 Unit Tests

**Task 4.1.1**: Test `normalize_prefix()` function
```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_normalize_prefix() {
        assert_eq!(normalize_prefix(None), None);
        assert_eq!(normalize_prefix(Some("".to_string())), None);
        assert_eq!(normalize_prefix(Some("docs".to_string())), Some("docs/".to_string()));
        assert_eq!(normalize_prefix(Some("docs/".to_string())), Some("docs/".to_string()));
        assert_eq!(normalize_prefix(Some(" docs ".to_string())), Some("docs/".to_string()));
    }
}
```

**Task 4.1.2**: Test `sort_blob_items()` function
```rust
#[test]
fn test_sort_blob_items() {
    let mut items = vec![
        BlobListItem::File(/* create test FileInfo */),
        BlobListItem::Directory { name: "zdir/".to_string(), full_path: "zdir/".to_string() },
        BlobListItem::File(/* another test FileInfo */),
        BlobListItem::Directory { name: "adir/".to_string(), full_path: "adir/".to_string() },
    ];

    sort_blob_items(&mut items);

    // Verify directories come first, then files, all alphabetically
    assert!(matches!(items[0], BlobListItem::Directory { name, .. } if name == "adir/"));
    assert!(matches!(items[1], BlobListItem::Directory { name, .. } if name == "zdir/"));
    // Files come after...
}
```

**Task 4.1.3**: Test `format_size()` function
```rust
#[test]
fn test_format_size() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(100), "100 B");
    assert_eq!(format_size(1024), "1.00 KB");
    assert_eq!(format_size(1048576), "1.00 MB");
    assert_eq!(format_size(1536), "1.50 KB");
}
```

#### 4.2 Integration Tests

**Task 4.2.1**: Create test file `tests/file_list_hierarchical_tests.rs`
```rust
#[tokio::test]
async fn test_hierarchical_listing_root() {
    // Setup: Upload test files in structure:
    //   - root_file.txt
    //   - dir1/file1.txt
    //   - dir2/file2.txt

    // Test: List root level without prefix
    // Expected: root_file.txt, dir1/, dir2/

    // Verify: 1 file, 2 directories
}

#[tokio::test]
async fn test_hierarchical_listing_with_prefix() {
    // Setup: Upload test files in structure:
    //   - docs/README.md
    //   - docs/api/v1/users.md
    //   - docs/guides/quickstart.md

    // Test: List with prefix "docs"
    // Expected: README.md, api/, guides/

    // Verify: 1 file, 2 directories
}

#[tokio::test]
async fn test_recursive_listing() {
    // Setup: Same structure as above

    // Test: List with --recursive flag and prefix "docs"
    // Expected: All 3 files listed

    // Verify: 3 files, no directories
}

#[tokio::test]
async fn test_empty_directory_prefix() {
    // Setup: Upload only docs/api/users.md

    // Test: List with prefix "docs"
    // Expected: api/ directory

    // Test: List with prefix "docs/api"
    // Expected: users.md file
}
```

#### 4.3 Manual Testing Checklist

- [ ] **Test 1**: List root level (`xv file list`)
  - Verify directories shown with `/`
  - Verify directories listed first
  - Verify files listed after directories

- [ ] **Test 2**: List with prefix (`xv file list --prefix docs`)
  - Verify only immediate children shown
  - Verify subdirectories displayed with `/`

- [ ] **Test 3**: List recursively (`xv file list --prefix docs --recursive`)
  - Verify all nested files shown
  - Verify no directories in output

- [ ] **Test 4**: JSON output (`xv file list --output-json`)
  - Verify proper JSON structure
  - Verify `type` field distinguishes files/directories

- [ ] **Test 5**: Empty container
  - Verify graceful "No files found" message

- [ ] **Test 6**: Prefix with no results
  - Verify "No files found" message

- [ ] **Test 7**: Deep nesting (5+ levels)
  - Verify correct level displayed
  - Verify navigation works through levels

- [ ] **Test 8**: Special characters in directory names
  - Test spaces, hyphens, underscores
  - Verify proper display and navigation

- [ ] **Test 9**: Large directory (100+ items)
  - Verify performance acceptable
  - Verify limit flag works correctly

- [ ] **Test 10**: Backward compatibility
  - Verify existing scripts using `xv file list` work (may need --recursive flag)

### Phase 5: Documentation

#### 5.1 Update Help Text

**Task 5.1.1**: Update command help in CLI definition
```rust
/// List files in blob storage
///
/// By default, lists only immediate children (files and directories) at the
/// current prefix level. Use --recursive to list all files recursively.
///
/// Directories are shown with a trailing '/' character and listed first.
///
/// Examples:
///   xv file list                        # List root level
///   xv file list --prefix docs          # List docs directory
///   xv file list --prefix docs --recursive  # List all files under docs
List { /* ... */ }
```

#### 5.2 Update README.md

**Task 5.2.1**: Add section "Directory-Style File Listing"
```markdown
### Directory-Style File Listing

The `file list` command provides filesystem-like navigation through your blob storage:

```bash
# List root level (shows files and directories)
xv file list

# Example output:
# Name           Size      Content-Type  Modified
# api/           <DIR>     -             -
# guides/        <DIR>     -             -
# README.md      1.2 KB    text/markdown 2024-01-15 10:30:00

# Navigate into a directory
xv file list --prefix api

# List all files recursively (old behavior)
xv file list --recursive
xv file list --prefix docs --recursive
```

**Features:**
- 📁 Directories shown with trailing `/` and listed first
- 🔍 Navigate through directory structure with `--prefix`
- 📊 Hierarchical view by default, use `--recursive` for flat listing
- 💾 Human-readable file sizes
- 🎨 Clean, organized output
```

#### 5.3 Update CLAUDE.md

**Task 5.3.1**: Document the hierarchical listing implementation
```markdown
### File Listing Behavior

The `file list` command has two modes:

1. **Hierarchical (default)**: Shows only immediate children at current prefix level
   - Uses Azure Blob Storage delimiter-based listing
   - Directories shown with trailing `/`
   - Sorted: directories first, then files (alphabetically)
   - Implementation: `BlobManager::list_files_hierarchical()`

2. **Recursive**: Shows all files under prefix (old behavior)
   - No delimiter used, returns all matching blobs
   - Only shows files, no directory indicators
   - Implementation: `BlobManager::list_files()`

The hierarchical mode uses Azure SDK's delimiter functionality to efficiently
extract common prefixes (directories) without loading all blobs.
```

#### 5.4 Create Migration Guide

**Task 5.4.1**: Add MIGRATION.md or section in README
```markdown
## Migration Guide: File List Behavior Change

### What Changed?
Starting in version X.Y.Z, `xv file list` shows directory-style hierarchical
listings by default instead of recursive flat listings.

### Old Behavior
```bash
xv file list --prefix docs
# Output: all files under docs/ recursively
# docs/README.md
# docs/api/v1/users.md
# docs/api/v2/users.md
```

### New Behavior
```bash
xv file list --prefix docs
# Output: immediate children only
# api/
# README.md

xv file list --prefix docs --recursive
# Output: same as old behavior (all files recursively)
```

### Update Your Scripts
If your scripts depend on the old behavior, add the `--recursive` flag:
```bash
# Old
xv file list --prefix docs | grep ".md"

# New
xv file list --prefix docs --recursive | grep ".md"
```
```

### Phase 6: Code Review and Refinement

#### 6.1 Performance Considerations

**Task 6.1.1**: Verify API call efficiency
- Confirm delimiter-based listing makes only one API call per page
- Compare performance vs recursive listing for large containers
- Consider caching for frequently-accessed prefixes

**Task 6.1.2**: Optimize sorting
- Profile sorting performance for large result sets (1000+ items)
- Consider lazy sorting only when needed
- Evaluate if Azure API returns sorted results

#### 6.2 Error Handling

**Task 6.2.1**: Add specific error cases
```rust
// Handle invalid prefix formats
if let Some(ref prefix) = request.prefix {
    if prefix.contains("//") || prefix.starts_with("/") {
        return Err(CrosstacheError::validation(
            "Prefix cannot start with '/' or contain '//'".to_string()
        ));
    }
}

// Handle Azure API errors gracefully
.map_err(|e| {
    if e.to_string().contains("ContainerNotFound") {
        CrosstacheError::not_found("Container not found".to_string())
    } else {
        CrosstacheError::azure_api(format!("Failed to list blobs: {e}"))
    }
})?
```

#### 6.3 Edge Case Handling

**Task 6.3.1**: Verify edge cases
- [ ] Empty container (no files or directories)
- [ ] Container with only files at root (no directories)
- [ ] Container with only directories (no files at current level)
- [ ] Prefix that doesn't exist
- [ ] Prefix ending with `/` vs without
- [ ] Very long directory names (>100 characters)
- [ ] Unicode characters in directory names
- [ ] Special Azure blob name characters

### Phase 7: Rollout Strategy

#### 7.1 Version Planning

**Task 7.1.1**: Determine version bump
- Breaking change → Major version bump (e.g., 0.1.x → 0.2.0)
- Include in CHANGELOG.md with prominent "Breaking Change" notice

**Task 7.1.2**: Feature flag consideration
- Consider adding temporary `--legacy-list` flag for transition period
- Document deprecation timeline if using feature flag
- Remove legacy flag in next major version

#### 7.2 Communication

**Task 7.2.1**: Update CHANGELOG.md
```markdown
## [0.2.0] - 2024-01-XX

### Breaking Changes
- **File List Default Behavior**: The `xv file list` command now shows hierarchical
  directory listings by default (like `ls` command). Previous behavior available
  with `--recursive` flag.
  - Directories shown with trailing `/` character
  - Only immediate children displayed at current prefix level
  - Use `--recursive` flag for old behavior (all files recursively)

### Added
- Directory-style file listing with hierarchical navigation
- `--recursive` flag for `file list` command
- Human-readable file sizes in list output
- Directories sorted before files in listings
```

**Task 7.2.2**: Add upgrade notice in CLI
```rust
// On first run after upgrade, show migration tip
if !recursive && is_first_run_after_upgrade() {
    eprintln!("ℹ️  Note: 'file list' now shows hierarchical listings by default.");
    eprintln!("   Use 'xv file list --recursive' for the previous behavior.");
    eprintln!("   Run 'xv file list --help' for more information.");
}
```

## Implementation Order

### Week 1: Core Implementation
1. **Day 1-2**: Phase 2.1 - Update data models
2. **Day 3-4**: Phase 2.2 - Implement hierarchical listing in BlobManager
3. **Day 5**: Phase 2.3 - Update CLI commands and execute_file_list

### Week 2: Features and Testing
1. **Day 1**: Phase 3 - Enhanced features (sorting, JSON, formatting)
2. **Day 2-3**: Phase 4.1-4.2 - Unit and integration tests
3. **Day 4**: Phase 4.3 - Manual testing with real Azure Storage
4. **Day 5**: Phase 6 - Code review, error handling, edge cases

### Week 3: Documentation and Rollout
1. **Day 1-2**: Phase 5 - Documentation updates
2. **Day 3**: Phase 7.1 - Version planning and CHANGELOG
3. **Day 4**: Final testing and validation
4. **Day 5**: Release preparation and communication

## Success Criteria

- [ ] `xv file list` shows only immediate children by default
- [ ] Directories displayed with trailing `/` character
- [ ] Directories sorted before files (both alphabetically)
- [ ] `--recursive` flag provides old behavior
- [ ] JSON output properly formatted with type discrimination
- [ ] All tests passing (unit, integration, manual)
- [ ] Documentation complete and accurate
- [ ] Performance acceptable for containers with 10,000+ blobs
- [ ] Zero regressions in existing file operations
- [ ] Migration guide clear and helpful

## Technical Notes

### Azure SDK API Usage
```rust
// Key Azure SDK methods for hierarchical listing
let mut list_builder = container_client.list_blobs();
list_builder = list_builder
    .prefix("docs/")           // Optional prefix filter
    .delimiter("/")            // Enable hierarchical mode
    .include_metadata(true);   // Include metadata for files

// Response contains:
// - page.blobs.blobs(): Vec<BlobItem> - files at this level
// - page.blobs.blob_prefixes(): Vec<BlobPrefix> - subdirectories
```

### Delimiter Behavior
- Without delimiter: Returns ALL blobs matching prefix (flat listing)
- With delimiter `/`: Returns only items between prefix and next `/`
  - Blobs: Files at current level
  - Prefixes: Subdirectory indicators

### Example Structure
```
Container contents:
  docs/README.md
  docs/api/v1/users.md
  docs/api/v2/users.md
  docs/guides/quickstart.md

Listing with prefix="docs/" and delimiter="/":
  Blobs: [docs/README.md]
  Prefixes: [docs/api/, docs/guides/]

Listing with prefix="docs/api/" and delimiter="/":
  Blobs: []
  Prefixes: [docs/api/v1/, docs/api/v2/]
```

## Alternative Approaches Considered

### Alternative 1: Client-Side Filtering
- Download all blobs and filter locally
- **Rejected**: Inefficient for large containers, unnecessary API calls

### Alternative 2: Custom Prefix Parsing
- Parse blob names manually to extract directories
- **Rejected**: Azure SDK provides native delimiter support

### Alternative 3: Always Show Both Modes
- Show both hierarchical and flat views simultaneously
- **Rejected**: Confusing output, doesn't match user expectations

## Risks and Mitigations

### Risk 1: Breaking Change Impact
- **Risk**: Users' scripts break due to behavior change
- **Mitigation**:
  - Clear documentation and migration guide
  - `--recursive` flag for backward compatibility
  - Prominent CHANGELOG entry
  - Consider temporary upgrade notice in CLI

### Risk 2: Performance Degradation
- **Risk**: Delimiter-based listing slower than flat listing
- **Mitigation**:
  - Profile both approaches
  - Add caching if needed
  - Optimize sorting algorithm

### Risk 3: Azure SDK Compatibility
- **Risk**: Delimiter API not stable or well-documented
- **Mitigation**:
  - Test with real Azure Storage
  - Verify behavior across SDK versions
  - Fallback to client-side parsing if needed

### Risk 4: Edge Case Bugs
- **Risk**: Unexpected behavior with unusual prefix/delimiter combinations
- **Mitigation**:
  - Comprehensive test suite
  - Manual testing with edge cases
  - Validation and error handling

## Conclusion

Implementing directory-style file listing will significantly improve the user experience by providing intuitive, filesystem-like navigation through blob storage. The hierarchical view makes it easier to explore and understand the structure of stored files, especially in containers with complex directory organizations.

The implementation leverages Azure SDK's native delimiter functionality for efficiency and reliability, while maintaining backward compatibility through the `--recursive` flag.
