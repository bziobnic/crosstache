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
| File storage | Azure Blob Storage; includes `xv file sync` | S3 when `[aws].s3_bucket` or `XV_AWS_S3_BUCKET` is set; sync unsupported | age-encrypted per-vault files; includes `xv file sync` |

---

## Secrets

| Command | Description |
|---------|-------------|
| `xv set <name>` | Create a secret (interactive prompt, `--stdin`, `--value`, or bulk `K1=v1 K2=v2`); write-time metadata via `--group` (repeatable), `--note`, `--folder`, `--expires`, `--not-before`, `--tag key=value` |
| `xv gen` | Generate a random password to the clipboard (`--length`, `--charset`, `--raw`); `--save <name>` stores it as a secret with the same write-time metadata flags as `set` (`--group`, `--note`, `--folder`, `--expires`, `--not-before`, `--tag`, `--vault`) |
| `xv get <name>` | Retrieve a secret (clipboard by default; `--raw` for stdout) |
| `xv list` (alias `xv ls`) | List secrets. Default TTY output is a folder-aware grid (folders first, shown as `prod/`); pass a `[FOLDER]` positional to list inside a folder. `-l` for a long listing (name, updated, groups, note), `-r` to recurse (folder-qualified names in the grid/long/`--names-only` views), `--format table` for the classic table. Filters: `--group`, `--all` (include disabled), `--expiring <period>`, `--expired`, `--deleted` (soft-deleted secrets; conflicts with `FOLDER`, `-r`, `--group`, `--all`, `--expiring`, `--expired`). `--sort name\|updated` (default `name`). `--names-only`, `--page-size`, `--page`, `--pager [auto\|always\|never]`, `--no-cache` |
| `xv delete <name>` | Soft-delete a secret (`--force` to skip confirmation) |
| `xv update <name>` | Update value, groups, folder, note, tags, expiry; supports `--rename`, `--tag`/`--tags`, `--enabled <true\|false>` (disable/enable — disabled secrets are excluded from `xv ls` and `xv group list` by default, `--all` reveals them), and clear flags such as `--clear-note` |
| `xv update <name> --rename <new>` | Rename a secret on any backend: creates `<new>` with the current value and metadata (tags, groups, note, folder, content type, expiry — not version history), then deletes `<name>` via the backend's normal delete (Azure: soft-deleted; AWS: 30-day recovery window; local: trash). Combined with other update flags, in-place updates apply first, then the rename. Renaming onto an existing name is refused (`xv-conflict`). Partial failure (new secret created, old one not deleted) exits `43` (`xv-rename-incomplete`) and never rolls back the new secret. Combining `--enabled false` with `--rename` fails on Azure (the disable applies first, then the rename's read gets a 403) — re-enable first or rename before disabling |
| `xv purge <name>` | Permanently delete a soft-deleted secret |
| `xv restore <name>` | Restore a soft-deleted secret |
| `xv history <name>` | Show version history |
| `xv rollback <name>` | Restore a previous version (`--version <id>`) |
| `xv rotate <name>` | Generate new random value (`--length`, `--charset`, `--generator`); `--native` triggers AWS Secrets Manager rotation |
| `xv copy <name>` | Copy a secret between vaults (`--from`, `--to`) |
| `xv move <name>` | Move a secret between vaults (`--from`, `--to`) |
| `xv group list` | List secret groups with member counts, derived from the `groups` metadata (`--no-cache`; full `--format`/`--columns` support) |

### Metadata & Organization

- **Folders** — `--folder "app/database"` on `set`, `gen --save`, or `update`
- **Groups** — `--group <name>` on `set`, `gen --save`, or `update` (multiple allowed); filter with `list --group`
- **Notes** — `--note "description"` on `set`, `gen --save`, or `update`
- **Tags** — `--tag key=value` on `set` or `gen --save`; `-t key=value`,
  `--tags key=value`, or `--tag key=value` on `update`; `--replace-tags` /
  `--replace-groups` for replace mode
- **Expiry** — `--expires YYYY-MM-DD` on `set`, `gen --save`, or `update`; `--clear-expires` to remove

`--value` is for a single non-interactive write:

```bash
xv set API_TOKEN --value "$TOKEN" --tag owner=platform --group prod
```

Prefer the prompt or `--stdin` for sensitive values when shell history is a
concern. `--value` is rejected with bulk `KEY=value` writes, and bulk writes do
not accept `--expires` / `--not-before`.

### Name Sanitization

Names are automatically sanitized for backend compatibility. Azure allows
alphanumeric + hyphen and hashes names beyond 127 characters; AWS accepts its
broader Secrets Manager charset and percent-encodes unsupported bytes; local
storage uses filename-safe encoding. Original names are preserved in metadata
where a backend needs reverse lookup.

### Search — `xv find`

Ranked fuzzy search over secrets (alias `xv search`); non-interactive, pipe
the output through `fzf` for an interactive picker. Default field is the
secret name; opt in to others with repeated `--in <field>` (`name`, `folder`,
`groups`, `note`, `tags`).

```bash
xv find db                    # rank by name
xv find db --in folder --in groups
xv find db --folder prod      # scope to the prod/ subtree (segment-boundary match)
xv find db --limit 10         # cap rows (default 50)
xv find db --min-score 0.5    # drop matches below this fraction of the top score (default 0.3)
xv find db --all-vaults       # search every vault the caller can list
xv find db --names-only       # pipe-friendly
xv find db --format csv       # standard row shape across json/yaml/csv
```

Machine formats (`json`/`yaml`/`csv`) emit the standard row shape: `score` is
a two-decimal string, `folder`/`groups` default to empty strings.

---

## Record Types

Typed records attach structured fields (username, URL, connection-string, …)
to a secret. Each field is `metadata` (a listable tag) or `secret` (JSON
envelope inside the value); every type has exactly one `primary` secret
field, so plain `get`/`run` on a record are byte-identical to an untyped
secret. Untyped secrets are unaffected on every code path — no envelope, no
new tags, unless explicitly converted.

| Command | Description |
|---------|-------------|
| `xv type list` (alias `ls`) | List resolved types (built-in + global `xv.conf` + project `.xv.toml`) with source; `--format json` |
| `xv type show <name>` | Field table for one type |
| `xv set <name> --type <type>` | Create a typed record; `--field name=value` for metadata/non-primary-secret fields (repeatable), `--field-secret name=value` for ad-hoc secret fields, primary value via `--value`/`--stdin`/prompt |
| `xv get <name> --field <name>` | Read one field (either kind) |
| `xv get <name> --record` | Full record (all fields) in the requested `--format` |
| `xv update <name> --field name=value` | Edit a metadata field (tag-only) or type-declared non-primary secret field (new version) |
| `xv update <name> --field-secret name=value` | Edit/add an ad-hoc secret field (rewrites the envelope, new version) |
| `xv update <name> --type <type>` | Explicit conversion: bare secret's value becomes the primary field |
| `xv update <name> --untype [--yes]` | Flatten a record back to a bare secret holding the primary value |
| `xv ls --type <type>` | Filter listing by record type; JSON output lifts `f.*` fields into a `fields` map plus `record_type` |

Built-ins: `login` (username\*, url; password\* primary), `api-key` (url,
account; key\* primary), `database` (host, port, database, username;
password\* primary, connection-string optional secret) — `\*` = required.
Custom types are `[types.<name>]` TOML blocks in global `xv.conf` or
project `.xv.toml`; a project type shadows a global one, shadowing a
built-in works but warns. **One invalid `[types.*]` block fails type
resolution globally** (fail-closed by design).

External consumers (Azure portal, raw SDKs, older `xv` binaries) see a
typed record's value as a JSON envelope (`application/vnd.xv.record`
content type), not the bare primary value — conversion between shapes is
always explicit (`--type`/`--untype`), never implicit.

---

## Secret Injection

| Command | Description |
|---------|-------------|
| `xv run -- <command>` | Run a process with secrets as env vars (`--group`, `--include`, `--exclude`, `--no-masking`, `--best-effort`). Injects a typed record's primary field under its name; no per-field expansion |
| `xv inject` | Render templates with `{{ secret:name }}` / `{{ secret:name.field }}` and `xv://vault/secret[#field]` refs (`--group`, `--best-effort`). `.field`/`#field` select one field of a typed record; exact secret-name matches (e.g. a secret literally named `a.b`) always win over the dotted split |

Advanced workflows (`run`, `inject`, default `rotate`, `scan`, `env pull`, and
`env push`) route through the active backend trait. They work with Azure, AWS,
and local backends; capability-gated variants still say so explicitly (for
example, `rotate --native` is AWS-only).

```bash
xv run --include DB_PASSWORD --include API_KEY -- ./script.sh
xv run --group prod --exclude LEGACY_TOKEN -- ./deploy.sh
```

`--include` narrows the candidate set before `--exclude` is applied. Both match
the original user-facing name shown by `xv list` or the backend-specific stored
name. If an explicit `--group`/`--include` filter matches nothing, `xv run`
exits non-zero; an empty vault or an exclusion that removes everything warns
and still runs the child process.

By default, `xv run` also aborts before spawning the child if any selected
secret or `xv://` reference fails to fetch — every failure is reported before
the non-zero exit. Pass `--best-effort` to fall back to the previous
warn-and-continue behavior.

By default, `xv inject` also aborts before writing/printing the rendered
output if any `{{ secret:name }}` or `xv://` reference fails to resolve —
every failure is reported before the non-zero exit, and no
partially-resolved output is ever written. Pass `--best-effort` to fall back
to the previous warn-and-continue behavior.

---

## Vault Management

| Command | Description |
|---------|-------------|
| `xv vault create <name>` | Create a new vault (`--resource-group`, `--location`) |
| `xv vault list` | List vaults (`--resource-group`, `--names-only`, `--no-cache`, `--page-size`, `--page`, `--pager [auto\|always\|never]`) |
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
| `xv context list` | Recent contexts; honors the global `--format` (`{status, vault, resource_group, last_used, usage_count}` rows) |
| `xv context clear` | Clear context |
| `xv env list` | List `[env.*]` blocks in the resolved `.xv.toml`; honors the global `--format` (`Name`/`Active`/`Backend`/`Vault`/`Resource Group` rows) |
| `xv env use <name>` | Set `default_env = "<name>"` in the nearest `.xv.toml` |
| `xv env create <name>` | Add `[env.<name>]` to the nearest `.xv.toml` (`--vault`, `--resource-group`, `--backend`, `--default`) |
| `xv env delete <name>` | Remove `[env.<name>]` from the resolved `.xv.toml` (`-f` to skip confirmation) |
| `xv env show` | Show the active env (source, backend, vault, resource_group, group, folder) |
| `xv env pull` | Download secrets as `.env` file |
| `xv env push <file>` | Upload `.env` contents as secrets |

Aliases: `xv cx` for `xv context`, `xv ls` for `xv list`.

### Multi-vault workspaces

`xv cx add` attaches multiple vaults as one workspace. Secret reads and union
views can span attached entries, while unqualified writes and `xv file` commands
target the workspace default entry.

| Command | Description |
|---------|-------------|
| `xv cx add <vault> --backend <name> --alias <alias>` | Attach a vault to the workspace; `--alias` is a visible alias of `--as`, and `--default` makes it the write target |
| `xv cx rm <alias>` | Detach an entry by alias |
| `xv cx default <alias>` | Change the default entry used by unqualified writes and file operations |
| `xv cx alias <entry> <new-alias>` | Rename an entry's alias, looking up `<entry>` by current alias or unique vault name |
| `xv cx alias <entry> --reset` | Clear the explicit alias so it displays as the backing vault name |
| `xv cx ls` | List attached entries with alias, backend, vault, default marker, and source |

Union `xv ls`/`xv find` prefixes rows with `alias/` when more than one vault is
attached. In long listing mode (`xv ls -l`), a renamed alias also shows the
backing vault name as `alias/SECRET (vault-name)` so operators can map short
aliases back to their real vaults.

---

## File Storage

Requires the `file-ops` feature flag (enabled by default). Setup depends on the
active backend:

- **Azure**: configure Azure Blob Storage with `xv init` or
  `AZURE_STORAGE_ACCOUNT` / `AZURE_STORAGE_CONTAINER`.
- **Local**: uses age-encrypted files in the configured local store (default
  `~/.xv/store/vaults/<vault>/files/`); no cloud storage configuration is
  required.
- **AWS**: build with `--features aws` (or use a release binary) and set
  `[aws].s3_bucket` or `XV_AWS_S3_BUCKET` to an existing bucket. `xv` stores
  objects under `<vault>/files/<name>` and does not create buckets.

| Command | Description |
|---------|-------------|
| `xv upload <file>` | Quick upload (alias for `xv file upload`) |
| `xv download <file>` | Quick download (alias for `xv file download`) |
| `xv file upload` | Upload files (`--recursive`, `--prefix`, `--flatten`) |
| `xv file download` | Download files (`--recursive`, `--flatten`, `--output`, `--force`) |
| `xv file list` | List files (hierarchical by default; `--recursive` for flat; `--names-only` (recursive, pipe-friendly), `--pager [auto\|always\|never]`, `--page-size`, `--page`, `--limit`, `--no-cache`) |
| `xv file delete` | Delete files (`--force`, `--continue-on-error`) |
| `xv file info` | File metadata |
| `xv file sync` | Sync local directory with blob prefix (`--direction` up/down/both, `--dry-run`, `--delete`); supported on Azure and local, unsupported on AWS |

AWS supports upload/download/list/delete/info through S3. Attempting
`xv file sync` on the AWS backend returns a setup-neutral error that recommends
recursive upload/download as the current bulk-transfer path.

In a multi-vault workspace, file commands resolve through the workspace default
entry's backend and vault. They do not provide a union listing or `alias:path`
addressing for files.

`xv file list --format csv` columns match the table: `Kind,Name,Size,Content-Type,Modified,Groups`.
JSON/YAML keep the full-fidelity serialization (etags, raw byte sizes, extra metadata).

---

## Utilities

| Command | Description |
|---------|-------------|
| `xv whoami` | Show authenticated identity and context |
| `xv audit <name>` | Access/change history for a secret or vault (Azure Activity Log or AWS CloudTrail; unsupported on local); `--vault`, `--days`, `--operation`; honors the global `--format` (JSON = array of `{timestamp, operation, resource, caller, status}` rows). |
| `xv info <resource>` | Auto-detect and display info for a vault or secret |
| `xv parse <conn-string>` | Parse and display connection string components |
| `xv completion <shell>` | Generate shell completions (bash, zsh, fish, powershell) |
| `xv version` | Build info (version, git hash, target) |

### Local backend maintenance

| Command | Description |
|---------|-------------|
| `xv local encrypt-metadata` | Re-encrypt existing plaintext local secret metadata after enabling `[local].encrypt_metadata = true`; safe to re-run |
| `xv local encrypt-metadata --dry-run` | Count plaintext metadata files without modifying the store |
| `xv local migrate` | Convert an existing store to opaque on-disk filenames after enabling `[local].opaque_filenames = true`; renames secret/version/trash files to keyed-hash stems and builds the encrypted index. Idempotent; safe to re-run |
| `xv local migrate --dry-run` | Print the rename plan without modifying the store |

With `[local].opaque_filenames = true`, a directory listing of the store reveals
no secret names: each secret's files are named by `HMAC-SHA256(key, name)` (base32,
keyed by the age identity), and an age-encrypted `.index.age` maps stems back to
names. Existing stores are unaffected until you set the flag and run
`xv local migrate`; afterwards any write upgrades that secret's layout
automatically and a one-release back-compat read path keeps un-migrated secrets
readable.

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
| `xv config edit` | Open the config file in `$VISUAL`/`$EDITOR` (or a platform default) |

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
opaque_filenames = false   # when true, run `xv local migrate` to hide names on disk
```

---

## Output Formats

Table (default), JSON (`--format json`), YAML (`--format yaml`), CSV (`--format csv`), plain (`--format plain`), raw (`--format raw`).

Human-readable status chrome (`[ok]`, warnings, hints, progress steps) writes to
stderr. Stdout is reserved for command data such as raw secret values, JSON,
YAML, CSV, table output, or names-only lists, so redirects and pipes stay clean:

```bash
xv get DB_PASSWORD --raw > db_password.txt
xv list --format json | jq '.[].name'
```

`--format json|yaml|csv` works across all list-style commands, including
`xv audit`, `xv find`, `xv share list`, `xv vault share list`,
`xv context list`, and `xv env list`. Empty results are
valid-empty on machine formats (`[]` for JSON, headers-only for CSV) instead of
nothing, so `| jq` works on empty results; the corresponding empty-state
message for human formats goes to stderr. Counts are plural-aware (`1 vault`,
`3 vaults`, `5 audit log entries`).

One documented exception: `xv config show --format json|yaml` serializes the
full configuration object (it is a resource view, not a list). Its human table
and the `--resolved` rows render through the shared formatter, so `--columns`
and `--no-color` apply there like everywhere else.

Long list-style output can be paged when both stdin and stdout are terminals:

```bash
xv list --pager              # same as --pager auto
xv vault list --pager never  # force direct printing
```

Global `--columns <COLS>` selects and orders columns for `table`/`plain`/`csv`
output on any list command (case-insensitive, e.g. `--columns Name,Updated`);
unknown names error and list the available columns. JSON/YAML/template ignore
it. Global `--no-color` disables colored output (same effect as `NO_COLOR`,
including stderr chrome).

---

## Build & Distribution

- **Binary:** `xv`
- **Platforms:** Windows x64, macOS (Intel + Apple Silicon), Linux x64
- **Feature flags:** `file-ops` (default on), `tui`, `aws`
- **Release binaries:** built with `--features tui,aws`; source builds need
  `--features aws` for AWS Secrets Manager / CloudTrail / S3 support
- **Security:** `zeroize` for secrets in memory, restricted file permissions, clipboard auto-clear
