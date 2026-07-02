# List Command P3 — Deleted Listing, Groups, Sort, Folder Ergonomics Design

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-02 | **Author:** Claude + Scott
> P3 tier of the 2026-07-01 list-command UX review (P0 = PR #289, P1 = PR #290, P2 Phase A = PR #291, Phase B = the `list-p3` branch base). Executed autonomously under Scott's standing "recommended options, don't stop" instruction; the batched up-front decisions are recorded below.

---

## Problem

Phases 0–2 unified flags, wording, and rendering, but the listing surface still has functional holes and inherited defects:

1. **No way to see soft-deleted secrets.** All three backends can enumerate them — `SecretBackend::list_deleted_secrets` exists on the trait (`src/backend/secret.rs:110`) and is implemented by Azure (`src/backend/azure/secrets.rs:244` → REST `GET {vault}/deletedsecrets`, `src/secret/manager.rs:1251`), local (`src/backend/local/secrets.rs:1793`, trash scan), and AWS (`src/backend/aws/secrets.rs:849`, `ListSecrets` + `include_planned_deletion`) — but no CLI command calls it; `xv restore` restores blind.
2. **No group overview.** Groups exist only as a `--group` filter; nothing lists which groups exist or how many secrets each holds.
3. **`xv ls` has exactly one sort order** (name ascending, baked into `scope_secrets`, `src/cli/ls_view.rs:57-58`); "what changed recently" requires piping JSON through `jq`.
4. **`context envs` duplicates `env list`** (`execute_context_envs` at `src/cli/config_ops.rs:1218` already delegates) but is still visible in help with no deprecation signal.
5. **`find` cannot scope by folder** even though `xv ls prod` can and `CandidateItem` already carries a `folder` field (`src/utils/fuzzy.rs:14`).
6. **Tab completion knows secret names but not folders** — the `FOLDER` positional of `xv ls` and the new `find --folder` have nothing to complete from, although the cached summaries contain every folder tag (`__complete-secrets` pattern: `src/main.rs:40-48`, `execute_complete_secrets` at `src/cli/secret_ops.rs:2110`).
7. **`xv ls -r` flattens folders into ambiguity**: two secrets named `db-pass` in `prod/` and `dev/` render as two identical `db-pass` cells.
8. **Display-width math counts chars, not columns.** `render_grid`/`render_long`/`truncate_note` (`src/cli/ls_view.rs:123,180,223,229,235`) and the note-wrap helpers (`src/cli/secret_ops.rs:322,331,346`) use `chars().count()`, so CJK/full-width names misalign every grid and long listing.
9. **Inherited defects:** (a) `xv parse` prints its table twice — `SecretManager::parse_connection_string` prints (`src/secret/manager.rs:~1973-1976`) and the CLI arm renders again (`src/cli/secret_ops.rs:3599-3624`); (b) `pagination_footer_text` still emits the abolished `"{noun}(s)"` style via `Page::human_summary` (`src/utils/pagination.rs:63,69`); (c) `xv cache refresh --key vaults` dumps the whole vault list as JSON to stdout because `refresh_vault_list` (`src/cli/config_ops.rs:884`) calls the printing `list_vaults_formatted` (`src/vault/manager.rs:116-139`).

## Decisions (adopted from the up-front batch, 2026-07-02)

1. **`xv ls --deleted` lists soft-deleted secrets**, capability-gated per backend via `BackendCapabilities.has_soft_delete` (all three backends declare `true`: `src/backend/azure/mod.rs:197`, `src/backend/local/mod.rs:222`, `src/backend/aws/mod.rs:94`) with the same error shape as `xv restore`'s gate (`src/cli/secret_ops.rs:1277-1286`); a `BackendError::Unsupported` from the trait call maps to the same capability message. Output: name + deleted date + scheduled-purge date **where available** (see per-backend reality below), through the shared `TableFormatter` (machine formats = row array). `--deleted` conflicts with the positional `FOLDER` arg and `-r` (clap `conflicts_with`); grid/long render as usual.
2. **`xv group list`**: new top-level `group` subcommand with `list`; derives groups from the secret summaries' comma-separated `groups` values (same parsing as `secret_summary_matches_group`, `src/cli/secret_ops.rs:223-232`); rows `{ Group, Secrets }` (member count); full format support via `TableFormatter`; empty-state/count via the `list_output` helpers.
3. **`xv ls --sort name|updated`** (clap `value_enum`, default `name`): applies before rendering in every ls output mode including machine formats; `updated` sorts descending (newest first).
4. **`context envs` is deprecated**: hidden from help (`#[command(hide = true)]`), warns `context envs is deprecated; use env list` on stderr, delegates unchanged.
5. **`find --folder <path>`** filters candidates by folder (exact-or-prefix with segment boundary, the same rule as `ls_view::scope_secrets` — the logic is extracted into a shared helper) before scoring.
6. **Folder completion**: hidden `__complete-folders` command mirroring the `__complete-secrets` pattern (pre-clap intercept in `src/main.rs`, cache-only executor in `secret_ops.rs`) that emits distinct folder paths derived from the cached summaries; wired into the shell-completion story exactly the way secret names are.
7. **Folder-qualified `ls -r` names**: recursive grid/long/names-only display shows `folder/name` qualified names relative to the listing root (root-level secrets unqualified).
8. **CJK-safe widths**: add the `unicode-width` crate; replace `chars().count()` display-width math in `ls_view.rs` (grid + long + note truncation) and the `xv ls` table note-wrap helpers in `secret_ops.rs`. `tabled`'s own width machinery is NOT touched — it already handles this.
9. **Inherited fixes ship in this tier**: (a) `xv parse` double-print — the manager stops printing (managers return data; the CLI renders; `execute_secret_parse` at `src/cli/secret_ops.rs:3589` is the only caller); (b) `pagination_footer_text` becomes plural-aware (takes singular + plural noun forms, like `count_label`); all 8 call sites updated; (c) `refresh_vault_list` fetches + caches silently via `vault_ops().list_vaults(..)` instead of the printing `list_vaults_formatted`.

### Resolved ambiguities (recorded, not re-litigated)

- **`--deleted` also conflicts with `--group`, `--all`, `--expiring`, `--expired`** (clap `conflicts_with`): those flags operate on live secrets (expiry filters even call `get_secret`, which 404s on deleted names). The settled decision named only `FOLDER`/`-r`; the extra conflicts are the same principle applied to flags that cannot mean anything in deleted mode.
- **`--sort` in `--deleted` mode:** `name` (default) sorts by name ascending; `updated` sorts by *deleted* date descending (it is the "when did this change state" axis of that view).
- **`--deleted` bypasses the cache** — the `SecretsList` cache holds live secrets only, and trash freshness matters right after a delete.
- **Missing dates render as empty cells** in `TableFormatter` rows (matching the `SecretListDisplayRow` empty-string convention and the hide-empty-columns machinery) and `-` in the borderless long view (matching folder placeholder rows).
- **`--names-only` without `-r` keeps its current flat, unqualified output** (shipped pipe shape); qualification applies only when `-r` is explicitly passed. The `-r --names-only` / `-r` grid/long change is a human/pipe display change, changelog-noted — machine formats (`--format json|yaml|csv`) keep the `SecretSummary`/display-row shapes, which already carry a Folder column.
- **`find --folder` composes with `--all-vaults`** (folder tags are vault-agnostic; the filter applies to the combined candidate list).
- **`__complete-folders` emits ancestor prefixes too** (`prod/db` yields `prod` and `prod/db`), sorted, one per line — completing `xv ls pr<Tab>` must offer intermediate folders that exist only as prefixes.
- **`xv group list` takes `--no-cache`** mirroring `xv ls` (it reads the same `CacheKey::SecretsList` dataset).

## Design

### 1. `xv ls --deleted`

**Per-backend deleted-listing reality** (this determines the data shape):

| Backend | Enumeration | Deleted date | Scheduled purge date |
|---|---|---|---|
| Azure | `GET {vault}/deletedsecrets` api-version 7.4, `nextLink`-paginated (`SecretManager::list_deleted_secrets`, `src/secret/manager.rs:1251-1324`) | ✅ `deletedDate` (epoch) per item — currently **discarded** by `parse_deleted_secret_summary` (`src/secret/manager.rs:449`) | ✅ `scheduledPurgeDate` (epoch) per item — currently discarded |
| Local | `.trash/` scan (`src/backend/local/secrets.rs:1793-1824`) | ✅ recoverable from the trash-entry dir name `{stem}@{deleted_at_millis}` (`trash_entry_dir`, `src/backend/local/secrets.rs:120-126`; also `.deleted.json`'s `deleted_at`, written at `:1099`) — currently discarded | ❌ none — trash persists until manual `xv purge` |
| AWS | `ListSecrets` + `include_planned_deletion(true)`, filtered to `deleted_date().is_some()` (`src/backend/aws/secrets.rs:849-906`) | ✅ `SecretListEntry.deleted_date` — currently **discarded** (`updated_on` set to `""`) | ❌ not in the list response (purge time = DeletedDate + per-secret recovery window, which `ListSecrets` doesn't return) |

**Data shape.** The trait's `Vec<SecretSummary>` return can't carry the dates, so `list_deleted_secrets` changes return type across the stack to a new dedicated type (internal refactor; the CLI never exposed the old shape):

```rust
// src/secret/manager.rs, near SecretSummary
pub struct DeletedSecretSummary {
    pub name: String,
    pub original_name: String,
    pub deleted_on: Option<String>,          // formatted timestamp, None when unknown
    pub scheduled_purge_on: Option<String>,  // None when the backend has no purge schedule
}
```

Touched: `SecretBackend::list_deleted_secrets` (`src/backend/secret.rs:110`), `SecretOperations::list_deleted_secrets` (`src/secret/manager.rs:236`) + Azure impl + `parse_deleted_secret_summary` (now reads `deletedDate`/`scheduledPurgeDate`), the Azure adapter (`src/backend/azure/secrets.rs:244`), local (parses the `@millis` dir suffix), AWS (keeps `entry.deleted_date()`), and the mock at `src/secret/manager.rs:2489`. Local-backend tests calling `list_deleted_secrets` update mechanically to the new fields.

**CLI.** New `--deleted` flag on `Commands::List` with the conflicts listed above. Dispatch routes to a new `execute_deleted_secret_list` (separate from the live-list path — different data, no cache, no folder scoping):

- Gate: registry active backend `capabilities().has_soft_delete`, else `InvalidArgument("The {name} backend does not support listing deleted secrets (soft-delete not available).")`; `BackendError::Unsupported` from the call maps to the same message.
- Sort per `--sort` (see §3 resolution above), then paginate with `paginate_slice`.
- Render:
  - `--names-only`: one display name per line.
  - Machine formats + explicit `--format table|plain|raw`: `TableFormatter` over `DeletedSecretListRow { Name, Deleted, Purge Scheduled }` (Tabled + Serialize, empty strings for unavailable dates) — machine formats get the row array, valid-empty via `format_table(&[])`'s existing empty branch (`src/utils/format.rs:205-220`).
  - Default (no explicit format): the usual ls-style views — grid of names, or `-l` → new borderless `render_deleted_long` (`NAME  DELETED  PURGE SCHEDULED`, `-` placeholders) in `ls_view.rs`, built on the same width helpers as `render_long`.
- Human count/empty: `count_label(n, total, "deleted secret", "deleted secrets", Some("vault 'X'"), paginated)` / `empty_state_message("deleted secrets", Some("vault 'X'"))`.

### 2. `xv group list`

New top-level `Commands::Group { command: GroupCommands }` (subcommand enum with `List { no_cache }`, modeled on `Commands::Env`). Executor in `secret_ops.rs` reuses the ls trait path's fetch-or-cache flow (`trait_secret_cache_key`, `CacheKey::SecretsList`), then folds summaries into a `BTreeMap<String, usize>` by splitting each `groups` value on `','` and trimming (empty segments skipped — identical tokenization to `secret_summary_matches_group`). Rows:

```rust
struct GroupListRow { group: String /* "Group" */, secrets: usize /* "Secrets" */ }
```

Rendered through `TableFormatter` with the global `--format`/`--columns`/`--template`/`--no-color`; human output adds `count_label(n, n, "group", "groups", Some("vault 'X'"), false)`; empty state via `empty_state_message("groups", Some("vault 'X'"))` on human formats, valid-empty rows on machine formats.

### 3. `xv ls --sort name|updated`

`LsSort` value-enum (`Name` default, `Updated`) on `Commands::List`, threaded through `execute_secret_list_direct` into `display_cached_secret_list` (`src/cli/secret_ops.rs:357`). `scope_secrets` keeps producing name-sorted output (its contract); when `Updated` is requested, a new `ls_view::sort_secrets_by_updated_desc` re-sorts `scoped.secrets` and `scoped.subtree` (descending `updated_on`, display-name ascending tiebreak — the backend timestamps are ISO-shaped so lexicographic order is chronological). Because every output mode (names-only, machine row array, explicit table, grid, long) renders from those two vectors, one sort site covers all modes. Folder cells in the grid stay alphabetical (folders have no update time).

### 4. `context envs` deprecation

`ContextCommands::Envs` (`src/cli/commands.rs:1284`) gains `#[command(hide = true)]`; `execute_context_envs` (`src/cli/config_ops.rs:1218`) emits `output::warn("context envs is deprecated; use env list")` before its existing delegation to `execute_env_list`. No behavior change otherwise; removal is a future release decision.

### 5. `find --folder <path>`

The segment-boundary scope rule currently inlined in `scope_secrets` (`src/cli/ls_view.rs:39-47`) is extracted into `ls_view::folder_in_scope(folder: &str, path: &str) -> bool` (exact match, or `folder` starts with `path + "/"`); `scope_secrets` is refactored onto it. `Commands::Find` gains `#[arg(long, value_name = "PATH")] folder: Option<String>`; the executor trims trailing `/`, validates via `validate_folder_path`, and filters `items` (the `CandidateItem` vec) with `folder_in_scope` **before** `score_matches` — in both the trait path (`src/cli/secret_ops.rs:~1880-1924`) and the legacy Azure path (`:~2013-2082`). `--names-only`, `--limit`, `--min-score`, and `--all-vaults` compose unchanged.

### 6. Folder completion (`__complete-folders`)

Mirror of the secret-name completion: `src/main.rs` intercepts `args[1] == "__complete-folders"` before clap (exactly like `__complete-secrets` at `src/main.rs:40-48`) and calls a new `execute_complete_folders(config)` in `secret_ops.rs` — cache-only (`CacheKey::SecretsList`), silent on cold cache, emitting every distinct folder path *and its ancestor prefixes* from the cached summaries, `BTreeSet`-sorted, one per line. Never touches a backend on a Tab press.

### 7. Folder-qualified `ls -r` names

New `ls_view::qualified_display_name(s: &SecretSummary, root: &str) -> String`: the secret's folder tag relative to the listing root (root itself stripped with the same segment-boundary rule) joined to `display_name` with `/`; secrets directly at the root stay unqualified. `display_cached_secret_list` applies it when `recursive` is set: names-only prints qualified names, and the grid/long entries are built from summaries whose display name has been qualified (so `xv ls -r` shows `prod/db-pass`, `xv ls prod -r` shows `db/db-pass`). Non-recursive output and machine formats are untouched.

### 8. CJK-safe display widths

Add `unicode-width` (workspace dependency). `ls_view.rs` gains `display_width(&str) -> usize` (`UnicodeWidthStr::width`) and a `pad_to(s, w)` helper (manual space padding — `format!("{:<w$}")` pads by char count and is abandoned where widths matter). Converted sites:

- `render_grid` label lengths (`src/cli/ls_view.rs:123`),
- `render_long` column widths and padding (`:223,229,235` + the `{:<name_w$}` format strings),
- `truncate_note` (`:180` — truncation accumulates per-char `UnicodeWidthChar` widths against the max column budget),
- the `xv ls` table note-wrap helpers `wrap_paragraph_to_width`/`push_wrapped_word` (`src/cli/secret_ops.rs:322,331,346`),
- the new `render_deleted_long` (§1) is born width-aware.

Grep confirms no other list-rendering site measures display width by chars; `tabled` (used by `TableFormatter`) has its own width handling and is not modified.

### 9. Inherited fixes

- **`xv parse` double-print:** `SecretManager::parse_connection_string` (`src/secret/manager.rs:1956`) drops its `TableFormatter` + `println!` block and just returns the components; `execute_secret_parse` (`src/cli/secret_ops.rs:3589`), its only caller (verified by grep), keeps rendering. One table per invocation, and `parse --fmt json` stops leaking a table.
- **Plural-aware pagination footer:** `Page::human_summary` and `pagination_footer_text` (`src/utils/pagination.rs:57,142`) take `noun_singular` + `noun_plural` (plural chosen from `total_items`, zero is plural — same rule as `count_label`); the dead `print_pagination_footer` (`:134`, `#[allow(dead_code)]`, no callers) is deleted. All 8 call sites updated: `file_ops.rs:700,752` and `file_ops_aws.rs:1118` (`item/items`), `vault_ops.rs:413` (`vault/vaults`), `vault_ops.rs:1290` + `secret_ops.rs:3762` (`assignment/assignments`), `secret_ops.rs:466` (`secret/secrets`), `secret_ops.rs:523` (`entry/entries`).
- **Silent vault-cache refresh:** `refresh_vault_list` (`src/cli/config_ops.rs:884`) calls `vault_manager.vault_ops().list_vaults(Some(&config.subscription_id), None)` (`src/vault/manager.rs:41` accessor) instead of `list_vaults_formatted`, then caches the same `Vec<VaultSummary>`. Nothing reaches stdout. (The background-spawned refresh already nulls stdio — `src/cache/refresh.rs:43-48` — the bug bit manual `xv cache refresh --key vaults` runs.)

## Out of scope

- `xv parse`'s bespoke `--fmt json|table` string flag (it predates the global `--format`; normalizing it is a separate breaking change).
- Any restore/purge UX changes (interactive pickers over the new deleted listing, purge-all, etc.).
- Group management verbs (`group add/rename/rm`) — `group` ships with `list` only.
- Completion-script generation changes (`xv completion` output stays vanilla clap; `__complete-folders`, like `__complete-secrets`, is for user shell wiring).
- `vault share list --fmt` removal (still release-gated), TUI changes, AWS `file sync`.

## Testing

- Unit: `DeletedSecretSummary` parsing (Azure `deletedDate`/`scheduledPurgeDate` epochs, local `@millis` trash stems, AWS entry mapping where testable), `folder_in_scope` boundary cases (`prod` vs `production`), `qualified_display_name` (root/nested/relative-root), `sort_secrets_by_updated_desc` (desc + name tiebreak), group derivation (split/trim/dedupe counts), plural `human_summary`, `display_width`/`pad_to`/CJK grid + long alignment + note wrap, `render_deleted_long` snapshot.
- Behavioral (local backend, temp config): delete → `xv ls --deleted` shows name + deleted date (purge column empty) in table and grid; `--deleted --format json | jq type` = array (empty vault ⇒ `[]`); `xv ls --deleted prod` and `--deleted -r` are clap errors; `xv group list --format csv` headers `Group,Secrets`; `xv ls --sort updated` newest-first in grid and JSON; `xv ls -r` shows `folder/name`; `xv find x --folder prod` excludes `production`; `xv __complete-folders` emits `prod` and `prod/db`; `xv context envs` warns on stderr and still lists; `xv parse "a=b;c=d"` prints exactly one table; `xv cache refresh --key vaults` prints nothing to stdout.
- Gates: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test --lib`, full `cargo test`.
- Safety: implementers never run `xv init` or write to `~/.config/xv/`; local-backend tests use `XV_BACKEND=local` with a temp `XDG_CONFIG_HOME`; any e2e vault fixtures are deleted **and purged** even on failure.
