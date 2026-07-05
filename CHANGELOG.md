# Changelog

## v0.20.1 — First cx add keeps the current vault attached (2026-07-05)

### Changed

- **The first `xv cx add` now auto-attaches the currently-resolved vault as the workspace default (#341).** v0.20.0's first-add behavior attached only the requested vault, making it the workspace's sole (and therefore default) entry — so `xv ls` right after `cx add` stopped showing whatever vault was already current, silently hiding pre-existing secrets (reported by the maintainer immediately after v0.20.0 shipped). The first `cx add` now also attaches the vault you were already using (whatever `--vault`/context/`default_vault` resolves to) as the default, then attaches the requested vault alongside it — `xv ls` immediately shows both, and unqualified writes keep landing where they already were. Passing `--default` on that first add makes the newly-added vault the default instead (the prior vault stays attached, just not the write target). If the requested vault already resolves to the same `(backend, vault)` as the current one, or the current vault can't be resolved at all (no context, no `default_vault`), this degenerates to the original single-entry behavior (the latter case notes the fallback in the success message). An auto-attach candidate whose alias collides with the requested `--as` alias errors with the existing duplicate-alias message, before anything is written. Subsequent `cx add`s (a workspace already exists) are unchanged. **Code review fix:** the auto-attach's "current backend" signal is now profile-aware (a new `Config::pre_flag_backend`, resolved in `main.rs` before this command's own `--backend` flag is considered) — a `.xv.toml` env profile's `backend` outranks the config file, and using the config-file-only value (as an earlier version of this fix did) recorded the WRONG backend for the auto-attached entry whenever a profile was active, making a subsequent `xv ls` fail against a backend that was never actually in use.

### Fixed

- **Unit tests no longer read the real global vault context/workspace, isolated via a new `XV_CONTEXT_DIR` env override (#342).** Several `#[tokio::test]`s in `src/cli/secret_ops.rs` exercise `execute_secret_rotate_native`/`execute_secret_update_direct`/`resolve_vault_for_trait`, all of which resolve the active workspace/vault through `ContextManager::load()` — with no isolation, that read the developer's REAL `$XDG_CONFIG_HOME/xv/context` (or `$HOME/.config/xv/context`), so a machine with a multi-vault workspace attached (e.g. one whose default entry is on a different backend, such as `azure`) silently hijacked which backend/vault these tests actually resolved against, surfacing as spurious failures unrelated to the change under test (traced to #341's own workspace testing). `XV_CONTEXT_DIR` (mirroring the `XV_CACHE_DIR`/#318 precedent exactly) overrides the context store's directory; the affected tests now point it at a `tempfile::TempDir` for their duration. CI was unaffected (no context file exists there), which is why this went unnoticed until now. **Code review fix:** the override now also skips the LOCAL (`cwd/.xv/context`) check entirely, not just the global path — setting `XV_CONTEXT_DIR` means "my context store lives here, full stop."

## v0.20.0 — Multi-vault workspaces (2026-07-04)

### Added

- **Multi-vault workspaces — Phase A: workspace core (`xv cx add`, colon addressing, default-vault writes).** Attach several vaults, potentially on different backends, so they behave like one workspace: `xv cx add <vault> [--backend] [--as alias] [--default] [--local] [--force]` attaches a vault (alias defaults to the vault name, backend to the active one, the first attach becomes the default, a vault-exists probe runs unless `--force`); `xv cx rm <alias>` detaches one (errors removing the default unless it's the last entry, in which case the workspace is deleted and single-vault behavior resumes); `xv cx default <alias>` changes the write target; `xv cx ls` lists attached vaults (alias, backend, vault, default marker, source). `cx` is now a visible alias of `context`. Colon addressing (`alias:path`, e.g. `xv get work:DB_PASSWORD`) qualifies a secret with its vault; an exact secret name always wins over alias interpretation.
  - **Reads search, writes don't, on every secret verb.** `xv get`/`xv history`/`xv rollback` on an unqualified name search every attached vault — a unique match resolves, no match is the normal not-found error, and two or more matches error with the new `xv-ambiguous-secret` code (exit `13`), listing every qualified `alias:name` form. `xv set`, `xv update`, `xv rotate` (including `--native`), `xv delete` (including `--group`), `xv restore`, and `xv purge` never search on an unqualified name — every one of them always targets the workspace's default vault; qualify with `alias:name` to reach another attached vault. Bulk `set` (`xv set KEY=val KEY2=val2`) resolves each `KEY=value` pair independently, so a mix of unqualified (→ default vault) and `alias:KEY=value` (→ that vault) pairs works in one command.
  - A `.xv.toml` `[env.<name>] vaults = [...]` block, when present, REPLACES the context-store workspace entirely for that project (no merging); because of that replacement, `xv cx add`/`rm`/`default` now ERROR (exit `3`, naming the `.xv.toml` path and env) instead of silently mutating a context-store workspace the project overlay would just override — there is no override flag in v1, editing `.xv.toml` directly is the explicit path. **No workspace attached ⇒ every command above is byte-identical to today** (pinned by a byte-golden test comparing full stdout/stderr) — the feature is entirely opt-in via `xv cx add`. Backend construction is lazy: a command that only touches one attached backend never authenticates the others.
  - **Not yet workspace-aware in Phase A** (landed in Phase B/C below): `ls`/`find` (union view across attached vaults, plus per-vault capability gating), alias support in `xv://` URIs/templates/`mv`/`copy`, and the TUI vault pane. `xv file`/blob storage is single-vault as today and stays out of scope for the whole workspaces feature per the design (workspaces only span secrets).
- **Multi-vault workspaces — Phase B: union `ls`/`find` views.** With a workspace attached, `xv ls` and `xv find` now span every attached vault instead of just the default:
  - `xv ls` merges each attached vault's listing (`--filter`/`--type`/folder scoping/`--group` apply per vault, then merge; stable sort alias-then-name; pagination over the merged set). A `Vault` column appears on the table/long views only when the workspace has ≥2 attached entries; the default grid view instead prefixes `alias/` onto the name. A single-entry workspace (or no workspace) renders exactly like today — byte-identical, pinned by test.
  - `xv find` candidates are the union of attached vaults, rows prefixed `alias/` (mirroring the existing `--all-vaults` convention). `--all-vaults` keeps its documented meaning — every vault the active backend can list, a strict superset of the workspace — and takes priority even with a workspace attached.
  - **No partial unions**: any attached vault erroring during a union read fails the whole command, naming the vault and backend.
  - **Capability gating, never silent**: `xv ls --deleted` in a union workspace applies per vault; a vault whose backend lacks soft-delete is skipped with a stderr note naming vault+backend, never a hard failure of the whole view.
  - Secrets-list cache keys are now scoped per `(backend, vault)` instead of `vault` alone (schema bump `secrets-list-v2.json` → `-v3`, nested under a `backend` directory) so two workspace entries sharing a vault NAME on different backends never collide; pre-existing v2 cache entries simply miss.
- **Multi-vault workspaces — Phase C: URI/template aliases, cross-vault `mv`/`copy`, TUI pane (feature complete).**
  - The vault slot of an `xv://` URI (both `xv run`'s environment-variable scan and `xv inject`'s templates) now checks workspace aliases FIRST, falling back to today's raw-vault-name meaning when nothing matches; `xv://azure:vault/name`-style explicit backend prefixes bypass alias resolution entirely, exactly as before. `#field` fragments compose with alias resolution unchanged.
  - `xv mv alias:secret otheralias:/` (and `otheralias:folder/`) now routes to the existing cross-vault copy+delete machinery — with resolved `(backend, vault)` pairs — when BOTH source and destination carry an attached alias resolving to a different vault; a single alias-qualified side (or none) falls straight through to the unchanged same-vault rename/re-folder path. `xv copy`/`xv move --from/--to` accept aliases in `--from`/`--to` too. Metadata (groups/note/folder/tags/record envelopes) rides along via the existing `rename_request_from_properties` path (#315); a new destination tag-budget pre-check fails loud, before any write, when the destination backend's tag cap (e.g. Azure's 15) can't hold the secret's tags. Cache invalidation for both sides is keyed per-side by each resolved entry's own backend name.
  - `xv tui`'s vault pane lists workspace entries as `alias (backend)` when a workspace is attached; selecting one scopes the secrets pane to that entry's own vault on its own backend, spanning multiple backends in one session. No workspace attached ⇒ the vault pane is unchanged.
  - README's "Multi-vault workspaces" section is promoted out of preview, covering all three phases end-to-end.

### Changed

- **`xv context ls` / `xv cx ls` now lists the attached multi-vault workspace** (alias, backend, vault, default marker, source), not recent vault contexts. Recent contexts are still available, unabbreviated, via `xv context list`. This follows the approved multi-vault workspaces spec, which specifies `xv cx ls` for the workspace listing — `cx` and `context` share one subcommand tree, so the `ls` alias could only mean one or the other.

## v0.19.3 — Record write-path integrity and types-only .xv.toml fixes (2026-07-04)

### Fixed

- **A `.xv.toml` with only a `[types.*]` block (no `[env.*]`, no `default_env`) no longer breaks write commands (#331).** The #320 write-default resolvers (`resolve_group`/`resolve_folder`, plus `resolve_vault_name`/`resolve_resource_group`) propagated `project::resolve_env`'s error even when the file simply defined zero environments — so `xv set`, `xv run`, and `xv gen --save` failed with `error[xv-env-not-defined]: Environment '(none)' not defined in .xv.toml; available: ` even though `xv list` worked fine. `resolve_env` now returns `Ok(None)` ("no active profile") instead of erroring when a `.xv.toml` defines no `[env.*]` tables and has no `default_env` — types-only project files are a legitimate shape since record types (#321). A file that *does* define environments keeps erroring exactly as before on an unknown/unselected `--env`/`XV_ENV`; an explicit `--env`/`XV_ENV` against a file with zero environments now gets a clearer "defines no environments" message instead of a rough empty `available: ` list.
- **Bare-value `xv update <record> <value>`/`--stdin` and `xv rotate <record>` no longer corrupt typed records (#330).** Both wrote the raw string over the record's whole JSON envelope while leaving `content_type` at `application/vnd.xv.record`, so every subsequent read (`get`, `get --field`, `get --record`) failed with `xv-config-invalid` and every non-primary field was lost from the latest version (recoverable only via `xv rollback`). Both now set the record's declared **primary field** inside the envelope instead — the same write-back path `--field`/`--field-secret` edits already use — preserving every other envelope field, metadata field, and tag/group/note/folder. A corrupt envelope or an unresolvable `xv-type` now fails loud before any write, rather than guessing. `--field-secret <primary>=…` (still rejected, since the primary only ever arrives via a bare-value write) now points at the real paths: `xv update <name> <value>`, `--stdin`, or `xv rotate <name>`. Untyped secrets are unaffected on all three paths. **Follow-up (code review):** combining a bare value/`--stdin` with any classic metadata flag (`--note`/`--group`/`--tags`/`--rename`/`--expires`/`--not-before`/`--enabled`/`--folder`/`--clear-*`) against a record used to apply the primary-field write and silently drop the flag while printing "Successfully updated" — now rejected loud, naming every flag supplied, with nothing written; run the value update and the metadata update as two separate commands. `xv rotate` on a record also re-enables it again, matching untyped `rotate`'s existing behavior (previously a disabled record stayed disabled after rotating).

## v0.19.2 — Bulk folder moves with mv --filter (2026-07-04)

### Added

- **`--filter <GLOB>` on `xv mv`**, bulk-moving every secret whose name matches the glob into a destination folder in one plan/confirm step, instead of a shell loop (`xv find --filter 'test-*' --names-only | while read -r n; do xv mv "$n" archive/; done`). Matching is identical to `ls`/`find --filter` (#326): case-sensitive, whole-name, either the display or backend (sanitized) name, whole-vault scope. `SOURCE` and `--filter` are mutually exclusive (exactly one is required); with `--filter`, `DEST` must be a folder destination (`folder/` or `/`) since a rename is impossible for a multi-secret move. Matched secrets keep their names — only the `folder` metadata is rewritten, the same metadata-only update `xv mv`'s bulk folder moves already use. Reuses the existing bulk-move machinery: count + sample plan confirmation, `--yes` bypass, non-TTY refusal without `--yes`, `--dry-run` preview, a collision pre-check before any move, and attempt-all/report-failure-count partial-failure behavior. Secrets already in the destination are skipped and noted (not counted as moves, not an error); zero matches fails loud; an invalid glob fails with `invalid_argument` before any backend call.

## v0.19.1 — Name-glob filtering on ls and find (2026-07-03)

### Added

- **`--filter <GLOB>` on `xv ls`/`list` and `xv find`**, consistent with the existing `xv migrate --filter`. The glob matches either the secret's user-facing (`original_name`) or backend (sanitized) name — the same either-name convention `xv mv` and `xv run --include`/`--exclude` use — case-sensitive and whole-name (`test-*` matches `test-db`, never `latest-db`). On `ls` it's applied client-side before pagination/rendering and composes with the folder positional, `--type`, `--deleted`, and every output format. On `find` it's a hard pre-filter on the candidate set applied before fuzzy scoring, so `PATTERN` ranks only within the filtered set; `--filter` with no `PATTERN` yields an unranked filtered list — `xv find --filter 'test-*' --names-only` is the canonical "names starting with test-" one-liner. An invalid glob pattern fails fast with `invalid_argument`, before any backend call.

## v0.19.0 — Record types, fail-fast run/inject, and backend-resolution fixes (2026-07-03)

### Added

- **Record types: typed secrets with structured fields (#321). Not a breaking change** — only secrets explicitly created with `--type` or converted with `update --type` change shape; every existing/untyped secret is byte-identical on every code path (`get`/`run`/`inject`/`ls`), no envelope, no new tags, unless you opt in.
  - Built-in types `login`, `api-key`, `database`, plus custom `[types.<name>]` blocks in `xv.conf`/`.xv.toml` (project shadows global, shadowing a built-in warns). Every type declares exactly one `primary` secret field, so plain `get`/`run` on a record return/inject that field, unchanged from today's contract.
  - Per-field sensitivity: `metadata` fields ride tags (`f.<name>`, listable without fetching the secret); `secret` fields live in a JSON envelope inside the value, marked by a reserved `application/vnd.xv.record` content type (never JSON-sniffed).
  - `xv type list`/`xv type show`; `xv set --type/--field/--field-secret`; `xv get --field/--record`; `xv update --field/--field-secret` (edit) and `--type/--untype` (explicit conversion, never implicit); `xv ls --type` filter plus `f.*`/`record_type` in JSON output.
  - `xv inject`'s `{{ secret:name.field }}` template syntax and `xv://vault/name#field` URI fragment select one field; an exact secret name always wins first, so an untyped secret literally named `a.b` still resolves as itself. `xv run` gets no per-field expansion — it injects a record's primary field under its name, same as `get`.
  - One invalid `[types.*]` block fails type resolution globally (fail-closed by design) rather than silently dropping just that type.
  - External consumers (Azure portal, raw SDKs, older `xv` binaries) see a typed record's raw JSON envelope as its value — documented in the README, alongside the explicit-conversion rule.

### Changed

- **Breaking: `xv run` now aborts before launching the child when any selected secret or `xv://` reference fails to fetch; use `--best-effort` for the old behavior** (#306). Previously a per-secret fetch failure only printed a warning and the command ran anyway, which could silently launch a process missing an env var (e.g. after a transient backend error or a permission problem). All failures across both the selected-secret list and `xv://` reference resolution are now collected and reported together before the exit.
- **Breaking: `xv inject` now aborts before writing/printing the rendered output when any `{{ secret:name }}` or `xv://` reference fails to resolve; use `--best-effort` for the old behavior** (#313). Previously a per-reference resolution failure only printed a warning and rendering continued, which could silently write a config file (e.g. `.env`) with unresolved `{{ secret:name }}` / `xv://` placeholders left in place while `xv inject` exited 0. An unparseable `xv://` reference in a template (e.g. a malformed backend prefix) is now also treated as a failure rather than silently skipped, since — unlike `xv run`'s scan of arbitrary parent-environment values, which can incidentally contain `xv://`-shaped substrings — every reference in a template the user wrote for `xv inject` is unambiguously intentional.

### Fixed

- **`.xv.toml` backend selection now resolves before config validation, and `XV_BACKEND` no longer outranks the project profile (#305).** A project profile selecting `local` could fail with "Subscription ID is required" whenever the global Azure config was incomplete, because validation ran before the profile was consulted; and setting `XV_BACKEND` silently suppressed the profile entirely (clap populates the `--backend` slot from the env var, making it indistinguishable from an explicit flag). The documented precedence — `--backend` flag > `.xv.toml` profile > `XV_BACKEND` > global config > `azure` — now actually holds, and purely-local commands (`context`, `cache`, `parse`, …) no longer demand Azure/AWS credentials they never use. A literal `--backend` token in `xv run`'s child command (after `--`) is no longer mistaken for the flag.
- **`xv move --force` can actually overwrite an existing target, and `xv copy`/`xv move` work on every backend (#307).** `--force` was a dead flag: the move detected the target, warned "Overwriting…", then delegated to the copy step whose unconditional exists-guard aborted — with an error message that pointed back at `--force`. The flag is now honored (source still deleted only after a successful copy; `xv copy` keeps refusing to overwrite). While fixing it: `copy`/`move` were silently Azure-only regardless of `--backend`; they now use the same backend dispatch as get/set/delete/list, preserving `groups`/`folder`/`note` through the canonical metadata extraction.
- **xfunction: JWT issuer now follows OIDC discovery; installer no longer passes the client secret via argv (#310).** The Azure Function validated tokens against a hard-coded v1 `sts.windows.net` issuer (while fetching v2 discovery metadata for keys only), so genuine v2 tokens were always rejected; the expected issuer now comes from `AZURE_AD_ISSUER` → the discovery document's `issuer` → the v1 fallback, fail-closed with no cache poisoning. The installer's optional credential storage uses `xv set --stdin` instead of `--value` (secret no longer visible in process listings) and reports per-secret store failures instead of unconditional success.
- **Small-fry from the same review batch (#311):** the cache-invalidation unit test no longer touches the real OS cache directory (new `XV_CACHE_DIR` override, documented in the README); AWS secret names are validated against the advertised `[a-zA-Z0-9/_+=.@-]` charset locally with a clear error naming the offending character, instead of a late remote `ValidationException`; Cargo.toml description/keywords reflect the multi-backend positioning.
- **Env profile `group`/`folder` defaults are now actually applied, not just parsed (#308).** `.xv.toml`'s `[env.*].group` and `.folder` were documented and read into `EnvProfile`, but no command consulted them — only `xv config show`/`xv env show` displayed them. `Config::resolve_group`/`resolve_folder` (mirroring `resolve_vault_name`) now back `xv run` (group as the injection filter when `--group` is omitted) and, via a shared `apply_profile_write_defaults` helper, both `xv set` and `xv gen --save` (group/folder as write-time defaults when the corresponding flag is omitted) — restoring the "set and gen --save build identical requests" invariant. An explicit `--group`/`--folder` always wins. `xv list`/`ls` is unaffected by either default — its documented contract already states the write-side folder default doesn't scope listings, and the same now holds for group. A blank `group = ""` / `folder = ""` in the profile resolves to no default at all (not an unfilterable empty-string tag); `xv run`'s fail-loud "nothing matched" error now says `(from env profile default)` when the unmatched group came from the profile rather than `--group`.
- AWS: `list_deleted_secrets` now exposes the `xv:original_name` tag in its summaries (matching `list_secrets`), so `xv ls --deleted` no longer loses the user-facing name on AWS (#301).
- **`xv scan` now honors every `[scan]` knob it already documented (#309).** `[scan].min_value_length` and `[scan].patterns` were parsed but silently ignored — the engine always used the hard-coded default minimum length, and every scan mode enabled all built-in patterns regardless of the allowlist. `XV_SCAN_DISABLE=1` (or `=true`) was documented as an escape hatch but read nowhere. All three now work, uniformly, across `xv scan [PATH]...`, `--staged`, and `--all`. An `[scan].patterns` allowlist that resolves to zero known pattern names (e.g. a typo) is now a hard config error listing the valid names, rather than a silent all-patterns-disabled scan that still exits 0. **Behavior change:** `xv scan --staged` (and therefore the installed pre-commit hook) now applies the same default excludes and `[scan].exclude` globs that `scan .` and `scan --all` already applied — previously it scanned every staged file regardless of excludes.

## v0.18.0 — Filesystem verbs: xv mv, ls aliases everywhere, and reliable rename (2026-07-02)

### Added

- `ls` is now an alias for `list` on every subcommand (`xv vault ls`, `xv group ls`, `xv share ls`, `xv vault share ls`, `xv context ls`, `xv env ls`, `xv file ls`), matching the top-level `xv ls`.
- New `xv mv <SOURCE> <DEST>` moves/renames secrets and re-folders whole folders (trailing `/` = folder, `/` = vault root). Folder-only moves are metadata-only; renames ride the trait-level rename. Bulk moves confirm with a count + sample plan (`--dry-run`, `--yes`); `--dry-run` also previews single-secret moves.

### Fixed

- AWS: `list_secrets` now exposes the folder/note/original-name tags, fixing folder-qualified `xv mv`/`xv ls` on AWS; `xv mv` collision pre-check also matches backend (sanitized) names.
- **`xv update --rename` works again on every backend (#295).** Rename is now a real trait-level operation (`SecretBackend::rename_secret`): read value + metadata, create under the new name (user tags, groups, note, folder, content type, and expiry ride along), then delete the old name with the backend's normal delete. Previously Azure created the duplicate and never deleted the original, while local and AWS silently ignored the flag; the `SecretUpdateRequest.new_name` field is gone so a backend can never ignore a rename again. Combined with other update flags, the in-place updates apply first, then the rename. Renaming onto an existing name is refused (`xv-conflict`); version history does not carry over. On Azure the old name is left soft-deleted (visible in `xv ls --deleted`; renaming back within the retention window conflicts); on AWS it sits in the standard 30-day recovery window; on local it lands in trash.
- **`RenameIncomplete` is restored** (removed in the v0.17.0 legacy cleanup while unreachable): if the new secret is created but deleting the original fails, `xv update --rename` exits 43 with code `xv-rename-incomplete`, names both copies and the vault, and prints the recovery steps (`xv get <new>`, then `xv delete <old>` or retry). The new secret is deliberately never rolled back. The 43 row is back in `docs/exit-codes.md`.

## v0.17.0 — Folder-aware listing, unified renderers, and legacy cleanup (2026-07-02)

### Added

- **`xv ls --deleted`** lists soft-deleted secrets (name + deleted date + scheduled-purge date where the backend can supply them: Azure has both, local and AWS report the deleted date only). Capability-gated — backends without soft delete get a clear error. Machine formats emit a `{name, deleted, purge_scheduled}` row array; the default view is the usual grid, `-l` is a `NAME/DELETED/PURGE SCHEDULED` long listing. Conflicts with the `FOLDER` positional, `-r`, `--group`, `--all`, `--expiring`, and `--expired`.
- **`xv group list`**: lists secret groups with member counts, derived from the comma-separated `groups` metadata. Full `--format`/`--columns` support; `--no-cache` to bypass the shared secrets cache.
- **`xv ls --sort name|updated`** (default `name`): `updated` shows the most recently updated secrets first in every output mode, including machine formats (in `--deleted` mode it sorts by deleted date).
- **`xv find --folder <path>`** scopes fuzzy search to a folder subtree (segment-boundary rule: `prod` matches `prod/db`, not `production`); composes with `--all-vaults`.
- **Hidden `xv __complete-folders`** emits cached folder paths (including ancestor prefixes) one per line for shell tab-completion, mirroring `__complete-secrets` (cache-only, silent when cold).
- **Global `--no-color` flag** (complements the `NO_COLOR` env var and config key).
- **`--names-only` on `vault list` and `file list`** (one name per line, pipe-friendly; `file list --names-only` lists recursively).
- **`file list --pager [auto|always|never]`** matching every other list command (bare `--pager` unchanged).
- **`xv ls` is folder-aware and ls-styled.** The default TTY output is now a multi-column name grid with folders listed first (`prod/`), derived from each secret's `folder` tag. `xv ls prod` lists inside a folder, `xv ls -l` is a borderless long listing (name, updated date, groups, note), `xv ls -r` flattens recursively, and the previous rounded table remains available via explicit `--format table`. Piped/machine output (`--format json|yaml|csv`, `--names-only`) keeps the flat schema unchanged, scoped to the requested subtree. Machine output rows are now sorted by display name (previously backend order).
- **Global `--columns <COLS>` flag returns** (removed as a silent no-op in the P0 pass): comma-separated, case-insensitive column names applied in the given order to `table`/`plain`/`csv` output of every list command. Unknown names error and list the available columns. Explicit `--columns` overrides the hide-empty-columns behavior; JSON/YAML/template keep the full schema.
- **`xv find --format csv`** now works (previously find had no CSV output).
- **`xv context list` and `xv env list` honor the global `--format`** (json/yaml/csv/…): `context list` rows are `{status, vault, resource_group, last_used, usage_count}`; `env list` renders `Name/Active/Backend/Vault/Resource Group` rows instead of a hand-rolled line format.
- **`xv config show --format yaml`** serializes the whole `Config` object (like `--format json` always did — `config show` is a resource view, not a list; this documented exception is the one command whose machine output is not the table's row set).
- **`xv update --enabled <true|false>`** enables or disables a secret directly (disabled secrets are excluded from `xv ls` and `xv group list` by default).

### Changed

- **`xv ls -r` shows folder-qualified names** (`prod/db-pass`, relative to the listing root) in the grid, long, and `--names-only` views. Non-recursive output and machine formats are unchanged.
- **`context envs` is deprecated**: hidden from help, warns `context envs is deprecated; use env list`, and delegates unchanged.
- **List empty-states now go to stderr** for human formats across all list commands (including `xv ls`, whose empty message previously landed on stdout — `xv ls > file` on an empty scope now writes an empty file), and empty-state/count wording is standardized via shared helpers. `xv history`'s count line moved from stderr to stdout (human formats only).
- **`vault share list -f/--fmt` is deprecated**: use the global `--format`. `--fmt` still works with a warning for one release; `-f` is removed. `vault list`'s redundant local `--format` was removed (the identical global flag takes over transparently).
- **BREAKING (machine shapes normalized).** Pre-1.0 breaking changes, deliberate and grouped here:
  - **`xv find`**: JSON/YAML output is now the standard row shape — `score` is a two-decimal string (was a raw integer) and `folder`/`groups` are empty strings (were `null`). The TTY output is the shared rounded table; the score bar and UPPERCASE header are gone. `--names-only` unchanged.
  - **`xv audit`**: honors the global `--format` (JSON = one array of `{timestamp, operation, resource, caller, status}` rows). `--raw` is deprecated to a hidden alias that warns and implies `--format json`; its old per-entry documents with `---` separators (and rich fields like `correlation_id`/`properties`) are no longer emitted. The contextual `Vault:`/`Secret:` lines moved to stderr so `xv audit --format json | jq` sees pure JSON, and the human timestamp is now full-date (`%Y-%m-%d %H:%M:%S`).
  - **`xv file list --format csv`**: columns now match the table — `Kind,Name,Size,Content-Type,Modified,Groups` (was a snake_case kitchen-sink set with raw byte sizes, etags, and JSON-blob metadata columns). JSON/YAML keep the rich full-fidelity serialization. The human table gains the leading `Kind` column.
- **Counts are plural-aware**: `1 vault`, `3 vaults`, `5 audit log entries` — the `"N noun(s)"` style from the previous pass is gone.
- **`xv config show` human table** renders through the shared formatter (uniform `--columns`/`--no-color` behavior); same for `config show --resolved`. `config show --format csv|plain` now emits `Setting`/`Value`/`Source` rows via the shared formatter (previously fell back to the human table).

### Fixed

- **CJK-safe list rendering**: grid/long listings and note wrapping now measure terminal display width (via `unicode-width`) instead of char count, so full-width characters no longer misalign columns.
- **`xv parse` printed its table twice** (and leaked a table into `--fmt json` output); the manager no longer prints — the CLI renders once.
- **Pagination footers are plural-aware** (`… of 1 secret`, `… of 3 secrets`) — the last `"{noun}(s)"` holdout is gone.
- **`xv cache refresh --key vaults` no longer dumps the vault list to stdout**; the refresh fetches and caches silently.
- **Empty `history`, `find`, and `audit` machine-format output is now valid-empty** (`[]` for JSON, headers-only for CSV) on stdout instead of nothing, so `| jq` works on empty results. Same for empty `context list`/`env list` machine output.
- **Empty machine-format output is now valid-empty** (`[]` for JSON) on stdout for `vault list`, `vault share list`, and `file list`, instead of a stderr-only message that broke `| jq` on empty results.
- **`xv ls` table rendering.** Columns whose cells are all empty are no longer rendered as blank zero-width headers, narrow terminals now shrink the widest column first instead of chopping every column (no more `UT`/`C` timestamp wrapping), and the `Updated` column shows the date only (`2026-05-17`). Machine formats (JSON/YAML/CSV) are unchanged.
- **`xv share list` honors the global `--format`** (json/yaml/csv/…) like `xv vault share list` already did; its empty-state message now goes to stderr, and machine formats emit valid empty output (`[]`) for pipes.
- **`NO_COLOR` now disables color for all table output.** The environment variable was previously honored only by status messages; it now also sets the config's `no_color`, and `xv context list` no longer hard-codes colored output.

### Removed

- Dead legacy `execute_secret_list` renderer and its `secret_count_label` helper; the `format_table()` free function (all tables now go through `TableFormatter`); the `xv find` score bar.
- **Four deprecated aliases removed outright** (Scott is the sole user; backwards compatibility is a non-feature): `vault share list --fmt` (use the global `--format`), `audit --raw` (use `--format json`), `context envs` (use `env list`), and `migrate --overwrite` (use `--on-conflict replace`). All four now produce a clap error instead of a deprecation warning.
- **Dead legacy (pre-backend-trait) non-trait code paths deleted**: `execute_secret_set`, `execute_secret_get`, `execute_secret_delete`, `execute_secret_set_bulk`, `execute_secret_delete_group`, and `execute_secret_update` in `src/cli/secret_ops.rs` (dead or reachable only through a degenerate registry-init failure, superseded by the backend-trait path), plus `SecretManager::update_secret_enhanced` and dead config helpers `ContextManager::migrate_from_config` and `init_default_config`. Fixes the tag-drop bug that lived in the deleted legacy update pipeline: metadata-only updates routed through it could drop custom tags; the live backend-trait path was already correct and is unaffected.

---

## v0.16.0 — Cross-backend advanced commands, new flags, and UX fixes (2026-06-29)

Advanced commands now work on every backend, the CLI's documented-but-missing
flags are implemented, and a batch of output/exit-code/confirmation issues are
fixed. Surfaced by a multi-model UX review and hardened against Cursor Bugbot
findings (#286).

### Added

- **Advanced commands work on local & AWS backends (#286).** `xv run`, `xv inject`, `xv rotate` (default), `xv scan`, and `xv env pull`/`env push` now route through the active backend trait instead of hardcoding Azure Key Vault, so they no longer fail with Azure auth errors on the local or AWS backends. Azure behavior is unchanged (its trait impl delegates to the same operations).
- **New flags (#286):** `set --value`, `set --tag`, `run --include`/`--exclude`, `update --tag` (alias of `--tags`), and `--pager [auto|always|never]`.
- **`xv scan --all` (#286)** performs a full `HEAD`-tree scan (`git ls-tree HEAD` + `git show HEAD:PATH`), honoring `[scan].exclude` and the default exclude globs. `scan --staged --all` is now a clap conflict instead of silently ignoring `--all`.

### Changed

- **Log output goes to stderr (#286).** `success`/`warn`/`info`/`hint`/`step` chrome now writes to stderr so stdout stays clean for pipes and redirects (`xv get X > file`, `xv ... | jq`); only data lands on stdout.
- **`run --include`/`--exclude` name matching (#286)** accepts either the original (user-facing) name shown by `xv list` or the backend name.
- `xv config show --resolved`, `xv context show`, and `xv context envs` now surface inline hints for the confusing env-profile vs vault-context vs global-config layers, including notes when active `.xv.toml` env fields override context/global fallbacks or inherit from them (#283).

### Fixed

- **`xv run` no longer exits 0 without running the child (#286).** An explicit `--group`/`--include` filter that matches nothing now errors; an empty vault (or `--exclude` removing everything) warns but still runs the command.
- **Partial failures now exit non-zero (#286)** for bulk `set`, `gen --save`, `vault import`, and `env push`, instead of reporting success; bulk `set` also persists `--tag`, and `vault import` no longer prints an `[ok]` summary on partial failure.
- **Destructive ops prompt or refuse (#286).** Trait-path `delete`/group-delete/`rollback`/`purge`/vault-delete now prompt on a TTY and refuse with a non-zero exit in non-interactive sessions instead of silently no-opping.

---

## v0.15.0 — Opaque local filenames (2026-06-22)

Adds opt-in opaque on-disk filenames for the local backend and includes a
small vault-create UX fix.

### Added

- **Opt-in opaque on-disk filenames for the local backend (#276).** Setting `[local].opaque_filenames = true` stores active secrets, version archives, and trash entries under keyed-hash stems instead of reversible secret-name filenames, with an age-encrypted index for name lookup. Existing stores remain unchanged until `xv local migrate` runs; `xv local migrate --dry-run` prints the rename plan first. See [`docs/FEATURES.md`](./docs/FEATURES.md#local-backend-maintenance) and the retained design plan in [`docs/plans/2026-06-19-local-secret-filename-opaquing.md`](./docs/plans/2026-06-19-local-secret-filename-opaquing.md).

### Fixed

- **Vault-create follow-up hint now suggests the real context command (#275).** After creating a vault, the CLI now points users to `xv cx use <name>` instead of the nonexistent `xv use <name>`.

## v0.14.0 — `gen`/`set` parity, `config edit`, and reliability fixes (2026-06-20)

Makes `xv gen --save` a complete replacement for `xv set`, adds an `xv config edit`
convenience command, and lands a batch of reliability/security hardening fixes
across the secret, cache, scan, auth, and config paths.

### Added

- **`xv gen --save` now carries full write-time metadata, matching `xv set` (#273).** A shared `SecretWriteArgs` clap struct (`--group` (repeatable), `--note`, `--folder`, `--expires`, `--not-before`) is flattened into both `set` and `gen`, so the two commands expose an identical metadata surface and cannot drift. Previously `gen --save` dropped all metadata and routed only through the Azure-only path; it now builds the same `SecretRequest` and goes through the same backend trait path as `set` (local/aws/azure), with a legacy Azure fallback when no backend registry is present. As the symmetric bonus, **`xv set` gains `--group`**, closing the create-time group gap (groups previously required a follow-up `xv update`). `gen` rejects metadata flags passed without `--save`.
- **`xv config edit` (#272)** — opens the config file in your editor, resolving `$VISUAL` → `$EDITOR` → a platform default (`nano` on Unix, `notepad` on Windows). Editor strings with arguments (e.g. `code --wait`) are supported. A missing config file is seeded with a valid serialized default (never an empty file, which would fail the next load); an existing config is never clobbered.

### Changed

- **`list_secrets` fetches per-secret details with bounded concurrency (#269)** — large vaults list materially faster while keeping a cap on in-flight requests.
- **`xv version` shows the Git ref (tag or branch) instead of `unknown` on release builds (#263).**
- **Transitive dependencies refreshed to clear Dependabot alerts (#271).**
- **Backend capability reference docs refreshed (#262); opaque-on-disk-filename design documented for the local backend (#268).**

### Fixed

- **`xv run` output masking buffer is now bounded (#270)** — the stream-masking buffer can no longer grow without limit on high-volume child output.
- **Config context files are written via the private 0600 writer (#266)** — context state lands with owner-only permissions, matching the rest of the config writes.
- **Azure auth hardening (#267)** — the `az` helper subprocess is time-bounded and JWT claim shapes are validated before use.
- **Scanner memory is bounded and fails loud on unscanned files (#265)** — the secret scanner no longer risks unbounded memory and surfaces files it could not scan instead of silently skipping them.
- **Cache lock acquisition closes a TOCTOU via atomic `create_new` (#264).**

---

## v0.13.0 — Local metadata encryption + UX & docs polish (2026-06-15)

Adds opt-in local-backend metadata encryption (ROADMAP P2) and closes the
entire UX P2 lane and P3-1..4 from `docs/UX-REVIEW.md` (2026-05-16
AWS-backend baseline).

### Added

- **Opt-in local-backend metadata encryption at rest (ROADMAP P2).** A new `encrypt_metadata` key under `[local]` (default `false`, fully backward-compatible) makes the local backend age-encrypt each secret's `.meta.json` — note, tags, folder, expiry, content-type — to the same recipients as the secret value, instead of storing it as plaintext JSON. Reads auto-detect ciphertext vs plaintext via the age header, so stores can mix both formats freely (e.g. mid-migration). A new `xv local encrypt-metadata [--dry-run]` command walks every vault (including archived `.versions/` and `.trash/`) and re-encrypts existing plaintext metadata in place, atomically and idempotently. `xv init` now warns that metadata and secret *names* are plaintext by default and points at the flag + command. **Known limitation:** secret *names* remain visible as on-disk filenames regardless of this setting (filename opaquing is tracked separately).

### Changed

- **crosstache no longer frames itself as Azure-only (§P2-1, §P2-5, #254)** — the README hero and `xv --help` intro mention AWS and local backends alongside Azure. Backend-unsupported operations are framed in neutral language and surface the active backend in the error instead of assuming Azure.
- **AWS-inherited flags hidden where they do nothing (§P2-2, #255)** — `--aws-profile` and `--region` are hidden from the default help of commands that ignore them, so they no longer appear on Azure/local-only operations.
- **`context envs` shows the effective profile (§P2-4) + config naming note (§P2-3, #256)** — the listing now displays the resolved backend (with an `(inherited)` marker for envs that set no `backend` of their own) and an `Effective (<env>): backend=… vault=…` summary that mirrors full `resolve_effective_backend` / vault-resolution precedence. A note disambiguates the overlapping `.xv.toml` vs `xv.conf` backend fields.

### Fixed

- **TUI clippy lint debt cleared (§P3-4, #257)** — `cargo clippy --features tui -- -D warnings` is clean (collapsible-if, `.clone()` on `Copy` `ListState`, manual `div_ceil`, non-binding `let` on futures).
- **`xv env create --group` disambiguated (§P3-1..4, #258)** — help text now explains that `--group` (secret-group filter) and `--resource-group` (Azure resource group) are distinct concepts; the minimal help template advertises `--show-options` for discoverability of hidden globals.

---

## v0.12.0 — AWS capability matrix completion (2026-06-12)

Closes all four P1 AWS capability gaps deferred since v0.10.0 (#248–#251).
AWS is an opt-in Cargo feature (`aws`); these paths are absent from the
default build.

### Added

- **Release binaries now ship AWS support.** The release workflow builds with `--features tui,aws` (was `tui` only), so the pre-built downloads on the Releases page support Azure, local, AND AWS backends out of the box — matching the phase-3 design intent ("distribution-channel binaries ship with `--features aws`"). Building from source still defaults to lean (no AWS) unless you pass the flag. Without this fix, the four AWS features below were unreachable to anyone using a downloaded binary.
- **`xv audit` on AWS via CloudTrail (#249)** — reads recent Secrets Manager events through CloudTrail `LookupEvents` (event-source filter + vault-prefix match), mirroring the Azure Activity Log output shapes (table/json, time-range/limit flags). `CreateSecret` events are resolved from `requestParameters.name` as well as `secretId`. Missing `cloudtrail:LookupEvents` permission yields an actionable error. AWS backend now reports `has_audit: true`. Adds optional dep `aws-sdk-cloudtrail`.
- **Native rotation on AWS (#250)** — new `xv rotate --native` flag invokes Secrets Manager `RotateSecret` (the secret's configured rotation Lambda); rotation is asynchronous and the command says so. Clear errors for no-Lambda-configured (with `aws secretsmanager rotate-secret` setup hint), missing permissions, and non-AWS backends (capability message, including when the backend registry failed to initialize). Without `--native`, behavior is unchanged on all backends. AWS backend now reports `has_secret_rotation: true`.
- **S3 file storage on AWS (#251)** — `xv file` upload/download/list/delete/info now work on the AWS backend, backed by S3 with vault-prefixed keys (`<vault>/files/<name>`) for per-vault isolation matching the local backend. Streaming both directions: multipart upload above the chunk threshold (reuses `chunk_size_mb`), streamed download with the same 5 GiB guard as the Azure path; containment via shared `safe_join` (no traversal/absolute-key escapes). Bucket comes from a new `aws_s3_bucket` config field / `--bucket` flag; unconfigured → clear setup hint; no bucket auto-creation. Truncated download bodies are rejected rather than reported as a full-size success. AWS backend now reports `has_file_storage: true`. Adds optional dep `aws-sdk-s3`.

### Changed

- **`xv share` on AWS returns a capability-aware hint (#248)** — share/grant/revoke/list operations on the AWS backend now exit 2 with a message naming the backend and giving a copyable `aws secretsmanager put-resource-policy` example, instead of failing opaquely. The hint is returned even when the AWS backend registry failed to initialize. Local secret-share messages are byte-identical; the vault-share message was unified to the share-specific text.

### Known limitations

- The `has_audit` capability flag is `false` for Azure even though `xv audit` works there, because Azure audit uses a legacy Activity Log path that bypasses the capability trait (AWS dispatches through the trait correctly). Tracked in `ROADMAP.md` (P3). Harmless — the CLI tries the trait first, then falls through.
- `rustls-webpki 0.101.7` (RUSTSEC DoS via malformed CRL panic, GHSA high) remains pinned transitively by `rustls 0.21` inside `aws-smithy-http-client`. It enters the tree only under the `aws` feature. Release binaries ARE built with `--features tui,aws` (batteries-included distribution), so the crate is present in shipped artifacts — but it is unreachable in practice (the AWS SDK only contacts trusted AWS TLS endpoints, never processing attacker-controlled CRLs). Will clear when the AWS SDK drops rustls 0.21 upstream — same posture as the documented `rand 0.7.3` pin.

---

## v0.11.2 — P2 security-hardening completion (2026-06-11)

Closes out all four remaining P2 items from the 2026-05-09 GPT-5.5 code
review, plus byte-fidelity and data-loss fixes that had been soaking in
`Unreleased`.

### Fixed

- **Secret rename failures are now recoverable (P2)** — `xv update --rename` performs create-new-then-delete-old; when the delete of the old name fails, the command now returns a dedicated `RenameIncomplete` error (exit code 43, `xv-rename-incomplete`) that names both secrets and the vault, states that both copies still exist and no material was lost, preserves the underlying failure, and gives concrete recovery steps (`xv get <new>`, then `xv delete <old>` or retry). The new secret is deliberately never rolled back. (#242, ROADMAP P2)
- **Blob downloads now stream instead of buffering the whole blob (P2)** — `download_file_stream` uses the Azure SDK's chunked ranged-GET stream (chunk size reuses `chunk_size_mb`, clamped to ≥1 MB), holding at most ~one chunk in memory, with a 5 GiB max-download guard. (#243, ROADMAP P2)
- **Local file backend resolves the vault per operation (P2)** — `FileBackend` trait methods now take `vault` per call (mirroring `SecretBackend`), so local `xv file` operations target the active vault instead of the default vault captured at construction. Same-named files in different vaults stay isolated; traversal protection is enforced on every call. (#244, ROADMAP P2)
- **Azure deleted-secret listing, backup, and restore implemented (P2)** — `list_deleted_secrets` (with pagination), `backup_secret` (base64url blob decode), and `restore_secret_from_backup` now use real Key Vault REST API v7.4 calls instead of returning "not yet implemented" errors. (#245, ROADMAP P2)
- **Local backend: soft-delete trash collisions (P2, data loss)** — Trash entries are now keyed by `<encoded_name>@<unix-millis>` instead of name alone, so `xv delete <X>`, recreate, delete again no longer clobbers previously trashed material. A same-name+same-timestamp collision is rejected with a clear error instead of overwriting. Recover restores the most recent trash entry; legacy un-suffixed trash entries from older versions remain listable and recoverable; purge removes all trash snapshots for a name.
- **Env export escaping** — `xv vault export --format env` now emits POSIX single-quoted values (`KEY='val'`, embedded single quotes escaped as `'\''`), so values containing newlines, `#`, `$`, quotes, spaces, or backslashes survive shell `source`/`eval` byte-for-byte. Secrets whose derived env name is not a valid shell identifier are skipped with a warning on stderr.
- **`--stdin` now preserves secret bytes exactly** (`xv set --stdin`, `xv update --stdin`): values read from stdin are stored byte-for-byte as piped — trailing newlines and leading/trailing whitespace are no longer stripped. Previously values were silently whitespace-trimmed, corrupting secrets where exact bytes matter (e.g. PEM keys, values whose consumers expect a trailing `\n`). Pass the new `--trim` flag (requires `--stdin`) to restore the old behavior of stripping leading/trailing whitespace. Empty stdin input is still rejected. (ROADMAP P3 — "`--stdin` trims whitespace")
- **Tri-state metadata updates (P3)** — `xv update` can now distinguish "leave unchanged" from "clear" for expiry, not-before, note, and folder. The internal update model uses `FieldUpdate<T> { Unchanged, Set(T), Clear }`; new `--clear-note` and `--clear-folder` flags join the existing `--clear-expires` / `--clear-not-before`, and setting + clearing the same field in one command is rejected. Applies across local, Azure, and AWS update paths. As part of this, the Azure update path no longer silently drops expiry/not-before when updating unrelated metadata (its underlying PUT now carries unchanged attributes forward).

### Dependencies

- `tar` bumped `0.4.45` → `0.4.46` — fixes PAX header desync (GHSA-3cv2-h65g-fgmm), clearing the high-severity Dependabot alert. (#228)

---

## v0.11.1 — Security fixes (2026-05-28 security review)

All 10 findings from `docs/security-review-2026-05-28.md` resolved in **#232**.

### Security

- **Critical** — xfunction: a vault without a `CreatedByID` tag is now refused (403) instead of proceeding to Owner/Key Vault Administrator role assignment.
- **High** — `xv upgrade` refuses to install a release that has no `.minisig` signature asset (previously warn-and-continue). All releases since v0.11.0 are signed in CI.
- **High** — `install.sh` / `install.ps1` abort on every checksum-verification failure path (missing/empty checksum file, no checksum utility, download failure).
- **High** — xfunction: storage RBAC discovery no longer falls back to *all* storage accounts in the resource group; accounts without an explicit `AssociatedVault` tag or naming-convention match are skipped.
- **High** — xfunction: `EXPECTED_AUDIENCE` and issuer configuration are required for JWT validation; tokens are never validated without audience+issuer checks. `setup-app-registration.ps1` now sets `EXPECTED_AUDIENCE`.
- **Medium** — Recursive blob download routes through `safe_join`, rejecting absolute blob names that previously escaped the output directory.
- **Medium** — `xv run` only resolves `xv://` references from parent environment variables when `--inherit-env` is active, closing an `env_clear` isolation bypass.
- **Medium** — Local age key files are opened with `O_NOFOLLOW`, group/world-accessible key files are rejected (with a `chmod 600` hint), the stat→read TOCTOU window is closed, and key material is read into a `Zeroizing` buffer.
- **Medium** — `setup-app-registration.ps1` no longer prints the client secret to the console.
- **Low** — Table and plain output visibly escape control characters (C0/DEL/C1) in untrusted content (blob names, metadata, tags); JSON/YAML/CSV output remains raw for scripts.

### Breaking / behavioral notes

- Pre-existing local-backend key files with permissions looser than 0600 are now rejected at load; run `chmod 600 <key-file>` to fix.
- xfunction deployments must set `EXPECTED_AUDIENCE`; untagged vaults no longer receive role assignments.

---

## v0.11.0 — Security hardening + dependency triage

### Security (P2 items from GPT-5.5 review)

- **#222** — Local file metadata now written with 0600 permissions via `write_private`; permissions asserted in tests.
- **#223** — Traversal guard added to single-file blob download; multi-download `--output` collision check enforced via shared containment helper.
- **#224** — Scanner `SecretRef.value` wrapped in `Zeroizing<String>` end-to-end; engine dropped promptly after use.
- **#225** — Every segment in ARM resource ID construction is URL-encoded; wrong-path addressing via malformed names is prevented.

### Dependencies

- `ratatui` bumped `0.28` → `0.30`; transitively updates `lru` `0.12.5` → `0.16.4` (clears Dependabot alert #2).
- 4 remaining Dependabot alerts triaged: #17, #8, #9 are dev-only (`aws-sdk-secretsmanager` `test-util` feature, not in shipped binary); #11 (`rand 0.7.3`) is pinned by `azure_core 0.21` and not exploitable without a custom logger.

---

## v0.10.0 — AWS Secrets Manager backend

_Release candidate: v0.10.0-rc.1 (rc soak in progress)_

### Added

- **AWS Secrets Manager backend** (`xv --backend aws ...`) behind the `aws` Cargo feature flag.
  - `[aws]` config block: `region`, `profile`, `endpoint_url`, `default_vault`.
  - `[named_backends.*]` map for multi-region setups (e.g., `aws-east`, `aws-west`).
  - Prefix-based virtual vaults via `<vault>/.xv-vault` marker secrets.
  - Full secrets CRUD: create, get, list, update, delete (soft), purge (force), restore.
  - Version history: list versions, get by version ID, rollback.
  - Group, folder, note, expiry, content-type — all preserved via tags.
- **`--aws-profile` and `--region` global CLI flags** (override config file per invocation).
- **`xv init` wizard** now offers "AWS Secrets Manager" as a backend option.
- **`xv migrate` hardening** (marquee feature):
  - `--on-conflict skip|replace|fail` — conflict resolution strategy (replaces deprecated `--overwrite`).
  - `--concurrency N` — bounded parallel transfers (default 8).
  - `--force-replace` — ignore idempotency tags, always overwrite.
  - Pre-flight diff and summary table before any writes.
  - Idempotent re-runs via `xv:migrated_from` / `xv:migrated_at` tags.
  - Exponential backoff on throttling (`BackendError::RateLimited`).
- **Documentation**: `docs/migration.md` — full cross-cloud migration guide.
- **Test coverage**: hermetic mock tests (aws-smithy-mocks), LocalStack-gated integration tests, migration round-trip tests, CLI dry-run smoke test.

### Changed

- `--overwrite` on `xv migrate` is deprecated; use `--on-conflict replace` instead. The flag still works with a deprecation warning for one minor version.

### Capabilities matrix (AWS backend)

| Feature | Status |
|---|---|
| Secrets CRUD | ✅ |
| Versioning (list, get, rollback) | ✅ |
| Soft-delete + restore + purge | ✅ |
| Vaults (prefix-based) | ✅ |
| Groups, folders, notes, expiry | ✅ (via tags) |
| `xv share` (RBAC) | ❌ Use AWS IAM directly |
| `xv audit` | ❌ Use AWS CloudTrail |
| Native rotation | ❌ `xv rotate` writes new versions |
| File storage (S3) | ❌ Deferred to future phase |

### Performance notes

- Binary size with `--features aws`: ~19 MB (stripped, LTO). Default binary (no AWS): ~11 MB.
- 100-secret cross-cloud migration completes in <60 s on a warm credential cache at `--concurrency 8`.

### Upgrade notes

- Existing Azure or local users: **no action required**. Default behavior is unchanged.
- New AWS users: run `xv init` and pick "AWS Secrets Manager", or set `backend = "aws"` in `~/.config/xv/xv.conf`.
