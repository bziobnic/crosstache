# Env Profiles (`.xv.toml`)

`xv` looks for a `.xv.toml` in the current directory and walks up to
the filesystem root. The first one it finds wins. Drop a `.xv.boundary`
file in any directory to stop the walk-up before that point — useful
in monorepos to prevent leaking parent config into a sibling project.

## Schema

```toml
default_env = "dev"

[env.dev]
vault = "myproj-dev-kv"
resource_group = "myproj-rg"
group = "backend"          # optional
folder = "app/database"    # optional
backend = "azure"          # optional: azure | local | aws

[env.prod]
vault = "myproj-prod-kv"
resource_group = "myproj-prod-rg"

[env.local-dev]
backend = "local"          # use local age-encrypted backend for this env
```

All fields except `[env.<name>]` blocks are optional. Unknown fields are ignored
when parsed and may be dropped when `xv env` rewrites the file, so keep custom
project metadata outside `.xv.toml`.

## Active env selection

Priority (highest first):

1. `XV_ENV` environment variable
2. `--env <name>` CLI flag
3. `default_env` field in `.xv.toml`
4. Error: `xv-env-not-defined` (exit `3`) listing the available envs.

## How env defaults are applied

Each command's `--vault` / `--resource-group` / `--group` / `--folder`
flag still overrides everything. When the flag is absent, `xv`
resolves in this order:

1. CLI flag (if provided)
2. The active env's field in `.xv.toml`
3. The legacy `.xv/context` JSON (deprecated; see below)
4. The user's global config default

### Backend resolution

The `backend` field in an env profile selects which secrets backend that env
targets when no global backend override is active. Resolution order (highest
first):

1. `--backend` CLI flag
2. `XV_BACKEND` environment variable (wired through the same global flag layer)
3. `backend` field in the active env profile (`.xv.toml`)
4. `backend` key in the global config file (`xv.conf`)
5. Default: `azure`

Valid values: `azure`, `local`, `aws` (canonical names only — aliases like `az`, `file`, or `secretsmanager` are not accepted in `.xv.toml`; named backend keys defined under `[backends.*]` in `xv.conf` are also not supported here).

Use `xv config show --resolved` when a command picks an unexpected backend or
vault. It prints the effective backend, env, vault, and resource group with the
source layer that supplied each value.

## Backend-prefixed addressing (`xv://backend:vault/secret`)

Secret references in templates and environment variables accept an optional
backend prefix before the vault name:

```
xv://vault/secret            # use the active/default backend (unchanged behaviour)
xv://azure:my-kv/API_KEY     # always resolve against the Azure backend
xv://aws:prod-sm/DB_PASS     # always resolve against the AWS Secrets Manager backend
xv://local:default/DEV_KEY   # always resolve against the local age-encrypted backend
```

The prefix is separated from the vault name by `:`. Vault names must not
contain `:` (they never do on any supported backend). When no prefix is given
the existing behaviour is preserved — the secret is fetched from whichever
backend the active env profile selects.

Valid backend names (and aliases):

| Canonical | Aliases |
|-----------|---------|
| `azure`   | `az`, `keyvault` |
| `local`   | `file`, `age` |
| `aws`     | `secretsmanager`, `asm` |

### Example: mixed-backend template

```
# config/.env.template
DATABASE_URL=postgres://{{ secret:db-host }}/app
STRIPE_KEY=xv://aws:prod-secrets/STRIPE_API_KEY
INTERNAL_TOKEN=xv://local:default/INTERNAL_TOKEN
```

`xv inject` resolves all three references — `db-host` from the active vault,
`STRIPE_API_KEY` from AWS Secrets Manager, and `INTERNAL_TOKEN` from the
local backend — without switching context.

### `xv migrate` with per-side vault

`xv migrate --from` and `--to` also accept the `backend:vault` form so each
side of a migration can target a different vault:

```bash
# migrate from Azure vault "dev-kv" to local vault "default"
xv migrate --from azure:dev-kv --to local:default

# migrate from AWS prod to Azure staging (vault from --vault flag)
xv migrate --from aws:prod-secrets --to azure:staging-kv

# original form (backend only, vault from --vault flag) still works
xv migrate --from local --to aws --vault my-vault
```

When both sides share the same vault name, the single `--vault` flag is
sufficient. When they differ, embed the vault in the `--from`/`--to` value.

## Cross-boundary notice

When a `.xv.toml` is found in an ancestor directory (not in cwd),
the first command in a process prints a one-time stderr line:

```
using config from /path/to/.xv.toml (env: dev)
```

To opt out of walk-up entirely, set `XV_NO_PARENT_CONFIG=1` in your
environment. With that set, only a `.xv.toml` directly in cwd will
be considered.

## Migration from `.xv/context`

The legacy `.xv/context` JSON file (created by `xv context use`) keeps
working as a fallback when no `.xv.toml` is found. You'll see this
warning the first time it loads in any process:

```
warning: legacy .xv/context loaded from <path>; consider migrating to .xv.toml — see docs/env-profiles.md
```

Migrating: run `xv context init` in the project root and answer the
prompts (or pass `--non-interactive --backend azure --vault X
--resource-group Y` for Azure scripts; local and AWS profiles do not require
`--resource-group`). You can then delete `.xv/context`.

## Commands

| Command | What it does |
|---------|--------------|
| `xv context init` | Creates `.xv.toml` in cwd. Interactive by default; pass `--non-interactive --backend B --vault X [--resource-group RG]` for scripts (`resource_group` is required only for Azure). `--force` to overwrite. |
| `xv env list` | Lists envs in the resolved `.xv.toml` with the active one starred. `xv context envs` is an alias. |
| `xv env use <name>` | Writes `default_env = "<name>"` into the nearest `.xv.toml`. |
| `xv env create <name> --vault V --resource-group RG [--backend B] [--group G] [--folder F] [--default]` | Adds `[env.<name>]` to the nearest `.xv.toml` (creates the file if absent). `--resource-group` is currently required by this command even for local/AWS profiles. `--default` also sets `default_env`. |
| `xv env delete <name> [-f]` | Removes `[env.<name>]` from the resolved `.xv.toml`. Clears `default_env` if it pointed at that env. |
| `xv env show` | Shows the currently-active env (source path, backend, vault, resource_group, group, folder). |
| `xv --env <name> <command>` | Override the active env for one command. |
| `xv config show --resolved` | Shows the winning backend/env/vault/resource-group values and the source layer for each. |
