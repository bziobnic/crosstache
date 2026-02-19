# FEATURES.md — crosstache Feature Inventory

> Cross-platform Azure Key Vault CLI (`xv`). File-related functions excluded.

---

## Secret Management

| Command | Description |
|---------|-------------|
| `xv set <name>` | Create a secret (interactive prompt or `--stdin` for piped input) |
| `xv get <name>` | Retrieve a secret (copies to clipboard by default; `--raw` for stdout) |
| `xv list` | List all secrets in current vault (`--group` filter, `--all` for disabled) |
| `xv delete <name>` | Soft-delete a secret (`--force` to skip confirmation) |
| `xv update <name>` | Update value, tags, groups, folder, note; supports `--rename` |
| `xv purge <name>` | Permanently delete a soft-deleted secret |
| `xv restore <name>` | Restore a soft-deleted secret |

### Secret Metadata & Organization

- **Folders** — Hierarchical organization via `--folder "app/database"` on set/update
- **Groups** — Tag-based grouping; assign with `--group`, filter with `list --group`
- **Notes** — Attach notes to secrets via `--note`
- **Tags** — Arbitrary key=value tags with `--tags key=value`; merge or replace (`--replace-tags`, `--replace-groups`)

### Name Sanitization

- Automatically sanitizes names to comply with Azure Key Vault rules (alphanumeric + hyphens only)
- Replaces invalid characters, collapses consecutive hyphens, trims edges
- Names >127 chars are SHA256-hashed
- Original name preserved in secret tags for reverse lookup

---

## Vault Management

| Command | Description |
|---------|-------------|
| `xv vault create <name>` | Create a new vault (`--resource-group`, `--location`) |
| `xv vault list` | List vaults (optional `--resource-group` filter) |
| `xv vault delete <name>` | Soft-delete a vault |
| `xv vault info <name>` | Show vault details |
| `xv vault restore <name>` | Restore a soft-deleted vault |
| `xv vault purge <name>` | Permanently purge a soft-deleted vault |
| `xv vault update <name>` | Update vault properties: tags, deployment/encryption/template flags, purge protection, soft-delete retention (7–90 days) |

### Import / Export

| Command | Description |
|---------|-------------|
| `xv vault export <name>` | Export secrets to JSON, ENV, or TXT (`--include-values`, `--group` filter) |
| `xv vault import <name>` | Import secrets from JSON, ENV, or TXT (`--overwrite`, `--dry-run`) |

---

## Access Control (RBAC)

### Vault-Level

| Command | Description |
|---------|-------------|
| `xv vault share grant` | Grant vault access to a user or service principal (reader, contributor, admin) |
| `xv vault share revoke` | Revoke vault access |
| `xv vault share list` | List vault access assignments |

### Secret-Level

| Command | Description |
|---------|-------------|
| `xv share grant` | Grant access to a specific secret (read, write, admin) |
| `xv share revoke` | Revoke secret-level access |
| `xv share list` | List secret access permissions |

---

## Vault Context Management

| Command | Description |
|---------|-------------|
| `xv context show` | Show current vault context |
| `xv context use <vault>` | Switch vault context (`--global`, `--local` for directory-scoped) |
| `xv context list` | List recent vault contexts |
| `xv context clear` | Clear current context |

Alias: `xv cx` for `xv context`.

---

## Configuration

| Command | Description |
|---------|-------------|
| `xv init` | Initialize default configuration interactively |
| `xv config show` | Show current configuration |
| `xv config set <key> <value>` | Set a configuration value |
| `xv config path` | Show config file location |

### Config Hierarchy (highest → lowest priority)

1. CLI flags
2. Environment variables
3. Config file (`$XDG_CONFIG_HOME/xv/xv.conf` or `~/.config/xv/xv.conf`)
4. Defaults

### Key Environment Variables

| Variable | Purpose |
|----------|---------|
| `AZURE_SUBSCRIPTION_ID` | Default Azure subscription |
| `AZURE_TENANT_ID` | Azure tenant ID |
| `AZURE_CREDENTIAL_PRIORITY` | Credential type priority (cli, managed_identity, environment, default) |
| `DEFAULT_VAULT` | Default vault name |
| `DEFAULT_RESOURCE_GROUP` | Default resource group |
| `CACHE_TTL` | Cache TTL in seconds |
| `DEBUG` | Enable debug logging |

---

## Authentication

- Uses Azure `DefaultAzureCredential` chain: environment variables → managed identity → Azure CLI → VS Code → PowerShell
- Configurable credential priority via `--credential-type`, env var, or config file
- Supports service principals, managed identity, and interactive CLI auth

---

## Resource Info

| Command | Description |
|---------|-------------|
| `xv info <resource>` | Auto-detect and display info for a vault or secret (`--type` to force) |
| `xv version` | Detailed build info (version, git hash, branch, target) |

---

## Utilities

| Feature | Description |
|---------|-------------|
| **Connection string parsing** | `xv parse <conn-string>` — parse and display components |
| **Retry with backoff** | Exponential backoff for transient Azure API failures |
| **Output formats** | Table (default), JSON, YAML, raw, and custom `--template` strings |
| **Column selection** | `--columns` to select specific table columns |
| **Clipboard integration** | `xv get` copies to clipboard by default |
| **Secure input** | Password-style prompts for secret values (no terminal echo) |
| **Azure resource detection** | Auto-detect resource types from identifiers |

---

## Build & Distribution

- **Binary name:** `xv`
- **Platforms:** Windows x64, macOS Intel, macOS Apple Silicon, Linux x64
- **Feature flags:** `file-ops` (default on) — can be disabled for a smaller binary
- **Release:** `cargo-release` with automated GitHub Actions CI/CD
- **Build metadata:** Git hash, branch, compiler info embedded via `built` crate
- **Security:** `zeroize` for sensitive data in memory; release binary is stripped + LTO
