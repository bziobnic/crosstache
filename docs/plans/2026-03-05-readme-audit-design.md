# README.md Accuracy Audit & Polish

**Date**: 2026-03-05
**Scope**: README.md only
**Focus**: Feature accuracy, missing command coverage, grammar, formatting, and presentation
**Approach**: Section-by-section audit comparing documented content against actual CLI (`--help` output and source code)

## Identified Issues

### Missing Commands (undocumented)

| Command | Description |
|---------|-------------|
| `find` / `search` | Interactive fuzzy search with clipboard copy |
| `diff` | Compare secrets between two vaults |
| `info` | Show info about a resource (vault, secret, or file) |
| `parse` | Connection string parser utility |
| `share` | Secret-level access management (grant/revoke/list) |
| `version` | Detailed build and version info |
| `completion` | Shell completion script generation |

### Inaccuracies in Existing Documentation

- **`set`**: Documents `--group` flag that doesn't exist on `set` — groups are assigned via `update`
- **`list`**: Shows `--format json` and `--format yaml` as if they're list-specific — they're global flags
- **`delete`**: Missing `--group`, `--force` flags and `rm` alias
- **`rollback`**: Shows `--version` as optional-looking — it's actually required
- **`rotate`**: Missing charset options (hex, base64, numeric, uppercase, lowercase), `--show-value`, and `--force` flags
- **`vault`**: Missing subcommands: `restore`, `purge`, `update`, `share`
- **`context`**: Missing `clear` subcommand
- **`config`**: Missing `path` subcommand
- **`audit`**: Missing `--days`, `--operation`, `--raw` flags
- **`update`**: Missing many options: `--rename`, `--tags`, `--replace-tags`, `--replace-groups`, `--expires`, `--not-before`, `--clear-expires`, `--clear-not-before`, `--stdin`
- **Output Formats**: Only shows table/json/yaml/raw — misses csv, plain, template, and `--columns` flag
- **`copy`/`move`**: Missing `--new-name` flag

## Proposed Changes

### Sections with no changes needed
- Header & Quick Start
- Installation
- Secret Injection
- Template Injection
- Environment Profiles
- File Storage
- Security
- Development

### Sections to fix

**Core Concepts > Secrets**
- Remove `--group` from `set` examples
- Add `find`/`search` command with examples
- Add `--force` and `--group` to `delete`, mention `rm` alias
- Expand `update` examples

**Organization**
- No changes (already correct)

**Secret History & Rotation**
- Fix `rollback` to show `--version` as required
- Expand `rotate` with full charset list and `--show-value`

**Vault Management**
- Add `vault restore`, `vault purge`, `vault update`, `vault share`

**Vault Context**
- Add `context clear`

**Cross-Vault Operations**
- Add `diff` command
- Add `--new-name` to `copy`/`move`

**Identity & Auditing**
- Add `audit` flags: `--days`, `--operation`, `--raw`
- Add `info` command

**Configuration**
- Add `config path`

**Output Formats**
- Expand to all 7 formats
- Add `--columns` flag
- Clarify these are global flags

### New sections to add

**Utilities** — `parse`, `info`, `version`
**Shell Completions** — `completion` with examples for each shell
**Global Options** — `--debug`, `--format`, `--credential-type`, `--columns`
