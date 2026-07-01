# crosstache

A cross-platform secrets manager for the command line. Pluggable backends: Azure Key Vault, AWS Secrets Manager, or local age-encrypted files. The binary is `xv`.

```bash
xv set DB_PASSWORD                     # store a secret (prompts for value)
xv get DB_PASSWORD                     # copy to clipboard (auto-clears in 30s)
xv get DB_PASSWORD --raw                # print to stdout (for scripts)
xv run -- npm start                    # run a process with all secrets injected as env vars
xv find db --names-only | fzf          # interactive picker, pipe-friendly
xv scan install                        # block secret leaks before commit
```

**Phase 1 (v0.7) highlights:** structured exit codes for scripting · `.xv.toml` env profiles with walk-up resolution · ranked fuzzy `xv find` · pre-commit leak scanner that matches files against your *actual* vault values · optional read-only TUI behind a feature flag.

---

## Table of contents

- [Quick start](#quick-start)
- [Local Backend (age-encrypted files)](#local-backend-age-encrypted-files)
- [Installation](#installation)
- [Common workflows](#common-workflows) — end-to-end recipes
- [Secrets — CRUD](#secrets--crud)
- [Reading secrets — clipboard, stdout, JSON](#reading-secrets--clipboard-stdout-json)
- [Search & filter](#search--filter) — `xv find`, `xv ls --names-only`, fzf integration
- [Secret injection — `xv run`](#secret-injection--xv-run)
- [Template rendering — `xv inject`](#template-rendering--xv-inject)
- [Project env profiles — `.xv.toml`](#project-env-profiles--xvtoml)
- [Vault management](#vault-management)
- [Cross-vault operations — diff, copy, move](#cross-vault-operations--diff-copy-move)
- [Files (blob storage)](#files-blob-storage)
- [Pre-commit leak scanner — `xv scan`](#pre-commit-leak-scanner--xv-scan)
- [Terminal UI — `xv tui`](#terminal-ui--xv-tui)
- [Scripting & CI](#scripting--ci) — exit codes, JSON envelope, examples
- [Configuration](#configuration)
- [Authentication](#authentication)
- [Troubleshooting](#troubleshooting)
- [Security model](#security-model)
- [Development](#development)

---

## Quick start

```bash
# 1. Install
curl -sSL https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.sh | bash

# 2. Set up your first vault (interactive)
xv init

# 3. Store and retrieve a secret
xv set DB_PASSWORD                       # prompts for value (won't echo)
xv get DB_PASSWORD                       # copies to clipboard, auto-clears in 30s
xv get DB_PASSWORD --raw                 # prints to stdout (scripts)

# 4. Inject secrets into a process
xv run -- ./my-app                       # all secrets in active vault → env vars

# 5. Browse interactively (optional TUI)
cargo install crosstache --features tui
xv tui
```

That's the 5-minute path. The rest of this doc shows what's possible once you've got the basics.

---

## Local Backend (age-encrypted files)

The local backend stores secrets as age-encrypted files on your machine — no cloud account needed.

### Quick start

```bash
# Initialize with local backend
xv init
# → Choose "Local" when prompted

# Or set backend via env
export XV_BACKEND=local

# Basic secret operations
xv set DB_PASSWORD
xv get DB_PASSWORD --raw
xv list
```

### How it works

Secrets are encrypted with [age](https://age-encryption.org/) and stored as
individual files. By default, existing stores keep the legacy reversible
filename layout for compatibility:

```
~/.xv/
├── key.txt              # Your age private key (0600 permissions)
├── recipients.txt       # Public key for encryption
└── store/
    └── vaults/
        └── default/
            ├── .vault.json
            └── secrets/
                ├── DB_PASSWORD.age          # Encrypted value
                └── DB_PASSWORD.meta.json    # Metadata (name, groups, tags)
```

For stronger local-at-rest privacy, enable opaque filenames. With
`[local].opaque_filenames = true`, active secrets, versions, and trash entries
use keyed-hash stems instead of secret names, and `secrets/.index.age` stores
the encrypted stem-to-name index needed for listing:

```text
secrets/
├── mjw4v2q6m4w7n6k5z3c2b7a8nq.age
├── mjw4v2q6m4w7n6k5z3c2b7a8nq.meta.json
├── .index.age
└── .versions/
    └── mjw4v2q6m4w7n6k5z3c2b7a8nq/
```

Use `xv local migrate --dry-run` before changing an existing store, then
`xv local migrate` to rename legacy active files, version archives, and trash
entries. New writes also upgrade the touched secret when opaque filenames are
enabled.

### Migrating between backends

```bash
# Copy all secrets from Azure to local
xv migrate --from azure --to local

# Copy from local to Azure
xv migrate --from local --to azure --vault my-keyvault

# Preview what would be migrated
xv migrate --from azure --to local --dry-run

# Filter by pattern
xv migrate --from azure --to local --filter "db-*"
```

### Configuration

```toml
# ~/.config/xv/xv.conf
backend = "local"

[local]
store_path = "~/.xv/store"
key_file = "~/.xv/key.txt"
default_vault = "default"
# Encrypt secret metadata (notes, tags, folders, expiry) at rest with the
# same age key as the values. Default false. Secret *names* stay visible as
# on-disk filenames unless opaque_filenames is also enabled. After enabling on
# an existing store, run `xv local encrypt-metadata` to convert already-written
# metadata.
encrypt_metadata = false
# Store active secrets, versions, and trash entries under opaque keyed-hash
# stems plus an encrypted `.index.age`. Default false so existing stores are
# unchanged until you opt in and migrate.
opaque_filenames = false
```

Local maintenance commands:

```bash
xv local encrypt-metadata --dry-run     # preview metadata encryption changes
xv local encrypt-metadata               # encrypt existing .meta.json files
xv local migrate --dry-run              # preview opaque filename renames
xv local migrate                        # apply opaque filename layout
```

---

## AWS Secrets Manager backend

Use AWS Secrets Manager as the underlying secret store.

```bash
xv init  # pick "aws" when prompted
# or edit ~/.config/xv/xv.conf:
# backend = "aws"
# [aws]
# region = "us-east-1"
# profile = "default"
# default_vault = "myproj-kv"
# Optional: enable `xv file` on AWS with an existing S3 bucket
# s3_bucket = "my-team-xv-files"
```

Multi-region:

```toml
backend = "aws-east"
[named_backends.aws-east]
type = "aws"
region = "us-east-1"
[named_backends.aws-west]
type = "aws"
region = "us-west-2"
```

### AWS-specific workflows

```bash
# CloudTrail-backed audit history (requires cloudtrail:LookupEvents)
xv audit DB_PASSWORD --backend aws --days 7
xv audit --vault myproj-kv --backend aws --operation PutSecretValue

# Native AWS Secrets Manager rotation (requires a rotation Lambda on the secret)
xv rotate DB_PASSWORD --backend aws --native

# S3-backed file storage (requires [aws].s3_bucket or XV_AWS_S3_BUCKET)
xv file upload ./config.json --backend aws
xv file list --backend aws --prefix releases/
```

AWS file storage uses one configured, pre-existing S3 bucket and stores objects
under `<vault>/files/<name>` so vaults stay isolated. `xv` does not create the
bucket. `xv file sync` is not supported on AWS yet; use recursive upload/download
for bulk transfers.

`xv share` is still not implemented against AWS because crosstache does not
manage IAM resource policies. The command returns a copyable
`aws secretsmanager put-resource-policy` hint instead of attempting a grant.

### Cross-cloud migration

```bash
# Move secrets from Azure to AWS
xv migrate --from azure --to aws --vault myproj-kv

# Preview first
xv migrate --from azure --to aws --vault myproj-kv --dry-run
```

See [docs/migration.md](docs/migration.md) for the full guide.

---

## Installation

### Quick install

**macOS / Linux:**

```bash
curl -sSL https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.sh | bash
```

**Windows (PowerShell):**

```powershell
iwr -useb https://raw.githubusercontent.com/bziobnic/crosstache/main/scripts/install.ps1 | iex
```

### Pre-built binaries

[Releases page](https://github.com/bziobnic/crosstache/releases) — choose the right archive:

| Platform | File |
|----------|------|
| Windows x64 | `xv-windows-x64.zip` |
| macOS Intel | `xv-macos-intel.tar.gz` |
| macOS Apple Silicon | `xv-macos-apple-silicon.tar.gz` |
| Linux x64 | `xv-linux-x64.tar.gz` |

### Build from source

```bash
git clone https://github.com/bziobnic/crosstache.git
cd crosstache
cargo install --path .
# With the read-only TUI:
cargo install --path . --features tui
# With AWS backend support (Secrets Manager / CloudTrail / S3):
cargo install --path . --features tui,aws
```

> **Note:** Pre-built release binaries (the downloads on the
> [Releases](https://github.com/bziobnic/crosstache/releases) page) are
> built with `--features tui,aws` — they support Azure, local, AND AWS
> backends out of the box. You only need the `aws` feature flag above when
> building from source; the default `cargo build` omits AWS to keep source
> builds lean.

### macOS Gatekeeper note

If macOS blocks the binary ("developer cannot be verified"):

```bash
xattr -d com.apple.quarantine ~/.local/bin/xv
```

---

## Common workflows

### Setting up a new project

```bash
# 1. Create the vault and grant yourself access
xv vault create myproj-dev-kv --resource-group myproj-rg --location eastus

# 2. Drop a project config so collaborators don't need --vault on every command
cd ~/code/myproj
xv context init --non-interactive --vault myproj-dev-kv --resource-group myproj-rg

# 3. Bulk-import existing .env file
xv env push .env

# 4. Verify
xv list
```

### Onboarding a new developer

```bash
# Clone the repo (which now contains .xv.toml)
git clone https://github.com/myorg/myproj.git
cd myproj

# Authenticate
az login

# Run — vault and env auto-resolve from the .xv.toml in the repo
xv list                                  # works without --vault
xv run -- npm start
```

### Secret rotation with zero downtime

```bash
# Generate a new value (32 chars alphanumeric by default; configurable)
xv rotate DB_PASSWORD --length 64

# Verify history
xv history DB_PASSWORD

# If something goes wrong, roll back
xv rollback DB_PASSWORD --version 2 --force
```

### Branching by environment

```bash
# .xv.toml in repo:
#
#   default_env = "dev"
#
#   [env.dev]
#   vault = "myproj-dev-kv"
#   resource_group = "myproj-rg"
#
#   [env.prod]
#   vault = "myproj-prod-kv"
#   resource_group = "myproj-prod-rg"

xv list                                  # uses dev (default)
xv --env prod list                       # explicit override
XV_ENV=prod xv list                      # via env var (highest priority)
xv --env staging list                    # error: xv-env-not-defined; lists available envs; exit 3
```

### Pre-commit leak prevention

```bash
xv scan install                          # writes .git/hooks/pre-commit
git commit -m "..."                      # hook scans staged files; exit 50 blocks commit on findings
```

---

## Secrets — CRUD

### Create

```bash
xv set API_KEY                           # interactive — prompts (no echo)
xv set API_KEY --stdin < key.txt         # from stdin (e.g. piped from openssl)
xv set API_KEY --value "literal-value"   # inline (avoid; appears in shell history)
xv set DB_HOST=db.prod DB_PORT=5432 DB_PASSWORD=@/etc/secret/db-pw  # bulk + file refs
xv set CONFIG --folder myapp/database    # organize hierarchically
xv set API_KEY --group production --group api-tier
xv set API_KEY --expires 2026-12-31 --not-before 2026-06-01
xv set DB_USER --note "primary db reader" --tag owner=team-data --tag env=prod
```

The `@filepath` syntax loads from a file at create time — useful for keys, certs, JWT signing material:

```bash
xv set TLS_CERT=@/etc/ssl/cert.pem JWT_PRIVATE_KEY=@./jwt.key
```

`xv gen --save` stores a generated value through the same secret-write path as
`xv set`, including write-time metadata:

```bash
xv gen --length 32 --save API_KEY --group production --note "rotated"
xv gen --charset base64 --save WEBHOOK_SECRET --folder integrations/payments
```

Metadata flags on `xv gen` require `--save`; plain `xv gen --group production`
is rejected because there is no saved secret to annotate.

### Update

```bash
xv update API_KEY --note "rotated 2026-04-30 by ops"
xv update API_KEY --group production --group api-tier   # repeatable
xv update API_KEY --folder myapp/edge                    # move to another folder
xv update API_KEY --tag rotated-by=alice                 # custom tag
```

### Delete and recover

```bash
xv delete API_KEY                        # soft-delete (alias: rm)
xv list                                  # gone from default list
xv list --all                            # see soft-deleted ones too
xv restore API_KEY                       # bring it back

xv delete API_KEY --force                # skip confirmation
xv delete --group legacy --force         # bulk delete every secret in 'legacy' group
xv purge API_KEY --force                 # permanent delete (irreversible)
```

### History and rollback

```bash
xv history API_KEY                       # all versions, newest first
xv history API_KEY --format json         # for scripts

xv get API_KEY --version v3              # read a specific historical version
xv rollback API_KEY --version 2 --force  # restore as new latest version
```

### Rotation

```bash
xv rotate API_KEY                            # new 32-char alphanumeric value
xv rotate API_KEY --length 64                # longer
xv rotate API_KEY --charset hex              # hex / base64 / numeric / uppercase / lowercase / alphanumeric / alphanumeric-symbols
xv rotate API_KEY --generator ./mygen.sh     # custom generator (validated for ownership + 0700 perms)
xv rotate API_KEY --show-value               # echo the new value to stdout (otherwise silent)
```

---

## Reading secrets — clipboard, stdout, JSON

### Default — clipboard with auto-clear

```bash
xv get DB_PASSWORD                       # copies to clipboard, auto-clears in 30s
```

The countdown is configurable (`xv config set clipboard_timeout 60`; `0` disables).

### Pipe-friendly raw

```bash
xv get DB_PASSWORD --raw                 # to stdout, no trailing newline noise
psql -U me -h db.prod -d main -W <<< "$(xv get DB_PASSWORD --raw)"
DB_PW=$(xv get DB_PASSWORD --raw); export DB_PW
```

### Structured JSON (for scripts)

```bash
xv get DB_PASSWORD --format json
# {"name":"DB_PASSWORD","value":"hunter2","groups":["backend","prod"], ...}

# Pipe into jq:
xv get DB_PASSWORD --format json | jq -r '.value'
```

### When the secret doesn't exist

```bash
xv get DB_PASSWURD                       # typo
# error[xv-secret-not-found]: Secret not found: DB_PASSWURD
#   did you mean: DB_PASSWORD?
#   hint: Run 'xv list' to see secrets in the active vault.
# Exit 10
```

The "did you mean" suggestion uses fuzzy matching (Levenshtein, distance ≤ 2). With `--format json`:

```json
{
  "error": {
    "code": "xv-secret-not-found",
    "message": "Secret not found: DB_PASSWURD",
    "exit_code": 10,
    "suggestion": "DB_PASSWORD"
  }
}
```

---

## Search & filter

### List

```bash
xv ls                                    # grid of folders (prod/) and root secrets
xv ls prod                               # inside a folder
xv ls prod -l                            # long listing: name, updated, groups, note
xv ls -r                                 # every secret, flattened
xv ls --format table                     # the classic table
xv list --group production               # filter by group
xv list --all                            # include disabled / soft-deleted
xv list --expiring 30d                   # secrets with expiry in next 30 days
xv list --expired                        # already expired
xv list --no-cache                       # bypass local cache
```

### Pagination

```bash
xv list --page-size 50                   # first 50 rows
xv list --page-size 50 --page 2          # next 50
xv list --pager auto                     # pipe through pager when output is a TTY
xv list --format json --page-size 50     # JSON: array of exactly 50 items, no envelope
```

Pagination footer (table format only):

```
Showing 51-100 of 137 item(s) — page 2 of 3
Next page: xv list --page 3 --page-size 50
```

`xv vault list`, `xv file list`, `xv share list`, and `xv vault share list` all accept `--page` / `--page-size` / `--pager` too.

### Names-only — for piping

```bash
xv ls --names-only                       # one name per line, no headers, no ANSI
xv ls --names-only | wc -l               # count secrets
xv ls --names-only --group production    # filter still applies
```

`--names-only` overrides `--format` and writes to stdout regardless of TTY status. Designed for scripts and pipes.

### Fuzzy — `xv find`

Ranked search using `nucleo` (the same matcher Helix uses):

```bash
xv find db                               # rank by name
xv find db --in folder                   # also search folder field
xv find db --in folder --in groups       # multiple fields
xv find db --in tags                     # search custom tags
xv find db --limit 10                    # cap rows (default 50)
xv find db --min-score 0.5               # tighter threshold (0.0..=1.0; default 0.3)
xv find db --all-vaults                  # search every vault you can list
xv find db --names-only                  # pipe-friendly
xv find db --format json                 # [{name, score, folder, groups}]
```

### Pipe into fzf — interactive picker

```bash
# By name only
xv get "$(xv ls --names-only | fzf)"

# By fuzzy match
xv get "$(xv find db --names-only | fzf)"

# Run a process with whichever secret you pick
selected=$(xv ls --names-only | fzf)
xv run --include "$selected" -- ./debug.sh
```

The previous interactive `xv find` was replaced in v0.6.1; see [`docs/find.md`](docs/find.md) for the migration table.

---

## Secret injection — `xv run`

Run a process with secrets available as environment variables:

```bash
xv run -- npm start                              # all secrets in active vault → env
xv run --group production -- ./deploy.sh          # only one group
xv run --include DB_PASSWORD --include API_KEY -- ./script.sh
xv run --exclude LEGACY_TOKEN -- ./script.sh
xv run --no-masking -- ./debug.sh                 # don't mask values in stdout/stderr
xv run --vault other-vault -- env                 # one-off vault override
```

Values are masked in stdout/stderr by default — accidental `echo $DB_PASSWORD`
shows `[REDACTED]`. Masking streams in bounded chunks (64 KiB read windows with
overlap for secrets split across chunk boundaries), so newline-free or very
large child output does not grow memory without limit. Use `--no-masking` only
when you understand the consequences.

---

## Template rendering — `xv inject`

Render config files with `{{ secret:NAME }}` references resolved:

```bash
# template.yml:
#   db_password: "{{ secret:DB_PASSWORD }}"
#   api_key:     "{{ secret:STRIPE_KEY }}"
#   cross_vault: "{{ xv://other-vault/SHARED_TOKEN }}"

xv inject --template template.yml --out app.config
cat template.yml | xv inject > resolved.yml          # also reads stdin

# Cross-vault references (xv://vault-name/secret-name) work without context switching.
```

---

## Project env profiles — `.xv.toml`

Drop a `.xv.toml` at your project root and `xv` resolves vault, resource group, group, and folder defaults from it. Walks up from cwd to find the nearest one.

### Schema

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

### Scaffold one interactively

```bash
xv context init                              # interactive prompts (seeded from your global config)
xv context init --non-interactive \
                --vault myproj-dev-kv \
                --resource-group myproj-rg   # CI-friendly
xv context init --force                      # overwrite an existing .xv.toml
```

### Active env selection (priority)

1. `XV_ENV` env var
2. `--env <name>` CLI flag
3. `default_env` in `.xv.toml`
4. Error `xv-env-not-defined` (exit 3) listing available envs

```bash
xv list                                  # uses default_env (dev)
xv --env prod list                       # one-off override
XV_ENV=staging xv list                   # session override
```

### Manage envs

```bash
xv env list                              # list envs with * on the active one
# Project envs (from /code/myproj/.xv.toml, default: dev):
#   * dev   backend=azure  vault=myproj-dev-kv  resource_group=myproj-rg
#     prod  backend=azure  vault=myproj-prod-kv

xv env use prod                          # set default_env = "prod" in .xv.toml
xv env show                              # show active env fields
xv env create stage \
    --vault myproj-stage-kv \
    --resource-group myproj-rg-stage     # add [env.stage] to .xv.toml
xv env delete stage -f                   # remove [env.stage]

xv context show                          # full context, including resolved env defaults
```

### Walk-up boundaries

When a `.xv.toml` is found in an ancestor directory, you'll see a one-time stderr line per process:

```
using config from /code/myproj/.xv.toml (env: dev)
```

To **stop the walk-up** (e.g., in a monorepo to prevent leaking parent config into a sibling project), drop a `.xv.boundary` file:

```bash
touch /code/monorepo/services/checkout/.xv.boundary
```

To **disable walk-up entirely**, set `XV_NO_PARENT_CONFIG=1`.

See [`docs/env-profiles.md`](docs/env-profiles.md) for the full reference.

---

## Vault management

### Lifecycle

```bash
xv vault create my-vault --resource-group my-rg --location eastus
xv vault list                                  # all vaults you can see
xv vault list --resource-group my-rg
xv vault list --page-size 25 --page 2          # pagination
xv vault info my-vault                         # detail
xv vault info my-vault --format json
xv vault delete my-vault                       # soft-delete
xv vault restore my-vault                      # within retention period
xv vault purge my-vault --force                # permanent delete
```

### Update properties

```bash
xv vault update my-vault --enable-purge-protection
xv vault update my-vault --retention-days 90
xv vault update my-vault --tag owner=platform-team
```

### Export and import

```bash
xv vault export my-vault --output secrets.json --format json
xv vault export my-vault --include-values --output backup.yaml --format yaml
xv vault export my-vault --group production --output prod-only.json

xv vault import target-vault --input secrets.json
xv vault import target-vault --input secrets.json --dry-run     # preview
xv vault import target-vault --input secrets.json --overwrite   # replace existing
```

### RBAC sharing (vault-level)

```bash
xv vault share grant my-vault --principal alice@example.com --role secrets-user
xv vault share revoke my-vault --principal alice@example.com
xv vault share list my-vault
xv vault share list my-vault --page-size 50 --page 2
```

---

## Cross-vault operations — diff, copy, move

### Diff

```bash
xv diff vault-a vault-b                            # name+metadata-only
xv diff vault-a vault-b --show-values              # include values (be careful)
xv diff vault-a vault-b --group production         # filter both vaults
xv diff vault-a vault-b --format json              # script-friendly
```

### Copy / move

```bash
xv copy API_KEY --from vault-a --to vault-b
xv copy API_KEY --from vault-a --to vault-b --new-name API_KEY_V2
xv copy --group production --from vault-a --to vault-b   # bulk

xv move API_KEY --from vault-a --to vault-b
xv move API_KEY --from vault-a --to vault-b --force      # skip confirmation
```

### Find across vaults

```bash
xv find db --all-vaults                          # rows prefixed 'vaultname/SECRET'
# myproj-dev-kv/DB_PASSWORD   ██████████   backend/database  backend,dev
# myproj-prod-kv/DB_PASSWORD  ████░░░░░░   backend/database  backend,prod
```

---

## Files (blob storage)

Optional file/blob storage. The backing service depends on the active backend:

- **Azure**: Azure Blob Storage. `xv init` can create/configure the storage
  account and container. All commands below, including `xv file sync`, use this
  path.
- **AWS**: S3. Set `[aws].s3_bucket` or `XV_AWS_S3_BUCKET` to an existing bucket;
  `xv` never creates buckets. Upload/download/list/delete/info are supported and
  objects are stored under `<vault>/files/<name>`. `xv file sync` is not
  supported on AWS yet.

### Single files

```bash
xv upload ./config.json
xv download config.json
xv download config.json --output ./local-name.json
xv file info config.json                         # metadata
xv file delete config.json
```

### Directories

```bash
xv file upload ./docs --recursive                                # preserves dir structure
xv file upload ./src --recursive --prefix backup/2026-04-30
xv file download docs --recursive --output ./local
```

### List + paginate

```bash
xv file list
xv file list --prefix backup/
xv file list --page-size 100 --page 3
xv file list --limit 100                         # legacy alias for first-page page-size 100
```

### Sync

```bash
xv file sync ./mydir                             # default direction: up
xv file sync ./mydir --direction up              # local → remote (changed/missing)
xv file sync ./mydir --direction down            # remote → local
xv file sync ./mydir --direction both            # bidirectional (mtime + size)
xv file sync ./mydir --dry-run                   # show planned transfers
xv file sync ./mydir --prefix backup/ --delete   # mirror; remove extra remote blobs
```

---

## Pre-commit leak scanner — `xv scan`

`xv scan` is unique because it matches files against your **actual vault values**, not just generic regex patterns. When you accidentally paste `DB_PASSWORD`'s real value into a config file, it tells you *"this file contains the value of secret DB_PASSWORD from vault dev-kv"* — not just "high-entropy string."

### Scan modes

```bash
xv scan                                          # current directory
xv scan src/ tests/                              # specific paths
xv scan --staged                                 # only files staged for commit (git diff --cached)
xv scan --all                                    # full HEAD tree
xv scan --hook                                   # quiet on success, exit 50 on findings (for CI)
xv scan --all-vaults                             # match against every vault you can list
xv scan --format json                            # JSON envelope on stdout
```

### Pre-commit hook

```bash
xv scan install                                  # writes .git/hooks/pre-commit (idempotent)
xv scan install --force                          # overwrite an existing non-managed hook
xv scan uninstall                                # removes the managed hook only
```

The installed hook is just:

```bash
#!/usr/bin/env bash
# xv-scan-managed
set -e
xv scan --staged --hook
```

### What it finds

- **User secret values** (Critical) — verbatim values from your vault.
- **Built-in patterns** (High / Medium): AWS access keys, GitHub tokens (ghp/ghs/gho/ghr/ghu prefixes), Stripe live+test keys, Slack tokens, JWTs, SSH/PEM private-key headers, high-entropy fallback.

User-secret matches always win over pattern matches at the same offset.

### Output

Plain-text findings always go to **stderr**, never the value itself:

```
src/config.js:42:10: matches DB_PASSWORD (kind=SecretValue, severity=Critical, vault=dev-kv)
```

JSON envelope (`--format json`) on **stdout**:

```json
[
  {"file":"src/config.js","line":42,"col":10,"secret_name":"DB_PASSWORD","vault":"dev-kv","kind":"secret-value","severity":"critical"}
]
```

### Tuning — `[scan]` block in `.xv.toml`

```toml
[scan]
exclude = ["dist/**", "*.lock", "vendor/**"]
min_value_length = 12
patterns = ["aws", "github", "stripe"]
```

Plus `.xvignore` (gitignore syntax, scanner-specific):

```
node_modules/
*.snap
test/fixtures/**
```

### Composition with gitleaks

`xv scan` ships ~7 patterns by design — broader coverage layers gitleaks alongside:

```bash
gitleaks protect --staged && xv scan --staged --hook
```

See [`docs/scan.md`](docs/scan.md) for the full reference.

---

## Terminal UI — `xv tui`

Read-only three-pane browser. Behind a `tui` feature flag (default off) so the scripting binary stays lean.

```bash
cargo install crosstache --features tui
xv tui
```

### Layout

```
┌──────────────┬────────────────────────────┬──────────────────┐
│ Vaults       │ Secrets (filter: /db_)     │ Detail           │
│ > dev-kv     │ > DB_PASSWORD              │ name: DB_PASSWORD│
│   stage-kv   │   DB_HOST                  │ value: ●●●●●●    │
│   prod-kv    │   DB_PORT                  │ groups: backend  │
└──────────────┴────────────────────────────┴──────────────────┘
status: dev-kv · 24 secrets · clipboard: 12s              ?:help
```

### Keymap

| Key | Action |
|-----|--------|
| `h j k l` / arrows | move within / between panes |
| `Tab` / `Shift-Tab` | cycle panes |
| `/` | live fuzzy filter on Secrets pane (uses the `xv find` matcher) |
| `Space` | toggle value reveal |
| `y` | copy value (clipboard countdown shows in status) |
| `Y` | copy secret name |
| `R` | refresh — invalidate cache and reload current scope |
| `H` | history (versions) overlay |
| `a` | audit log overlay |
| `?` | help overlay (full keymap) |
| `e` | expand error toast into modal |
| `q` / `Esc` | quit (or close current overlay) |

`c`, `d`, `r` are reserved for a future write mode; the current TUI remains read-only.

Values load on demand: settle the cursor on a row for ~200 ms and the value fetches in the background, lands in an in-memory `Zeroizing` cache (cleared on quit), and the detail pane shows `●●●●●●` until you press `Space`.

> **Audit overlay:** the TUI overlay is still a placeholder. Use CLI
> `xv audit` for Azure Activity Log and AWS CloudTrail history in the meantime.

See [`docs/tui.md`](docs/tui.md) for the full reference.

---

## Scripting & CI

### Exit codes

Stable across releases — part of the public scripting contract.

| Code  | Family                | Examples |
|-------|-----------------------|----------|
| `0`   | Success | command completed |
| `1`   | Unknown / catch-all | unrecoverable I/O, JSON parse, regex |
| `2`   | Invalid argument | bad CLI flag; clap parse failure |
| `3`   | Configuration error | missing config; invalid `.xv.toml`; env not defined |
| `10`  | Secret not found | `xv get` on a missing secret |
| `11`  | Vault not found | `xv vault info` on a missing vault |
| `12`  | Invalid secret name | name fails sanitization |
| `20`  | Authentication failed | bad token, expired credential |
| `21`  | Permission denied | RBAC check failed |
| `30`  | Network error | generic transport failure |
| `31`  | DNS resolution failed | vault hostname did not resolve |
| `32`  | Connection timeout | TCP connect or request timeout |
| `33`  | Connection refused | TCP refused |
| `34`  | SSL/TLS error | certificate or handshake failure |
| `35`  | Invalid URL | malformed URL |
| `40`  | Azure API error | Azure returned an error response |
| `50`  | Scan: leak detected | `xv scan` found a finding |

### Stable error codes

Every error has a stable kebab-case code (`xv-vault-not-found`, `xv-network-dns`, `xv-env-not-defined`, `xv-scan-leak-detected`, …) for scripting:

```bash
if ! out=$(xv get DB_PASSWORD --format json 2>/dev/null); then
  code=$(echo "$out" | jq -r '.error.code')
  case "$code" in
    xv-secret-not-found)  echo "secret missing — provisioning…"   ; provision_secret ;;
    xv-permission-denied) echo "RBAC: ask the platform team"      ; exit 1 ;;
    xv-network-dns)       echo "DNS — check vault name spelling"  ; exit 2 ;;
    *)                    echo "unexpected: $code"                ; exit 1 ;;
  esac
fi
```

### JSON error envelope

When `--format json` or `--format yaml` is in effect, errors render to **stdout** (not stderr) as a structured envelope:

```json
{
  "error": {
    "code": "xv-vault-not-found",
    "message": "Vault not found: myproj-prood",
    "exit_code": 11,
    "suggestion": "myproj-prod"
  }
}
```

`suggestion` is omitted when no near-match was found. The plain-text rendering for non-JSON outputs is:

```
error[xv-vault-not-found]: Vault not found: myproj-prood
  did you mean: myproj-prod?
  hint: Run 'xv vault list' to see available vaults.
```

The `hint:` line is TTY-only (suppressed in piped/captured output).

### CI examples

#### GitHub Actions — fetch a secret into the build

```yaml
- name: Authenticate to Azure
  uses: azure/login@v2
  with:
    client-id: ${{ secrets.AZURE_CLIENT_ID }}
    tenant-id: ${{ secrets.AZURE_TENANT_ID }}
    subscription-id: ${{ secrets.AZURE_SUBSCRIPTION_ID }}

- name: Fetch deploy token
  run: |
    DEPLOY_TOKEN=$(xv get DEPLOY_TOKEN --raw --vault myproj-prod-kv)
    echo "::add-mask::$DEPLOY_TOKEN"
    echo "DEPLOY_TOKEN=$DEPLOY_TOKEN" >> "$GITHUB_ENV"
```

#### GitLab CI — pre-deploy leak scan

```yaml
leak_scan:
  stage: test
  script:
    - xv scan --hook
  # Exits 50 on findings → job fails. Pipe-friendly JSON if you want to surface findings to a dashboard.
```

#### Generic shell — handle missing secret

```bash
#!/usr/bin/env bash
set -euo pipefail

token=$(xv get DEPLOY_TOKEN --raw 2>&1) || {
  case $? in
    10) echo "secret not in vault — provisioning…"; ./scripts/provision.sh ;;
    20) echo "auth failed — re-running az login"; az login --use-device-code ;;
    *)  echo "unexpected: $token"; exit 1 ;;
  esac
}
```

### `xv scan` in CI

```bash
xv scan --hook                  # exit 50 on findings; quiet on clean
xv scan --hook --format json    # findings as JSON array on stdout
xv scan --hook --all-vaults     # broaden the secret-value match set
```

See [`docs/exit-codes.md`](docs/exit-codes.md) for the full table.

---

## Configuration

### Hierarchy (highest → lowest priority)

1. CLI flags (`--credential-type cli`, `--vault foo`)
2. Environment variables (`XV_ENV`, `AZURE_SUBSCRIPTION_ID`)
3. Project config (`.xv.toml`, walk-up from cwd)
4. Legacy `.xv/context` (deprecated; prints one-time warning per process; removed in v0.8)
5. User config file (`$XDG_CONFIG_HOME/xv/xv.conf` or `~/.config/xv/xv.conf`)
6. Defaults

### Setup

```bash
xv init                                  # interactive — vault + storage account
xv config show                           # full effective config
xv config show --format json
xv config set default_vault my-vault
xv config set clipboard_timeout 60
xv config set azure_credential_priority cli
xv config path                           # path to the config file
xv config edit                           # open xv.conf in $VISUAL/$EDITOR
xv config unset clipboard_timeout
```

`xv config edit` creates the parent directory and seeds a missing config with a
valid default file before opening it. Editor resolution is `$VISUAL`, then
`$EDITOR`, then `nano` on Unix or `notepad` on Windows; values with arguments
such as `code --wait` are supported. A non-zero editor exit is surfaced as a
configuration error.

### Key environment variables

| Variable | Purpose |
|----------|---------|
| `AZURE_SUBSCRIPTION_ID` | Azure subscription |
| `AZURE_TENANT_ID` | Azure tenant |
| `AZURE_CLIENT_ID` / `AZURE_CLIENT_SECRET` | Service-principal auth |
| `AZURE_CREDENTIAL_PRIORITY` | `cli` / `managed_identity` / `environment` / `default` |
| `DEFAULT_VAULT` | Default vault name |
| `DEFAULT_RESOURCE_GROUP` | Default resource group |
| `DEFAULT_LOCATION` | Default Azure location (e.g., `eastus`) |
| `XV_ENV` | Active env from `.xv.toml` (highest priority for env selection) |
| `XV_NO_PARENT_CONFIG` | `1` disables `.xv.toml` walk-up |
| `CACHE_TTL` | Cache TTL in seconds |
| `DEBUG` | `true` / `1` enables debug logging |
| `NO_COLOR` | Disable colored output (any value; standard [NO_COLOR](https://no-color.org/) convention) |
| `AZURE_STORAGE_ACCOUNT` / `AZURE_STORAGE_CONTAINER` | Blob storage destination |
| `BLOB_CHUNK_SIZE_MB` | Upload chunk size |
| `BLOB_MAX_CONCURRENT_UPLOADS` | Upload concurrency |
| `XV_BACKEND` | Active backend override (`azure`, `aws`, `local`, or a named backend) |
| `AWS_REGION` / `AWS_PROFILE` | AWS backend region/profile fallbacks |
| `XV_AWS_S3_BUCKET` | Existing S3 bucket for AWS file storage |
| `AGE_KEY` / `AGE_KEY_FILE` | Local backend age identity override |

### Global CLI flags

These work with any command:

| Flag | Purpose |
|------|---------|
| `--format <FORMAT>` | `table` / `json` / `yaml` / `csv` / `plain` / `raw` / `template` (default: `auto` — table on TTY, json for pipes) |
| `--credential-type <TYPE>` | Azure credential type (`cli`, `managed_identity`, `environment`, `default`) |
| `--template <TEMPLATE>` | Custom template string for template format |
| `--env <NAME>` | Active env from `.xv.toml` (overridden by `XV_ENV`) |
| `--debug` | Enable debug logging |
| `--show-options` | Show global options in `--help` output |

---

## Authentication

crosstache uses Azure's `DefaultAzureCredential` chain. You can control the order:

```bash
# Per-command
xv list --credential-type cli

# Per-shell-session
export AZURE_CREDENTIAL_PRIORITY=cli

# Persistent
xv config set azure_credential_priority cli
```

Supported priorities: `cli` (Azure CLI), `environment` (env vars), `managed_identity` (for Azure-hosted workloads), `default` (the full chain).

The chain tries (in priority order):
1. Environment variables (`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`)
2. Managed Identity (when running on Azure VMs / App Service / AKS / etc.)
3. Azure CLI (`az login`)
4. Visual Studio Code
5. Azure PowerShell

For service-principal auth from a script:

```bash
export AZURE_CLIENT_ID=...
export AZURE_CLIENT_SECRET=...
export AZURE_TENANT_ID=...
export AZURE_CREDENTIAL_PRIORITY=environment
xv list
```

---

## Troubleshooting

The structured-error layer makes most failures self-explanatory. A few common ones:

### `error[xv-secret-not-found]: Secret not found: X`

```bash
xv list                                  # see what's actually in the vault
xv list --all                            # include disabled / soft-deleted
xv find X --names-only                   # fuzzy search
```

The error often suggests a near-match (`did you mean: X-other?`).

### `error[xv-vault-not-found]: Vault not found: X`

```bash
xv vault list                            # confirm the name
xv whoami                                # confirm you're authenticated
xv config show | grep subscription_id    # confirm correct subscription
```

### `error[xv-permission-denied]`

You're authenticated but lack the RBAC role.

```bash
xv whoami                                # who am I?
xv vault share list my-vault             # current grants on the vault
# Ask an admin to grant you 'Key Vault Secrets User' or 'Key Vault Administrator'
```

### `error[xv-network-dns]`

The vault hostname didn't resolve. Either the vault name is wrong, or your DNS is misconfigured (corporate VPN, custom resolver, etc.):

```bash
nslookup my-vault.vault.azure.net
xv vault list                            # if this works, DNS is fine — typo in vault name
```

### `error[xv-env-not-defined]: Environment 'X' not defined in .xv.toml`

```bash
xv context envs                          # see what's defined
xv context show                          # see which .xv.toml is being used
```

### `error[xv-auth-failed]`

```bash
az login                                 # re-authenticate with Azure CLI
xv config show | grep credential         # check current priority
xv list --credential-type cli            # try Azure CLI explicitly
```

### Debug logging

```bash
xv list --debug                          # one-shot
RUST_LOG=crosstache=debug xv list        # also enables tracing-subscriber output
DEBUG=1 xv list                          # crosstache-specific shorthand
```

### Bypass `.xv.toml` discovery

```bash
XV_NO_PARENT_CONFIG=1 xv list            # only the cwd's .xv.toml is considered (no walk-up)
```

---

## Security model

- **Memory hygiene.** Secret values are wrapped in `Zeroizing<String>` and zeroed on drop. The TUI's value cache, the scanner's match-engine, and the run-time injection layer all use this.
- **Clipboard auto-clear.** Default 30 s; configurable via `clipboard_timeout` (`0` to disable).
- **File permissions.** Config, context, and export files are written `0600`
  (owner-only); config/context parent directories are created `0700`.
- **Path traversal.** Recursive downloads validate paths to prevent `../../../etc/passwd` shenanigans.
- **Generator scripts.** `xv rotate --generator <script>` validates the script is owned by you and `0700`.
- **`xv run` masking.** Secret values are masked in stdout/stderr by default
  using bounded streaming chunks, including values split across chunk
  boundaries; use `--no-masking` only when you understand the consequences.
- **`xv scan` value-never-leaked invariant.** The `Finding` struct never contains the matched value — only file/line/col + the secret's *name*. Enforced by a hand-maintained banned-key test on the on-disk schema.
- **Secret-name handling.** Names are sanitized for Azure (alphanumeric + hyphens; original preserved in tags); names > 127 chars are SHA256-hashed.

---

## Name sanitization

Azure Key Vault only allows alphanumeric characters and hyphens, but you can use anything:

```bash
xv set "my-app/database:connection@prod"
# Stored as: my-app-database-connection-prod
# Original preserved in the 'original_name' tag for round-trip lookup
```

Names longer than 127 chars are SHA256-hashed; the full original is still stored in the tag.

---

## Development

```bash
cargo build                              # debug build
cargo build --release                    # release build
cargo build --features tui               # include the TUI
cargo test                               # full suite
cargo test --features tui                # also include TUI snapshot tests
cargo test -- --test-threads=1           # required for some env-var-mutating tests
cargo fmt --all                          # format
cargo clippy --all-targets               # lint
```

Build without file operations: `cargo build --no-default-features`.

- Tests: see [`docs/testing.md`](docs/testing.md) for the hermetic vs live track split.

### Release process

```bash
cargo release patch                      # 0.7.0 → 0.7.1
cargo release minor                      # 0.7.0 → 0.8.0
```

### Documentation

- [`docs/exit-codes.md`](docs/exit-codes.md) — exit-code table and JSON error envelope
- [`docs/env-profiles.md`](docs/env-profiles.md) — `.xv.toml` walk-up reference
- [`docs/find.md`](docs/find.md) — `xv find` ranked search
- [`docs/scan.md`](docs/scan.md) — pre-commit leak scanner
- [`docs/tui.md`](docs/tui.md) — terminal UI keymap
- [`docs/GROUPS.md`](docs/GROUPS.md) — group-based organization

---

## Release Verification

Release archives are signed with [minisign](https://jedisct1.github.io/minisign/). To verify a download:

```bash
minisign -Vm xv-linux-x64.tar.gz -P RWRuXFh34rB613dgsXyAMmtKvYK0SFwxq4i44dhGFXVTrhAQ7hJXf6Ym
```

The public key is also embedded in the `xv` binary — `xv upgrade` automatically verifies signatures.

---

## License

MIT — see [LICENSE](LICENSE).
