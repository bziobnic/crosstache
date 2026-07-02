# List Command P2 Phase B — Renderer Unification Design

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-01 | **Author:** Claude + Scott
> Phase B of the P2 tier of the 2026-07-01 list-command UX review (P0 = PR #289, P1 = PR #290, Phase A = PR #291). Executed autonomously under Scott's standing "recommended options, don't stop" instruction; the batched up-front decisions are recorded below.

---

## Problem

Phase A unified flags and wording but left four rendering engines alive: `TableFormatter` (secrets, vaults, history, both share lists), the standalone `format_table()` free function (`config show`, `context list`), hand-rolled `println!`/fixed-width output (`audit`, `find`, `env list`), and a bespoke per-format `match` (`file list`). Consequences:

1. `--format json|yaml|csv` works only on the first group — `audit`, `find` (no CSV), `context list`, and `env list` cannot be scripted uniformly.
2. Machine-format quirks persist: `find` has a custom JSON envelope with no CSV; `file list` CSV uses snake_case headers and a kitchen-sink column set disagreeing with its own table; `audit` has JSON only via `--raw` (per-entry documents with `---` separators, ignoring `--format`).
3. `history`, `find`, and `audit` still emit nothing on stdout for empty machine-format results (deferred from Phase A).
4. `--columns` (removed as a no-op in P0 with the promise it returns on a shared renderer) still doesn't exist.
5. The `(s)` count style produces "5 audit log entry(s)".
6. The dead legacy `execute_secret_list` (~120 lines, `#[allow(dead_code)]`) embodies two conventions this effort abolished.

## Decisions (adopted from the up-front batch, 2026-07-01)

- **Machine shapes normalize now.** Pre-1.0; breaking changes are the point of unification and are changelog-documented: `find`'s JSON envelope becomes the standard row shape (+ gains CSV, loses the TTY score bar), `file list` CSV becomes the table's column set, `audit` adopts the global `--format` (array JSON) with `--raw` deprecated to a hidden alias.
- **`--columns` returns as a global flag**: comma-separated, case-insensitive column names, applied to `Table`/`Plain`/`CSV` renders of every `TableFormatter` consumer; unknown names error, listing the available columns. Explicit `--columns` disables Phase 0's hide-empty-columns behavior (explicit selection wins).
- **Counts become plural-aware** (`count_label` takes singular + plural nouns): "1 vault", "3 vaults", "5 audit log entries" — replacing the "(s)" style shipped in Phase A (small wording change, changelog-noted).
- **`config show --format json|yaml` keeps serializing the whole `Config` object** — it is a resource view, not a list; only its human table routes through `TableFormatter`. Documented exception.
- **`vault share list --fmt` alias is NOT removed** — its deprecation warning has not shipped in a release yet.

## Design

### 1. `--columns` in `TableFormatter`

- New global CLI flag `--columns <COLS>` (comma-separated), parsed into `Config.runtime_columns: Option<Vec<String>>` at dispatch (`#[serde(skip)]`, `#[tabled(skip)]`).
- `TableFormatter::new` gains the selection (new constructor `with_columns` or an extra parameter — plan decides; all call sites updated mechanically).
- Selection applies inside `format_as_table`, `format_as_plain`, and `format_as_csv`: match requested names case-insensitively against `T::headers()`; project the header/row records to the selected columns in the requested order; on any unknown name return `CrosstacheError::invalid_argument("unknown column 'X'; available: A, B, C")`. JSON/YAML/Template ignore `--columns` (full schema).
- When `--columns` is present, skip `visible_column_indices` empty-column hiding.

### 2. One renderer for the bespoke commands

All of the following route their rendering through `TableFormatter` with the global `--format`, inheriting `--columns`, `--no-color`, valid-empty machine output, and sanitization for free:

- **`audit`** (`src/cli/system_ops.rs`): new `AuditRow { Timestamp, Operation, Resource, Caller, Status }` (Tabled + Serialize) replaces the hand-rolled fixed-width `|` table and its string truncation. Global `--format` honored (JSON = array of rows). `--raw` becomes a hidden deprecated flag that warns and implies `--format json`. Count/empty wording already standardized in Phase A stays, via the plural-aware helper.
- **`find`** (`src/cli/secret_ops.rs`): new `FindRow { Name, Score, Folder, Groups }` (score formatted to 2 decimals as a string for stable CSV/table output) replaces both the hand-rolled UPPERCASE table (score bar dropped) and the custom `serde_json::json!` envelope. All formats work, CSV included. `--names-only` unchanged.
- **`context list`** (`src/cli/config_ops.rs`): existing `ContextItem` gains `Serialize`, renders via `TableFormatter` (drops the `format_table` free fn here), so `xv context list --format json` works. The `● Current` glyph stays in the Status column (it serializes as text).
- **`env list`** (`src/cli/config_ops.rs`): new `EnvRow { Name, Active, Backend, Vault, Resource Group }` replaces the `println!` line format; all formats work. `context envs` delegates unchanged (its deprecation is P3).
- **`config show`**: human table switches from the `format_table` free fn to `TableFormatter` (Setting/Value/Source rows) so `--columns`/`--no-color` behave uniformly; machine formats keep the whole-`Config` serialization (documented exception above).
- **`file list`** (`src/cli/file_ops.rs`): the bespoke CSV row (`FileListCsvRow`, snake_case kitchen-sink) is replaced by the display row set + a leading `Kind` column (`file`/`directory`): `Kind, Name, Size, Content-Type, Modified, Groups`. JSON/YAML keep the current rich `BlobListItem` serialization (full-fidelity formats). Table/Plain/Template paths collapse onto the same display rows they already build.

The `format_table()` free function is deleted once `config show`/`context list` migrate (grep for stragglers).

### 3. Machine valid-empty for `history`, `find`, `audit`

Empty results on machine formats print the format's valid-empty output on stdout (`[]` for JSON/CSV-headers-only per `TableFormatter`'s existing empty branch) instead of stderr-only info. Human behavior unchanged (stderr info per Phase A).

### 4. Plural-aware counts

`count_label(displayed, total, noun_singular, noun_plural, scope, paginated)` — callers pass both forms; output uses the grammatically correct one ("1 vault", "3 vaults", "Showing 10 of 42 secrets in vault 'kv'"). All Phase A adopters updated; `empty_state_message` already takes a plural and is unchanged. Phase A's `"N noun(s)"` strings disappear (changelog).

### 5. Legacy deletion

`execute_secret_list` (`src/cli/secret_ops.rs` ~3147, `#[allow(dead_code)]`) and the now-orphaned `secret_count_label` (+ its test) are deleted.

## Out of scope

- P3 items (deleted-secret listing, `group list`, `--sort`, `context envs` dedup, folder extras, unicode-width) — next spec.
- `vault share list --fmt` removal (release-gated).
- Any TUI change.

## Testing

- Unit: `--columns` projection (order, case-insensitivity, unknown-name error, interplay with empty-column hiding), plural `count_label`, `FindRow`/`AuditRow`/`EnvRow` render snapshots via `TableFormatter`.
- Behavioral: `xv audit --format json | jq type` = array; `xv find <pat> --format csv` has headers; `xv context list --format json` parses; `xv env list --format yaml` parses; `xv ls --columns Name,Updated --format table` shows exactly two columns; `xv ls --columns Bogus` errors listing columns; empty `history`/`find`/`audit` machine output valid; `config show --format json` still emits the Config object.
- Gates: `cargo fmt --check`, `cargo clippy --all-targets` (0 warnings), `cargo test --lib`, full `cargo test`.
