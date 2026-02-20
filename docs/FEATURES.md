# Feature Reference

> Complete command reference for `xv`. Current backend: Azure Key Vault.

---

## Secrets

| Command | Description |
|---------|-------------|
| `xv set <name>` | Create a secret (interactive prompt, `--stdin`, or bulk `K1=v1 K2=v2`) |
| `xv get <name>` | Retrieve a secret (clipboard by default; `--raw` for stdout) |
| `xv list` | List secrets (`--group`, `--all`, `--expiring <period>`, `--expired`) |
| `xv delete <name>` | Soft-delete a secret (`--force` to skip confirmation) |
| `xv update <name>` | Update value, groups, folder, note, tags, expiry; supports `--rename` |
| `xv purge <name>` | Permanently delete a soft-deleted secret |
| `xv restore <name>` | Restore a soft-deleted secret |
| `xv history <name>` | Show version history |
| `xv rollback <name>` | Restore a previous version (`--version <id>`) |
| `xv rotate <name>` | Generate new random value (`--length`, `--charset`, `--generator`) |
| `xv copy <name>` | Copy a secret between vaults (`--from`, `--to`) |
| `xv move <name>` | Move a secret between vaults (`--from`, `--to`) |

### Metadata & Organization

- **Folders** — `--folder "app/database"` on `set` or `update`
- **Groups** — `--group <name>` on `update` (multiple allowed); filter with `list --group`
- **Notes** — `--note "description"` on `set` or `update`
- **Tags** — `-t key=value` on `update`; `--replace-tags` / `--replace-groups` for replace mode
- **Expiry** — `--expires YYYY-MM-DD` on `set` or `update`; `--clear-expires` to remove

### Name Sanitization

Names are automatically sanitized for backend compatibility (Azure: alphanumeric + hyphens only). Original names are preserved in metadata for reverse lookup. Names >127 chars are SHA256-hashed.

---

## Secret Injection

| Command | Description |
|---------|-------------|
| `xv run -- <command>` | Run a process with secrets as env vars (`--group`, `--no-masking`) |
| `xv inject` | Render templates with `{{ secret:name }}` and `xv://vault/secret` refs |

---

## Vault Management

| Command | Description |
|---------|-------------|
| `xv vault create <name>` | Create a new vault (`--resource-group`, `--location`) |
| `xv vault list` | List vaults |
| `xv vault info <name>` | Show vault details |
| `xv vault delete <name>` | Soft-delete a vault |
| `xv vault restore <name>` | Restore a soft-deleted vault |
| `xv vault purge <name>` | Permanently purge a soft-deleted vault |
| `xv vault update <name>` | Update vault properties and tags |
| `xv vault export <name>` | Export secrets to JSON, ENV, or TXT |
| `xv vault import <name>` | Import secrets from file (`--overwrite`, `--dry-run`) |

### Access Control

| Command | Description |
|---------|-------------|
| `xv vault share grant` | Grant vault access (reader, contributor, admin) |
| `xv vault share revoke` | Revoke vault access |
| `xv vault share list` | List vault access assignments |
| `xv share grant` | Grant secret-level access |
| `xv share revoke` | Revoke secret-level access |
| `xv share list` | List secret permissions |

---

## Context & Environments

| Command | Description |
|---------|-------------|
| `xv context use <vault>` | Switch vault context (`--global`, `--local`) |
| `xv context show` | Show current context |
| `xv context list` | Recent contexts |
| `xv context clear` | Clear context |
| `xv env create <name>` | Create named profile (`--vault`, `--group`) |
| `xv env use <name>` | Switch to profile |
| `xv env list` | List profiles |
| `xv env pull` | Download secrets as `.env` file |
| `xv env push <file>` | Upload `.env` contents as secrets |

Aliases: `xv cx` for `xv context`, `xv ls` for `xv list`.

---

## File Storage

Requires blob storage setup via `xv init`. Gated behind the `file-ops` feature flag.

| Command | Description |
|---------|-------------|
| `xv upload <file>` | Quick upload (alias for `xv file upload`) |
| `xv download <file>` | Quick download (alias for `xv file download`) |
| `xv file upload` | Upload files (`--recursive`, `--prefix`, `--flatten`) |
| `xv file download` | Download files (`--recursive`, `--flatten`, `--output`, `--force`) |
| `xv file list` | List files (hierarchical by default; `--recursive` for flat) |
| `xv file delete` | Delete files (`--force`, `--continue-on-error`) |
| `xv file info` | File metadata |

---

## Utilities

| Command | Description |
|---------|-------------|
| `xv whoami` | Show authenticated identity and context |
| `xv audit <name>` | Access/change history for a secret or vault |
| `xv info <resource>` | Auto-detect and display info for a vault or secret |
| `xv parse <conn-string>` | Parse and display connection string components |
| `xv completion <shell>` | Generate shell completions (bash, zsh, fish, powershell) |
| `xv version` | Build info (version, git hash, target) |

---

## Configuration

| Command | Description |
|---------|-------------|
| `xv init` | Interactive setup |
| `xv config show` | Show current config |
| `xv config set <key> <value>` | Set a config value |
| `xv config path` | Show config file location |

### Hierarchy

1. CLI flags
2. Environment variables
3. Config file (`~/.config/xv/xv.conf`)
4. Defaults

---

## Output Formats

Table (default), JSON (`--format json`), YAML (`--format yaml`), raw (`--format raw`).

---

## Build & Distribution

- **Binary:** `xv`
- **Platforms:** Windows x64, macOS (Intel + Apple Silicon), Linux x64
- **Feature flags:** `file-ops` (default on)
- **Security:** `zeroize` for secrets in memory, restricted file permissions, clipboard auto-clear
