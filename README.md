# Crossvault - Azure Key Vault CLI

A comprehensive command-line tool for managing Azure Key Vaults, written in Rust. Crossvault provides simplified access to secrets, vault management capabilities, and advanced features like secret name sanitization and group-based organization.

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

Crossvault uses a hierarchical configuration system:

1. Command-line flags (highest priority)
2. Environment variables
3. Configuration file (`~/.config/xv/xv.conf`)
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
- `CROSSVAULT_DEFAULT_VAULT`: Default vault name
- `CROSSVAULT_DEFAULT_RESOURCE_GROUP`: Default resource group

## Authentication

Crossvault uses Azure DefaultAzureCredential, supporting multiple authentication methods:

1. Environment variables (`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`)
2. Managed Identity
3. Azure CLI
4. Visual Studio Code
5. Azure PowerShell

## Usage Examples

### Secret Management

```bash
# Set a secret
xv secret set "api-key" "my-secret-value" --vault my-vault

# Get a secret
xv secret get "api-key" --vault my-vault

# List all secrets
xv secret list --vault my-vault

# Delete a secret
xv secret delete "api-key" --vault my-vault
```

### Group Management

```bash
# Set a secret with groups
xv secret set "db-password" "secret123" --group production --group database

# List secrets by group
xv secret list --group production

# List secrets organized by groups
xv secret list --group-by

# Update secret groups
xv secret update "db-password" --group staging --replace-groups
```

### Advanced Secret Operations

```bash
# Update secret with tags and notes
xv secret update "api-key" \
  --tags env=prod \
  --tags app=frontend \
  --note "Production API key for frontend service"

# Rename a secret
xv secret update "old-name" --rename "new-name"

# Parse connection string
xv secret parse "db-connection" --vault my-vault
```

### Vault Management

```bash
# Create a new vault
xv vault create my-new-vault --resource-group my-rg --location eastus

# List all vaults
xv vault list

# Get vault information
xv vault info my-vault

# Delete vault (soft delete)
xv vault delete my-vault --resource-group my-rg

# Restore deleted vault
xv vault restore my-vault --location eastus

# Export all secrets
xv vault export my-vault --format json --output secrets.json

# Import secrets
xv vault import my-vault --file secrets.json
```

### Access Management

```bash
# Grant read access to a user
xv vault share grant my-vault user@example.com --access-level reader

# Grant admin access to a service principal
xv vault share grant my-vault sp-object-id --access-level admin

# List access assignments
xv vault share list my-vault

# Revoke access
xv vault share revoke my-vault user@example.com
```

## Secret Name Sanitization

Crossvault automatically sanitizes secret names to comply with Azure Key Vault requirements:

- Replaces invalid characters with hyphens
- Removes consecutive hyphens
- Trims hyphens from start/end
- Hashes names longer than 127 characters
- Preserves original names in tags

Example:
```bash
# Original name with special characters
xv secret set "my-app/database:connection@prod" "value"

# Sanitized to: my-app-database-connection-prod
# Original name stored in tags for reference
```

## Group Organization

Groups provide a logical way to organize related secrets:

```bash
# Hierarchical organization
xv secret set "myapp/database/host" "localhost" --group "myapp/database"
xv secret set "myapp/database/port" "5432" --group "myapp/database"
xv secret set "myapp/api/key" "abc123" --group "myapp/api"

# Filter by group
xv secret list --group "myapp/database"

# View organized by groups
xv secret list --group-by
```

## Output Formats

Crossvault supports multiple output formats:

```bash
# Table format (default)
xv secret list

# JSON format
xv secret list --output json

# Plain text (for scripting)
xv secret get "api-key" --output plain

# Raw value only
xv secret get "api-key" --raw
```

## Implementation Details

### Architecture

Crossvault uses a hybrid approach for Azure integration:

- **Authentication**: Azure SDK v0.20 for credential management
- **Secret Operations**: Direct REST API calls to Azure Key Vault API v7.4
- **Vault Management**: REST API for full control over operations
- **Tag Support**: Full tag persistence for groups and metadata

### Why REST API?

Azure SDK v0.20 has limitations with tag support in secret operations. By using the REST API directly, Crossvault ensures:

- Complete tag persistence (groups, original_name, created_by)
- Full control over secret metadata
- Compatibility with all Azure Key Vault features
- Reliable group management functionality

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Acknowledgments

- Original Go implementation: [bbv](https://github.com/your-org/bbv)
- Built with Rust and the Azure SDK for Rust
- Uses REST API for enhanced functionality
