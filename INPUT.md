# CrossVault Enhanced Secret Value Input - Implementation Plan

## Overview

This document provides a detailed, step-by-step implementation plan for **Recommendation 3** from `IMPROVE.md`: **Improved Secret Value Input**. The plan will transform CrossVault's basic password prompt/stdin input into a comprehensive secret value input system.

## Current State Analysis

### Existing Implementation (`src/cli/commands.rs` lines 922-981, 1098-1227)

**Current Input Methods:**
- **Interactive prompt**: `rpassword::prompt_password()` for secure, hidden input
- **Stdin mode**: `--stdin` flag reads from `io::stdin().read_to_string()`
- **Direct value**: CLI argument for updates only

**Limitations:**
- No file-based input support
- No multi-line editor support
- No environment variable substitution
- Limited handling of complex JSON/YAML values
- No binary data support

## Target Implementation

Transform the current system to support:

```bash
xv secret set api-config --editor        # Opens $EDITOR
xv secret set cert --from-file cert.pem  # Read from file  
xv secret set db-url --env-subst         # Use env var substitution
```

## Implementation Phases

### Phase 1: Foundation & Core Infrastructure

#### Step 1.1: Add New Dependencies
**File:** `Cargo.toml`
**Action:** Add required dependencies

```toml
# Add to [dependencies] section
tempfile = "3.0"           # For secure temporary file handling
shellwords = "1.1"         # For environment variable parsing
serde_yaml = "0.9"         # For YAML validation
edit = "0.1"               # For external editor integration
```

**Rationale:** These dependencies provide secure file handling, environment variable parsing, YAML support, and editor integration.

#### Step 1.2: Create Input Utilities Module
**File:** `src/utils/input.rs` (new file)
**Action:** Create a dedicated input utilities module

```rust
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use tempfile::NamedTempFile;
use crate::error::CrossvaultError;

pub struct InputValue {
    pub content: String,
    pub is_multiline: bool,
    pub source: InputSource,
}

pub enum InputSource {
    Interactive,
    Stdin,
    File(String),
    Editor,
    EnvSubst,
}

pub async fn get_secret_value(
    name: &str,
    options: &InputOptions,
) -> Result<InputValue, CrossvaultError> {
    // Implementation will be added in subsequent steps
}
```

#### Step 1.3: Update CLI Command Structure
**File:** `src/cli/commands.rs`
**Action:** Extend `Set` and `Update` command structures

```rust
// Add to SecretCommands::Set
Set {
    name: String,
    vault: Option<String>,
    #[arg(long)]
    stdin: bool,
    #[arg(long)]
    editor: bool,           // NEW: --editor flag
    #[arg(long)]
    from_file: Option<String>, // NEW: --from-file flag
    #[arg(long)]
    env_subst: bool,        // NEW: --env-subst flag
    #[arg(long)]
    note: Option<String>,
    #[arg(long)]
    folder: Option<String>,
}

// Add to SecretCommands::Update  
Update {
    name: String,
    vault: Option<String>,
    value: Option<String>,
    #[arg(long)]
    stdin: bool,
    #[arg(long)]
    editor: bool,           // NEW: --editor flag
    #[arg(long)]
    from_file: Option<String>, // NEW: --from-file flag
    #[arg(long)]
    env_subst: bool,        // NEW: --env-subst flag
    // ... existing fields
}
```

### Phase 2: Multi-line Editor Support

#### Step 2.1: Implement Editor Integration
**File:** `src/utils/input.rs`
**Action:** Add editor support function

```rust
pub fn launch_editor(
    secret_name: &str,
    initial_content: Option<&str>,
) -> Result<String, CrossvaultError> {
    // Get editor from environment ($EDITOR, $VISUAL, or default)
    let editor = env::var("EDITOR")
        .or_else(|_| env::var("VISUAL"))
        .unwrap_or_else(|_| "nano".to_string());

    // Create secure temporary file
    let mut temp_file = NamedTempFile::new()
        .map_err(|e| CrossvaultError::InputError(format!("Failed to create temp file: {}", e)))?;

    // Write initial content if provided
    if let Some(content) = initial_content {
        temp_file.write_all(content.as_bytes())
            .map_err(|e| CrossvaultError::InputError(format!("Failed to write temp file: {}", e)))?;
    }

    // Add helpful comment header
    let header = format!(
        "# Enter secret value for '{}'\n# Lines starting with # are ignored\n\n",
        secret_name
    );
    temp_file.write_all(header.as_bytes())?;

    // Launch editor
    let status = std::process::Command::new(&editor)
        .arg(temp_file.path())
        .status()
        .map_err(|e| CrossvaultError::InputError(format!("Failed to launch editor '{}': {}", editor, e)))?;

    if !status.success() {
        return Err(CrossvaultError::InputError("Editor exited with error".to_string()));
    }

    // Read content back
    let content = fs::read_to_string(temp_file.path())
        .map_err(|e| CrossvaultError::InputError(format!("Failed to read temp file: {}", e)))?;

    // Filter out comment lines and clean up
    let cleaned_content = content
        .lines()
        .filter(|line| !line.trim().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    Ok(cleaned_content)
}
```

#### Step 2.2: Add Editor Input Validation
**File:** `src/utils/input.rs`
**Action:** Add validation for editor input

```rust
pub fn validate_editor_input(content: &str) -> Result<(), CrossvaultError> {
    if content.trim().is_empty() {
        return Err(CrossvaultError::InputError(
            "Secret value cannot be empty".to_string()
        ));
    }

    // Warn about potential issues
    if content.len() > 25000 {
        eprintln!("Warning: Secret value is very large ({} bytes)", content.len());
        eprintln!("Azure Key Vault secrets are limited to 25KB");
    }

    if content.contains('\0') {
        return Err(CrossvaultError::InputError(
            "Secret value cannot contain null bytes".to_string()
        ));
    }

    Ok(())
}
```

### Phase 3: File-Based Input Support

#### Step 3.1: Implement File Reading
**File:** `src/utils/input.rs`
**Action:** Add file reading functionality

```rust
pub async fn read_from_file<P: AsRef<Path>>(
    file_path: P,
) -> Result<String, CrossvaultError> {
    let path = file_path.as_ref();
    
    // Validate file exists and is readable
    if !path.exists() {
        return Err(CrossvaultError::InputError(
            format!("File not found: {}", path.display())
        ));
    }

    if !path.is_file() {
        return Err(CrossvaultError::InputError(
            format!("Path is not a file: {}", path.display())
        ));
    }

    // Check file size (Azure Key Vault limit: 25KB)
    let metadata = fs::metadata(path)
        .map_err(|e| CrossvaultError::InputError(format!("Cannot read file metadata: {}", e)))?;

    if metadata.len() > 25000 {
        return Err(CrossvaultError::InputError(
            format!("File too large: {} bytes (limit: 25KB)", metadata.len())
        ));
    }

    // Read file content
    let content = fs::read_to_string(path)
        .map_err(|e| CrossvaultError::InputError(format!("Failed to read file: {}", e)))?;

    // Detect and handle binary content
    if content.contains('\0') {
        // For binary files, encode as base64
        let binary_content = fs::read(path)
            .map_err(|e| CrossvaultError::InputError(format!("Failed to read binary file: {}", e)))?;
        
        return Ok(base64::encode(binary_content));
    }

    Ok(content)
}
```

#### Step 3.2: Add File Input Validation
**File:** `src/utils/input.rs`
**Action:** Add file-specific validation

```rust
pub fn validate_file_input(file_path: &str) -> Result<(), CrossvaultError> {
    let path = Path::new(file_path);
    
    // Security check: prevent reading sensitive system files
    let canonical_path = path.canonicalize()
        .map_err(|e| CrossvaultError::InputError(format!("Cannot resolve file path: {}", e)))?;

    let sensitive_paths = [
        "/etc/passwd", "/etc/shadow", "/etc/hosts",
        "/proc/", "/sys/", "/dev/"
    ];

    let path_str = canonical_path.to_string_lossy();
    for sensitive in &sensitive_paths {
        if path_str.starts_with(sensitive) {
            return Err(CrossvaultError::InputError(
                format!("Cannot read sensitive system file: {}", path_str)
            ));
        }
    }

    Ok(())
}
```

### Phase 4: Environment Variable Substitution

#### Step 4.1: Implement Environment Variable Parser
**File:** `src/utils/input.rs`
**Action:** Add environment variable substitution

```rust
use regex::Regex;

pub fn substitute_env_vars(input: &str) -> Result<String, CrossvaultError> {
    // Support both ${VAR} and $VAR syntax
    let env_regex = Regex::new(r"\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z0-9_]*)")
        .map_err(|e| CrossvaultError::InputError(format!("Regex error: {}", e)))?;

    let mut result = input.to_string();
    let mut missing_vars = Vec::new();

    // Find all environment variable references
    for captures in env_regex.captures_iter(input) {
        let full_match = captures.get(0).unwrap().as_str();
        let var_name = captures.get(1)
            .or_else(|| captures.get(2))
            .unwrap()
            .as_str();

        match env::var(var_name) {
            Ok(value) => {
                result = result.replace(full_match, &value);
            }
            Err(_) => {
                missing_vars.push(var_name.to_string());
            }
        }
    }

    if !missing_vars.is_empty() {
        return Err(CrossvaultError::InputError(
            format!("Missing environment variables: {}", missing_vars.join(", "))
        ));
    }

    Ok(result)
}
```

#### Step 4.2: Add Interactive Environment Variable Substitution
**File:** `src/utils/input.rs`
**Action:** Add interactive mode for missing variables

```rust
pub fn substitute_env_vars_interactive(input: &str) -> Result<String, CrossvaultError> {
    let env_regex = Regex::new(r"\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z0-9_]*)")
        .map_err(|e| CrossvaultError::InputError(format!("Regex error: {}", e)))?;

    let mut result = input.to_string();

    for captures in env_regex.captures_iter(input) {
        let full_match = captures.get(0).unwrap().as_str();
        let var_name = captures.get(1)
            .or_else(|| captures.get(2))
            .unwrap()
            .as_str();

        let value = match env::var(var_name) {
            Ok(val) => val,
            Err(_) => {
                // Prompt user for missing variable
                let prompt = format!("Enter value for ${}: ", var_name);
                rpassword::prompt_password(prompt)?
            }
        };

        result = result.replace(full_match, &value);
    }

    Ok(result)
}
```

### Phase 5: JSON/YAML Value Handling

#### Step 5.1: Implement JSON/YAML Validation
**File:** `src/utils/input.rs`
**Action:** Add JSON/YAML validation and formatting

```rust
pub fn validate_and_format_structured_data(
    content: &str,
) -> Result<String, CrossvaultError> {
    let trimmed = content.trim();
    
    // Try to parse as JSON first
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(parsed) => {
                // Re-serialize with consistent formatting
                return Ok(serde_json::to_string_pretty(&parsed)
                    .map_err(|e| CrossvaultError::InputError(format!("JSON serialization error: {}", e)))?);
            }
            Err(_) => {
                // Not valid JSON, continue
            }
        }
    }

    // Try to parse as YAML
    if trimmed.contains(':') || trimmed.contains('-') {
        match serde_yaml::from_str::<serde_yaml::Value>(trimmed) {
            Ok(parsed) => {
                // Convert YAML to JSON for consistent storage
                let json_value: serde_json::Value = serde_yaml::from_str(trimmed)
                    .map_err(|e| CrossvaultError::InputError(format!("YAML conversion error: {}", e)))?;
                
                return Ok(serde_json::to_string_pretty(&json_value)
                    .map_err(|e| CrossvaultError::InputError(format!("JSON serialization error: {}", e)))?);
            }
            Err(_) => {
                // Not valid YAML, treat as plain text
            }
        }
    }

    // Return as-is if not structured data
    Ok(content.to_string())
}
```

#### Step 5.2: Add Structured Data Detection
**File:** `src/utils/input.rs`
**Action:** Add automatic structured data detection

```rust
pub fn detect_structured_data(content: &str) -> StructuredDataType {
    let trimmed = content.trim();
    
    // JSON detection
    if (trimmed.starts_with('{') && trimmed.ends_with('}')) ||
       (trimmed.starts_with('[') && trimmed.ends_with(']')) {
        if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
            return StructuredDataType::Json;
        }
    }

    // YAML detection
    if trimmed.contains(':') && trimmed.lines().count() > 1 {
        if serde_yaml::from_str::<serde_yaml::Value>(trimmed).is_ok() {
            return StructuredDataType::Yaml;
        }
    }

    // Base64 detection
    if trimmed.len() % 4 == 0 && trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=') {
        if base64::decode(trimmed).is_ok() {
            return StructuredDataType::Base64;
        }
    }

    StructuredDataType::PlainText
}

pub enum StructuredDataType {
    Json,
    Yaml,
    Base64,
    PlainText,
}
```

### Phase 6: Integration & Command Updates

#### Step 6.1: Update Secret Set Command
**File:** `src/cli/commands.rs`
**Action:** Replace current input logic in `execute_secret_set`

```rust
async fn execute_secret_set(
    name: String,
    vault: Option<String>,
    stdin: bool,
    editor: bool,
    from_file: Option<String>,
    env_subst: bool,
    note: Option<String>,
    folder: Option<String>,
) -> Result<(), CrossvaultError> {
    // Validate input method exclusivity
    let input_methods = [stdin, editor, from_file.is_some(), env_subst];
    let method_count = input_methods.iter().filter(|&&x| x).count();
    
    if method_count > 1 {
        return Err(CrossvaultError::InputError(
            "Only one input method can be specified: --stdin, --editor, --from-file, or --env-subst".to_string()
        ));
    }

    // Get secret value using appropriate method
    let input_options = InputOptions {
        stdin,
        editor,
        from_file: from_file.clone(),
        env_subst,
    };

    let input_value = crate::utils::input::get_secret_value(&name, &input_options).await?;
    
    // Validate and process the value
    let processed_value = if input_value.is_multiline || matches!(input_value.source, InputSource::Editor) {
        crate::utils::input::validate_and_format_structured_data(&input_value.content)?
    } else {
        input_value.content
    };

    // Continue with existing secret creation logic...
    let config = crate::config::Config::load()?;
    let manager = crate::secret::SecretManager::new(&config).await?;
    
    manager.create_secret(
        &vault.unwrap_or(config.default_vault.clone()),
        &name,
        &processed_value,
        note.as_deref(),
        folder.as_deref(),
        None, // tags
        None, // groups
    ).await?;

    println!("Secret '{}' created successfully", name);
    Ok(())
}
```

#### Step 6.2: Update Secret Update Command
**File:** `src/cli/commands.rs`
**Action:** Update `execute_secret_update` with new input methods

```rust
async fn execute_secret_update(
    name: String,
    vault: Option<String>,
    value: Option<String>,
    stdin: bool,
    editor: bool,
    from_file: Option<String>,
    env_subst: bool,
    // ... other parameters
) -> Result<(), CrossvaultError> {
    // Validate input method exclusivity
    let input_methods = [
        value.is_some(),
        stdin,
        editor,
        from_file.is_some(),
        env_subst
    ];
    let method_count = input_methods.iter().filter(|&&x| x).count();
    
    if method_count > 1 {
        return Err(CrossvaultError::InputError(
            "Only one input method can be specified".to_string()
        ));
    }

    // Determine new value if any input method is specified
    let new_value = if let Some(v) = value {
        Some(v)
    } else if stdin || editor || from_file.is_some() || env_subst {
        let input_options = InputOptions {
            stdin,
            editor,
            from_file: from_file.clone(),
            env_subst,
        };
        
        let input_value = crate::utils::input::get_secret_value(&name, &input_options).await?;
        
        let processed_value = if input_value.is_multiline || matches!(input_value.source, InputSource::Editor) {
            crate::utils::input::validate_and_format_structured_data(&input_value.content)?
        } else {
            input_value.content
        };
        
        Some(processed_value)
    } else {
        None
    };

    // Continue with existing update logic...
    let config = crate::config::Config::load()?;
    let manager = crate::secret::SecretManager::new(&config).await?;
    
    manager.update_secret(
        &vault.unwrap_or(config.default_vault.clone()),
        &name,
        new_value.as_deref(),
        // ... other update parameters
    ).await?;

    println!("Secret '{}' updated successfully", name);
    Ok(())
}
```

### Phase 7: Core Input Router Implementation

#### Step 7.1: Complete the Input Router
**File:** `src/utils/input.rs`
**Action:** Implement the main `get_secret_value` function

```rust
pub struct InputOptions {
    pub stdin: bool,
    pub editor: bool,
    pub from_file: Option<String>,
    pub env_subst: bool,
}

pub async fn get_secret_value(
    name: &str,
    options: &InputOptions,
) -> Result<InputValue, CrossvaultError> {
    match options {
        InputOptions { stdin: true, .. } => {
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)
                .map_err(|e| CrossvaultError::InputError(format!("Failed to read from stdin: {}", e)))?;
            
            let content = buffer.trim().to_string();
            if content.is_empty() {
                return Err(CrossvaultError::InputError("Empty input from stdin".to_string()));
            }

            Ok(InputValue {
                content,
                is_multiline: buffer.lines().count() > 1,
                source: InputSource::Stdin,
            })
        }

        InputOptions { editor: true, .. } => {
            let content = launch_editor(name, None)?;
            validate_editor_input(&content)?;
            
            Ok(InputValue {
                content,
                is_multiline: content.lines().count() > 1,
                source: InputSource::Editor,
            })
        }

        InputOptions { from_file: Some(file_path), .. } => {
            validate_file_input(file_path)?;
            let content = read_from_file(file_path).await?;
            
            Ok(InputValue {
                content,
                is_multiline: content.lines().count() > 1,
                source: InputSource::File(file_path.clone()),
            })
        }

        InputOptions { env_subst: true, .. } => {
            // Prompt for template string
            let template = rpassword::prompt_password(
                format!("Enter template for secret '{}' (use $VAR or ${{VAR}}): ", name)
            )?;
            
            let content = substitute_env_vars_interactive(&template)?;
            
            Ok(InputValue {
                content,
                is_multiline: content.lines().count() > 1,
                source: InputSource::EnvSubst,
            })
        }

        _ => {
            // Default: interactive prompt
            let content = rpassword::prompt_password(
                format!("Enter value for secret '{}': ", name)
            )?;
            
            if content.trim().is_empty() {
                return Err(CrossvaultError::InputError("Secret value cannot be empty".to_string()));
            }

            Ok(InputValue {
                content,
                is_multiline: false,
                source: InputSource::Interactive,
            })
        }
    }
}
```

### Phase 8: Error Handling & User Experience

#### Step 8.1: Add Enhanced Error Messages
**File:** `src/error.rs`
**Action:** Add input-specific error variants

```rust
// Add to CrossvaultError enum
#[derive(Debug, thiserror::Error)]
pub enum CrossvaultError {
    // ... existing variants
    
    #[error("Input error: {0}")]
    InputError(String),
    
    #[error("File error: {0}")]
    FileError(String),
    
    #[error("Editor error: {0}")]
    EditorError(String),
    
    #[error("Environment variable error: {0}")]
    EnvVarError(String),
}
```

#### Step 8.2: Add Help Text & Usage Examples
**File:** `src/cli/commands.rs`
**Action:** Add comprehensive help text for new flags

```rust
// Update Set command with help text
Set {
    #[arg(help = "Name of the secret")]
    name: String,
    
    #[arg(long, help = "Read value from stdin")]
    stdin: bool,
    
    #[arg(long, help = "Open $EDITOR to input multi-line value")]
    editor: bool,
    
    #[arg(long, value_name = "FILE", help = "Read value from file")]
    from_file: Option<String>,
    
    #[arg(long, help = "Enable environment variable substitution ($VAR or ${VAR})")]
    env_subst: bool,
    
    // ... other fields
}
```

### Phase 9: Testing & Validation

#### Step 9.1: Create Input Integration Tests
**File:** `tests/integration/secret_input_tests.rs` (new file)
**Action:** Create comprehensive tests

```rust
use std::env;
use std::fs;
use tempfile::NamedTempFile;
use crossvault::utils::input::*;

#[tokio::test]
async fn test_file_input() {
    let mut temp_file = NamedTempFile::new().unwrap();
    writeln!(temp_file, "test-secret-value").unwrap();
    
    let result = read_from_file(temp_file.path()).await.unwrap();
    assert_eq!(result.trim(), "test-secret-value");
}

#[tokio::test]
async fn test_env_var_substitution() {
    env::set_var("TEST_VAR", "test-value");
    
    let result = substitute_env_vars("prefix-${TEST_VAR}-suffix").unwrap();
    assert_eq!(result, "prefix-test-value-suffix");
    
    env::remove_var("TEST_VAR");
}

#[tokio::test]
async fn test_json_validation() {
    let json_input = r#"{"key": "value", "number": 42}"#;
    let result = validate_and_format_structured_data(json_input).unwrap();
    
    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["key"], "value");
}
```

#### Step 9.2: Add CLI Integration Tests
**File:** `tests/integration/cli_tests.rs`
**Action:** Add tests for new CLI flags

```rust
#[tokio::test]
async fn test_secret_set_with_file() {
    // Create test file
    let mut temp_file = NamedTempFile::new().unwrap();
    writeln!(temp_file, "file-secret-content").unwrap();
    
    // Test CLI command
    let result = tokio::process::Command::new("cargo")
        .args(&["run", "--", "secret", "set", "test-secret", 
                "--from-file", temp_file.path().to_str().unwrap()])
        .output()
        .await
        .unwrap();
    
    assert!(result.status.success());
}
```

### Phase 10: Documentation & User Guidance

#### Step 10.1: Update Command Help
**File:** `src/cli/mod.rs`
**Action:** Add comprehensive help examples

```rust
const SECRET_SET_HELP: &str = r#"
Set a secret value using various input methods:

EXAMPLES:
    # Interactive prompt (default)
    xv secret set api-key

    # Multi-line editor
    xv secret set config --editor
    
    # From file
    xv secret set certificate --from-file cert.pem
    
    # From stdin
    echo "secret-value" | xv secret set api-key --stdin
    
    # Environment variable substitution
    xv secret set database-url --env-subst
    # Then enter: postgresql://${DB_USER}:${DB_PASS}@${DB_HOST}/mydb
"#;
```

#### Step 10.2: Create Usage Examples
**File:** `examples/secret_input_examples.md` (new file)
**Action:** Create comprehensive usage examples

```markdown
# Secret Input Examples

## Basic Usage
```bash
# Interactive prompt
xv secret set api-key

# Direct value (for updates only)
xv secret update api-key "new-value"
```

## Multi-line Content
```bash
# Use editor for complex JSON/YAML
xv secret set app-config --editor

# From file containing JSON
xv secret set app-config --from-file config.json
```

## Environment Variable Substitution
```bash
# Set environment variables
export DB_USER=myuser
export DB_PASS=mypassword
export DB_HOST=localhost

# Use substitution
xv secret set database-connection --env-subst
# Enter: postgresql://${DB_USER}:${DB_PASS}@${DB_HOST}/myapp
```

## File-based Input
```bash
# SSL certificate
xv secret set ssl-cert --from-file server.crt

# Private key
xv secret set ssl-key --from-file server.key

# Configuration file
xv secret set app-config --from-file config.yaml
```
```

## Implementation Timeline

### Week 1: Foundation
- [ ] Add dependencies to Cargo.toml
- [ ] Create input utilities module structure
- [ ] Update CLI command structures
- [ ] Basic validation framework

### Week 2: Core Features
- [ ] Implement multi-line editor support
- [ ] Add file-based input functionality
- [ ] Create environment variable substitution
- [ ] Basic error handling

### Week 3: Integration
- [ ] Update secret set/update commands
- [ ] Implement JSON/YAML validation
- [ ] Add comprehensive error messages
- [ ] Integration testing

### Week 4: Polish & Documentation
- [ ] Add help text and examples
- [ ] Performance testing
- [ ] Security review
- [ ] Documentation updates

## Risk Mitigation

### Security Considerations
- **Temporary files**: Use `tempfile` crate for secure temp file handling
- **File permissions**: Validate file access and prevent reading sensitive system files
- **Environment variables**: Clear sensitive environment variables after use
- **Editor security**: Validate editor command execution

### Backward Compatibility
- **Existing behavior**: Maintain current interactive prompt as default
- **Flag conflicts**: Explicit validation of mutually exclusive flags
- **Error messages**: Clear guidance when migration is needed

### Performance Impact
- **File size limits**: Enforce Azure Key Vault 25KB limit
- **Memory usage**: Stream large files instead of loading entirely into memory
- **Editor responsiveness**: Timeout handling for editor operations

## Success Metrics

### User Experience
- [ ] Reduced setup time for complex secrets
- [ ] Improved automation capabilities
- [ ] Better JSON/YAML handling
- [ ] Enhanced multi-line secret support

### Technical Metrics
- [ ] Zero regression in existing functionality
- [ ] <100ms overhead for new input methods
- [ ] 100% test coverage for new features
- [ ] Clean error handling with helpful messages

This implementation plan provides a comprehensive roadmap for transforming CrossVault's secret input capabilities while maintaining security, usability, and backward compatibility.