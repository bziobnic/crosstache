# Azure Credential Priority Configuration Checklist

## Overview
This checklist guides the implementation of a configuration setting to control which Azure credential type is used first, specifically to prioritize Azure CLI credentials over managed identity.

## Implementation Checklist

### 1. Configuration Definition
- [x] Add new field to `Config` struct in `src/config/settings.rs`
  - [x] Field name: `azure_credential_priority`
  - [x] Type: enum `AzureCredentialType`
  - [x] Default value: `AzureCredentialType::Default`
  - [x] Supported values: "cli", "managed_identity", "environment", "default"

### 2. Environment Variable Support
- [x] Define environment variable in `src/config/settings.rs`
  - [x] Variable name: `AZURE_CREDENTIAL_PRIORITY`
  - [x] Add to `load_from_env()` method (line 364-368)
  - [x] Document in config loading hierarchy

### 3. Configuration File Support
- [x] Update config file parser in `src/config/settings.rs`
  - [x] Add field to TOML/JSON config structure
  - [x] Example config entry: `azure_credential_priority = "cli"`
  - [x] Automatic serialization/deserialization support via serde

### 4. CLI Flag Support
- [x] Add global flag to `src/cli/commands.rs`
  - [x] Flag: `--credential-type`
  - [x] Add to `Cli` struct with `#[arg(global = true)]` (line 90-99)
  - [x] Include help text explaining available options
  - [x] Integration in execute method (line 643-649)

### 5. Authentication Module Updates
- [x] Modify `src/auth/provider.rs`
  - [x] Update `DefaultAzureCredentialProvider::new()` to use default priority
  - [x] Implement `with_credential_priority()` method (line 149)
  - [x] Create method `create_prioritized_credential()` (line 178-216)
  
### 6. Custom Credential Chain Implementation
- [x] Create new function in `src/auth/provider.rs`
  ```rust
  fn create_prioritized_credential(priority: AzureCredentialType) -> Result<Arc<dyn TokenCredential>>
  ```
  - [x] If priority == Cli: Use AzureCliCredential
  - [x] If priority == ManagedIdentity: Use DefaultAzureCredential (SDK limitation)
  - [x] If priority == Environment: Try EnvironmentCredential first
  - [x] If priority == Default: Use DefaultAzureCredential as-is

### 7. Context Integration
- [x] Update credential provider creation throughout codebase
  - [x] All `DefaultAzureCredentialProvider::new()` calls updated to use `with_credential_priority()`
  - [x] Priority passed from config to all authentication operations

### 8. Error Handling
- [x] Error handling in `AzureCredentialType::from_str()`
  - [x] Returns error with clear message for invalid credential types
  - [x] Lists valid options in error message

### 9. Testing
- [x] Unit tests in `src/config/settings.rs`
  - [x] Test `AzureCredentialType::from_str()` with valid values
  - [x] Test `AzureCredentialType::from_str()` with invalid values
  - [x] Test `AzureCredentialType::Display` implementation
  - [x] Test default value

- [x] Integration tests in `tests/auth_tests.rs`
  - [x] Test environment variable name
  - [x] Test credential priority parsing

### 10. Documentation Updates
- [x] Update README.md
  - [x] Add new configuration option to environment variables section
  - [x] Add "Credential Priority Configuration" section with examples
  - [x] Document use case for overriding managed identity

- [x] Update CLAUDE.md
  - [x] Document new authentication flow with priority support
  - [x] Update "Authentication Flow" section with custom priority

- [ ] Update inline documentation
  - [ ] Document new config field with doc comments
  - [ ] Add examples in function documentation

### 11. Example Implementation Snippets

#### Config Structure Addition
```rust
// In src/config/settings.rs
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    // ... existing fields ...
    #[serde(default)]
    pub azure_credential_priority: Option<String>,
}
```

#### CLI Flag Addition
```rust
// In src/cli/commands.rs
#[derive(Parser)]
pub struct Cli {
    // ... existing fields ...
    #[arg(
        long,
        global = true,
        help = "Azure credential type to use first (cli, managed_identity, environment, default)",
        env = "AZURE_CREDENTIAL_PRIORITY"
    )]
    pub credential_type: Option<String>,
}
```

#### Custom Credential Chain
```rust
// In src/auth/azure.rs
use azure_identity::{AzureCliCredential, ManagedIdentityCredential, EnvironmentCredential};

fn create_prioritized_credential(priority: Option<String>) -> Result<Arc<dyn TokenCredential>> {
    match priority.as_deref() {
        Some("cli") => {
            // Try Azure CLI first, then fall back to default chain
            Ok(Arc::new(ChainedTokenCredential::new(vec![
                Arc::new(AzureCliCredential::new()),
                Arc::new(ManagedIdentityCredential::default()),
                Arc::new(EnvironmentCredential::default()),
            ])))
        },
        Some("managed_identity") => {
            Ok(Arc::new(ChainedTokenCredential::new(vec![
                Arc::new(ManagedIdentityCredential::default()),
                Arc::new(AzureCliCredential::new()),
                Arc::new(EnvironmentCredential::default()),
            ])))
        },
        Some("environment") => {
            Ok(Arc::new(ChainedTokenCredential::new(vec![
                Arc::new(EnvironmentCredential::default()),
                Arc::new(AzureCliCredential::new()),
                Arc::new(ManagedIdentityCredential::default()),
            ])))
        },
        Some("default") | None => {
            Ok(Arc::new(DefaultAzureCredential::default()))
        },
        Some(other) => {
            Err(crosstacheError::Configuration(format!(
                "Invalid credential type: {}. Valid options: cli, managed_identity, environment, default",
                other
            )))
        }
    }
}
```

### 12. Validation & Edge Cases
- [ ] Handle case-insensitive credential type values
- [ ] Validate credential type during config loading
- [ ] Provide helpful error messages for typos
- [ ] Test behavior when preferred credential is not available on the system
- [ ] Ensure graceful fallback when preferred method fails

### 13. Performance Considerations
- [ ] Cache the credential chain to avoid rebuilding on each request
- [ ] Consider lazy initialization of credentials
- [ ] Test performance impact of custom credential chain

### 14. Backward Compatibility
- [ ] Ensure existing configurations continue to work
- [ ] Default behavior should match current implementation
- [ ] No breaking changes to existing CLI commands

## Testing Commands

After implementation, test with these commands:

```bash
# Test with environment variable
export AZURE_CREDENTIAL_PRIORITY=cli
xv secret list

# Test with CLI flag
xv secret list --credential-type cli

# Test with config file
echo 'azure_credential_priority = "cli"' >> ~/.config/xv/xv.conf
xv secret list

# Verify priority order (CLI flag should override env var)
export AZURE_CREDENTIAL_PRIORITY=managed_identity
xv secret list --credential-type cli  # Should use CLI auth
```

## Notes
- The Azure SDK for Rust may have limitations on credential chain customization
- Consider using `ChainedTokenCredential` if available, or implement custom chaining logic
- Monitor Azure SDK updates as credential handling may improve in future versions