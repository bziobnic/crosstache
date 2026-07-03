# Record Types: Structured Secrets with Typed Fields

**Date:** 2026-07-03
**Status:** Approved design, not yet implemented
**Depends on:** metadata-preserving copy/move (PR #315, merged); profile write defaults (PR #320, merged)

## Motivation

Other secret managers (1Password, Bitwarden, Vault) support typed records:
a "login" carries a username and URL alongside the password, a "database"
carries host/port/connection details. xv secrets today are one opaque value
plus a handful of bookkeeping tags. Users who want to record which account
a password belongs to must abuse the `note` tag or maintain sidecar secrets.

This design adds **record types** — built-in and user-defined — that attach
structured fields (username, URL, connection-string, …) to a secret, with
per-field control over whether a field is listable metadata or encrypted
secret material, without breaking any existing secret or consumer.

## Decisions (settled during brainstorm)

1. **Per-field sensitivity, with type-supplied defaults.** Each field is
   `metadata` (listable without fetching the secret) or `secret`
   (encrypted in the value). Built-ins ship sensible defaults (username/url
   metadata; password/key/token secret).
2. **Primary-field `get` compatibility.** Every type declares exactly one
   `primary` secret field. Plain `xv get <name>` returns it, byte-identical
   to today's behavior, so `get`/`run`/`inject` never break when a secret
   gains a type.
3. **Custom types live in config files.** `[types.<name>]` blocks in global
   `xv.conf`, overridable per-project in `.xv.toml` — same hierarchy as all
   other config. No vault-stored registry in v1.
4. **Templates, not schemas.** A type prompts for its declared fields and
   enforces `required` ones; ad-hoc extra fields are always allowed.
5. **Encoding: JSON envelope + field tags (Approach A).** Secret fields
   live in a JSON document in the secret value, marked by content type;
   metadata fields and the type name live in reserved/prefixed tags.
   Single backend object per record; rejected alternatives were companion
   secrets (two-object atomicity/verb complexity) and envelope-only-when-
   needed (dual representation in every reader/writer).

## Data model

```
RecordType { name: String, fields: Vec<FieldDef> }
FieldDef   { name: String, kind: Metadata | Secret, required: bool, primary: bool }
```

- Exactly one field per type is `primary`; `primary` implies
  `kind = Secret` and `required = true`. Type resolution rejects types with
  zero or multiple primaries.
- Field names are kebab-case, validated with the same charset rules as
  secret names.
- A **record** is a secret whose reserved `xv-type` tag names a type.
  Any secret without that tag is an untyped secret with today's exact
  semantics.

### Storage mapping (identical across Azure / AWS / local)

| Piece | Where | Details |
|---|---|---|
| Secret fields | secret value | JSON object `{"<field>": "<value>", …}`; `content_type = application/vnd.xv.record` |
| Metadata fields | tags | prefixed `f.` (`f.username`, `f.url`), beside existing `groups`/`note`/`folder` lifting |
| Type name | tag | reserved `xv-type` |

The **content-type marker**, not JSON sniffing, is what makes a secret a
record. A user who stores a JSON document as a plain secret value is never
misinterpreted as a record.

## Built-in types

Deliberately small; custom types cover the rest.

| Type | Metadata fields | Secret fields |
|---|---|---|
| `login` | username (required), url | **password** (primary) |
| `api-key` | url, account | **key** (primary) |
| `database` | host, port, database, username | **password** (primary), connection-string (optional) |

There is no `note` type — the existing `note` tag already covers free-text
annotation on any secret.

## Custom types

```toml
# xv.conf (global) or .xv.toml (project override)
[types.smtp]
fields = [
  { name = "host" },                          # metadata by default
  { name = "port" },
  { name = "username", required = true },
  { name = "password", kind = "secret", primary = true },
]
```

- Project type shadows a global type of the same name; shadowing a
  built-in emits a warning but works.
- `xv type list` shows resolved types with their source
  (built-in / global / project); `xv type show <name>` prints the field
  table.

## CLI surface

### Create

- `xv set mail-cred --type smtp` — interactive: prompts for each declared
  field in order, secret fields masked, primary last. Missing required
  field → error listing what is absent (fail before any write).
- Non-interactive: `--field name=value` (repeatable) for metadata and
  non-primary secret fields; the primary value arrives via the existing
  `--value` / `--stdin`. Ad-hoc `--field` names not in the type are
  accepted (decision 4) and default to metadata kind; `--field-secret`
  variant marks an ad-hoc field secret.

### Read

- `xv get name` → primary field only. Unchanged behavior and output for
  untyped secrets and typed records alike.
- `xv get name --field username` → that field's value (either kind).
  Unknown field → error listing the record's actual fields.
- `xv get name --record` → the full record (secret fields included) in the
  requested `--format` (json/yaml/table).

### Update / convert

- `xv update name --field username=new` — edits one field. A secret-field
  change writes a new secret version (envelope rewritten); a metadata-field
  change is tag-only.
- `xv update name --type login` — **explicit** conversion of a bare secret:
  current value becomes the primary field, envelope + tags written.
  Conversion never happens implicitly.
- `xv update name --untype` — flattens a record back to a bare secret
  holding the primary field's value; non-primary secret fields are
  **dropped with an interactive confirmation** (or `--yes`), metadata
  fields removed from tags.

### List / TUI

- `xv ls`: type column in table output; `f.*` fields included in
  `--format json` output; `ls --type login` filters by type.
- TUI detail view lists fields, secret ones masked.

## Integrations

- **`xv inject`**: `{{ secret:name.username }}` selects a field; bare
  `{{ secret:name }}` remains the primary. `xv://` URIs use a fragment:
  `xv://vault/name#username` (`#` cannot appear in secret names on any
  backend, so the grammar is unambiguous).
- **`xv run`**: injects the primary under the record's name, unchanged.
  Per-field env expansion is out of scope for v1.
- **`mv` / `copy` / `move`**: no changes — the envelope is the value and
  fields are tags, both already preserved end-to-end (guaranteed since
  PR #315's metadata-fidelity work).

## Limits and error handling

- **Azure 15-tag cap**: `set`/`update` counts reserved tags + `f.*` fields
  + user tags and fails with a per-category breakdown *before writing* when
  the cap would be exceeded. AWS (50 tags) and local (unbounded) get the
  same check with their own caps.
- **Azure 256-char tag values**: a metadata field value exceeding the cap
  errors with a suggestion to declare that field `kind = "secret"`.
- **Corrupt envelope**: a record-marked secret whose value fails JSON
  parsing makes `get` fail loudly (naming the secret and content type);
  xv never silently returns raw JSON as if it were the primary value.
- **Type not found** (record's `xv-type` names no resolvable type):
  degrade gracefully — `get` still works via the envelope's contents
  (primary unknown → require `--field` or `--record`), `ls` shows the type
  name with a marker; a warning explains how to define the missing type.

## Compatibility and migration

- Untyped secrets are untouched in every code path: no envelope, no new
  tags, `get`/`run`/`inject` byte-identical.
- Older xv versions reading a typed record `get` the raw envelope JSON.
  CHANGELOG note, not a breaking change — only explicitly-converted or
  newly-created records are affected.
- External consumers (Azure portal, raw SDKs) see JSON values for typed
  records. Documented in README with the explicit-conversion rule.

## Testing

- Hermetic e2e on the local backend (isolated harness per the #317
  lesson): create/get/update per built-in type; primary-field
  compatibility; `--field` reads; conversion round-trip
  (`--type` → `--untype`); required-field enforcement; ad-hoc fields;
  tag-cap failure; `inject` field syntax; `ls --type` filter; corrupt
  envelope failure; unknown-type degradation.
- Unit tests: type resolution/shadowing (built-in vs global vs project),
  envelope encode/parse, FieldDef validation (one primary, kebab names).
- AWS tag-encoding parity via the existing LocalStack suite.

## Out of scope (v1)

- Vault-stored shared type registry (config-only for now; revisit if teams
  need vault-attached types).
- `xv run` per-field env expansion.
- Typed value validation (URL/port formats).
- TOTP generation, attachment fields.
