# Implementation Plan: Output Format Consistency & Enhancement

## Overview
**Improvement #6**: Output Format Consistency & Enhancement  
**Current State**: Mixed output formats, limited JSON support  
**Goal**: Standardize output formats across all commands, add template-based formatting, improve table layouts, and add progress indicators

## Phase 1: Output Format Standardization (Week 1-2)

### Step 1.1: Define Output Format Types
- [ ] Create `OutputFormat` enum in `src/utils/format.rs`
  - `Json` - Machine-readable JSON output
  - `Table` - Human-readable table format
  - `Plain` - Simple text output
  - `Template` - Custom template-based output
  - `Yaml` - YAML format for configuration files

### Step 1.2: Create Standardized Output Trait
- [ ] Define `FormattableOutput` trait with methods:
  - `to_json(&self) -> Result<String, serde_json::Error>`
  - `to_table(&self) -> String`
  - `to_plain(&self) -> String`
  - `to_template(&self, template: &str) -> Result<String, TemplateError>`

### Step 1.3: Implement Output Trait for All Data Models
- [ ] Update `src/vault/models.rs` - implement for `VaultInfo`, `VaultList`
- [ ] Update `src/secret/manager.rs` - implement for `SecretInfo`, `SecretList`
- [ ] Update `src/config/settings.rs` - implement for `ConfigInfo`
- [ ] Create comprehensive test coverage for all format implementations

### Step 1.4: Update CLI Arguments
- [ ] Add global `--format` flag to `src/cli/commands.rs`
  - Possible values: `json`, `table`, `plain`, `template`, `yaml`
  - Default to `table` for human interaction, `json` for scripts
- [ ] Add `--template` flag for custom template strings
- [ ] Add `--columns` flag for table column selection

## Phase 2: Template System Implementation (Week 3)

### Step 2.1: Template Engine Integration
- [ ] Add `handlebars` crate dependency to `Cargo.toml`
- [ ] Create `TemplateEngine` struct in `src/utils/format.rs`
- [ ] Implement template compilation and rendering
- [ ] Add template helper functions:
  - Date/time formatting: `{{date updated "2006-01-02"}}`
  - String manipulation: `{{truncate name 20}}`
  - Conditional display: `{{#if groups}}{{groups}}{{/if}}`

### Step 2.2: Predefined Templates
- [ ] Create template library in `src/utils/templates.rs`:
  ```rust
  pub const SECRET_SUMMARY: &str = "{{name}}: {{#if groups}}[{{groups}}]{{/if}} ({{updated}})";
  pub const VAULT_BRIEF: &str = "{{name}} ({{location}}) - {{secret_count}} secrets";
  pub const CONFIG_DISPLAY: &str = "{{key}} = {{value}}";
  ```
- [ ] Allow users to save custom templates in config

### Step 2.3: Template Validation
- [ ] Add template syntax validation before execution
- [ ] Provide helpful error messages for invalid templates
- [ ] Add template testing utilities

## Phase 3: Enhanced Table Formatting (Week 4)

### Step 3.1: Terminal-Aware Table Rendering
- [ ] Add `terminal_size` crate dependency
- [ ] Create `TableRenderer` struct in `src/utils/format.rs`
- [ ] Implement responsive column sizing:
  - Auto-detect terminal width
  - Intelligently truncate/wrap long content
  - Priority-based column display (show most important first)

### Step 3.2: Advanced Table Features
- [ ] Add table sorting capabilities:
  - `--sort-by name,updated,groups`
  - Support ascending/descending order
- [ ] Implement column selection:
  - `--columns name,groups,updated`
  - Default column sets per command
- [ ] Add table pagination for large result sets:
  - `--limit` and `--offset` parameters
  - Interactive pagination prompt

### Step 3.3: Table Styling Options
- [ ] Create multiple table styles:
  - `minimal` - Basic ASCII borders
  - `unicode` - Unicode box drawing characters
  - `github` - GitHub markdown table format
- [ ] Add color coding support using `colored` crate:
  - Different colors for different secret groups
  - Status indicators (active/disabled vaults)
  - Error highlighting

## Phase 4: Progress Indicators (Week 5)

### Step 4.1: Progress Bar Infrastructure
- [ ] Add `indicatif` crate dependency for progress bars
- [ ] Create `ProgressReporter` trait in `src/utils/progress.rs`:
  ```rust
  pub trait ProgressReporter {
      fn start(&self, message: &str, total: Option<u64>);
      fn update(&self, current: u64, message: Option<&str>);
      fn finish(&self, message: &str);
  }
  ```

### Step 4.2: Integrate Progress Bars
- [ ] Add progress indicators to long-running operations:
  - Vault creation/deletion
  - Bulk secret operations
  - Large secret list operations
  - Network-intensive operations
- [ ] Implement different progress styles:
  - Spinner for indeterminate operations
  - Progress bar for operations with known duration
  - Multi-progress for parallel operations

### Step 4.3: Progress Configuration
- [ ] Add `--progress` flag to enable/disable progress indicators
- [ ] Auto-detect when output is piped (disable progress in scripts)
- [ ] Configure progress update frequency

## Phase 5: Output Command Extensions (Week 6)

### Step 5.1: New Format-Specific Commands
- [ ] Implement suggested command examples:
  ```bash
  xv secret list --format template --template "{{.name}}: {{.updated}}"
  xv secret list --columns name,groups,updated
  xv vault create my-vault --progress
  ```

### Step 5.2: Format Presets
- [ ] Create format presets in config:
  - `brief` - Essential information only
  - `detailed` - All available information
  - `export` - Machine-readable format for backups
- [ ] Allow users to define custom presets

### Step 5.3: Output Redirection Handling
- [ ] Detect output redirection/piping
- [ ] Auto-adjust format for scripting contexts
- [ ] Suppress interactive elements when output is redirected

## Phase 6: Testing & Documentation (Week 7)

### Step 6.1: Comprehensive Testing
- [ ] Unit tests for all formatters
- [ ] Integration tests with different terminal sizes
- [ ] Template rendering tests with edge cases
- [ ] Progress indicator tests
- [ ] Output redirection tests

### Step 6.2: Documentation Updates
- [ ] Update `README.md` with new formatting options
- [ ] Create format examples in documentation
- [ ] Add template usage guide
- [ ] Update help text for all commands

### Step 6.3: User Experience Testing
- [ ] Test with different terminal emulators
- [ ] Verify behavior with various screen sizes
- [ ] Test accessibility with screen readers
- [ ] Performance testing with large datasets

## Implementation Details

### Key Files to Modify
1. `src/utils/format.rs` - Core formatting logic
2. `src/utils/templates.rs` - Template definitions
3. `src/utils/progress.rs` - Progress indicator system
4. `src/cli/commands.rs` - CLI argument handling
5. `src/vault/models.rs` - Vault output formatting
6. `src/secret/manager.rs` - Secret output formatting
7. `src/config/settings.rs` - Configuration output formatting

### Dependencies to Add
```toml
[dependencies]
handlebars = "4.4"          # Template engine
terminal_size = "0.2"       # Terminal size detection
indicatif = "0.17"          # Progress bars
colored = "2.0"             # Color output
serde_yaml = "0.9"          # YAML support
```

### Configuration Schema Extension
```toml
[output]
default_format = "table"
default_template = "brief"
enable_colors = true
table_style = "unicode"
progress_enabled = true

[templates]
secret_brief = "{{name}}: {{groups}}"
vault_summary = "{{name}} ({{location}})"
```

### Error Handling Strategy
- Graceful fallback to plain text if formatting fails
- Clear error messages for template syntax errors
- Preserve original data if template rendering fails
- Log formatting errors for debugging

### Performance Considerations
- Cache compiled templates
- Lazy load progress bars only when needed
- Minimize memory allocation for large datasets
- Stream output for very large result sets

### Backward Compatibility
- Maintain existing output format as default
- Provide migration guide for scripts using current output
- Add deprecation warnings for old format flags
- Support legacy format flags for transition period

## Success Criteria

1. **Consistency**: All commands support the same format options
2. **Flexibility**: Users can customize output for different use cases
3. **Performance**: No significant performance degradation
4. **Usability**: Output looks good in various terminal sizes
5. **Scriptability**: JSON/YAML output is stable and machine-readable
6. **Accessibility**: Works well with screen readers and assistive tools

## Risk Mitigation

1. **Breaking Changes**: Implement feature flags for gradual rollout
2. **Performance Impact**: Profile and optimize critical paths
3. **Template Security**: Validate and sanitize template inputs
4. **Terminal Compatibility**: Test with major terminal emulators
5. **Large Datasets**: Implement streaming and pagination

This implementation plan provides a structured approach to delivering improvement #6 while maintaining code quality, backward compatibility, and user experience.