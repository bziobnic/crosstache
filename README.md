# crosstache

A cross-platform secrets manager for the command line. Currently backed by Azure Key Vault, with plans to support additional backends (AWS Secrets Manager, HashiCorp Vault, etc.).

The binary is called `xv`.

## Why crosstache?

Most cloud secret managers have clunky CLIs or force you into their ecosystem. crosstache gives you a clean, consistent interface for everyday secret operations — with features like group organization, secret injection, template rendering, and automatic name sanitization that the native tools lack.

## Quick Start

```bash
# Install (macOS/Linux)
curl -sSL https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.sh | bash

# Set up your first vault
xv init

# Store a secret
xv set "db-password"

# Retrieve it
xv get "db-password"

# Run a process with secrets injected as env vars
xv run -- ./my-app
```

## Installation

### Quick Install

**macOS/Linux:**
```bash
curl -sSL https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.sh | bash
```

**Windows (PowerShell):**
```powershell
iwr -useb https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.ps1 | iex
```

### Pre-built Binaries

Download from the [releases page](https://github.com/bziobnic/crosstache/releases):

| Platform | Binary |
|----------|--------|
| Windows x64 | `xv-windows-x64.zip` |
| macOS Intel | `xv-macos-intel.tar.gz` |
| macOS Apple Silicon | `xv-macos-apple-silicon.tar.gz` |
| Linux x64 | `xv-linux-x64.tar.gz` |

### From Source

```bash
git clone https://github.com/bziobnic/crosstache.git
cd crosstache
cargo install --path .
```

### macOS Security Note

If macOS blocks the binary ("developer cannot be verified"), run:
```bash
xattr -d com.apple.quarantine ~/.local/bin/xv
```

## Core Concepts

### Secrets

```bash
xv set "api-key"                          # Create (prompts for value)
xv set "api-key" --stdin < key.txt        # Create from stdin
xv set K1=val1 K2=val2 K3=@file.pem      # Bulk create
xv get "api-key"                          # Copy to clipboard (auto-clears after 30s)
xv get "api-key" --raw                    # Print to stdout
xv list                                   # List all secrets
xv list --group production                # Filter by group
xv list --expiring 30d                    # Show secrets expiring soon
xv update "api-key" --group prod --note "Frontend key"
xv delete "api-key"                       # Soft-delete
xv restore "api-key"                      # Restore soft-deleted
xv purge "api-key"                        # Permanently delete
```

### Secret Injection

Run processes with secrets available as environment variables:

```bash
# Inject all secrets from current vault
xv run -- npm start

# Inject only a specific group
xv run --group production -- ./deploy.sh

# Secret values are masked in stdout/stderr by default
xv run --no-masking -- ./debug.sh
```

### Template Injection

Render config files with secret references resolved:

```bash
# Template uses {{ secret:name }} syntax
xv inject --template app.config.tmpl --out app.config

# Also supports cross-vault references: xv://vault-name/secret-name
cat template.yml | xv inject > resolved.yml
```

### Organization

**Folders** — hierarchical organization:
```bash
xv set "host" --folder "myapp/database"
xv set "port" --folder "myapp/database"
```

**Groups** — tag-based, assigned via update:
```bash
xv update "db-host" --group "production" --group "database"
xv list --group production
```

See [docs/GROUPS.md](docs/GROUPS.md) for details.

### Secret History & Rotation

```bash
xv history "api-key"                      # Version history
xv rollback "api-key" --version <id>      # Restore previous version
xv rotate "api-key"                       # Generate new random value
xv rotate "api-key" --length 64 --charset alphanumeric
xv rotate "api-key" --generator ./gen.sh  # Custom generator
```

### Vault Management

```bash
xv vault create my-vault --resource-group my-rg --location eastus
xv vault list
xv vault info my-vault
xv vault delete my-vault
xv vault export my-vault --output secrets.json
xv vault import my-vault --input secrets.json --dry-run
```

### Vault Context

Switch between vaults without repeating `--vault` on every command:

```bash
xv context use my-vault         # Switch
xv cx use my-vault              # Alias
xv context show                 # Current context
xv context list                 # Recent contexts
```

### Environment Profiles

Named profiles that map to different vaults/groups:

```bash
xv env create prod --vault prod-vault --group production
xv env use prod
xv env pull --output .env       # Download as .env file
xv env push .env                # Upload .env contents as secrets
```

### Cross-Vault Operations

```bash
xv copy "api-key" --from vault-a --to vault-b
xv move "api-key" --from vault-a --to vault-b
```

### File Storage

Optional blob storage for files (requires setup via `xv init`):

```bash
xv upload ./config.json
xv download config.json
xv file list
xv file upload ./docs --recursive              # Preserves directory structure
xv file download "docs" --recursive --output ./local
xv file upload ./src --recursive --prefix "backup/2024-01-15"
```

### Identity & Auditing

```bash
xv whoami                       # Show authenticated identity
xv audit "api-key"              # Access/change history
xv audit --vault my-vault       # Vault-wide activity
```

## Configuration

### Hierarchy (highest → lowest priority)

1. CLI flags (e.g., `--credential-type cli`)
2. Environment variables
3. Config file (`~/.config/xv/xv.conf`)
4. Defaults

### Setup

```bash
xv init                                   # Interactive setup
xv config show                            # View current config
xv config set default_vault my-vault      # Set a value
```

### Key Environment Variables

| Variable | Purpose |
|----------|---------|
| `AZURE_SUBSCRIPTION_ID` | Azure subscription |
| `AZURE_TENANT_ID` | Azure tenant |
| `AZURE_CREDENTIAL_PRIORITY` | Auth method priority (`cli`, `managed_identity`, `environment`, `default`) |
| `DEFAULT_VAULT` | Default vault name |
| `DEFAULT_RESOURCE_GROUP` | Default resource group |

### Authentication

crosstache uses Azure's credential chain. You can control priority:

```bash
xv list --credential-type cli             # Prefer Azure CLI
export AZURE_CREDENTIAL_PRIORITY=cli      # For all commands
xv config set azure_credential_priority cli  # Persistent
```

Supported methods: Azure CLI, environment variables, managed identity, VS Code, PowerShell.

## Name Sanitization

Azure Key Vault only allows alphanumeric characters and hyphens. crosstache handles this transparently:

```bash
xv set "my-app/database:connection@prod"
# Stored as: my-app-database-connection-prod
# Original name preserved in tags for lookup
```

Names longer than 127 characters are SHA256-hashed.

## Output Formats

```bash
xv list                         # Table (default)
xv list --format json           # JSON
xv list --format yaml           # YAML
xv get "key" --raw              # Raw value (for scripting)
```

## Security

- Secret values are zeroized from memory after use (`zeroize` crate)
- Clipboard auto-clears 30 seconds after copy
- Config and export files are written with restricted permissions (0600)
- Recursive downloads are protected against path traversal
- Generator scripts are validated for ownership and permissions
- Secret values in `xv run` output are masked by default

## Development

```bash
cargo build                     # Debug build
cargo build --release           # Release build
cargo test                      # Run tests
cargo fmt && cargo clippy       # Format + lint
```

Build without file operations: `cargo build --no-default-features`

### Release

```bash
cargo release patch             # 0.1.0 → 0.1.1
cargo release minor             # 0.1.0 → 0.2.0
```

## License

MIT — see [LICENSE](LICENSE).
