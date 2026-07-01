# List Command P0 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four P0 list-command defects from `docs/superpowers/specs/2026-07-01-list-p0-fixes-design.md`: broken table width rendering, the dead `--columns` flag, `xv share list` ignoring `--format`, and `xv context list` ignoring color settings.

**Architecture:** Two shared-formatter changes in `TableFormatter` (hide all-empty columns via a `tabled` `Builder`, priority-based width wrapping) benefit every table-rendering command; three surgical per-command fixes (date-only `Updated`, share-list format plumbing, context-list `no_color`) land in their handlers. No new modules, no schema changes to machine output.

**Tech Stack:** Rust, `tabled` 0.15 (`Builder::push_record`, `Wrap::priority::<PriorityMax>()` — both verified present in the vendored 0.15.0 source), `clap` derive, existing `cargo test` unit-test modules.

## Global Constraints

- Branch: `list-p0-ux-fixes` (already exists, spec committed). All work happens there.
- Machine-readable output schemas (JSON/YAML/CSV field sets and timestamp precision) must NOT change — all fixes are human-render only, except `share list` gaining machine formats it never had.
- stdout is for data; status/empty-state chrome goes to stderr via `crate::utils::output` (repo convention since v0.16.0).
- Every task: code compiles (`cargo check`), then commit. `cargo fmt` before each commit.
- Commit messages follow repo style: `fix: …` / `docs: …`, ending with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: TableFormatter — hide all-empty columns and priority-wrap widths

**Files:**
- Modify: `src/utils/format.rs` (imports ~line 17-23; `format_as_table` lines 168-192; `format_as_plain` lines 237-244; tests module from line 418)

**Interfaces:**
- Consumes: `tabled::builder::Builder`, `tabled::settings::peaker::PriorityMax`, existing `Tabled::headers()` / `Tabled::fields()`.
- Produces: private helpers `visible_column_indices(column_count: usize, rows: &[Vec<String>]) -> Vec<usize>` and `table_hiding_empty_columns<T: Tabled>(data: &[T]) -> Table`, used by `format_as_table` and `format_as_plain`. No public API change.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/utils/format.rs` (it already defines `TestData` with columns `Name`/`Value`/`Status`):

```rust
    #[test]
    fn table_hides_all_empty_columns() {
        let data = vec![
            TestData {
                name: "alpha".to_string(),
                value: String::new(),
                status: "ok".to_string(),
            },
            TestData {
                name: "beta".to_string(),
                value: String::new(),
                status: "ok".to_string(),
            },
        ];
        let formatter = TableFormatter::new(OutputFormat::Table, true, None);
        let out = formatter.format_table(&data).unwrap();
        assert!(
            !out.contains("Value"),
            "all-empty column must be hidden from table output:\n{out}"
        );
        assert!(out.contains("Name"), "populated columns stay:\n{out}");
        assert!(out.contains("Status"), "populated columns stay:\n{out}");
    }

    #[test]
    fn plain_hides_all_empty_columns() {
        let data = vec![TestData {
            name: "alpha".to_string(),
            value: String::new(),
            status: "ok".to_string(),
        }];
        let formatter = TableFormatter::new(OutputFormat::Plain, true, None);
        let out = formatter.format_table(&data).unwrap();
        assert!(!out.contains("Value"), "plain output hides empty columns too:\n{out}");
        assert!(out.contains("Name"));
    }

    #[test]
    fn table_keeps_partially_filled_columns() {
        let data = vec![
            TestData {
                name: "alpha".to_string(),
                value: String::new(),
                status: "ok".to_string(),
            },
            TestData {
                name: "beta".to_string(),
                value: "present".to_string(),
                status: "ok".to_string(),
            },
        ];
        let formatter = TableFormatter::new(OutputFormat::Table, true, None);
        let out = formatter.format_table(&data).unwrap();
        assert!(
            out.contains("Value"),
            "column with any content must remain:\n{out}"
        );
    }

    #[test]
    fn machine_formats_keep_empty_columns() {
        let data = vec![TestData {
            name: "alpha".to_string(),
            value: String::new(),
            status: "ok".to_string(),
        }];
        let csv = TableFormatter::new(OutputFormat::Csv, true, None)
            .format_table(&data)
            .unwrap();
        assert!(csv.contains("Value"), "CSV keeps the full schema:\n{csv}");
        let json = TableFormatter::new(OutputFormat::Json, true, None)
            .format_table(&data)
            .unwrap();
        assert!(json.contains("\"value\""), "JSON keeps the full schema:\n{json}");
    }

    #[test]
    fn visible_columns_fall_back_when_every_column_is_empty() {
        let rows = vec![vec![String::new(), String::new()]];
        assert_eq!(visible_column_indices(2, &rows), vec![0, 1]);
    }

    #[test]
    fn whitespace_only_cells_count_as_empty() {
        let rows = vec![vec!["  ".to_string(), "x".to_string()]];
        assert_eq!(visible_column_indices(2, &rows), vec![1]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib utils::format -- --nocapture`
Expected: FAIL — `visible_column_indices` not found (compile error). That's the correct failure mode for TDD on a missing function.

- [ ] **Step 3: Implement the helpers and wire them in**

In `src/utils/format.rs`, extend the `tabled` import block (currently lines 17-23) to add `peaker::PriorityMax`:

```rust
use tabled::{
    settings::{
        object::{Rows, Segment},
        peaker::PriorityMax,
        Alignment, Color, Format, Modify, Padding, Style, Width,
    },
    Table, Tabled,
};
```

Add the helpers above `impl TableFormatter` (near line 120):

```rust
/// Column indices that contain at least one non-empty cell across all rows.
/// If every column is empty (impossible for real listings, where Name is
/// always populated), keep all columns rather than rendering an empty table.
fn visible_column_indices(column_count: usize, rows: &[Vec<String>]) -> Vec<usize> {
    let keep: Vec<usize> = (0..column_count)
        .filter(|&i| rows.iter().any(|row| !row[i].trim().is_empty()))
        .collect();
    if keep.is_empty() {
        (0..column_count).collect()
    } else {
        keep
    }
}

/// Build a `Table` from `data`, omitting columns whose cells are all empty.
/// Human table/plain views only — machine formats keep the full field set.
fn table_hiding_empty_columns<T: Tabled>(data: &[T]) -> Table {
    let headers: Vec<String> = T::headers().iter().map(|h| h.to_string()).collect();
    let rows: Vec<Vec<String>> = data
        .iter()
        .map(|item| item.fields().iter().map(|f| f.to_string()).collect())
        .collect();
    let keep = visible_column_indices(headers.len(), &rows);

    let mut builder = tabled::builder::Builder::default();
    builder.push_record(keep.iter().map(|&i| headers[i].clone()));
    for row in &rows {
        builder.push_record(keep.iter().map(|&i| row[i].clone()));
    }
    builder.build()
}
```

In `format_as_table` (line 169), change the constructor and the wrap call:

```rust
    /// Format data as a styled table
    fn format_as_table<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let mut table = table_hiding_empty_columns(data);

        // Neutralize terminal escape sequences in untrusted cell content
        table.with(Modify::new(Segment::all()).with(Format::content(sanitize_control_chars)));

        // Apply styling
        table
            .with(Style::rounded())
            .with(Modify::new(Rows::first()).with(Alignment::center()))
            .with(Padding::new(1, 1, 0, 0));

        // Apply color if enabled
        if !self.no_color {
            table.with(Modify::new(Rows::first()).with(Color::FG_CYAN));
        }

        // Auto-adjust width to terminal, shrinking the widest column first
        // (Note in practice) instead of chopping fixed-width columns like dates.
        if let Ok((width, _)) = size() {
            table.with(Width::wrap(width as usize).priority::<PriorityMax>());
        }

        Ok(table.to_string())
    }
```

In `format_as_plain` (line 238), swap only the constructor (no wrapping exists there today and none is added):

```rust
    /// Format data as plain text
    fn format_as_plain<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let mut table = table_hiding_empty_columns(data);
        // Neutralize terminal escape sequences in untrusted cell content
        table.with(Modify::new(Segment::all()).with(Format::content(sanitize_control_chars)));
        table.with(Style::ascii()).with(Padding::new(1, 1, 0, 0));
        Ok(table.to_string())
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib utils::format -- --nocapture`
Expected: PASS, including the two pre-existing sanitization tests (the escape-sequence test must still pass — the builder path must not bypass `sanitize_control_chars`, which it doesn't because sanitization is applied via `table.with(...)` after construction).

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/utils/format.rs && git commit -m "fix: hide all-empty columns and prioritize widest column when wrapping tables

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Date-only `Updated` column in the secrets table

**Files:**
- Modify: `src/cli/secret_ops.rs` (`format_secret_list_rows_for_human` lines 293-310; new helper next to `wrap_text_to_width` ~line 312; tests module from line 4243)

**Interfaces:**
- Consumes: `SecretSummary.updated_on: String` (formatted as `YYYY-MM-DD HH:MM:SS UTC` by `timestamp_string` in `src/secret/manager.rs:587`).
- Produces: private helper `date_portion_for_display(timestamp: &str) -> String` used only by `format_secret_list_rows_for_human`. Machine formats keep serializing the raw `SecretSummary` — do not touch `display_cached_secret_list`'s non-human branch.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/cli/secret_ops.rs`:

```rust
    #[test]
    fn date_portion_truncates_standard_timestamp() {
        assert_eq!(
            date_portion_for_display("2026-05-17 01:19:00 UTC"),
            "2026-05-17"
        );
        assert_eq!(date_portion_for_display("2026-05-17"), "2026-05-17");
    }

    #[test]
    fn date_portion_passes_through_nonstandard_values() {
        // Not date-shaped: return the raw value unmodified, never error.
        assert_eq!(date_portion_for_display("yesterday"), "yesterday");
        assert_eq!(date_portion_for_display("2026-5-7 01:19"), "2026-5-7 01:19");
        assert_eq!(date_portion_for_display("N/A"), "N/A");
        // Empty stays empty.
        assert_eq!(date_portion_for_display(""), "");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib cli::secret_ops::tests::date_portion -- --nocapture`
Expected: FAIL — compile error, `date_portion_for_display` not found.

- [ ] **Step 3: Implement the helper and use it**

Add next to `wrap_text_to_width` (~line 312) in `src/cli/secret_ops.rs`:

```rust
/// Reduce a backend timestamp like "2026-05-17 01:19:00 UTC" to its date
/// portion for human tables. Values that don't lead with a YYYY-MM-DD token
/// pass through unmodified; machine formats always get the full timestamp.
fn date_portion_for_display(timestamp: &str) -> String {
    let first = timestamp.split_whitespace().next().unwrap_or("");
    let is_date_shaped = first.len() == 10
        && first.chars().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        });
    if is_date_shaped {
        first.to_string()
    } else {
        timestamp.to_string()
    }
}
```

In `format_secret_list_rows_for_human` (line 307), change:

```rust
            updated_on: secret.updated_on.clone(),
```

to:

```rust
            updated_on: date_portion_for_display(&secret.updated_on),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cli::secret_ops -- --nocapture`
Expected: PASS (including the pre-existing note-wrap tests in that module).

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/cli/secret_ops.rs && git commit -m "fix: show date-only Updated column in xv ls table output

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Remove the dead global `--columns` flag

**Files:**
- Modify: `src/cli/commands.rs:164-166` (delete the field)
- Modify: `README.md:1149` (delete the flag row from the global-options table)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing — the field `Cli.columns` has zero readers (verified by repo-wide grep), so deleting it cannot break other tasks.

- [ ] **Step 1: Delete the field**

In `src/cli/commands.rs`, remove lines 164-166 entirely:

```rust
    /// Select specific columns for table output (comma-separated)
    #[arg(long, global = true, hide = should_hide_options())]
    pub columns: Option<String>,
```

- [ ] **Step 2: Delete the README row**

In `README.md`, remove line 1149:

```markdown
| `--columns <COLS>` | Select specific columns for table output (comma-separated) |
```

- [ ] **Step 3: Verify the build and the new error behavior**

Run: `cargo check`
Expected: clean compile — if this errors with an unused/missing `columns` reference, a reader existed after all; stop and re-inspect before proceeding.

Run: `cargo run --quiet -- --columns Name list 2>&1 | head -3`
Expected: clap error `unexpected argument '--columns' found` (non-zero exit), not a silent no-op.

Run: `grep -rn 'columns' src/ | grep -v 'tabled\|visible_column\|column_count\|hiding_empty' | head`
Expected: no leftover references to the CLI flag.

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/commands.rs README.md && git commit -m "fix: remove documented-but-unimplemented global --columns flag

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `xv share list` honors `--format`

**Files:**
- Modify: `src/cli/secret_ops.rs:3839-3892` (the `ShareCommands::List` match arm)

**Interfaces:**
- Consumes: `config.runtime_output_format` (the global `--format`, already TTY-resolved at dispatch in `commands.rs:1380-1382`), `crate::utils::output::info` (stderr), `TableFormatter::format_table` (already emits valid empty output like `[]` for empty slices on machine formats).
- Produces: no new symbols. Reference pattern: the `VaultShareCommands::List` arm at `src/cli/vault_ops.rs:1173-1226`.

- [ ] **Step 1: Replace the match arm**

Replace the body of `ShareCommands::List { .. } => { ... }` (currently lines 3839-3892) with:

```rust
        ShareCommands::List {
            secret_name,
            all,
            page,
            page_size,
            pager,
        } => {
            use crate::utils::pagination::{paginate_slice, pagination_footer_text, Pagination};
            use std::fmt::Write as _;

            let pager = pager
                .map(crate::cli::commands::PagerWhen::wants_pager)
                .unwrap_or(false);
            let mut roles = vault_manager
                .list_secret_access(&vault_name, &resource_group, &secret_name)
                .await?;

            vault_manager
                .resolve_and_filter_roles(&mut roles, all)
                .await?;

            let pagination = Pagination::from_args(page, page_size)?;
            let paged = paginate_slice(&roles, pagination);

            let fmt = config.runtime_output_format;
            let human_table_like = matches!(
                fmt,
                crate::utils::format::OutputFormat::Table
                    | crate::utils::format::OutputFormat::Plain
                    | crate::utils::format::OutputFormat::Raw
            );
            let formatter = crate::utils::format::TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
            );

            if roles.is_empty() {
                if human_table_like {
                    // Chrome goes to stderr; stdout stays clean for pipes.
                    crate::utils::output::info(&format!(
                        "No access assignments found for secret '{secret_name}' in vault '{vault_name}'"
                    ));
                } else {
                    // Machine formats emit valid empty output (e.g. `[]`).
                    println!("{}", formatter.format_table(&paged.items)?);
                }
            } else {
                let mut output = String::new();
                if human_table_like {
                    let _ = writeln!(
                        output,
                        "Access assignments for secret '{secret_name}' in vault '{vault_name}':"
                    );
                }
                let table_output = formatter.format_table(&paged.items)?;
                output.push_str(&table_output);
                if let Some(footer) = pagination_footer_text(&paged, "assignment", fmt) {
                    output.push('\n');
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
        }
```

Behavioral notes for the implementer:
- `pagination_footer_text` already suppresses the footer for non-human formats (`src/utils/pagination.rs:147`), so no extra guard is needed.
- The header wording is unchanged from today; it just becomes conditional on human formats.
- Do NOT add a local `--fmt`/`--format` flag to the clap definition — the global flag is the interface (spec decision).

- [ ] **Step 2: Verify the build**

Run: `cargo check`
Expected: clean compile.

Run: `cargo test --lib cli::secret_ops`
Expected: PASS (no existing tests cover this arm; the module must still compile and pass).

- [ ] **Step 3: Manual verification (requires a real vault; skip in sandboxed execution and flag for the final checklist)**

```bash
cargo run --quiet -- share list <some-secret> --format json | jq type
```
Expected: `"array"` — both when assignments exist and when there are none (`[]`).

```bash
cargo run --quiet -- share list <some-secret> 2>/dev/null
```
Expected: piped stdout gets JSON via auto-resolution (dispatch resolves `auto` → `json` for pipes).

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/secret_ops.rs && git commit -m "fix: honor --format in xv share list and route empty-state to stderr

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `xv context list` respects color settings

**Files:**
- Modify: `src/cli/config_ops.rs:1063` (signature) and `src/cli/config_ops.rs:1123` (the `format_table` call)

**Interfaces:**
- Consumes: `config.no_color` (already derived from `--no-color`/`NO_COLOR` upstream; `share list` and others use it the same way). Call site at `config_ops.rs:903` already passes `&config`.
- Produces: no new symbols.

- [ ] **Step 1: Use the config parameter**

Change the signature at line 1063:

```rust
async fn execute_context_list(config: &Config) -> Result<()> {
```

(from `_config: &Config`), and change line 1123:

```rust
        println!("{}", format_table(table, config.no_color));
```

(from `format_table(table, false)`).

- [ ] **Step 2: Verify**

Run: `cargo check`
Expected: clean compile with no unused-variable warning for `config`.

Run: `NO_COLOR=1 cargo run --quiet -- context list | grep -c $'\x1b'; true`
Expected: `0` — no ANSI escapes in the table.

Run: `cargo run --quiet -- context list --format table` in an interactive terminal (or eyeball locally)
Expected: colored output still present without `NO_COLOR` (unchanged default behavior).

- [ ] **Step 3: Commit**

```bash
cargo fmt && git add src/cli/config_ops.rs && git commit -m "fix: respect --no-color and NO_COLOR in xv context list

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: CHANGELOG, full gates, and end-to-end verification

**Files:**
- Modify: `CHANGELOG.md` (new `## Unreleased` section at the top, above `## v0.16.0`)

**Interfaces:**
- Consumes: everything from Tasks 1-5.
- Produces: release notes; the verified branch ready for review/PR.

- [ ] **Step 1: Add the CHANGELOG entry**

Insert at the top of `CHANGELOG.md`, immediately after the `# Changelog` heading:

```markdown
## Unreleased

### Fixed

- **`xv ls` table rendering.** Columns whose cells are all empty are no longer rendered as blank zero-width headers, narrow terminals now shrink the widest column first instead of chopping every column (no more `UT`/`C` timestamp wrapping), and the `Updated` column shows the date only (`2026-05-17`). Machine formats (JSON/YAML/CSV) are unchanged.
- **`xv share list` honors the global `--format`** (json/yaml/csv/…) like `xv vault share list` already did; its empty-state message now goes to stderr, and machine formats emit valid empty output (`[]`) for pipes.
- **`xv context list` respects `--no-color` and `NO_COLOR`** instead of always coloring the table.

### Removed

- The global `--columns` flag, which was documented but never implemented (a silent no-op since introduction). Column selection will return with the planned list-renderer unification.
```

- [ ] **Step 2: Run the full gates**

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test --lib
```
Expected: all clean/passing. (Integration tests in `tests/` need Azure credentials; `--lib` is the required gate per repo norms. Run full `cargo test` too if credentials are available, and report any failures rather than skipping silently.)

- [ ] **Step 3: End-to-end verification from the spec (interactive terminal, real vault)**

```bash
cargo run --quiet -- --format table ls
```
Expected: no blank-header columns; `Updated` shows `YYYY-MM-DD` unwrapped; rows with short notes occupy one line each at 80+ columns.

```bash
cargo run --quiet -- --columns Name ls
```
Expected: clap unexpected-argument error, non-zero exit.

Record actual outputs; if any expectation fails, fix before committing.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md && git commit -m "docs: changelog for list-command P0 fixes

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec coverage map

| Spec section | Task |
|---|---|
| 1a hide all-empty columns (Table + Plain) | Task 1 |
| 1b priority wrapping (`format_as_table` only) | Task 1 |
| 1c date-only `Updated` with raw fallback | Task 2 |
| 2 remove `--columns` (flag + help + README) | Task 3 |
| 3 `share list` format / header / stderr empty-state / `[]` | Task 4 |
| 4 `context list` `no_color` | Task 5 |
| Testing gates + manual verification | Tasks 1-5 steps + Task 6 |
