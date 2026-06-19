# Feature Reference

> Complete command reference for `xv`. Backend behavior varies across Azure Key
> Vault, AWS Secrets Manager, and the local age-encrypted backend; backend-
> specific constraints are called out below.

---

## Backend capability notes

| Area | Azure | AWS | Local |
|------|-------|-----|-------|
| Secrets CRUD, groups, notes, folders, expiry | Native Key Vault secrets + tags | Secrets Manager + tags/description | age-encrypted files |
| Vaults | Azure Key Vault resources | Prefix-based virtual vaults (`<vault>/.xv-vault`) | Directories under the local store |
| Audit | Azure Activity Log path | CloudTrail `LookupEvents` (`cloudtrail:LookupEvents` required) | Unsupported |
| Native rotation | Unsupported (`xv rotate` generates a new value) | `xv rotate --native` calls `RotateSecret` and requires a rotation Lambda | Unsupported |
| Sharing / RBAC commands | Azure RBAC | Unsupported; commands return IAM resource-policy hints | Unsupported |
| File storage | Azure Blob Storage; includes `xv file sync` | S3 when `[aws].s3_bucket` or `XV_AWS_S3_BUCKET` is set; sync unsupported | Not a public `xv file` workflow today |

---

## Secrets

| Command | Description |
|---------|-------------|
| `xv set <name>` | Create a secret (interactive prompt, `--stdin`, or bulk `K1=v1 K2=v2`) |
| `xv get <name>` | Retrieve a secret (clipboard by default; `--raw` for stdout) |
| `xv list` | List secrets (`--group`, `--all`, `--expiring <period>`, `--expired`, `--page-size`, `--page`) |
| `xv delete <name>` | Soft-delete a secret (`--force` to skip confirmation) |
| `xv update <name>` | Update value, groups, folder, note, tags, expiry; supports `--rename` |
| `xv purge <name>` | Permanently delete a soft-deleted secret |
| `xv restore <name>` | Restore a soft-deleted secret |
| `xv history <name>` | Show version history |
| `xv rollback <name>` | Restore a previous version (`--version <id>`) |
| `xv rotate <name>` | Generate new random value (`--length`, `--charset`, `--generator`); `--native` triggers AWS Secrets Manager rotation |
| `xv copy <name>` | Copy a secret between vaults (`--from`, `--to`) |
| `xv move <name>` | Move a secret between vaults (`--from`, `--to`) |

### Metadata & Organization

- **Folders** — `--folder "app/database"` on `set` or `update`
- **Groups** — `--group <name>` on `update` (multiple allowed); filter with `list --group`
- **Notes** — `--note "description"` on `set` or `update`
- **Tags** — `-t key=value` on `update`; `--replace-tags` / `--replace-groups` for replace mode
- **Expiry** — `--expires YYYY-MM-DD` on `set` or `update`; `--clear-expires` to remove

### Name Sanitization

Names are automatically sanitized for backend compatibility. Azure allows
alphanumeric + hyphen and hashes names beyond 127 characters; AWS accepts its
broader Secrets Manager charset and percent-encodes unsupported bytes; local
storage uses filename-safe encoding. Original names are preserved in metadata
where a backend needs reverse lookup.

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
| `xv vault list` | List vaults (`--page-size`, `--page`) |
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
| `xv vault share list` | List vault access assignments (`--page-size`, `--page`) |
| `xv share grant` | Grant secret-level access |
| `xv share revoke` | Revoke secret-level access |
| `xv share list` | List secret permissions (`--page-size`, `--page`) |

---

## Context & Environments

| Command | Description |
|---------|-------------|
| `xv context use <vault>` | Switch vault context (`--global`, `--local`) |
| `xv context show` | Show current context |
| `xv context list` | Recent contexts |
| `xv context clear` | Clear context |
| `xv env list` | List `[env.*]` blocks in the resolved `.xv.toml` (`xv context envs` is an alias) |
| `xv env use <name>` | Set `default_env = "<name>"` in the nearest `.xv.toml` |
| `xv env create <name>` | Add `[env.<name>]` to the nearest `.xv.toml` (`--vault`, `--resource-group`, `--backend`, `--default`) |
| `xv env delete <name>` | Remove `[env.<name>]` from the resolved `.xv.toml` (`-f` to skip confirmation) |
| `xv env show` | Show the active env (source, backend, vault, resource_group, group, folder) |
| `xv env pull` | Download secrets as `.env` file |
| `xv env push <file>` | Upload `.env` contents as secrets |

Aliases: `xv cx` for `xv context`, `xv ls` for `xv list`.

---

## File Storage

Requires the `file-ops` feature flag (enabled by default). Setup depends on the
active backend:

- **Azure**: configure Azure Blob Storage with `xv init` or
  `AZURE_STORAGE_ACCOUNT` / `AZURE_STORAGE_CONTAINER`.
- **AWS**: build with `--features aws` (or use a release binary) and set
  `[aws].s3_bucket` or `XV_AWS_S3_BUCKET` to an existing bucket. `xv` stores
  objects under `<vault>/files/<name>` and does not create buckets.

| Command | Description |
|---------|-------------|
| `xv upload <file>` | Quick upload (alias for `xv file upload`) |
| `xv download <file>` | Quick download (alias for `xv file download`) |
| `xv file upload` | Upload files (`--recursive`, `--prefix`, `--flatten`) |
| `xv file download` | Download files (`--recursive`, `--flatten`, `--output`, `--force`) |
| `xv file list` | List files (hierarchical by default; `--recursive` for flat; `--page-size`, `--page`, `--limit`) |
| `xv file delete` | Delete files (`--force`, `--continue-on-error`) |
| `xv file info` | File metadata |
| `xv file sync` | Sync local directory with blob prefix (`--direction` up/down/both, `--dry-run`, `--delete`); Azure path only today |

AWS supports upload/download/list/delete/info through S3. Attempting
`xv file sync` on the AWS backend returns a setup-neutral error that recommends
recursive upload/download as the current bulk-transfer path.

---

## Utilities

| Command | Description |
|---------|-------------|
| `xv whoami` | Show authenticated identity and context |
| `xv audit <name>` | Access/change history for a secret or vault (Azure Activity Log or AWS CloudTrail; unsupported on local) |
| `xv info <resource>` | Auto-detect and display info for a vault or secret |
| `xv parse <conn-string>` | Parse and display connection string components |
| `xv completion <shell>` | Generate shell completions (bash, zsh, fish, powershell) |
| `xv version` | Build info (version, git hash, target) |

### Local backend maintenance

| Command | Description |
|---------|-------------|
| `xv local encrypt-metadata` | Re-encrypt existing plaintext local secret metadata after enabling `[local].encrypt_metadata = true`; safe to re-run |
| `xv local encrypt-metadata --dry-run` | Count plaintext metadata files without modifying the store |

---

## Cross-cloud migration (v0.10)

`xv migrate --from <source> --to <target>` copies secrets between backends. Supports Azure ↔ AWS ↔ Local in any combination. Hardening features:

- `--on-conflict skip|replace|fail` — controls behavior when target secret exists
- `--dry-run` — preview without changes
- `--filter "<glob>"` — restrict to matching names
- `--concurrency N` — bounded parallel transfers (default 8)
- Idempotent: re-runs detect previously-migrated secrets via `xv:migrated_from` tag
- Exponential backoff on rate limiting

See [migration.md](migration.md) for the full guide.

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
3. Project config (`.xv.toml`, discovered by walking up from the current directory)
4. User config file (`~/.config/xv/xv.conf`)
5. Defaults

Key backend config:

```toml
# Azure remains the default when no backend is selected.
backend = "aws" # or "azure", "local", or a named backend

[aws]
region = "us-east-1"       # falls through to AWS_REGION
profile = "default"        # falls through to AWS_PROFILE
default_vault = "myproj-kv"
s3_bucket = "my-xv-files"  # optional; enables S3-backed `xv file`

[local]
store_path = "~/.xv/store"
key_file = "~/.xv/key.txt"
default_vault = "default"
encrypt_metadata = false   # when true, run `xv local encrypt-metadata`
```

---

## Output Formats

Table (default), JSON (`--format json`), YAML (`--format yaml`), CSV (`--format csv`), plain (`--format plain`), raw (`--format raw`).

---

## Build & Distribution

- **Binary:** `xv`
- **Platforms:** Windows x64, macOS (Intel + Apple Silicon), Linux x64
- **Feature flags:** `file-ops` (default on), `tui`, `aws`
- **Release binaries:** built with `--features tui,aws`; source builds need
  `--features aws` for AWS Secrets Manager / CloudTrail / S3 support
- **Security:** `zeroize` for secrets in memory, restricted file permissions, clipboard auto-clear
