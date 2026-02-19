# UPGRADES.md — Feature Gaps & Enhancement Ideas

> Features found in other secret management CLIs (HashiCorp Vault, 1Password CLI, Infisical, Doppler) that crosstache does not currently offer. File operations excluded.

---

## 🔥 High Impact

### Secret Injection (`xv run`)
Run a subprocess with secrets injected as environment variables. Every major competitor has this.
- `xv run -- npm start` — inject all secrets from current vault/context as env vars
- `xv run --group production -- ./deploy.sh` — inject only a specific group
- Support `--watch` to auto-restart the process when secrets change (à la Infisical)
- Mask secrets in stdout/stderr output by default (à la 1Password `--no-masking` to disable)

**References:** `op run`, `infisical run`, `doppler run`

### Template / Config Injection (`xv inject`)
Inject secrets into config file templates using placeholder references, outputting the resolved file.
- `xv inject --template app.config.tmpl --out app.config`
- Support `{{ secret:my-api-key }}` or similar reference syntax
- Pipe support: `cat template.yml | xv inject > resolved.yml`

**References:** `op inject`, Infisical secret references, Doppler config file integration

### Secret Versioning
Azure Key Vault already stores secret versions — expose them in the CLI.
- `xv get <name> --version <id>` — retrieve a specific version
- `xv history <name>` — list all versions with timestamps and metadata
- `xv rollback <name> --version <id>` — restore a previous version as the current value
- `xv diff <name> --v1 <id> --v2 <id>` — compare two versions (metadata only; never diff values in plaintext unless `--show-values`)

**References:** `vault kv rollback`, `vault kv metadata get` (version history)

### Shell Completion
Auto-complete commands, subcommands, flags, and (optionally) vault/secret names.
- `xv completion bash|zsh|fish|powershell`
- Install hook: `xv --completions-install`

**References:** `op completion`, `doppler completion`, `vault -autocomplete-install`

---

## ⚡ Medium Impact

### Environment / Profile Support
Named environment profiles (dev, staging, production) that map to different vaults or groups.
- `xv env list`
- `xv env use production` — switch default vault + group in one step
- Per-directory `.xv.json` or `.xv.yaml` config (auto-detected) for project-scoped defaults

**References:** Doppler configs/environments, Infisical `--env`, 1Password Environments

### Secret References / URI Scheme
A stable URI format for referencing secrets programmatically.
- `xv://vault-name/secret-name` or `xv://vault-name/folder/secret-name`
- Usable in templates, env vars, and `xv run` substitution
- Enables cross-vault references in a single config file

**References:** `op://vault/item/field` (1Password secret references)

### Audit Log / Activity Trail
View who accessed or modified secrets and when.
- `xv audit <name>` — show access/change history for a secret
- `xv audit --vault <name>` — vault-wide activity
- Pull from Azure Activity Log / diagnostic logs

**References:** `doppler activity`, Azure Key Vault diagnostic logs

### Bulk Operations
Set or delete multiple secrets in a single command.
- `xv set KEY1=val1 KEY2=val2 KEY3=@/path/to/file` (inline multi-set)
- `xv delete --group staging` — delete all secrets in a group
- `xv set KEY=@file.pem` — load value from a local file

**References:** `infisical secrets set KEY1=val1 KEY2=val2 KEY3=@file`, Doppler bulk import

### Secret Rotation Helpers
Utilities to simplify credential rotation workflows.
- `xv rotate <name>` — generate a new random value and set it
- `xv rotate <name> --length 32 --charset alphanumeric`
- `xv rotate <name> --generator custom-script.sh`
- Optionally notify downstream services via webhook

**References:** HashiCorp Vault dynamic secrets, Azure Key Vault rotation policies

### `whoami` / Session Info
Quick check of the authenticated identity and active context.
- `xv whoami` — show logged-in user/principal, tenant, subscription, default vault

**References:** `op whoami`, `doppler me`

---

## 🧩 Nice to Have

### Interactive TUI
A terminal UI for browsing vaults and secrets, with search, preview, and quick actions.
- Browse vault → folder → secret hierarchy
- Fuzzy search across all secrets
- Copy value to clipboard from TUI

**References:** `doppler tui` (beta), `fzf`-based secret selectors

### Secret Diff Between Vaults
Compare secrets across two vaults or environments.
- `xv diff --vault prod-vault --vault staging-vault`
- Show added/removed/changed keys (never plaintext values by default)

**References:** Common request in Doppler/Infisical communities

### `.env` File Sync
Two-way sync between a local `.env` file and vault secrets.
- `xv env pull --format dotenv > .env` — download secrets as `.env`
- `xv env push .env` — upload `.env` contents as secrets
- `xv run --env-file .env.template -- npm start` — resolve references in a `.env` template

**References:** `infisical secrets set --file .env`, Doppler `.env` integration

### Secret Expiration & TTL
Set expiration dates on secrets and surface warnings.
- `xv set <name> --expires 2025-12-31`
- `xv list --expiring 30d` — show secrets expiring within 30 days
- Azure Key Vault already supports expiry attributes; just expose them

### Webhook / Event Notifications
Notify external systems when secrets change.
- `xv webhook add <url> --event secret.updated --vault my-vault`
- Leverages Azure Event Grid or Key Vault events

### Secret Masking in Logs
When using `xv run`, scan subprocess stdout/stderr and redact any values matching known secrets.

**References:** 1Password `op run` masks by default

### Cross-Vault Copy / Move
Copy or move secrets between vaults without manual export/import.
- `xv copy <name> --from vault-a --to vault-b`
- `xv move <name> --from vault-a --to vault-b`

### Plugin / Extension System
Allow custom commands or integrations via a plugin mechanism.
- `xv plugin install slack-notify`
- Enables community extensions without bloating the core CLI

**References:** 1Password `op plugin`, HashiCorp Vault plugin architecture

### Self-Update
Built-in update command to fetch the latest release.
- `xv update` — check and install the latest version
- `xv update --check` — just check without installing

**References:** `doppler update`, `op update`, `infisical update`

---

## Summary by Priority

| Priority | Feature | Complexity |
|----------|---------|------------|
| 🔥 High | Secret injection (`xv run`) | Medium |
| 🔥 High | Template injection (`xv inject`) | Medium |
| 🔥 High | Secret versioning & rollback | Low–Medium |
| 🔥 High | Shell completion | Low |
| ⚡ Med | Environment profiles | Medium |
| ⚡ Med | Secret references / URI scheme | Medium |
| ⚡ Med | Audit log | Low–Medium |
| ⚡ Med | Bulk set/delete | Low |
| ⚡ Med | Secret rotation helpers | Medium |
| ⚡ Med | `whoami` | Low |
| 🧩 Nice | Interactive TUI | High |
| 🧩 Nice | Vault diff | Medium |
| 🧩 Nice | `.env` file sync | Low–Medium |
| 🧩 Nice | Secret expiration/TTL | Low |
| 🧩 Nice | Webhook notifications | Medium |
| 🧩 Nice | Secret masking in logs | Medium |
| 🧩 Nice | Cross-vault copy/move | Low |
| 🧩 Nice | Plugin system | High |
| 🧩 Nice | Self-update | Low |
