# crosstache - Cross-platform Azure Key Vault CLI

A comprehensive command-line tool for managing Azure Key Vaults, written in Rust. crosstache provides simplified access to secrets, vault management capabilities, and advanced features like secret name sanitization and group-based organization.

## Features

- 🔐 **Full Secret Management**: Create, read, update, delete, and list secrets
- 🏷️ **Group Organization**: Organize secrets into logical groups using tags
- 🔄 **Name Sanitization**: Support for any secret name through automatic sanitization
- 🏗️ **Vault Operations**: Create, delete, restore, and manage vaults
- 👥 **Access Control**: RBAC-based access management for users and service principals
- 📦 **Import/Export**: Bulk secret operations with JSON/TXT/ENV formats
- 🔧 **Configuration**: Persistent settings with environment variable overrides
- 🔍 **Connection String Parsing**: Parse and display connection string components

## Installation

### Quick Install (Recommended)

**Linux/macOS:**
```bash
curl -sSL https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.sh | bash
```

**Windows (PowerShell):**
```powershell
iwr -useb https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.ps1 | iex
```

### Manual Download

Download pre-built binaries from the [releases page](https://github.com/bziobnic/crosstache/releases):

- **Windows**: `xv-windows-x64.zip`
- **macOS Intel**: `xv-macos-intel.tar.gz`
- **macOS Apple Silicon**: `xv-macos-apple-silicon.tar.gz`  
- **Linux**: `xv-linux-x64.tar.gz`

Extract and add the `xv` binary to your PATH.

### From Source

```bash
# Clone the repository
git clone https://github.com/bziobnic/crosstache.git
cd crosstache

# Build and install
cargo build --release
cargo install --path .
```

### Verify Installation

```bash
xv --version
```

### macOS Security Note

On macOS, you may see "cannot be opened because the developer cannot be verified" because the binary isn't code-signed. To fix this:

**Option 1 - Right-click method:**

1. Right-click the `xv` binary in Finder
2. Select "Open"
3. Click "Open" when prompted

**Option 2 - Command line:**

```bash
# Remove quarantine attribute (done automatically by install script)
xattr -d com.apple.quarantine ~/.local/bin/xv
```

This only needs to be done once per binary.

## Configuration

crosstache uses a hierarchical configuration system:

1. Command-line flags (highest priority)
2. Environment variables
3. Configuration file (`$XDG_CONFIG_HOME/xv/xv.conf` or `$HOME/.config/xv/xv.conf`)
4. Default values

### Initial Setup

```bash
# Initialize configuration
xv init

# View current configuration
xv config show

# Set default vault
xv config set default_vault my-vault

# Set default subscription
xv config set subscription_id YOUR_SUBSCRIPTION_ID
```

### Environment Variables

- `AZURE_SUBSCRIPTION_ID`: Default Azure subscription
- `AZURE_TENANT_ID`: Azure tenant ID
- `DEFAULT_VAULT`: Default vault name
- `DEFAULT_RESOURCE_GROUP`: Default resource group
- `FUNCTION_APP_URL`: Function app URL for extended functionality
- `CACHE_TTL`: Cache time-to-live in seconds
- `DEBUG`: Enable debug logging (true/1)

## Authentication

crosstache uses Azure DefaultAzureCredential, supporting multiple authentication methods:

1. Environment variables (`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`)
2. Managed Identity
3. Azure CLI
4. Visual Studio Code
5. Azure PowerShell

## Usage Examples

### Secret Management

```bash
# Set a secret (prompts for value)
xv set "api-key"

# Set a secret with value from stdin
echo "my-secret-value" | xv set "api-key" --stdin

# Get a secret
xv get "api-key"

# List all secrets
xv list

# Delete a secret
xv delete "api-key"
```

### Group Management

```bash
# Set a secret with folder organization
xv set "db-password" --folder "production/database"

# List secrets by group
xv list --group production

# Update secret with folder and note
xv update "db-password" --folder "staging/database" --note "Updated for staging"
```

### Advanced Secret Operations

```bash
# Update secret with note and folder
xv update "api-key" \
  --note "Production API key for frontend service" \
  --folder "app/frontend"

# Parse connection string
xv parse "db-connection"

# Get secret value only (for scripting)
xv get "api-key" --format raw
```

### Vault Management

```bash
# Create a new vault
xv vault create my-new-vault --resource-group my-rg --location eastus

# List all vaults
xv vault list

# Get vault information
xv info my-vault
# or
xv vault info my-vault

# Delete vault (soft delete)
xv vault delete my-vault --resource-group my-rg

# Restore deleted vault
xv vault restore my-vault --location eastus

# Switch vault context
xv context set my-vault
# or
xv cx set my-vault

# View current context
xv context show
```

### File Operations

crosstache includes basic file management capabilities using Azure Blob Storage:

```bash
# Quick file upload
xv upload config.json

# Quick file download
xv download config.json

# Full file command syntax
xv file upload config.json
xv file download config.json --output ./downloads/
xv file list
xv file delete config.json

# File editing (download, edit, upload)
xv file edit config.json
```

**Note**: File operations require blob storage configuration during `xv init`.

### Access Management

**Note**: Vault sharing commands are currently not implemented (see TODO.md for status)

```bash
# These commands are planned but not yet functional:
# xv vault share grant my-vault user@example.com --access-level reader
# xv vault share list my-vault
# xv vault share revoke my-vault user@example.com
```

## Secret Name Sanitization

crosstache automatically sanitizes secret names to comply with Azure Key Vault requirements:

- Replaces invalid characters with hyphens
- Removes consecutive hyphens
- Trims hyphens from start/end
- Hashes names longer than 127 characters
- Preserves original names in tags

Example:
```bash
# Original name with special characters
xv set "my-app/database:connection@prod"

# Sanitized to: my-app-database-connection-prod
# Original name stored in tags for reference
```

## Folder Organization

Use folders to organize related secrets hierarchically:

```bash
# Hierarchical organization using folders
xv set "host" --folder "myapp/database"
xv set "port" --folder "myapp/database"
xv set "key" --folder "myapp/api"

# Filter by group (folder-based)
xv list --group "myapp/database"
```

## Output Formats

crosstache supports multiple output formats:

```bash
# Table format (default)
xv list

# JSON format
xv list --format json

# YAML format
xv list --format yaml

# Raw value only (for scripting)
xv get "api-key" --format raw
```

## Architecture

crosstache uses a hybrid approach for Azure integration:

- **Authentication**: Azure SDK v0.21 with DefaultAzureCredential
- **Secret Operations**: Direct REST API calls to Azure Key Vault API v7.4
- **Vault Management**: REST API for full control over operations
- **File Storage**: Azure Blob Storage for file operations (in development)
- **Tag Support**: Full tag persistence for groups and metadata

### Module Structure

- `auth/`: Azure authentication using DefaultAzureCredential pattern
  - `provider.rs`: Core Azure authentication implementation with Graph API integration
- `vault/`: Vault management operations (create, delete, list, restore)
  - `manager.rs`: Core vault operations and lifecycle management
  - `operations.rs`: Specific vault operations (RBAC, access control)
- `secret/`: Secret CRUD operations with group and metadata support
  - `manager.rs`: Core secret operations with REST API integration
  - `name_manager.rs`: Name sanitization and validation logic
- `blob/`: File storage operations using Azure Blob Storage (in development)
  - `manager.rs`: Core file operations
  - `models.rs`: File-related data structures
- `config/`: Configuration management with hierarchy (CLI → env vars → config file → defaults)
  - `settings.rs`: Configuration structure and loading
  - `context.rs`: Runtime context management
- `utils/`: Sanitization, formatting, retry logic, and helper functions
  - `sanitizer.rs`: Azure Key Vault name sanitization with hashing
  - `network.rs`: HTTP client configuration with proper error handling
  - `retry.rs`: Retry logic for Azure API calls
  - `format.rs`: Output formatting (JSON, table, plain text)
- `cli/`: Command parsing using `clap` with derive macros
  - `commands.rs`: All CLI command definitions and execution logic

### Key Implementation Details

- **Group Management**: Groups stored as comma-separated values in single "groups" tag
- **Name Sanitization**: Client-side sanitization with original names preserved in "original_name" tag; names >127 chars are SHA256 hashed
- **Error Handling**: Custom `crosstacheError` enum with `thiserror` for structured errors
- **Async**: Full `tokio` async runtime throughout
- **REST API Integration**: Uses `reqwest` with bearer tokens from Azure SDK for secret operations
- **Context System**: Vault context management for seamless multi-vault workflows

### Why REST API?

The Azure SDK has limitations with tag support in secret operations. By using the REST API directly, crosstache ensures:

- Complete tag persistence (groups, original_name, created_by)
- Full control over secret metadata
- Compatibility with all Azure Key Vault features
- Reliable group management functionality

## Development

### Building and Running
```bash
# Build in debug mode
cargo build

# Build release version
cargo build --release

# Run the CLI tool
cargo run -- [COMMAND]

# Install locally
cargo install --path .
```

### Testing
```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test file
cargo test --test auth_tests
cargo test --test vault_tests

# Run tests in single thread (useful for Azure API tests)
cargo test -- --test-threads=1

# Run unit tests only (exclude integration tests)
cargo test --lib
```

### Code Quality
```bash
# Format code
cargo fmt

# Run clippy linter
cargo clippy

# Check without building
cargo check
```

### Build System

The tool uses a custom `build.rs` that:
- Auto-increments build numbers
- Embeds git commit hash, branch, and build timestamp
- Creates version strings like `0.1.0.123+abc1234`
- Build metadata available via environment variables: `BUILD_NUMBER`, `GIT_HASH`, `BUILD_TIME`, `GIT_BRANCH`

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Acknowledgments

- Built with Rust and the Azure SDK for Rust
- Uses REST API for enhanced functionality
