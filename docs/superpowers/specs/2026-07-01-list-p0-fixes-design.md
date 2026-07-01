# List Command P0 Fixes Design

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-01 | **Author:** Claude + Scott
> Tier P0 of the 2026-07-01 list-command UX review. Later tiers (ls-style default output, folder-aware `xv ls`, format-flag unification) will get their own specs.

---

## Problem

A holistic UX review of the `xv` list commands found four defects that are broken *today*, independent of any redesign:

1. **The human table render is broken in practice.** Real `xv --format table ls` output on a typical terminal shows two columns (Folder, Groups) crushed to zero width with blank headers, and the `Updated` column hard-wrapped mid-token (`2026-05-17 01:19:00 UT` / `C`), so every row occupies three lines.
2. **The global `--columns` flag is a no-op.** It is defined at `src/cli/commands.rs:164-166` and advertised in `--help --show-options` ("Select specific columns for table output"), but nothing anywhere in the codebase reads it.
3. **`xv share list` ignores `--format` entirely.** The handler (`src/cli/secret_ops.rs:3839-3892`) hard-codes `OutputFormat::Table`, so `xv share list foo --format json` still prints a table — share audits cannot be scripted. Its sibling `xv vault share list` (`src/cli/vault_ops.rs:1173-1225`) honors format correctly. The empty-state message also goes to stdout via `println!`, unlike every other list command, which routes empties to stderr via `output::info`.
4. **`xv context list` ignores color settings.** `execute_context_list` (`src/cli/config_ops.rs:1063-1132`) passes a hard-coded `false` as `no_color` to `format_table` (line 1123), so `--no-color` and `NO_COLOR` have no effect on the table body.

## Decisions (settled with Scott, 2026-07-01)

- **`--columns` is removed**, not wired. It returns as a real feature in the later consistency-pass spec, once all list commands share one renderer. Rationale: since the flag never did anything, any script passing it is already broken; an "unexpected argument" error is more honest than a silent no-op.
- **Human tables show date-only** (`2026-05-17`) for `Updated`, not the full `2026-05-17 01:19:00 UTC` timestamp and not relative ages. Machine formats (JSON/YAML/CSV) keep the full timestamp.

## Design

### 1. Table renderer width behavior

Two changes land in the shared formatter so every `TableFormatter` consumer (secret list, vault list, history, vault share list) benefits; one is specific to the secrets list.

**1a. Hide all-empty columns (shared).** In `TableFormatter::format_as_table` (`src/utils/format.rs:169`), before styling, remove any column whose data cells are all empty (header text is ignored for the emptiness test). Use `tabled`'s column-disable or builder API. This fixes the blank-header columns and returns their width to columns with content.

- Applies to the human `Table` and `Plain` renders only. JSON, YAML, and CSV keep the full, stable field set — piped output schemas do not change based on data.
- A table where *every* column is empty is impossible in practice (Name is always populated); if it occurs, render as today rather than special-casing.

**1b. Priority-based wrapping (shared).** Replace the blanket `Width::wrap(width)` (`src/utils/format.rs:186-189`) with `tabled` priority wrapping (`PriorityMax`): when the terminal is too narrow, the widest column shrinks first. In practice this targets Note and leaves the 10-char date column intact. This applies only to `format_as_table`; `format_as_plain` performs no width wrapping today and gains none.

**1c. Date-only in the secrets table (per-command).** In `format_secret_list_rows_for_human` (`src/cli/secret_ops.rs:293-310`), render `updated_on` as its date portion only. Implementation: parse/split the formatted `YYYY-MM-DD HH:MM:SS UTC` string and keep the leading `YYYY-MM-DD` token; if the value does not match that shape, fall back to the raw string unmodified (never error, never emit an empty cell for a non-empty timestamp). The machine-format path continues to serialize the raw `SecretSummary` untouched.

The existing 40-char Note pre-wrap (`SECRET_LIST_NOTE_WRAP_WIDTH`, `src/cli/secret_ops.rs:277`) stays as-is.

### 2. Remove the global `--columns` flag

Delete the `columns` field from the `Cli` struct (`src/cli/commands.rs:164-166`) and any dead plumbing that copies it into `Config`. `--help --show-options` stops advertising it. Passing `--columns` after this change produces clap's standard "unexpected argument" error.

### 3. `xv share list` honors `--format`

Rework the `ShareCommands::List` arm (`src/cli/secret_ops.rs:3839-3892`) to mirror `xv vault share list`:

- Construct the `TableFormatter` from `config.runtime_output_format` (the already-resolved global `--format`, TTY-aware auto included) instead of hard-coded `OutputFormat::Table`.
- Print the `"Access assignments for secret '…' in vault '…':"` header only when the resolved format is human table-like (`Table`/`Plain`/`Raw`), matching `vault_ops.rs:1216`.
- Empty state: for human formats, replace the stdout `println!` with `output::info(...)` (stderr), keeping the existing wording. For machine formats, print the formatter's valid-empty output on stdout (`[]` for JSON, etc. — `TableFormatter::format_table` already does this for empty slices), so `xv share list foo --format json | jq` always receives valid JSON.
- Pagination footer continues to render only for human formats (existing `pagination_footer_text` behavior).

No local `--fmt`/`--format` flag is added; the global flag is the interface. Flag-surface unification is later-tier work.

### 4. `xv context list` respects color settings

In `execute_context_list` (`src/cli/config_ops.rs:1063`), use the `Config` parameter (currently bound as `_config`) and pass `config.no_color` to `format_table` at line 1123 instead of the literal `false`.

## Testing

Unit tests:

- `format.rs`: a `Tabled` struct with one all-empty column — Table output omits the column (header and cells); CSV and JSON output still include the field. A narrow-width render wraps the widest column, not the date-shaped column.
- `secret_ops.rs`: date-only rendering of `updated_on` for the standard timestamp shape; fallback passthrough for a nonstandard string; empty timestamp stays empty.

Manual verification (real terminal, real vault):

- `xv --format table ls`: no blank-header columns; `Updated` shows `YYYY-MM-DD` on one line; rows with short notes occupy one line each at 80+ columns.
- `xv share list <secret> --format json` emits JSON (including `[]` when there are no assignments); with no `--format` and piped stdout, emits JSON via auto-resolution.
- `NO_COLOR=1 xv context list` and `xv --no-color context list` emit no ANSI escapes.
- `xv --columns Name ls` fails with an unexpected-argument error.

Gates: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`.

## Out of scope

- `ls`-style default output and `-l` long mode for `xv ls` (P1 spec).
- Folder-aware listing / `--folder` filter (P1 spec).
- Format-flag, pager-flag, count/header/empty-state unification across list commands (P2 spec).
- Re-introducing column selection (returns with the P2 shared-renderer work).
