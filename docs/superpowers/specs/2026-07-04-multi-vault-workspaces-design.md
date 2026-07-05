# Multi-Vault Workspaces: Simultaneous Vaults Across Backends

**Date:** 2026-07-04
**Status:** Approved design, not yet implemented
**Depends on:** multi-backend registry (`BackendRegistry`, v0.10+), `BackendRef`
`backend:vault` addressing (`src/backend/addressing.rs`), env profiles
(docs/env-profiles.md), record types (#321), `--filter` glob helpers (#326)

## Motivation

xv resolves exactly one `(backend, vault)` pair per invocation. Working
across several vaults — a work Key Vault, a personal local store, a shared
AWS staging vault — means constant `--vault`/`--backend` flags or context
switching, and no way to see everything at once. This design adds
**workspaces**: a set of attached vaults, potentially on different
backends, open simultaneously.

```bash
xv cx add work-kv --backend azure --as work --default
xv cx add personal --backend local
xv cx add staging --backend aws-east --as stage

xv ls                        # union of all three, VAULT column
xv get DB_PASSWORD           # searches all three; unique match wins
xv get stage:DB_PASSWORD     # qualified read
xv set API_KEY               # writes to the default (work)
xv set personal:API_KEY      # qualified write
```

## Decisions (settled during brainstorm)

1. **Union reads + default writes.** Attached vaults behave like one big
   vault for reading (`ls`/`find` union; unqualified `get` searches);
   writes go to the designated default vault unless qualified.
2. **Colon addressing.** `alias:path` qualifies a secret with its vault
   (`work:app/db/pass` — alias, then a normal folder path). `/` stays
   folders-only. Exact-name-first protects literal names containing `:`.
3. **Context + `.xv.toml` overlay.** `xv cx add` writes the context
   (global or `--local`) — the workspace is personal, machine-level
   state. An env profile may declare `vaults = [...]`; when the active
   profile has one, it **replaces** the context workspace in that
   directory. No merging — one source of truth per location.
4. **Hard error on ambiguity.** An unqualified read matching ≥2 attached
   vaults errors, listing every match and its qualified form.

## Architecture

**Approach: a workspace resolution layer above the existing registry**
(chosen over per-command vault loops — N× auth, duplicated iteration
logic — and over a virtual multiplexing `Backend` facade — vault/alias
semantics and `BackendCapabilities` don't survive the trait boundary).

New `src/workspace/` module:

```rust
pub struct WorkspaceEntry {
    pub alias: String,          // unique within the workspace
    pub backend: String,        // registry backend name ("azure", "aws-east", …)
    pub vault: String,
}
pub struct Workspace {
    pub entries: Vec<WorkspaceEntry>,
    pub default_alias: String,
}
impl Workspace {
    pub fn resolve(source) -> Option<Workspace>;   // .xv.toml overlay > context; None = no workspace
    pub fn entry(&self, alias: &str) -> Option<&WorkspaceEntry>;
    pub fn default_entry(&self) -> &WorkspaceEntry;
}
pub struct SecretAddress {                          // parsed "alias:path" | "path"
    pub alias: Option<String>,
    pub path: String,                               // folder path + name, unchanged grammar
}
```

- `BackendRegistry` construction takes the workspace's backend set and
  instantiates **lazily per backend**: attaching an AWS vault must never
  trigger Azure auth for a command that only touches AWS. First use of a
  backend materializes it; failures name the backend and vault.
- **No workspace ⇒ degenerate case.** With no attached vaults, every
  command resolves exactly as today (single active backend + resolved
  vault), byte-identical output. The workspace layer is only consulted
  when a workspace exists.

## Workspace management — `xv cx`

`cx` becomes a top-level alias of `context`. New verbs:

```bash
xv cx add <vault> [--backend <name>] [--as <alias>] [--default] [--local]
xv cx rm <alias>            [--local]
xv cx default <alias>       [--local]
xv cx ls                    # alias, backend, vault, default marker, source (context/.xv.toml)
```

- Alias defaults to the vault name; backend defaults to the currently
  active backend; the first attached vault becomes the default.
- Alias rules: same charset as vault names, unique within the workspace,
  and may not collide with a *registry backend name* (would make
  `xv://alias/...` ambiguous with `xv://backend:vault/...` parsing).
- `cx add` validates the vault exists (a list call) before writing state;
  `--force` skips the probe for offline setup.
- `cx rm` of the default errors unless another `--default` is named or
  only one entry remains. Removing the last entry deletes the workspace
  (back to single-vault behavior).
- Existing `xv context use <vault>` keeps its current meaning (set the
  single current vault) and is what the degenerate case runs on. With a
  workspace present, `context use` errors with guidance to use
  `cx default` (mixing the two models silently would be confusing).

### `.xv.toml` schema

```toml
[env.dev]
vault = "myproj-dev-kv"          # existing single-vault field, still honored when no `vaults`
vaults = [
  { vault = "myproj-dev-kv", backend = "azure", alias = "dev", default = true },
  { vault = "shared-staging", backend = "aws-east", alias = "stage" },
]
```

- `vaults` present ⇒ it replaces any context workspace inside the project.
- Exactly one `default = true` required (or single-entry implicit);
  validation errors follow the fail-closed `[types.*]` precedent.
- `vault` and `vaults` both present: `vaults` wins; warn once.

## Addressing

- Grammar: `alias ":" path` where path is today's `folder/name` grammar,
  unchanged. Applies everywhere a secret name is accepted: `get`, `set`,
  `update`, `delete`, `history`, `rollback`, `rotate`, `mv`, `run
  --include/--exclude`, `--filter` interplay (`--filter` matches names
  within each vault; it does not match aliases).
- **Exact-name-first:** if a secret literally named `work:x` exists in
  scope, it wins over alias interpretation (mirrors inject's dot-split
  rule; realistic only on the local backend's unrestricted charset).
- Templates and URIs: `xv://vault/name`'s vault slot resolves against
  workspace aliases FIRST, then as a raw vault name (today's meaning).
  `xv://backend:vault/name` (explicit backend) bypasses aliases entirely.
  `#field` fragments compose. `{{ secret:… }}` templates take the SAME
  alias resolution via the **colon** form — `{{ secret:work:name }}` /
  `{{ secret:work:app/db/pass }}` (alias `work` + today's path grammar) —
  not the slash slot: `{{ secret:X/Y }}`'s `/` is a plain literal-name
  match today (grammar decision #2 above — `/` stays folders-only, `:`
  introduces the alias), so aliasing the slash slot instead would have
  broken every existing folder-shaped template reference. (Corrected from
  an earlier drafting error in this section that proposed aliasing the
  slash slot.) Exact-name-first applies the same way here too: the FULL
  raw token (colon included) is probed as a literal secret name across
  every attached vault before alias interpretation. Record field dots
  compose with the alias form: `{{ secret:work:mail-cred.username }}`.

## Read semantics

- **Unqualified point reads** (`get`, `history`, `rollback` target
  resolution): search every attached vault (concurrent, bounded).
  Exactly one match → proceed. ≥2 → error `xv-ambiguous-secret` (new
  stable code, exit `13` — next free slot in the secret family after
  10/11/12; added to docs/exit-codes.md):
  `DB_PASSWORD exists in work, stage — qualify as work:DB_PASSWORD or
  stage:DB_PASSWORD`. Zero → not-found, with did-you-mean suggestions
  gathered across the workspace.
- **Union listings**: `ls` and `find` span attached vaults. A VAULT
  column appears only when the workspace has ≥2 entries (single-vault
  output stays byte-pinned by test). `--filter`, `--type`, folder
  scoping, `--group` apply per vault, then results merge (stable sort:
  alias, then name). Pagination applies to the merged set.
- `find --all-vaults` keeps its meaning ("every vault I can list") and
  is documented as the superset of the workspace.
- **Partial failure fails loud:** if any attached vault errors during a
  union read or ambiguity search (auth, network), the command fails
  naming that vault — a partial union can silently hide a match or mask
  an ambiguity. No `--best-effort` for workspaces in v1; revisit only
  with real-usage evidence.

## Write semantics

- Unqualified `set`/`update`/`rotate`/`delete`/`restore`/`purge` target
  the **default vault only** — never search-then-write (a write landing
  in a vault the user didn't intend is the worst outcome; qualification
  is one token). Point reads embedded in writes (e.g. `update` fetching
  before editing) use the same default-vault targeting, not ambiguity
  search.
- Qualified writes (`set personal:API_KEY`) go where addressed.
- Cross-vault within the workspace: `mv work:secret stage:/` (and
  `stage:folder/`) performs copy+delete via the existing `move`
  machinery, reusing its metadata preservation (the #315
  `rename_request_from_properties` path). `xv mv` has no `--force` flag
  anywhere — same-vault renames refuse a destination name collision too —
  so cross-vault alias `mv` never overwrites either; `xv move --from/--to
  --force` is the dedicated overwrite path, and `copy`/`move --from/--to`
  gain the same alias support as `mv`. Record envelopes ride along
  untouched (the value + tags move as-is; cross-backend tag-budget checks
  apply at the destination's caps before any write).

## Capability differences in a mixed workspace

Capability-gated operations in union views apply per vault; a vault whose
backend lacks the capability is **skipped with a stderr note** naming it
(`note: 'personal' (local) has no soft-delete; --deleted skipped for it`)
— never silently omitted, never a hard failure of the whole view. Point
operations against a specific vault keep today's per-backend errors.

## Caching

Cache keys gain the `(backend, vault)` pair with a schema-namespace bump
(the #323 `secrets-list-v2` precedent) so pre-workspace entries miss
cleanly. Union reads populate per-vault entries; TTL semantics unchanged.

## Error handling summary

| Case | Behavior |
|---|---|
| Unknown alias in `alias:path` | error listing attached aliases (after exact-name check) |
| Ambiguous unqualified read | `xv-ambiguous-secret`, lists qualified forms |
| Attached vault unreachable during union | fail loud naming the vault |
| `cx add` duplicate alias | error naming the existing entry |
| Alias colliding with a backend name | error at `cx add` / `.xv.toml` validation |
| `vaults` missing a default (multi-entry) | fail-closed config error |
| `context use` while a workspace exists | error pointing at `cx default` |

## Backward compatibility

- No workspace attached ⇒ every command byte-identical to today
  (pinned by tests, per the record-types precedent).
- `xv://vault/name` keeps its raw-vault meaning when the vault slot
  matches no alias; `{{ secret:work:name }}` (the colon form) does the
  same. `{{ secret:X/Y }}` (the slash form, no colon) is untouched by
  aliasing entirely — it was, and remains, a plain literal-name match.
- `find --all-vaults`, `--vault` flags, `migrate`, `diff`, and
  cross-vault `copy`/`move --from/--to` are unchanged; aliases are
  accepted anywhere a vault name is accepted once Phase C lands.
- Not a breaking release: the feature is opt-in via `cx add`.

## Phasing (each phase an independently shippable PR)

- **Phase A — workspace core:** `src/workspace/` model + persistence
  (context schema + `.xv.toml` `vaults`), `cx` alias + add/rm/default/ls
  verbs, `SecretAddress` colon parsing with exact-name-first, lazy
  multi-backend registry construction, `get`/`set` semantics (qualified +
  default-vault writes + ambiguity search for `get`), `xv-ambiguous-secret`
  error code, degenerate-case byte-identity tests.
- **Phase B — union views:** `ls`/`find` union + VAULT column, ambiguity
  search extended to the remaining read verbs (`history`, `rollback`
  version listing — destructive verbs stay default-vault-only per Write
  semantics), per-vault capability gating with stderr notes, cache
  re-keying, pagination over merged sets.
- **Phase C — integration:** alias resolution in `xv://` URIs and
  templates (inject/run), alias support in `mv`/`copy`/cross-vault moves,
  TUI vault pane presents the workspace, README/docs/env-profiles/
  exit-codes updates, CHANGELOG.

## Testing

- Hermetic multi-vault fixtures on the local backend: multiple stores
  via `[named_backends.local-a/local-b]` entries give real cross-backend
  workspaces with zero cloud dependencies.
- AWS smithy-mock coverage for lazy construction (Azure never built when
  only AWS vaults are touched) and capability gating.
- Byte-pinned single-vault outputs (no-workspace degenerate case).
- Matrices: ambiguity (0/1/N matches × qualified/unqualified),
  exact-name-first, overlay replacement (context vs `.xv.toml`),
  alias/backend-name collision validation, partial-failure fail-loud.
- The #317 lesson applies: every e2e fixture fully isolated from host
  config; scrubbed-env runs before merge.

## Out of scope (v1)

- Merging context and `.xv.toml` workspaces (replace-only).
- `--best-effort` partial unions.
- Cross-vault atomic transactions; workspace-wide `rotate`.
- Secret-name federation/renaming layers (aliases name vaults, not secrets).
- `xv file`/blob operations across the workspace (single-vault as today).
