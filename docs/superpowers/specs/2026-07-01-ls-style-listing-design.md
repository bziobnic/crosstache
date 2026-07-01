# ls-Style Folder-Aware `xv ls` Design (P1)

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-01 | **Author:** Claude + Scott
> Tier P1 of the 2026-07-01 list-command UX review. Builds on the merged P0 fixes (PR #289). The P2 tier (format/pager/count unification across all list commands) is separate.

---

## Problem

`xv ls` renders every secret in a flat table regardless of folder organization, even though secrets carry a hierarchical `folder` tag (paths like `prod/db`, validated by `validate_folder_path` in `src/utils/helpers.rs:160`) set via `xv set --folder` / `xv update --folder`. Two consequences:

1. **Folders are invisible in practice.** Everything appears at "root"; the folder tag is at best a table column. There is no way to list a folder's contents, see which folders exist, or navigate.
2. **The default output is a heavy box-drawn table** where a scan of names is what's usually wanted. The unix `ls` model — names in columns, a long mode on request — fits the mental model better.

`xv file list` already implements the target model for blobs: immediate children per level, directories listed first with a marker, `--recursive` to flatten. Secrets should feel the same.

## Decisions

Settled in-session (user AFK for the option prompts; the recommended options were adopted and are overridable at spec review):

- **Descend is positional:** `xv ls prod`, `xv ls prod/db`. A trailing slash is tolerated and stripped. No `--folder` flag alias for listing (deferred; `--folder` remains a write-side flag on `set`/`update`).
- **Default human output is an ls-style grid** (names in terminal-width columns, folders first with trailing `/`). `-l/--long` gives a borderless long listing. The existing rounded table remains available via explicit `--format table`.
- **Machine output shape is unchanged:** flat `SecretSummary` JSON/YAML/CSV exactly as today, scoped to the requested subtree; no folder pseudo-entries.
- **Empty folder / no matches exits 0** with the standard stderr empty-state (xv's existing convention for empty lists; scripts rely on empty-success), not ls's exit 2.

## Approach

**View-layer folder derivation.** Folders stay a tag; the CLI derives the virtual tree from the already-fetched summaries at render time. No backend-trait changes, no new API calls, identical behavior on Azure/AWS/local. (Rejected: first-class backend folder APIs — the data is already in the summary list; a minimal `--folder` filter without ls semantics — doesn't meet the goal.)

## Design

### 1. Command surface (`src/cli/commands.rs`, `List` variant)

```
xv ls [PATH] [-l] [-r] [existing flags]
```

- `PATH` (optional positional): folder path to list. Trailing `/` stripped, then validated with `validate_folder_path`; invalid paths error with that function's message.
- `-l, --long`: long listing (see §3b).
- `-r, --recursive`: flatten — list all secrets in scope (root scope = whole vault, i.e. today's behavior) instead of one level.
- All existing flags compose: `-g/--group`, `--all`, `--expiring`, `--expired`, `--no-cache`, `--page`/`--page-size`, `--pager`, `--names-only`. Filters apply to the secret set **first**; the folder view is derived from the filtered survivors (so `xv ls -g xfunction` shows only folders that contain a matching secret).
- `-l` conflicts with nothing today (`-g` is the only existing short flag on `list`).

### 2. Scoping model (pure logic)

For a requested path `P` (empty at root) and a secret with folder tag `F` (absent = root):

- **In scope (recursive):** `F == P` or `F` starts with `P + "/"` (root scope: everything).
- **Direct child secret:** `F == P` (root: no folder tag).
- **Child folder entry:** the next path segment of every in-scope `F` strictly longer than `P`. Distinct set, e.g. at root, tags `prod/db` and `prod/api` yield one entry `prod/`.

A path with no in-scope secrets prints `No secrets found in folder '<P>'` (plus the existing `--all` hint when disabled-only). The message is emitted in the human output (stdout, matching `xv ls`'s existing empty-state behavior), exit 0. Machine formats print valid-empty (`[]`) on stdout instead, matching the P0 `share list` pattern.

### 3. Rendering (human, resolved format = Table)

Mode selection: `-l` → long; explicit `--format table` → legacy table; otherwise → grid.

**3a. Grid (new default).** Column-major multi-column layout like `ls -C`:

- Entries: folders first (alphabetical, rendered `name/`), then secrets (alphabetical by `original_name`).
- Layout: fit to terminal width (crossterm `size()`, fallback 80), 2-space gutters, column-major fill; degrade to one per line when the longest entry exceeds the width.
- Color: folders cyan when color enabled (consistent with the existing cyan chrome); `NO_COLOR`/config `no_color` disables.
- Header `Vault: <name>` and footer `N secret(s), M folder(s)` (omit the folder clause when M = 0) keep the existing stdout placement of the current count line.

**3b. Long (`-l`).** Borderless, space-aligned columns: `NAME  UPDATED  GROUPS  NOTE` — name (folders as `name/`), date-only updated (reusing P0's `date_portion_for_display`), groups, first line of note (truncated with `…` past 60 chars). Folder rows show `-` in the non-name columns (mirroring `file list`'s `<DIR>` rows). Same header/footer as grid. No box-drawing characters.

**3c. Legacy table (explicit `--format table`).** The P0-fixed rounded table over the scoped recursive subtree (folder entries excluded; the Folder column carries each secret's folder), so the classic table always shows everything in scope and `-r` is a no-op for it, plus the existing count line. Distinguishing "explicit table" from auto-resolved requires a new `Config.format_explicit: bool` set at dispatch next to `runtime_output_format` (`src/cli/commands.rs` dispatch, currently resolving at ~line 1372).

### 4. Machine formats and `--names-only`

- JSON/YAML/CSV/template (piped auto included): the flat `SecretSummary` rows for the **recursive subtree** of `P` — `xv ls prod --format json` returns everything under `prod`, the useful scripting semantics, and `-r` is therefore a no-op for machine formats. No folder pseudo-entries.
- `--names-only`: unchanged contract (one secret name per line, no folders, no ANSI), scoped to the recursive subtree like machine formats.

### 5. Pagination and pager

`--page`/`--page-size` paginate the entry list (folders + secrets) in grid/long modes and secrets in table/machine modes, using the existing `Pagination`/`paginate_slice`/footer helpers. `--pager` wraps whichever rendering was produced, as today.

### 6. Structure

New module `src/cli/ls_view.rs` owning the pure view logic, unit-testable without a vault:

```rust
pub struct FolderEntry { pub name: String }            // display segment, no trailing slash
pub enum LsEntry { Folder(FolderEntry), Secret(SecretSummary) }
pub fn scope_secrets(secrets: Vec<SecretSummary>, path: &str, recursive: bool) -> ScopedList
    // ScopedList { folders: Vec<FolderEntry>, secrets: Vec<SecretSummary>, subtree: Vec<SecretSummary> }
pub fn render_grid(entries: &[LsEntry], width: usize, color: bool) -> String
pub fn render_long(entries: &[LsEntry], color: bool) -> String
```

`display_cached_secret_list` (`src/cli/secret_ops.rs`) gains the mode dispatch and calls into `ls_view`; `commands.rs` gains the positional arg + flags and `format_explicit`. `secret_ops.rs` receives wiring only — the rendering lives in the new module.

### 7. Interactions called out

- **Expiry filters** (`--expiring`/`--expired`) keep their current per-secret fetch behavior, applied before scoping.
- **Cache:** scoping happens after the cached summary fetch; no cache-shape changes.
- **`xv find`** and the TUI are untouched (P2+ candidates for folder awareness).
- **Active env `folder` default** (write-side prefix from `.xv.toml`) does NOT implicitly scope `xv ls`; a note in `--help` text is sufficient. Deferred as a possible opt-in later.

## Testing

Unit (in `ls_view.rs`):

- `scope_secrets`: root with mixed tags (folders derived from first segments, root secrets separated); nested path (`prod` yields `db/` + direct secrets); deep path; trailing-slash input; no-folder-tag-only vault (zero folder entries); recursive flag; empty scope.
- `render_grid`: column fill at width 80 with known entries; single-column degrade; folder-first ordering; no trailing whitespace on lines.
- `render_long`: alignment; folder placeholder row; note truncation with `…`; date-only reuse.

Integration/manual (real vault):

- `xv ls` (TTY): grid, folders first with `/`, footer counts.
- `xv ls <folder>`, `xv ls <folder>/`, `xv ls <folder> -l`, `xv ls -r`, `xv ls -g <group>`.
- `xv ls --format json | jq length` — flat array, unchanged schema; `xv ls prod --format json` — subtree only.
- `xv ls --names-only`, `xv ls nosuchfolder` (stderr message, exit 0), `xv ls --format table` (legacy table).

Gates: `cargo fmt`, `cargo clippy --all-targets`, `cargo test --lib`.

## Out of scope

- Folder name completion in `xv completion`; TUI folder tree; `xv find --folder` (P2+).
- A `--folder` listing alias, folder pseudo-entries in machine output, and env-default listing scope (all deferred until demanded).
- P2 consistency work (format/pager flag unification, shared count/empty-state conventions across all list commands).
