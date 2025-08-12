# Azure Credential Priority Configuration Checklist

## Overview
This checklist guides the implementation of a configuration setting to control which Azure credential type is used first, specifically to prioritize Azure CLI credentials over managed identity.

## Implementation Checklist

### 1. Configuration Definition
- [ ] Add new field to `Config` struct in `src/config/settings.rs`
  - [ ] Field name: `azure_credential_priority` or `preferred_credential_type`
  - [ ] Type: `Option<String>` or enum like `AzureCredentialType`
  - [ ] Default value: `None` (use SDK default order)
  - [ ] Supported values: "cli", "managed_identity", "environment", "default"

### 2. Environment Variable Support
- [ ] Define environment variable in `src/config/settings.rs`
  - [ ] Variable name: `AZURE_CREDENTIAL_PRIORITY` or `XV_CREDENTIAL_TYPE`
  - [ ] Add to `Config::from_env()` method
  - [ ] Document in config loading hierarchy

### 3. Configuration File Support
- [ ] Update config file parser in `src/config/settings.rs`
  - [ ] Add field to TOML/JSON config structure
  - [ ] Example config entry: `azure_credential_priority = "cli"`
  - [ ] Update `Config::load_from_file()` method

### 4. CLI Flag Support
- [ ] Add global flag to `src/cli/commands.rs`
  - [ ] Flag: `--credential-type` or `--azure-auth-method`
  - [ ] Short flag: `-c` (if available)
  - [ ] Add to `Cli` struct with `#[arg(global = true)]`
  - [ ] Include help text explaining available options

### 5. Authentication Module Updates
- [ ] Modify `src/auth/azure.rs`
  - [ ] Update `AzureAuth::new()` to accept credential priority parameter
  - [ ] Implement custom credential chain builder based on priority
  - [ ] Create method like `build_credential_chain(priority: Option<String>)`
  
### 6. Custom Credential Chain Implementation
- [ ] Create new function in `src/auth/azure.rs`
  ```rust
  fn create_prioritized_credential(priority: Option<String>) -> Result<Arc<dyn TokenCredential>>
  ```
  - [ ] If priority == "cli": Try AzureCliCredential first, then fall back to others
  - [ ] If priority == "managed_identity": Try ManagedIdentityCredential first
  - [ ] If priority == "environment": Try EnvironmentCredential first
  - [ ] If priority == "default" or None: Use DefaultAzureCredential as-is

### 7. Context Integration
- [ ] Update `src/config/context.rs`
  - [ ] Pass credential priority from config to AzureAuth initialization
  - [ ] Ensure priority is available in `Context` struct if needed

### 8. Error Handling
- [ ] Add new error variant if needed in `src/error.rs`
  - [ ] `InvalidCredentialType(String)` for unsupported credential types
  - [ ] Clear error messages for credential priority issues

### 9. Testing
- [ ] Unit tests in `src/auth/azure.rs`
  - [ ] Test credential chain with "cli" priority
  - [ ] Test credential chain with "managed_identity" priority
  - [ ] Test fallback behavior when preferred credential fails
  - [ ] Test invalid credential type handling

- [ ] Integration tests in `tests/auth_tests.rs`
  - [ ] Test with environment variable set
  - [ ] Test with config file setting
  - [ ] Test with CLI flag
  - [ ] Test priority order (CLI > env > config > default)

### 10. Documentation Updates
- [ ] Update README.md
  - [ ] Add new configuration option to configuration section
  - [ ] Provide example usage with Azure CLI priority
  - [ ] Document use case for overriding managed identity

- [ ] Update CLAUDE.md
  - [ ] Document new authentication flow with priority support
  - [ ] Update "Authentication Flow" section with custom priority

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