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

[env.prod]
vault = "myproj-prod-kv"
resource_group = "myproj-prod-rg"
```

All fields except `[env.<name>]` blocks are optional. New fields (output
defaults, mask lists, etc.) will be added in v0.7.x without breaking
existing files.

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
prompts (or pass `--non-interactive --vault X --resource-group Y` for scripts).
You can then delete `.xv/context`.

The legacy fallback is removed in v0.8.

## Commands

| Command | What it does |
|---------|--------------|
| `xv context init` | Creates `.xv.toml` in cwd. Interactive by default; pass `--non-interactive --vault X --resource-group Y` for scripts. `--force` to overwrite. |
| `xv context envs` | Lists envs in the resolved `.xv.toml` with the active one starred. |
| `xv context show` | Existing command; now also shows the active env block when a `.xv.toml` resolves. |
| `xv --env <name> <command>` | Override the active env for one command. |

## How `xv env` differs

`xv env create / use / list / pull / push` manage **global, user-scoped**
profiles in your user config (one set of named profiles per machine,
per user). `.xv.toml` env profiles are **project-scoped**, checked into
the repo, shared across the team. They coexist; when both are present,
the project `.xv.toml` wins.
