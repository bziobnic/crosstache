# List Command P2 Surface-Consistency Design

> **Status:** 📋 Approved design — not yet implemented. | **Date:** 2026-07-01 | **Author:** Claude + Scott
> Phase A of the P2 tier of the 2026-07-01 list-command UX review (P0 = PR #289, P1 = PR #290). Phase B (renderer unification through `TableFormatter`, `--columns`, machine formats for the bespoke renderers) is a separate future spec.

---

## Problem

The list commands drifted apart because each one hand-rolled its own surface. Concretely, today:

1. **Format selection is inconsistent.** `xv vault list` declares a local `--format` that shadows the identical global flag (`src/cli/commands.rs:933` region); `xv vault share list` uses `-f/--fmt`; everything else uses the global `--format`.
2. **Pager flags differ.** `xv file list` takes a bare `--pager` boolean (`src/cli/file.rs:90`), while `ls`, `vault list`, `share list`, and `vault share list` take `--pager [auto|always|never]` (`PagerWhen`).
3. **`--names-only` exists only on `ls` and `find`** — the most pipe-friendly flag is missing from `vault list` and `file list`.
4. **Empty-states and counts are five different conventions.** Wording, punctuation, and stream (stdout vs stderr) vary per command; `xv vault share list` with no assignments prints stderr info even for machine formats, leaving stdout empty and breaking `| jq` (defect flagged in the P1 final review). Counts range from `"N secret(s) in vault 'X'"` to `"Total files: N"` to nothing at all.
5. **No `--no-color` flag exists** — color is controlled only by the config `no_color` key and the `NO_COLOR` env var (added in P0).

## Decisions

Settled in-session (user AFK at the option prompts; recommended options adopted, overridable at spec review):

- **P2 is split.** This spec is the mechanical surface pass (Phase A). Renderer unification and `--columns` are Phase B, later.
- **Convention is enforced by small shared helpers** (`src/utils/list_output.rs`), not a full rendering abstraction and not tests alone.
- **Human empty-states go to stderr; machine formats emit valid-empty stdout.** This changes `xv ls`, whose empty message currently lands in the stdout buffer — changelog-noted.
- **`--fmt` on `vault export/import`, `env pull`, and `parse` is untouched** — there it selects file formats, not output rendering.

## Design

### 1. Flag unification

- **`xv vault list`:** delete the local `--format` field and its plumbing; the global `--format` takes over transparently (same name, same values, same default `auto`). No user-visible change. Add `--names-only`.
- **`xv vault share list`:** replace `-f/--fmt` with reliance on the global `--format`. Keep `--fmt` as a **hidden** local `Option<OutputFormat>` (`hide = true`, no `-f` short) for one release: when supplied it wins and prints `output::warn("--fmt is deprecated; use the global --format")`. Remove entirely in the release after.
- **`xv file list`:** change `--pager: bool` to `--pager [WHEN]` (`Option<PagerWhen>`, `num_args 0..=1`, `default_missing_value = "auto"`) exactly matching the other list commands. Bare `--pager` behaves as before. Add `--names-only` (one blob name per line, files only, no directory entries, recursive listing semantics like `ls --names-only` uses the subtree).
- **Global `--no-color`:** new global bool flag on the `Cli` struct; when set, `config.no_color = true` at dispatch (highest priority, consistent with CLI > env > file hierarchy). Documented alongside `NO_COLOR` in README.

### 2. Shared conventions module (`src/utils/list_output.rs`)

Pure helpers; no I/O:

```rust
/// "No <nouns> found[ in <scope>]." — human empty-state wording.
pub fn empty_state_message(noun_plural: &str, scope: Option<&str>) -> String
/// Optional follow-up hint line, e.g. "Use --all to show disabled secrets."
/// Callers pass it separately so wording stays caller-owned but placement uniform.
/// "N <noun>(s) in <scope>" or "Showing X of Y <noun>(s) in <scope>".
pub fn count_label(displayed: usize, total: usize, noun: &str, scope: Option<&str>, paginated: bool) -> String
```

Adopters and their conventions after this change (human formats):

| Command | Empty-state (stderr `output::info`) | Count (stdout, after listing) |
|---|---|---|
| `ls` (grid/long/table) | `No secrets found in folder 'p'.` / `No secrets found in vault 'v'.` + `--all` hint when filtered | existing `N secret(s)[, M folder(s)] in vault 'v'` reworded via `count_label` |
| `vault list` | `No vaults found.` | **new** `N vault(s)` |
| `file list` | `No files found.` | `N file(s), M directory(ies)` replacing `Total files:`/`Total:` wording |
| `share list` / `vault share list` | `No access assignments found for …` (existing wording kept, stream already stderr) | **new** `N assignment(s)` |
| `history` | existing wording | count moves from stderr to stdout, `N version(s) of 'name'` |
| `audit`, `find`, `context list`, `env list` | wording normalized to the pattern; streams already stderr | unchanged (audit keeps its `Found N…` header position but reworded via helper) |

Machine formats: every adopter prints its format's valid-empty output on stdout (`[]` for JSON — via `TableFormatter` where used, via the command's own serializer for `find`) and never the info message — this fixes the `vault share list` defect. Counts never appear in machine output.

**`ls` stream change called out:** its human empty message moves from the stdout buffer to stderr `output::info`, aligning with the v0.16.0 stdout-purity rule. `xv ls > file` on an empty vault now produces an empty file. The P1 spec's §2 note about "existing stdout convention" is superseded by this spec.

### 3. Explicitly out of scope (Phase B or later)

- Routing `audit`/`find`/`config show`/`context list`/`env list`/`file list` rendering through `TableFormatter`; adding JSON/YAML/CSV to commands that lack them; any change to `find`'s JSON envelope or `file list`'s CSV header casing.
- `--columns` column selection (returns with the shared renderer).
- `find --folder`, TUI folder tree, folder completion (P1 deferrals, unchanged).

## Testing

- Unit tests for `list_output.rs` helpers (scoped/unscoped wording, paginated wording, singular/plural form left as `(s)` style matching current output).
- Behavioral checks per adopter: empty vault/folder/prefix cases send info to stderr and keep stdout clean (or valid-empty for machine formats); `xv vault share list --format json` with zero assignments emits `[]`; `xv file list --pager never` parses; `--fmt` on `vault share list` warns but works; `xv --no-color ls` emits no ANSI.
- Gates: `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib`, full `cargo test`.

## Out of scope

Everything in §3 above, plus any renderer or schema change. Machine output shapes are byte-identical after this phase except: empty `vault share list` machine output (bug fix: `[]` instead of nothing).
