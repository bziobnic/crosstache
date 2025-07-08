# Smart Vault Context Detection Implementation Guide

This document provides step-by-step instructions for implementing smart vault context detection in crosstache, allowing users to work with vaults without repeatedly specifying `--vault` flags.

## Overview

Smart vault context detection will:
- Automatically detect the current vault context from local configuration
- Allow users to switch vault contexts with persistence
- Show current context in command prompts and outputs
- Fall back gracefully to global defaults

## Implementation Steps

### Step 1: Define Context Data Structures

Create new types in `src/config/mod.rs` or a new `src/config/context.rs` file:

```rust
// src/config/context.rs
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultContext {
    /// Current vault name
    pub vault_name: String,
    /// Resource group for the vault
    pub resource_group: Option<String>,
    /// Subscription ID (optional override)
    pub subscription_id: Option<String>,
    /// Last used timestamp
    pub last_used: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextManager {
    /// Current active context
    pub current: Option<VaultContext>,
    /// Recently used contexts (max 10)
    pub recent: Vec<VaultContext>,
    /// Context file path
    #[serde(skip)]
    pub context_file: Option<PathBuf>,
}
```

### Step 2: Context File Management

Add context persistence methods:

```rust
impl ContextManager {
    /// Load context from local directory or global config
    pub async fn load() -> Result<Self> {
        // 1. Check for .xv/context in current directory
        if let Ok(local_context) = Self::load_local_context().await {
            return Ok(local_context);
        }
        
        // 2. Fall back to global context
        Self::load_global_context().await
    }
    
    /// Load context from current directory (.xv/context)
    async fn load_local_context() -> Result<Self> {
        let context_path = std::env::current_dir()?.join(".xv").join("context");
        if !context_path.exists() {
            return Err(crosstacheError::config("No local context found"));
        }
        
        let content = tokio::fs::read_to_string(&context_path).await?;
        let mut context: ContextManager = serde_json::from_str(&content)?;
        context.context_file = Some(context_path);
        Ok(context)
    }
    
    /// Load context from global config directory
    async fn load_global_context() -> Result<Self> {
        let context_path = Self::global_context_path()?;
        if !context_path.exists() {
            return Ok(Self::default());
        }
        
        let content = tokio::fs::read_to_string(&context_path).await?;
        let mut context: ContextManager = serde_json::from_str(&content)?;
        context.context_file = Some(context_path);
        Ok(context)
    }
    
    /// Save current context
    pub async fn save(&self) -> Result<()> {
        if let Some(ref path) = self.context_file {
            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            
            let content = serde_json::to_string_pretty(self)?;
            tokio::fs::write(path, content).await?;
        }
        Ok(())
    }
    
    /// Get global context file path
    fn global_context_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| crosstacheError::config("Could not determine config directory"))?;
        Ok(config_dir.join("xv").join("context"))
    }
}
```

### Step 3: Context Switching Commands

Add new CLI commands in `src/cli/commands.rs`:

```rust
// Add to Commands enum
#[derive(Subcommand)]
pub enum Commands {
    // ... existing commands ...
    
    /// Vault context management
    Context {
        #[command(subcommand)]
        command: ContextCommands,
    },
}

#[derive(Subcommand)]
pub enum ContextCommands {
    /// Show current vault context
    Show,
    /// Switch to a vault context
    Use {
        /// Vault name
        vault_name: String,
        /// Resource group
        #[arg(short, long)]
        resource_group: Option<String>,
        /// Make this the global default
        #[arg(long)]
        global: bool,
        /// Set for current directory only
        #[arg(long)]
        local: bool,
    },
    /// List recent vault contexts
    List,
    /// Clear current context
    Clear {
        /// Clear global context
        #[arg(long)]
        global: bool,
    },
}
```

### Step 4: Context Resolution Logic

Create context resolution in `src/config/mod.rs`:

```rust
impl Config {
    /// Resolve vault name with context awareness
    pub async fn resolve_vault_name(&self, vault_arg: Option<String>) -> Result<String> {
        // 1. Command line argument takes precedence
        if let Some(vault) = vault_arg {
            return Ok(vault);
        }
        
        // 2. Check local/global context
        let context_manager = ContextManager::load().await?;
        if let Some(ref context) = context_manager.current {
            return Ok(context.vault_name.clone());
        }
        
        // 3. Fall back to config default
        if !self.default_vault.is_empty() {
            return Ok(self.default_vault.clone());
        }
        
        Err(crosstacheError::config(
            "No vault specified. Use --vault, set context with 'xv context use', or configure default_vault"
        ))
    }
    
    /// Resolve resource group with context awareness
    pub async fn resolve_resource_group(&self, rg_arg: Option<String>) -> Result<String> {
        // Similar logic for resource group resolution
        if let Some(rg) = rg_arg {
            return Ok(rg);
        }
        
        let context_manager = ContextManager::load().await?;
        if let Some(ref context) = context_manager.current {
            if let Some(ref rg) = context.resource_group {
                return Ok(rg.clone());
            }
        }
        
        if !self.default_resource_group.is_empty() {
            return Ok(self.default_resource_group.clone());
        }
        
        Err(crosstacheError::config("No resource group specified"))
    }
}
```

### Step 5: Update Secret Commands

Modify secret command implementations to use context resolution:

```rust
// In execute_secret_set function
async fn execute_secret_set(
    secret_manager: &crate::secret::manager::SecretManager,
    name: &str,
    vault: Option<String>,
    stdin: bool,
    note: Option<String>,
    folder: Option<String>,
    config: &Config,
) -> Result<()> {
    // Replace vault resolution logic
    let vault_name = config.resolve_vault_name(vault).await?;
    
    // Update context with usage
    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    context_manager.update_usage(&vault_name, config).await?;
    
    // Rest of the function remains the same...
}
```

### Step 6: Context Command Implementation

Implement context command handlers:

```rust
async fn execute_context_command(command: ContextCommands, config: Config) -> Result<()> {
    match command {
        ContextCommands::Show => {
            execute_context_show(&config).await?;
        }
        ContextCommands::Use { vault_name, resource_group, global, local } => {
            execute_context_use(&vault_name, resource_group, global, local, &config).await?;
        }
        ContextCommands::List => {
            execute_context_list(&config).await?;
        }
        ContextCommands::Clear { global } => {
            execute_context_clear(global, &config).await?;
        }
    }
    Ok(())
}

async fn execute_context_show(config: &Config) -> Result<()> {
    let context_manager = ContextManager::load().await.unwrap_or_default();
    
    if let Some(ref context) = context_manager.current {
        println!("Current Vault Context:");
        println!("  Vault: {}", context.vault_name);
        if let Some(ref rg) = context.resource_group {
            println!("  Resource Group: {}", rg);
        }
        println!("  Last Used: {}", context.last_used.format("%Y-%m-%d %H:%M:%S UTC"));
        
        // Show context source
        if context_manager.context_file.as_ref().map(|p| p.to_string_lossy().contains(".xv")).unwrap_or(false) {
            println!("  Scope: Local (current directory)");
        } else {
            println!("  Scope: Global");
        }
    } else {
        println!("No vault context set");
        if !config.default_vault.is_empty() {
            println!("Using config default: {}", config.default_vault);
        }
    }
    
    Ok(())
}

async fn execute_context_use(
    vault_name: &str,
    resource_group: Option<String>,
    global: bool,
    local: bool,
    config: &Config,
) -> Result<()> {
    let mut context_manager = if local {
        // Create local context
        let local_path = std::env::current_dir()?.join(".xv").join("context");
        ContextManager {
            context_file: Some(local_path),
            ..Default::default()
        }
    } else if global {
        // Use global context
        ContextManager {
            context_file: Some(ContextManager::global_context_path()?),
            ..Default::default()
        }
    } else {
        // Load existing or create new
        ContextManager::load().await.unwrap_or_else(|_| {
            ContextManager {
                context_file: Some(ContextManager::global_context_path().unwrap()),
                ..Default::default()
            }
        })
    };
    
    // Create new context
    let new_context = VaultContext {
        vault_name: vault_name.to_string(),
        resource_group: resource_group.or_else(|| {
            if !config.default_resource_group.is_empty() {
                Some(config.default_resource_group.clone())
            } else {
                None
            }
        }),
        subscription_id: None,
        last_used: chrono::Utc::now(),
    };
    
    // Update context manager
    context_manager.set_context(new_context).await?;
    
    let scope = if local { "local" } else { "global" };
    println!("✅ Switched to vault '{}' ({} context)", vault_name, scope);
    
    Ok(())
}
```

### Step 7: Context Usage Tracking

Add methods to track and update context usage:

```rust
impl ContextManager {
    /// Set current context and update recent list
    pub async fn set_context(&mut self, context: VaultContext) -> Result<()> {
        // Update recent contexts
        self.recent.retain(|c| c.vault_name != context.vault_name);
        self.recent.insert(0, context.clone());
        self.recent.truncate(10); // Keep only 10 recent
        
        self.current = Some(context);
        self.save().await?;
        Ok(())
    }
    
    /// Update usage timestamp for current context
    pub async fn update_usage(&mut self, vault_name: &str, config: &Config) -> Result<()> {
        if let Some(ref mut context) = self.current {
            if context.vault_name == vault_name {
                context.last_used = chrono::Utc::now();
                self.save().await?;
            }
        }
        Ok(())
    }
    
    /// Clear current context
    pub async fn clear_context(&mut self) -> Result<()> {
        self.current = None;
        self.save().await?;
        Ok(())
    }
}
```

### Step 8: Enhanced User Experience

Add context indicators to command outputs:

```rust
// Add to command output formatting
fn format_command_header(vault_name: &str, is_context: bool) {
    if is_context {
        println!("📍 Using vault context: {}", vault_name);
    } else {
        println!("Using vault: {}", vault_name);
    }
}
```

### Step 9: Shell Integration (Optional)

Create shell completion helpers:

```bash
# Add to completion scripts
_xv_context_vaults() {
    # Return list of available vaults for completion
    xv vault list --format json 2>/dev/null | jq -r '.[].name' 2>/dev/null
}

_xv_context_recent() {
    # Return recent vault contexts
    xv context list --format json 2>/dev/null | jq -r '.[].vault_name' 2>/dev/null
}
```

### Step 10: Error Handling & Migration

Add migration logic for existing configurations:

```rust
impl ContextManager {
    /// Migrate from old configuration format
    pub async fn migrate_from_config(config: &Config) -> Result<()> {
        if !config.default_vault.is_empty() {
            let context = VaultContext {
                vault_name: config.default_vault.clone(),
                resource_group: if !config.default_resource_group.is_empty() {
                    Some(config.default_resource_group.clone())
                } else {
                    None
                },
                subscription_id: None,
                last_used: chrono::Utc::now(),
            };
            
            let mut context_manager = Self {
                current: Some(context),
                context_file: Some(Self::global_context_path()?),
                ..Default::default()
            };
            
            context_manager.save().await?;
            println!("✅ Migrated default vault to context system");
        }
        Ok(())
    }
}
```

## Testing Strategy

### Unit Tests
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_context_local_vs_global() {
        // Test local context takes precedence over global
    }
    
    #[tokio::test]
    async fn test_context_persistence() {
        // Test saving and loading context
    }
    
    #[tokio::test]
    async fn test_vault_resolution_priority() {
        // Test CLI arg > context > config default
    }
}
```

### Integration Tests
- Test context switching in real directory scenarios
- Verify fallback behavior when context files are missing
- Test migration from existing configurations

## Documentation Updates

### User Documentation
```markdown
## Vault Context Management

crosstache supports smart vault context detection to reduce typing:

# Set vault context globally
xv context use my-vault --global

# Set vault context for current project
xv context use project-vault --local

# Now you can omit --vault from commands
xv secret list
xv secret get api-key

# Show current context
xv context show

# List recent contexts
xv context list
```

### Migration Guide
```markdown
## Migrating to Context System

If you have `default_vault` configured:
1. Run `xv context use <your-default-vault> --global`
2. Your existing default will be automatically migrated
```

## Implementation Checklist

- [ ] Create context data structures
- [ ] Implement context file management
- [ ] Add context CLI commands
- [ ] Update vault resolution logic
- [ ] Modify existing commands to use context
- [ ] Add context tracking and usage updates
- [ ] Implement error handling and migration
- [ ] Write unit and integration tests
- [ ] Update documentation
- [ ] Add shell completion support

## Future Enhancements

1. **Project Templates**: Auto-detect vault context from project files
2. **Environment Integration**: Context switching based on environment variables
3. **Team Sharing**: Share context configurations across team members
4. **Context Validation**: Verify vault accessibility when switching contexts

This implementation provides a smooth transition to context-aware vault operations while maintaining backward compatibility with existing workflows.