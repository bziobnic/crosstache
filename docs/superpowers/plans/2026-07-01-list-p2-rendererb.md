# List Command P2 Phase B — Renderer Unification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Phase B of the P2 spec (`docs/superpowers/specs/2026-07-01-list-p2-rendererb-design.md`): route `audit`, `find`, `context list`, `env list`, `config show`, and `file list` CSV through the one shared `TableFormatter`, bring back the global `--columns` flag, make counts plural-aware, give `history`/`find`/`audit` valid-empty machine output, and delete the dead legacy list path plus the `format_table()` free function.

**Architecture:** One renderer (`TableFormatter`) gains an optional column selection (`--columns`) applied to Table/Plain/CSV; every bespoke rendering engine (hand-rolled fixed-width tables, custom JSON envelopes, `format_table()` free-fn tables, `println!` line formats) is replaced by a small `Tabled + Serialize` row struct fed to that renderer. Machine-format shape changes are deliberate normalization, documented in the CHANGELOG.

**Tech Stack:** Rust, `clap` derive, `tabled`, `serde`/`serde_json`/`serde_yaml`, `csv`, existing `TableFormatter`/`list_output`/`output` helpers. No new dependencies.

## Global Constraints

- Branch: `list-p2-rendererb` (exists, spec committed).
- **Machine-shape breaks are deliberate and changelog-documented** (pre-1.0): `find`'s JSON envelope becomes the standard row shape (+ gains CSV, loses the TTY score bar), `file list` CSV becomes the table's column set, `audit` adopts the global `--format` (array JSON) with `--raw` deprecated to a hidden alias.
- **Sanitization parity (`sanitize_control_chars`) for any newly-Tabled row types.** `TableFormatter::format_as_table`/`format_as_plain` already apply `sanitize_control_chars` to every cell (`src/utils/format.rs`, the `Modify::new(Segment::all()).with(Format::content(sanitize_control_chars))` lines), so newly-Tabled rows (`AuditRow`, `FindRow`, `EnvRow`, `FileRow`, `ContextItem`, `ConfigItem`) inherit it — do NOT bypass the formatter with hand-rolled `println!` for these rows. Machine formats stay raw by design.
- **Commit style `feat:`/`fix:`/`docs:`** and every commit message ends with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- **`cargo fmt` before every commit.**
- **Locate anchors by symbol not line number** — line numbers cited below are current as of writing and may drift a few lines.
- `config show --format json|yaml` keeps serializing the whole `Config` object (resource view, documented exception); only its human table routes through `TableFormatter`.
- `vault share list --fmt` alias is NOT removed (its deprecation warning has not shipped in a release yet) — do not touch it.
- Human empty-states go to stderr via `crate::utils::output::info`; counts appear on human formats only; machine formats never carry counts or info messages on stdout.
- Out of scope: `pagination_footer_text`'s `"{noun}(s)"` wording in `src/utils/pagination.rs` (a pre-Phase-A pagination element, not the count label — P3 candidate), all P3 items, `vault share list --fmt` removal, any TUI change.

---

### Task 1: Plural-aware `count_label` + `pluralize` helper + all adopters

**Files:**
- Modify: `src/utils/list_output.rs` (whole module — signature change + tests)
- Modify: `src/cli/vault_ops.rs` (`count_label` calls in `render_vault_list` ~line 398 and the `VaultShareCommands::List` arm ~line 1273)
- Modify: `src/cli/secret_ops.rs` (`count_label` calls: `xv ls` legacy-table branch ~line 464, `xv ls` grid/long count ~line 506 + the `", {} folder(s)"` suffix ~line 517, history count ~line 772, `xv share list` arm ~line 3982)
- Modify: `src/cli/system_ops.rs` (audit count sites ~lines 372 and 492)
- Modify: `src/cli/file_ops.rs` (file count ~line 677 + the `", {} directory(ies)"` suffix ~line 679)

**Interfaces:**
- Consumes: nothing.
- Produces (all later tasks rely on these exact signatures):
  - `pub fn count_label(displayed: usize, total: usize, noun_singular: &str, noun_plural: &str, scope: Option<&str>, paginated: bool) -> String` — `(1, 1, "vault", "vaults", None, false)` → `"1 vault"`; `(3, 3, "vault", "vaults", None, false)` → `"3 vaults"`; `(10, 42, "secret", "secrets", Some("vault 'kv'"), true)` → `"Showing 10 of 42 secrets in vault 'kv'"`. Plural keys on `total` (`displayed == total` when unpaginated).
  - `pub fn pluralize(count: usize, singular: &str, plural: &str) -> String` — `(1, "folder", "folders")` → `"1 folder"`; `(3, "directory", "directories")` → `"3 directories"`.
  - `empty_state_message` is UNCHANGED (already takes a plural).

- [ ] **Step 1: Rewrite the tests in `src/utils/list_output.rs` to the new expectations**

Replace the entire `#[cfg(test)] mod tests` block with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_without_scope() {
        assert_eq!(empty_state_message("vaults", None), "No vaults found.");
    }

    #[test]
    fn empty_state_with_scope() {
        assert_eq!(
            empty_state_message("secrets", Some("folder 'prod'")),
            "No secrets found in folder 'prod'."
        );
    }

    #[test]
    fn count_singular() {
        assert_eq!(count_label(1, 1, "vault", "vaults", None, false), "1 vault");
    }

    #[test]
    fn count_plural() {
        assert_eq!(count_label(3, 3, "vault", "vaults", None, false), "3 vaults");
    }

    #[test]
    fn count_zero_is_plural() {
        assert_eq!(
            count_label(0, 0, "secret", "secrets", None, false),
            "0 secrets"
        );
    }

    #[test]
    fn count_irregular_plural() {
        assert_eq!(
            count_label(5, 5, "audit log entry", "audit log entries", None, false),
            "5 audit log entries"
        );
    }

    #[test]
    fn count_unpaginated_scoped() {
        assert_eq!(
            count_label(65, 65, "secret", "secrets", Some("vault 'kv'"), false),
            "65 secrets in vault 'kv'"
        );
    }

    #[test]
    fn count_paginated() {
        assert_eq!(
            count_label(10, 42, "secret", "secrets", Some("vault 'kv'"), true),
            "Showing 10 of 42 secrets in vault 'kv'"
        );
    }

    #[test]
    fn count_paginated_singular_total() {
        assert_eq!(
            count_label(1, 1, "secret", "secrets", None, true),
            "Showing 1 of 1 secret"
        );
    }

    #[test]
    fn pluralize_picks_form() {
        assert_eq!(pluralize(1, "folder", "folders"), "1 folder");
        assert_eq!(pluralize(2, "folder", "folders"), "2 folders");
        assert_eq!(pluralize(0, "directory", "directories"), "0 directories");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib utils::list_output`
Expected: compile error — `count_label` takes 5 arguments but 6 were supplied / `pluralize` not found.

- [ ] **Step 3: Implement the new signatures**

Replace the `count_label` function body (keep `empty_state_message` untouched) and add `pluralize`:

```rust
/// "N <noun>[ in <scope>]", or "Showing X of Y <noun>[ in <scope>]" when
/// paginated. Grammatically pluralized: callers pass both noun forms and the
/// correct one is chosen from `total` (equal to `displayed` when unpaginated).
pub fn count_label(
    displayed: usize,
    total: usize,
    noun_singular: &str,
    noun_plural: &str,
    scope: Option<&str>,
    paginated: bool,
) -> String {
    let noun = if total == 1 { noun_singular } else { noun_plural };
    let base = if paginated {
        format!("Showing {displayed} of {total} {noun}")
    } else {
        format!("{displayed} {noun}")
    };
    match scope {
        Some(scope) => format!("{base} in {scope}"),
        None => base,
    }
}

/// "N <noun>" with the grammatically correct form — for count-line suffixes
/// like ", 3 folders" that are composed outside `count_label`.
pub fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{count} {noun}")
}
```

Also update the module doc comment's last sentence: delete `Matches the pre-existing "(s)" pluralization style.` from the `count_label` doc (already replaced above).

- [ ] **Step 4: Update every adopter (10 call sites, locate by the `count_label(` call)**

1. `src/cli/vault_ops.rs` `render_vault_list` (~398): change `"vault",` → `"vault",\n            "vaults",`
2. `src/cli/vault_ops.rs` `VaultShareCommands::List` arm (~1273): `"assignment",` → `"assignment",\n                        "assignments",`
3. `src/cli/secret_ops.rs` `xv ls` legacy-table branch (~464): `"secret",` → `"secret",\n                "secrets",`
4. `src/cli/secret_ops.rs` `xv ls` grid/long count (~506): `"secret",` → `"secret",\n        "secrets",` — and replace the folder suffix two statements below:

```rust
    if folder_count > 0 {
        let _ = write!(
            count_line,
            ", {}",
            crate::utils::list_output::pluralize(folder_count, "folder", "folders")
        );
    }
```

5. `src/cli/secret_ops.rs` history count (~772): `"version",` → `"version",\n                        "versions",`
6. `src/cli/secret_ops.rs` `xv share list` arm (~3982): `"assignment",` → `"assignment",\n                        "assignments",`
7. `src/cli/system_ops.rs` audit count in `execute_audit_command` (~372): `"audit log entry",` → `"audit log entry",\n            "audit log entries",`
8. `src/cli/system_ops.rs` audit count in `execute_backend_audit` (~492): same change.
9. `src/cli/file_ops.rs` file count in `display_file_list_items` (~677): `count_label(file_count, file_count, "file", None, false)` → `count_label(file_count, file_count, "file", "files", None, false)`
10. `src/cli/file_ops.rs` directory suffix (~679): replace

```rust
            if !recursive && dir_count > 0 {
                let _ = write!(
                    count_line,
                    ", {}",
                    crate::utils::list_output::pluralize(dir_count, "directory", "directories")
                );
            }
```

Do NOT touch `secret_count_label` in `src/cli/secret_ops.rs` (~259) — it is only called by the dead legacy `execute_secret_list` and both are deleted in Task 7.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: all green, including 10 `utils::list_output` tests.

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add src/utils/list_output.rs src/cli/vault_ops.rs src/cli/secret_ops.rs src/cli/system_ops.rs src/cli/file_ops.rs && git commit -m "feat: plural-aware count labels (1 vault, 3 vaults, 5 audit log entries)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Global `--columns` flag through `TableFormatter`

**Files:**
- Modify: `src/utils/format.rs` — `TableFormatter` struct + `new` (~line 154-170), `table_hiding_empty_columns` (~137, becomes a method), `format_as_table` (~202), `format_as_csv` (~239), `format_as_plain` (~272), tests module (17 `TableFormatter::new` call sites + new tests)
- Modify: `src/config/settings.rs` — `Config` struct (add `runtime_columns` near `runtime_output_format`, ~line 198-210) and `impl Default for Config` (~line 295)
- Modify: `src/config/init.rs` — the three exhaustive `Config { ... }` literals (search `runtime_output_format:` — ~lines 230, 312, 1070)
- Modify: `src/cli/commands.rs` — `Cli` struct (after the `template` field, ~line 162) and `Cli::execute` dispatch (after `config.template = self.template.clone();`, ~line 1393)
- Modify (mechanical `new` call-site updates): `src/cli/vault_ops.rs` (3 sites: ~112, ~383, ~1250), `src/cli/secret_ops.rs` (6 sites: ~415, ~457, ~758, ~3280, ~3325, ~3954), `src/cli/file_ops.rs` (3 sites: ~639, ~663, ~703), `src/vault/manager.rs` (3 sites: ~134, ~292, ~460), `src/secret/manager.rs` (3 sites: ~1973, ~2054, ~2104)

**Interfaces:**
- Consumes: nothing new.
- Produces (later tasks rely on these):
  - `TableFormatter::new(format: OutputFormat, no_color: bool, template: Option<String>, columns: Option<Vec<String>>) -> Self` — the 4th parameter is the parsed `--columns` selection; `None` = no selection (hide-empty behavior unchanged).
  - `Config.runtime_columns: Option<Vec<String>>` — parsed at dispatch, `#[serde(skip)]` + `#[tabled(skip)]`.
  - Selection semantics: case-insensitive match against `T::headers()`, projected in requested order, applies to Table/Plain/CSV only; unknown name → `CrosstacheError::invalid_argument("unknown column 'X'; available: A, B, C")`; explicit selection disables hide-empty-columns.

- [ ] **Step 1: Write the failing tests**

Append to the `mod tests` block in `src/utils/format.rs`:

```rust
    #[derive(Tabled, Serialize)]
    struct ColRow {
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Note")]
        note: String,
        #[tabled(rename = "Updated")]
        updated: String,
    }

    fn col_rows() -> Vec<ColRow> {
        vec![ColRow {
            name: "alpha".to_string(),
            note: String::new(),
            updated: "2026-07-01".to_string(),
        }]
    }

    #[test]
    fn columns_projects_in_requested_order_case_insensitive() {
        let formatter = TableFormatter::new(
            OutputFormat::Csv,
            true,
            None,
            Some(vec!["updated".to_string(), "NAME".to_string()]),
        );
        let out = formatter.format_table(&col_rows()).unwrap();
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "Updated,Name");
        assert_eq!(lines.next().unwrap(), "2026-07-01,alpha");
    }

    #[test]
    fn columns_unknown_name_errors_listing_available() {
        let formatter = TableFormatter::new(
            OutputFormat::Table,
            true,
            None,
            Some(vec!["Bogus".to_string()]),
        );
        let err = formatter.format_table(&col_rows()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown column 'Bogus'"), "got: {msg}");
        assert!(msg.contains("Name, Note, Updated"), "got: {msg}");
    }

    #[test]
    fn columns_selection_overrides_hide_empty() {
        // Note is all-empty: hidden without a selection, shown when selected.
        let hidden = TableFormatter::new(OutputFormat::Table, true, None, None)
            .format_table(&col_rows())
            .unwrap();
        assert!(!hidden.contains("Note"));

        let shown = TableFormatter::new(
            OutputFormat::Table,
            true,
            None,
            Some(vec!["Name".to_string(), "Note".to_string()]),
        )
        .format_table(&col_rows())
        .unwrap();
        assert!(shown.contains("Note"));
        assert!(!shown.contains("Updated"));
    }

    #[test]
    fn columns_ignored_for_json() {
        let formatter = TableFormatter::new(
            OutputFormat::Json,
            true,
            None,
            Some(vec!["Name".to_string()]),
        );
        let out = formatter.format_table(&col_rows()).unwrap();
        // Full schema regardless of selection.
        assert!(out.contains("\"updated\""));
    }

    #[test]
    fn columns_apply_to_empty_csv_headers() {
        let formatter = TableFormatter::new(
            OutputFormat::Csv,
            true,
            None,
            Some(vec!["Name".to_string()]),
        );
        let empty: Vec<ColRow> = vec![];
        let out = formatter.format_table(&empty).unwrap();
        assert_eq!(out.trim_end(), "Name");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib utils::format`
Expected: compile error — `TableFormatter::new` takes 3 arguments but 4 were supplied.

- [ ] **Step 3: Implement in `src/utils/format.rs`**

Add the field and parameter:

```rust
pub struct TableFormatter {
    _theme: ColorTheme,
    format: OutputFormat,
    no_color: bool,
    template: Option<String>,
    /// Parsed global `--columns` selection; applies to Table/Plain/CSV only.
    columns: Option<Vec<String>>,
}

impl TableFormatter {
    /// Create a new table formatter
    pub fn new(
        format: OutputFormat,
        no_color: bool,
        template: Option<String>,
        columns: Option<Vec<String>>,
    ) -> Self {
        Self {
            _theme: ColorTheme::default(),
            format: format.resolve_for_stdout(),
            no_color,
            template,
            columns,
        }
    }
```

Add the selection resolver as a private method (below `format_as_raw` is fine):

```rust
    /// Resolve the `--columns` selection against `headers`, case-insensitively.
    /// `Ok(None)` = no selection requested (hide-empty behavior applies).
    fn selected_indices(&self, headers: &[String]) -> Result<Option<Vec<usize>>> {
        let Some(requested) = &self.columns else {
            return Ok(None);
        };
        let mut indices = Vec::with_capacity(requested.len());
        for want in requested {
            match headers.iter().position(|h| h.eq_ignore_ascii_case(want)) {
                Some(i) => indices.push(i),
                None => {
                    return Err(crate::error::CrosstacheError::invalid_argument(format!(
                        "unknown column '{want}'; available: {}",
                        headers.join(", ")
                    )))
                }
            }
        }
        Ok(Some(indices))
    }

    /// Build a `Table` from `data`. With an explicit `--columns` selection the
    /// requested columns are projected in order (explicit selection wins over
    /// empty-column hiding); otherwise all-empty columns are omitted.
    fn build_table<T: Tabled>(&self, data: &[T]) -> Result<Table> {
        let headers: Vec<String> = T::headers().iter().map(|h| h.to_string()).collect();
        let rows: Vec<Vec<String>> = data
            .iter()
            .map(|item| item.fields().iter().map(|f| f.to_string()).collect())
            .collect();
        let keep = match self.selected_indices(&headers)? {
            Some(selection) => selection,
            None => visible_column_indices(headers.len(), &rows),
        };
        let mut builder = tabled::builder::Builder::default();
        builder.push_record(keep.iter().map(|&i| headers[i].clone()));
        for row in &rows {
            builder.push_record(keep.iter().map(|&i| row[i].clone()));
        }
        Ok(builder.build())
    }
```

Delete the free function `table_hiding_empty_columns` (its body moved into `build_table`; keep `visible_column_indices` and its doc comment). In `format_as_table`, replace:

```rust
        let mut table = table_hiding_empty_columns(data);
```

with:

```rust
        let mut table = self.build_table(data)?;
```

Same one-line replacement in `format_as_plain`.

Rewrite `format_as_csv` to project the selection:

```rust
    /// Format data as CSV
    fn format_as_csv<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let headers: Vec<String> = T::headers().iter().map(|h| h.to_string()).collect();
        let selection = self.selected_indices(&headers)?;

        let mut writer = csv::WriterBuilder::new()
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(Vec::new());

        let header_record: Vec<&str> = match &selection {
            Some(keep) => keep.iter().map(|&i| headers[i].as_str()).collect(),
            None => headers.iter().map(|h| h.as_str()).collect(),
        };
        writer.write_record(&header_record).map_err(|err| {
            crate::error::CrosstacheError::SerializationError(format!("CSV error: {err}"))
        })?;

        for item in data {
            let fields: Vec<String> = item.fields().iter().map(|f| f.to_string()).collect();
            let record: Vec<&str> = match &selection {
                Some(keep) => keep.iter().map(|&i| fields[i].as_str()).collect(),
                None => fields.iter().map(|f| f.as_str()).collect(),
            };
            writer.write_record(&record).map_err(|err| {
                crate::error::CrosstacheError::SerializationError(format!("CSV error: {err}"))
            })?;
        }

        let bytes = writer.into_inner().map_err(|err| {
            crate::error::CrosstacheError::SerializationError(format!("CSV error: {}", err.error()))
        })?;

        String::from_utf8(bytes).map_err(|err| {
            crate::error::CrosstacheError::SerializationError(format!(
                "CSV output was not UTF-8: {err}"
            ))
        })
    }
```

JSON/YAML/Template/Raw paths: unchanged (they ignore `--columns` — full schema). Note the empty-data branch of `format_table` already routes CSV through `format_as_csv`, so empty CSV headers get projected (and unknown names error) automatically.

- [ ] **Step 4: `Config.runtime_columns` in `src/config/settings.rs`**

After the `format_explicit` field:

```rust
    /// Parsed global `--columns` selection (set in `Cli::execute`, not persisted).
    /// Applies to table/plain/csv renders of every `TableFormatter` consumer.
    #[serde(skip)]
    #[tabled(skip)]
    pub runtime_columns: Option<Vec<String>>,
```

In `impl Default for Config`, after `format_explicit: false,`:

```rust
            runtime_columns: None,
```

In `src/config/init.rs`, all three exhaustive `Config { ... }` literals (search for `runtime_output_format:` — the local-backend setup, the AWS setup, and the interactive-init builder) get `runtime_columns: None,` added after their `format_explicit: false,` line. (`src/cli/system_ops.rs` tests and `tests/*.rs` use `..Default::default()` and need nothing.)

- [ ] **Step 5: CLI flag + dispatch in `src/cli/commands.rs`**

In the `Cli` struct, after the `template` field:

```rust
    /// Comma-separated column names for table/plain/csv output, applied in the
    /// given order (case-insensitive, e.g. --columns Name,Updated).
    /// JSON/YAML/template ignore it. Unknown names error.
    #[arg(long, global = true, value_name = "COLS", hide = should_hide_options())]
    pub columns: Option<String>,
```

In `Cli::execute`, immediately after `config.template = self.template.clone();`:

```rust
        // Parse the global --columns selection (empty segments dropped;
        // an all-empty value behaves like no flag).
        config.runtime_columns = self.columns.as_deref().and_then(|raw| {
            let cols: Vec<String> = raw
                .split(',')
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            if cols.is_empty() {
                None
            } else {
                Some(cols)
            }
        });
```

- [ ] **Step 6: Mechanical call-site updates (35 sites)**

Every existing `TableFormatter::new(a, b, c)` becomes 4-arg. Rule: pass `config.runtime_columns.clone()` where a runtime `Config` is in scope; pass `None` where there is none (manager-internal legacy display paths and unit tests):

- `config.runtime_columns.clone()` (12 sites): `src/cli/vault_ops.rs` ~112 (`VaultCommands::Info` trait arm), ~383 (`render_vault_list`), ~1250 (`VaultShareCommands::List`); `src/cli/secret_ops.rs` ~415 (machine `ls` path), ~457 (legacy-table `ls` branch), ~758 (history), ~3280 and ~3325 (dead legacy `execute_secret_list` — still must compile until Task 7 deletes it), ~3954 (`share list`); `src/cli/file_ops.rs` ~639, ~663, ~703 (`display_file_list_items` — these three collapse to one in Task 6; update all now anyway).
- `None` (6 sites): `src/vault/manager.rs` ~134, ~292, ~460; `src/secret/manager.rs` ~1973, ~2054, ~2104 (manager-internal formatting without Config access; the live CLI paths above are the ones `--columns` serves).
- `None` (17 test sites): every `TableFormatter::new(` in `src/utils/format.rs`'s `mod tests` gains a trailing `, None` (except the new tests from Step 1, already 4-arg).

Verify none were missed: `cargo check` (the compiler enforces the arity).

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: all green including the 5 new `columns_*` tests.
Run: `cargo run --quiet -- ls --columns Name,Updated --format table 2>/dev/null | head -4`
Expected: a two-column table (Name, Updated).
Run: `cargo run --quiet -- ls --columns Bogus --format table 2>&1 | tail -1`
Expected: error mentioning `unknown column 'Bogus'; available: Name, Note, Folder, Groups, Updated` (exit code 2).
Run: `cargo run --quiet -- ls --columns Name --format json 2>/dev/null | head -3`
Expected: full-schema JSON objects (columns ignored for JSON).

- [ ] **Step 8: Commit**

```bash
cargo fmt && git add src/utils/format.rs src/config/settings.rs src/config/init.rs src/cli/commands.rs src/cli/vault_ops.rs src/cli/secret_ops.rs src/cli/file_ops.rs src/vault/manager.rs src/secret/manager.rs && git commit -m "feat: global --columns selection in TableFormatter (table/plain/csv)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: `xv audit` → `TableFormatter` (`AuditRow`, `--raw` deprecation, machine valid-empty)

**Files:**
- Modify: `src/cli/system_ops.rs` — `execute_audit_command` (rendering half, from the `if logs.is_empty()` check through the `raw`/table blocks) and `execute_backend_audit` (same shape); delete `truncate_column`; add `AuditRow` + `render_audit_rows`
- Modify: `src/cli/commands.rs` — the `Commands::Audit` variant's `raw` field (~line 793-795)

**Interfaces:**
- Consumes: Task 1's `count_label(…, "audit log entry", "audit log entries", …)`, Task 2's 4-arg `TableFormatter::new` + `config.runtime_columns`.
- Produces (internal to `system_ops.rs`): `struct AuditRow { timestamp, operation, resource, caller, status: String }` (Tabled + Serialize, headers `Timestamp/Operation/Resource/Caller/Status`) and `fn render_audit_rows(rows: &[AuditRow], raw: bool, config: &Config) -> Result<()>`.

- [ ] **Step 1: Deprecate the `--raw` flag in `src/cli/commands.rs`**

Replace in the `Commands::Audit` variant:

```rust
        /// Show raw Azure Activity Log output
        #[arg(long)]
        raw: bool,
```

with:

```rust
        /// Deprecated: use the global --format json
        #[arg(long, hide = true)]
        raw: bool,
```

- [ ] **Step 2: Add `AuditRow` + `render_audit_rows` in `src/cli/system_ops.rs`**

Place directly above `execute_audit_command`:

```rust
/// One audit event as rendered by every output format. Machine formats emit
/// exactly these five fields (the pre-unification `--raw` per-entry documents
/// with `---` separators are gone — changelog-documented breaking change).
#[derive(tabled::Tabled, serde::Serialize)]
struct AuditRow {
    #[tabled(rename = "Timestamp")]
    timestamp: String,
    #[tabled(rename = "Operation")]
    operation: String,
    #[tabled(rename = "Resource")]
    resource: String,
    #[tabled(rename = "Caller")]
    caller: String,
    #[tabled(rename = "Status")]
    status: String,
}

/// Render audit rows through the shared TableFormatter: global `--format`
/// honored (JSON = array of rows), `--columns`/`--no-color` inherited, valid
/// empty machine output, human count/empty on stderr.
fn render_audit_rows(rows: &[AuditRow], raw: bool, config: &Config) -> Result<()> {
    use crate::utils::format::{OutputFormat, TableFormatter};

    let fmt = if raw {
        output::warn("--raw is deprecated; use the global --format json");
        OutputFormat::Json
    } else {
        config.runtime_output_format
    };
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if rows.is_empty() {
        if human_table_like {
            output::info(&crate::utils::list_output::empty_state_message(
                "audit log entries",
                None,
            ));
        } else {
            // Valid-empty machine output on stdout (e.g. `[]` for JSON).
            println!("{}", formatter.format_table(rows)?);
        }
        return Ok(());
    }

    if human_table_like {
        output::info(&format!(
            "{}:",
            crate::utils::list_output::count_label(
                rows.len(),
                rows.len(),
                "audit log entry",
                "audit log entries",
                None,
                false
            )
        ));
    }
    println!("{}", formatter.format_table(rows)?);
    if human_table_like {
        output::hint("Use --operation <type> to filter by operation type");
    }
    Ok(())
}
```

- [ ] **Step 3: Rewire `execute_audit_command` (Azure Activity Log path)**

Keep everything through the `logs.retain(...)` operation filter. First, keep stdout clean for machine formats — the two contextual blocks that `println!` to stdout become stderr info. Replace:

```rust
    let mut logs = if let Some(secret_name) = name {
        println!("  Secret: {}", secret_name);
        println!("  Vault: {}", vault_name);
```

with:

```rust
    let mut logs = if let Some(secret_name) = name {
        output::info(&format!("  Secret: {}", secret_name));
        output::info(&format!("  Vault: {}", vault_name));
```

and the `else` branch's `println!("  Vault: {}", vault_name);` with `output::info(&format!("  Vault: {}", vault_name));`.

Then DELETE everything from `if logs.is_empty() {` down to (and including) the closing brace of the `if raw { ... } else { ... }` rendering block (the empty-check, the blank `println!()`, the count `output::info`, the raw per-entry JSON loop, the hand-rolled `{:<20} | {:<25} | ...` table, its truncation logic, and the trailing `--raw` hint) and replace with:

```rust
    let rows: Vec<AuditRow> = logs
        .iter()
        .map(|log| {
            // Resource name: keep the last path segment, as before.
            let resource_display = log
                .resource_name
                .split('/')
                .next_back()
                .unwrap_or(&log.resource_name);
            AuditRow {
                timestamp: log.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                operation: log.operation.clone(),
                resource: resource_display.to_string(),
                caller: log.caller.clone(),
                status: log.status.clone(),
            }
        })
        .collect();

    render_audit_rows(&rows, raw, &config)
```

(The function ends there — remove the now-unreachable trailing `Ok(())` if one remains.)

- [ ] **Step 4: Rewire `execute_backend_audit` the same way**

Replace its contextual stdout prints. Change:

```rust
    let mut events: Vec<crate::backend::AuditEvent> = if let Some(secret_name) = name {
        println!("  Secret: {}", secret_name);
        println!("  Vault: {}", vault_name);
```

to:

```rust
    let mut events: Vec<crate::backend::AuditEvent> = if let Some(secret_name) = name {
        output::info(&format!("  Secret: {}", secret_name));
        output::info(&format!("  Vault: {}", vault_name));
```

and the `else` branch's `println!("  Vault: {}", vault_name);` to `output::info(&format!("  Vault: {}", vault_name));`. Then delete from `if events.is_empty() {` through the end of its `if raw { ... } else { ... }` block and replace with:

```rust
    let rows: Vec<AuditRow> = events
        .iter()
        .map(|event| AuditRow {
            timestamp: event.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            operation: event.operation.clone(),
            resource: event.resource_name.clone(),
            caller: event.caller.clone(),
            status: event.status.clone(),
        })
        .collect();

    render_audit_rows(&rows, raw, &config)
```

Update the function's doc comment (it currently promises "mirroring the Azure Activity Log output shapes (table and `--raw` JSON)") to:

```rust
/// Render audit logs fetched through the backend-agnostic [`AuditBackend`]
/// trait via the shared `AuditRow` renderer (global `--format` honored).
```

Then delete the now-unused `fn truncate_column` and remove any now-unused imports (`cargo clippy` will flag them). Note the human timestamp format changes from `%m-%d %H:%M:%S` to `%Y-%m-%d %H:%M:%S` (one row type feeds all formats — changelog wording in Task 8 covers audit wholesale).

- [ ] **Step 5: Verify**

Run: `cargo test --lib`
Expected: green.
Run: `cargo run --quiet -- audit --days 1 --format json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(type(d).__name__)'`
Expected: `list` (array even when empty — valid-empty machine output).
Run: `cargo run --quiet -- audit --days 7 --format table 2>/dev/null | head -4`
Expected: rounded `TableFormatter` table with `Timestamp | Operation | Resource | Caller | Status` headers (or, if no entries, empty stdout with `No audit log entries found.` on stderr).
Run: `cargo run --quiet -- audit --days 1 --raw 2>&1 >/dev/null | grep -c "deprecated"`
Expected: `1` (deprecation warning on stderr); stdout of the same command is a JSON array.
(If the machine lacks audit-log permissions, record these as blocked-by-environment in the report rather than skipping silently.)

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add src/cli/system_ops.rs src/cli/commands.rs && git commit -m "feat: route xv audit through TableFormatter; deprecate --raw to a hidden --format json alias

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `xv find` → `TableFormatter` (`FindRow`, CSV gain, envelope normalization, machine valid-empty)

**Files:**
- Modify: `src/cli/secret_ops.rs` — add `FindRow` + `find_empty_message` + `render_find_matches`; rewire the trait-path rendering in `execute_secret_find_direct` (~lines 1839-1872) and the legacy Azure path in `execute_secret_find` (~lines 2025-2089)
- Modify: `src/utils/fuzzy.rs` — delete `score_bar` (~line 129) and its `mod score_bar_tests` (~lines 251-274)

**Interfaces:**
- Consumes: Task 2's 4-arg `TableFormatter::new`; `crate::utils::fuzzy::Match<'_>` (`{ item: &CandidateItem, score: u32 }`; `CandidateItem { name: String, folder: Option<String>, groups: Option<String>, .. }`).
- Produces (internal to `secret_ops.rs`):
  - `struct FindRow { name, score, folder, groups: String }` — Tabled headers `Name/Score/Folder/Groups`, serde keys `name/score/folder/groups` (lowercase, matching the old envelope's keys; `score` becomes a 2-decimal string, `folder`/`groups` become `""` instead of `null` — changelog-documented).
  - `fn find_empty_message(pattern: Option<&str>, all_vaults: bool, vault_name: Option<&str>) -> String`
  - `fn render_find_matches(matches: &[crate::utils::fuzzy::Match<'_>], format: crate::utils::format::OutputFormat, empty_msg: &str, config: &Config) -> Result<()>`

- [ ] **Step 1: Add the row type and shared renderer**

Place directly above `execute_secret_find_direct` in `src/cli/secret_ops.rs`:

```rust
/// One `xv find` result as rendered by every output format. Serde keys match
/// the pre-unification JSON envelope (`name`/`score`/`folder`/`groups`);
/// `score` is a 2-decimal string for stable CSV/table output and `folder`/
/// `groups` are empty strings instead of null (changelog-documented).
#[derive(tabled::Tabled, serde::Serialize)]
struct FindRow {
    #[tabled(rename = "Name")]
    #[serde(rename = "name")]
    name: String,
    #[tabled(rename = "Score")]
    #[serde(rename = "score")]
    score: String,
    #[tabled(rename = "Folder")]
    #[serde(rename = "folder")]
    folder: String,
    #[tabled(rename = "Groups")]
    #[serde(rename = "groups")]
    groups: String,
}

/// Empty-state wording for `xv find`, shared by the trait and legacy paths.
fn find_empty_message(pattern: Option<&str>, all_vaults: bool, vault_name: Option<&str>) -> String {
    match (all_vaults, pattern, vault_name) {
        (true, Some(p), _) => format!("No secrets match '{p}' across all vaults."),
        (true, None, _) => "No secrets found across all vaults.".to_string(),
        (false, Some(p), Some(v)) => format!("No secrets match '{p}' in vault '{v}'."),
        (false, None, Some(v)) => format!("No secrets in vault '{v}'."),
        (false, _, None) => "No matching secrets found.".to_string(),
    }
}

/// Render find matches through the shared TableFormatter: all formats work
/// (CSV included), `--columns`/`--no-color` inherited, machine formats emit
/// valid-empty output on stdout when nothing matched.
fn render_find_matches(
    matches: &[crate::utils::fuzzy::Match<'_>],
    format: crate::utils::format::OutputFormat,
    empty_msg: &str,
    config: &Config,
) -> Result<()> {
    let rows: Vec<FindRow> = matches
        .iter()
        .map(|m| FindRow {
            name: m.item.name.clone(),
            score: format!("{:.2}", m.score as f64),
            folder: m.item.folder.clone().unwrap_or_default(),
            groups: m.item.groups.clone().unwrap_or_default(),
        })
        .collect();

    let fmt = format.resolve_for_stdout();
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    if rows.is_empty() && human_table_like {
        output::info(empty_msg);
        return Ok(());
    }

    let formatter = crate::utils::format::TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );
    // Non-empty rows render for every format; empty rows reach here only on
    // machine formats, where format_table emits valid-empty output.
    println!("{}", formatter.format_table(&rows)?);
    Ok(())
}
```

- [ ] **Step 2: Rewire the trait path in `execute_secret_find_direct`**

The single-vault branch resolves `vault_name` inside its own scope; hoist it so the empty message can use it. Replace:

```rust
        let items: Vec<CandidateItem> = if all_vaults {
```

with:

```rust
        let mut scope_vault: Option<String> = None;
        let items: Vec<CandidateItem> = if all_vaults {
```

and in that `else` branch, after `let vault_name = resolve_vault_for_trait(&config, registry).await?;`, add:

```rust
            scope_vault = Some(vault_name.clone());
```

Then replace the whole rendering tail — from `let resolved = format.resolve_for_stdout();` through the `} else { for m in &matches { println!(...) } }` block, ending just before `return Ok(());` — with:

```rust
        let empty_msg = find_empty_message(pattern.as_deref(), all_vaults, scope_vault.as_deref());
        render_find_matches(&matches, format, &empty_msg, &config)?;
        return Ok(());
```

(The `if names_only { ... }` early-return above it stays exactly as-is.)

- [ ] **Step 3: Rewire the legacy Azure path in `execute_secret_find`**

Keep everything through `matches.truncate(limit);` and the `if names_only { ... }` early return. Replace the entire rendering tail — from `// Format-aware rendering.` / `let resolved = format.resolve_for_stdout();` through the hand-rolled `println!("{:<40}  {:<10}  {:<24}  GROUPS", ...)` loop and its `Ok(())` — with:

```rust
    let empty_msg = find_empty_message(pattern, all_vaults, single_vault.as_deref());
    render_find_matches(&matches, format, &empty_msg, config)
```

This deletes: the `serde_json::json!` envelope block, the four inline empty-message `output::info` calls, the `use crate::utils::fuzzy::score_bar;` import, and the UPPERCASE header + score-bar loop.

- [ ] **Step 4: Delete `score_bar`**

In `src/utils/fuzzy.rs`, delete `pub fn score_bar` (and its doc comment) and the whole `mod score_bar_tests` block. Grep to confirm no consumers remain:

Run: `rg -n "score_bar" src/`
Expected: no matches.

- [ ] **Step 5: Verify**

Run: `cargo test --lib`
Expected: green (score_bar tests removed, no orphaned imports — clippy in Task 8 backstops).
Run: `cargo run --quiet -- find a --format json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(type(d).__name__, sorted(d[0].keys()) if d else "empty")'`
Expected: `list ['folder', 'groups', 'name', 'score']` (score values are strings like `"142.00"`).
Run: `cargo run --quiet -- find a --format csv 2>/dev/null | head -2`
Expected: `Name,Score,Folder,Groups` header + one data row (CSV newly works).
Run: `cargo run --quiet -- find zzz-no-such-pattern-zzz --format json 2>/dev/null`
Expected: `[]` on stdout.
Run: `cargo run --quiet -- find a --format table 2>/dev/null | head -3`
Expected: rounded table with `Name | Score | Folder | Groups` (no score bar).
Run: `cargo run --quiet -- find a --names-only 2>/dev/null | head -2`
Expected: bare names, unchanged.

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add src/cli/secret_ops.rs src/utils/fuzzy.rs && git commit -m "feat: route xv find through TableFormatter (standard row shape, CSV support)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `context list` + `env list` + `config show` off the free fn; delete `format_table()`

**Files:**
- Modify: `src/cli/config_ops.rs` — `execute_config_show` (~37-176), `execute_config_show_resolved` (machine/table branch, ~593-628), `execute_context_list` (~1063-1135), `execute_env_list` (~1334-1428)
- Modify: `src/cli/secret_ops.rs` — `execute_secret_parse` table branch (~3838-3847); delete the dead legacy `execute_secret_history` (`#[allow(dead_code)]`, ~2122-2183)
- Modify: `src/utils/format.rs` — delete the `pub fn format_table(mut table: Table, no_color: bool) -> String` free function (~438-449)

**Interfaces:**
- Consumes: Task 2's 4-arg `TableFormatter::new` + `config.runtime_columns`; `crate::utils::list_output::empty_state_message`.
- Produces: `xv context list --format json|yaml|csv` works; `xv env list --format json|yaml|csv` works via a new `EnvRow { Name, Active, Backend, Vault, Resource Group }`; `config show` human table goes through `TableFormatter` while `--format json|yaml` still serialize the whole `Config`; zero remaining `format_table(` free-fn consumers.

- [ ] **Step 1: `execute_config_show`**

Add `serde::Serialize` to the local `ConfigItem` derive:

```rust
    #[derive(Tabled, serde::Serialize)]
    struct ConfigItem {
```

Remove `use crate::utils::format::format_table;` and the `Table` import (keep `Tabled`). Replace the output block:

```rust
    if config.output_json {
        let json_output = serde_json::to_string_pretty(config).map_err(|e| {
            CrosstacheError::serialization(format!("Failed to serialize config: {e}"))
        })?;
        println!("{json_output}");
    } else {
        let table = Table::new(&items);
        println!("{}", format_table(table, config.no_color));
    }
```

with:

```rust
    // Documented exception: json/yaml serialize the whole Config object
    // (resource view, not a list); only the human table goes through
    // TableFormatter so --columns/--no-color behave uniformly.
    use crate::utils::format::OutputFormat;
    match config.runtime_output_format {
        OutputFormat::Json | OutputFormat::Auto => {
            let json_output = serde_json::to_string_pretty(config).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize config: {e}"))
            })?;
            println!("{json_output}");
        }
        OutputFormat::Yaml => {
            let yaml_output = serde_yaml::to_string(config).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize config: {e}"))
            })?;
            println!("{yaml_output}");
        }
        fmt => {
            let formatter = crate::utils::format::TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            println!("{}", formatter.format_table(&items)?);
        }
    }
```

(`runtime_output_format` is always resolved by dispatch; the `Auto` arm is a JSON-safe fallback for programmatic construction.)

- [ ] **Step 2: `execute_config_show_resolved`**

Its `Row` struct already derives `Tabled + serde::Serialize`, and its machine output is the rows array (not the whole Config) — keep that. Remove `use crate::utils::format::format_table;` and the `Table` import. Replace the output block (`if config.output_json { ... } else { ... }`) with:

```rust
    use crate::utils::format::OutputFormat;
    match config.runtime_output_format {
        OutputFormat::Json | OutputFormat::Auto => {
            let json = serde_json::to_string_pretty(&rows).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize resolved config: {e}"))
            })?;
            println!("{json}");
        }
        OutputFormat::Yaml => {
            let yaml = serde_yaml::to_string(&rows).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize resolved config: {e}"))
            })?;
            println!("{yaml}");
        }
        fmt => {
            if let Some(p) = &project_path {
                println!("Project config: {}", p.display());
            } else {
                println!("Project config: (none — no .xv.toml found)");
            }
            let formatter = crate::utils::format::TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            println!("{}", formatter.format_table(&rows)?);
            println!();
            println!("Precedence (highest → lowest):");
            println!("  backend         : --backend flag > .xv.toml profile > XV_BACKEND / global config > built-in (azure)");
            println!("  env             : XV_ENV > --env flag > .xv.toml default_env");
            println!("  vault           : --vault arg > .xv.toml profile.vault > context > global default_vault");
            println!("  resource_group  : --resource-group > .xv.toml profile.resource_group > context > global default_resource_group");
            println!();
            println!("Naming convention: the global config uses a `default_` prefix (default_vault,");
            println!("default_resource_group) because those values are fallbacks; a .xv.toml env");
            println!("profile uses the bare name (vault, resource_group) because it sets a specific");
            println!(
                "value that overrides the global default. Same concept, the prefix signals the layer."
            );
            if !resolution_notes.is_empty() {
                println!();
                println!("Layer notes:");
                resolution_notes.sort();
                resolution_notes.dedup();
                for note in resolution_notes {
                    println!("  - {note}");
                }
            }
        }
    }
```

(Everything in the human arm from `println!();` down is today's chrome verbatim — only the two lines `let table = Table::new(&rows);` / `println!("{}", format_table(table, config.no_color));` changed to the `formatter` pair, and the whole block moved inside the `match` arm.)

- [ ] **Step 3: `execute_context_list`**

Rewrite the function (same data, new renderer, machine formats supported):

```rust
async fn execute_context_list(config: &Config) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::format::OutputFormat;
    use tabled::Tabled;

    let context_manager = ContextManager::load().await.unwrap_or_default();

    #[derive(Tabled, serde::Serialize)]
    struct ContextItem {
        #[tabled(rename = "Status")]
        status: String,
        #[tabled(rename = "Vault")]
        vault: String,
        #[tabled(rename = "Resource Group")]
        resource_group: String,
        #[tabled(rename = "Last Used")]
        last_used: String,
        #[tabled(rename = "Usage Count")]
        usage_count: String,
    }

    let mut items = Vec::new();

    // Add current context
    if let Some(ref context) = context_manager.current {
        items.push(ContextItem {
            status: "● Current".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    // Add recent contexts
    for context in context_manager.list_recent() {
        // Skip if it's the current context
        if let Some(ref current) = context_manager.current {
            if current.vault_name == context.vault_name {
                continue;
            }
        }

        items.push(ContextItem {
            status: "  Recent".to_string(),
            vault: context.vault_name.clone(),
            resource_group: context.resource_group.as_deref().unwrap_or("-").to_string(),
            last_used: context.last_used.format("%Y-%m-%d %H:%M").to_string(),
            usage_count: context.usage_count.to_string(),
        });
    }

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = crate::utils::format::TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if items.is_empty() {
        if human_table_like {
            output::info(&crate::utils::list_output::empty_state_message(
                "vault contexts",
                None,
            ));
            output::hint("Use 'xv context use <vault-name>' to create a context");
        } else {
            println!("{}", formatter.format_table(&items)?);
        }
        return Ok(());
    }

    println!("{}", formatter.format_table(&items)?);
    if human_table_like {
        println!("\nScope: {}", context_manager.scope_description());
        if ContextManager::local_context_exists() {
            println!("Note: Local context file found in current directory (.xv/context)");
        }
    }

    Ok(())
}
```

(The `● Current` glyph stays — it serializes as text. The old early `recent.is_empty() && current.is_none()` check is subsumed by `items.is_empty()`.)

- [ ] **Step 4: `execute_env_list`**

Replace the per-env `println!` loop with an `EnvRow` table. Keep the `No .xv.toml found...` early return exactly as-is (it is a config-missing diagnostic with an actionable path, not a list-empty — Phase A precedent). Keep the resolution comments. The function becomes:

```rust
async fn execute_env_list(config: &Config) -> Result<()> {
    use crate::config::project;
    use crate::utils::format::OutputFormat;

    let cwd = std::env::current_dir()?;
    let Some((path, cfg)) = project::find_project_config(&cwd).await? else {
        output::info(&format!(
            "No .xv.toml found from {}. Create one with: xv context init",
            cwd.display()
        ));
        return Ok(());
    };

    let active = project::resolve_env(&cfg, config.env_flag.as_deref())
        .ok()
        .map(|(name, _)| name.to_string());

    use crate::config::project::resolve_effective_backend;
    // Precedence for every row mirrors `resolve_effective_backend`:
    //   cli_backend (--backend / XV_BACKEND via clap) > profile.backend > global.
    // `cli_backend` is the raw flag/env snapshot; `disk_backend` is the global
    // config value taken BEFORE main.rs folded the active env's profile in
    // (using `effective_backend_name()` here would make inactive envs inherit
    // the active env's backend). A `None` profile backend falls through to the
    // global layer rather than silently defaulting to "azure".
    let cli_backend = config.cli_backend.as_deref();
    let global_backend = config.disk_backend.as_deref();

    #[derive(tabled::Tabled, serde::Serialize)]
    struct EnvRow {
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Active")]
        active: String,
        #[tabled(rename = "Backend")]
        backend: String,
        #[tabled(rename = "Vault")]
        vault: String,
        #[tabled(rename = "Resource Group")]
        resource_group: String,
    }

    let rows: Vec<EnvRow> = cfg
        .envs
        .iter()
        .map(|(name, profile)| {
            let resolved =
                resolve_effective_backend(cli_backend, profile.backend.as_deref(), global_backend);
            // "(inherited)" marks rows whose env profile set no `backend` of
            // its own — the displayed value came from outside the profile.
            // Keys strictly on the profile field: the CLI override is populated
            // from XV_BACKEND even when --backend is absent.
            let backend_note = if profile.backend.is_none() {
                " (inherited)"
            } else {
                ""
            };
            EnvRow {
                name: name.clone(),
                active: if active.as_deref() == Some(name.as_str()) {
                    "*".to_string()
                } else {
                    String::new()
                },
                backend: format!("{resolved}{backend_note}"),
                vault: profile
                    .vault
                    .clone()
                    .unwrap_or_else(|| "(unset)".to_string()),
                resource_group: profile.resource_group.clone().unwrap_or_default(),
            }
        })
        .collect();

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = crate::utils::format::TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if rows.is_empty() {
        if human_table_like {
            output::info(&crate::utils::list_output::empty_state_message(
                "environments",
                None,
            ));
        } else {
            println!("{}", formatter.format_table(&rows)?);
        }
        return Ok(());
    }

    if human_table_like {
        let default_label = cfg
            .default_env
            .as_deref()
            .map(|d| format!(", default: {d}"))
            .unwrap_or_default();
        println!("Project envs (from {}{}):", path.display(), default_label);
    }
    println!("{}", formatter.format_table(&rows)?);

    // Summary: what the active env actually resolves to right now, after full
    // precedence (this is the "effective profile" §P2-4 asks for). Only shown
    // when an env is active so single-env or no-active-env cases stay terse.
    if human_table_like {
        if let Some(active_name) = &active {
            if let Some(profile) = cfg.envs.get(active_name) {
                let eff_backend = resolve_effective_backend(
                    cli_backend,
                    profile.backend.as_deref(),
                    global_backend,
                );
                // Vault resolution must mirror Config::resolve_vault_name and
                // `config show --resolved`: profile.vault > context vault >
                // global default_vault.
                let context_manager = crate::config::ContextManager::load()
                    .await
                    .unwrap_or_default();
                let eff_vault = profile
                    .vault
                    .as_deref()
                    .or_else(|| context_manager.current_vault())
                    .or(if config.default_vault.is_empty() {
                        None
                    } else {
                        Some(config.default_vault.as_str())
                    })
                    .unwrap_or("(unset)");
                println!();
                println!("Effective ({active_name}): backend={eff_backend}  vault={eff_vault}");
            }
        }
    }
    output::hint(
        "`context envs` lists .xv.toml env profiles, not the vault context; run `xv config show --resolved` to see the effective backend/vault after env → context → global fallbacks.",
    );
    Ok(())
}
```

(`context envs` delegates to this function unchanged — its dedup/deprecation is P3.)

- [ ] **Step 5: Migrate `execute_secret_parse`'s table branch and delete the dead legacy history fn** (`src/cli/secret_ops.rs`)

In `execute_secret_parse`, replace the `"table"` arm's body:

```rust
        "table" => {
            if components.is_empty() {
                println!("No components found in connection string");
            } else {
                let formatter = crate::utils::format::TableFormatter::new(
                    crate::utils::format::OutputFormat::Table,
                    config.no_color,
                    None,
                    None,
                );
                println!("{}", formatter.format_table(&components)?);
            }
        }
```

(`ConnectionComponent` in `src/secret/manager.rs` already derives `Serialize` + `Tabled` — its JSON branch serializes it. `parse`'s own `--fmt` stays a file-format arg, untouched per Phase A. `--columns` intentionally not wired here: `parse` is not a list command.)

Delete the entire dead legacy `async fn execute_secret_history(...)` (marked `#[allow(dead_code)] // legacy non-trait impl, superseded by backend-trait path`, defines a local `VersionInfo` struct and calls `format_table`) — it is the last `format_table` free-fn consumer in this file.

- [ ] **Step 6: Delete the free function** (`src/utils/format.rs`)

Delete:

```rust
/// Convenience function for formatting a table with default settings
pub fn format_table(mut table: Table, no_color: bool) -> String {
    ...
}
```

Grep for stragglers:

Run: `rg -n "format::format_table|format_table\(table" src/`
Expected: no matches (the remaining `format_table` hits are all the `TableFormatter::format_table` method).

- [ ] **Step 7: Verify**

Run: `cargo test --lib`
Expected: green.
Run: `cargo run --quiet -- context list --format json 2>/dev/null | python3 -c 'import json,sys; json.load(sys.stdin); print("valid")'`
Expected: `valid`.
Run: `cargo run --quiet -- env list --format yaml 2>/dev/null | python3 -c 'import yaml,sys; yaml.safe_load(sys.stdin); print("valid")'` (run from a directory with a `.xv.toml`, e.g. the repo root if one exists; otherwise note blocked-by-environment)
Expected: `valid`.
Run: `cargo run --quiet -- config show --format json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print("object" if isinstance(d, dict) else "WRONG")'`
Expected: `object` (whole-Config exception preserved).
Run: `cargo run --quiet -- config show 2>/dev/null | head -3`
Expected: rounded `TableFormatter` table with `Setting | Value | Source`.
Run: `cargo run --quiet -- context list --columns Vault 2>/dev/null | head -4`
Expected: single-column table (proves `--columns` inheritance).

- [ ] **Step 8: Commit**

```bash
cargo fmt && git add src/cli/config_ops.rs src/cli/secret_ops.rs src/utils/format.rs && git commit -m "feat: route context list, env list, and config show through TableFormatter; drop format_table free fn

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: `file list` CSV normalization + `history` machine valid-empty

**Files:**
- Modify: `src/cli/file_ops.rs` — `display_file_list_items` (~533-710: delete `FileListCsvRow`, unify on a `FileRow` with a leading `Kind` column)
- Modify: `src/cli/secret_ops.rs` — `execute_secret_history_direct` empty branch (~754-755)

**Interfaces:**
- Consumes: Task 2's 4-arg `TableFormatter::new`; Task 1's plural count line (already in place in this function).
- Produces: `file list` Table/Plain/CSV/Template all render `Kind, Name, Size, Content-Type, Modified, Groups` rows; JSON/YAML keep the rich `BlobListItem` serialization (full-fidelity formats); empty `history` machine output is valid-empty.

- [ ] **Step 1: Unify the `file list` row set**

In `display_file_list_items`, delete the `FileListCsvRow` struct entirely and replace the `ListItem` struct with:

```rust
    // One display row set for every non-JSON/YAML format (spec: file list CSV
    // becomes the table's column set + a leading Kind column). JSON/YAML keep
    // the rich BlobListItem serialization as the full-fidelity formats.
    #[derive(Tabled, Serialize)]
    struct FileRow {
        #[tabled(rename = "Kind")]
        kind: String,
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Size")]
        size: String,
        #[tabled(rename = "Content-Type")]
        content_type: String,
        #[tabled(rename = "Modified")]
        modified: String,
        #[tabled(rename = "Groups")]
        groups: String,
    }
```

Build the rows once, before the `match fmt` (replacing the two duplicated `display_items` mapping blocks and the CSV mapping block):

```rust
    let rows: Vec<FileRow> = items
        .iter()
        .map(|item| match item {
            BlobListItem::Directory { name, .. } => FileRow {
                kind: "directory".to_string(),
                name: name.clone(),
                size: "<DIR>".to_string(),
                content_type: "-".to_string(),
                modified: "-".to_string(),
                groups: "-".to_string(),
            },
            BlobListItem::File(file) => FileRow {
                kind: "file".to_string(),
                name: file.name.clone(),
                size: format_size(file.size),
                content_type: file.content_type.clone(),
                modified: file.last_modified.format("%Y-%m-%d %H:%M:%S").to_string(),
                groups: file.groups.join(", "),
            },
        })
        .collect();
```

Collapse the `match fmt` to:

```rust
    match fmt {
        OutputFormat::Json => {
            let json_output = serde_json::to_string_pretty(items).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
            })?;
            output.push_str(&json_output);
        }
        OutputFormat::Yaml => {
            let yaml_output = serde_yaml::to_string(items).map_err(|e| {
                CrosstacheError::serialization(format!("Failed to serialize items: {e}"))
            })?;
            output.push_str(&yaml_output);
        }
        OutputFormat::Auto => unreachable!("resolve_for_stdout must not return Auto"),
        _ => {
            // Table / Plain / Raw / Csv / Template share the FileRow set.
            let formatter = TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
                config.runtime_columns.clone(),
            );
            output.push_str(&formatter.format_table(&rows)?);

            if human_table_like {
                let file_count = items
                    .iter()
                    .filter(|i| matches!(i, BlobListItem::File(_)))
                    .count();
                let dir_count = items
                    .iter()
                    .filter(|i| matches!(i, BlobListItem::Directory { .. }))
                    .count();

                output.push('\n');
                let mut count_line = crate::utils::list_output::count_label(
                    file_count, file_count, "file", "files", None, false,
                );
                if !recursive && dir_count > 0 {
                    let _ = write!(
                        count_line,
                        ", {}",
                        crate::utils::list_output::pluralize(
                            dir_count,
                            "directory",
                            "directories"
                        )
                    );
                }
                let _ = writeln!(output, "{}", count_line);
            }
        }
    }
```

(The count line stays human-table-like-only, matching today: CSV and Template never carried it. The existing top-of-function `items.is_empty() && human_table_like` empty branch stays as-is — machine formats fall through and serialize valid-empty. The human table gains the `Kind` column — changelog-noted in Task 8.)

- [ ] **Step 2: `history` machine valid-empty** (`src/cli/secret_ops.rs`, `execute_secret_history_direct`)

Replace the empty branch:

```rust
        if versions.is_empty() {
            output::info(&format!("No version history for '{name}'"));
        } else {
```

with:

```rust
        if versions.is_empty() {
            let fmt = config.runtime_output_format;
            if matches!(
                fmt,
                OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
            ) {
                output::info(&format!("No version history for '{name}'"));
            } else {
                // Valid-empty machine output on stdout (e.g. `[]` for JSON).
                use crate::utils::format::TableFormatter;
                let formatter = TableFormatter::new(
                    fmt,
                    config.no_color,
                    config.template.clone(),
                    config.runtime_columns.clone(),
                );
                println!("{}", formatter.format_table(&versions)?);
            }
        } else {
```

- [ ] **Step 3: Verify**

Run: `cargo test --lib` and `cargo test --test file_commands_tests`
Expected: green (report honestly if the integration suite needs storage config not present).
Run: `cargo run --quiet -- file list --format csv 2>/dev/null | head -2`
Expected: header `Kind,Name,Size,Content-Type,Modified,Groups` (no more snake_case kitchen-sink).
Run: `cargo run --quiet -- file list --format json 2>/dev/null | head -3`
Expected: rich `BlobListItem` objects, unchanged.
Run: `cargo run --quiet -- file list 2>/dev/null | head -4`
Expected: table with a leading `Kind` column.
Run: `cargo run --quiet -- history xv-definitely-no-such-secret --format json 2>/dev/null` — expect `[]` on stdout if the backend returns an empty version list; if it errors with secret-not-found instead, verify with an existing secret name that HAS versions plus a truly empty case on the local backend, and record what was observed.
(If no storage account is configured, record the `file list` commands as blocked-by-environment rather than skipping silently.)

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/file_ops.rs src/cli/secret_ops.rs && git commit -m "fix: normalize file list CSV to the table column set; valid-empty machine output for history

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Delete legacy `execute_secret_list` + `secret_count_label`

**Files:**
- Modify: `src/cli/secret_ops.rs` — delete `execute_secret_list` (~3146-3331, `#[allow(dead_code)] // legacy non-trait impl, superseded by backend-trait path`, ~185 lines including its two `#[allow(...)]` attributes), delete `fn secret_count_label` (~259-275), delete the test `test_secret_count_label_distinguishes_paginated_total` (~4815-4822)

**Interfaces:**
- Consumes: nothing.
- Produces: nothing — pure deletion of the dead path that embodied the pre-unification conventions (stdout empty-states, `"(s)"` counts).

- [ ] **Step 1: Confirm both are dead before deleting**

Run: `rg -n "execute_secret_list\b|secret_count_label" src/ tests/`
Expected: matches only inside `src/cli/secret_ops.rs` — the two definitions, `execute_secret_list`'s attribute lines, `secret_count_label`'s two call sites inside `execute_secret_list`, and the one test. (`execute_secret_list_direct` is a different, live function — do not touch it.)

- [ ] **Step 2: Delete**

Delete, in `src/cli/secret_ops.rs`:
1. The whole `fn secret_count_label(...) -> String` (with its blank-line padding).
2. The whole `async fn execute_secret_list(...) -> Result<Vec<crate::secret::manager::SecretSummary>>` including its `#[allow(clippy::too_many_arguments)]` and `#[allow(dead_code)]` attributes.
3. The whole `#[test] fn test_secret_count_label_distinguishes_paginated_total() { ... }` in the tests module.

- [ ] **Step 3: Verify**

Run: `rg -n "secret_count_label|fn execute_secret_list\b" src/`
Expected: no matches.
Run: `cargo test --lib`
Expected: green (no dangling references; clippy in Task 8 backstops unused imports freed by the deletion — remove any it flags, e.g. if `TableFormatter`/`paginate_slice` imports were only used by the deleted fn they are still used elsewhere in this file, so likely nothing).

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/secret_ops.rs && git commit -m "fix: delete dead legacy execute_secret_list and secret_count_label

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: CHANGELOG, README, gates, e2e

**Files:**
- Modify: `CHANGELOG.md` (extend `## Unreleased` — breaking machine-shape changes MUST be spelled out)
- Modify: `README.md` (global CLI flags table ~line 1155; `xv find` format comment ~line 563)

**Interfaces:**
- Consumes: everything from Tasks 1-7.
- Produces: release notes + fully gated branch.

- [ ] **Step 1: CHANGELOG**

Merge into the existing `## Unreleased` sections (add these bullets; keep the Phase A bullets already there):

Under `### Added`:

```markdown
- **Global `--columns <COLS>` flag returns** (removed as a silent no-op in the P0 pass): comma-separated, case-insensitive column names applied in the given order to `table`/`plain`/`csv` output of every list command. Unknown names error and list the available columns. Explicit `--columns` overrides the hide-empty-columns behavior; JSON/YAML/template keep the full schema.
- **`xv find --format csv`** now works (previously find had no CSV output).
- **`xv context list` and `xv env list` honor the global `--format`** (json/yaml/csv/…): `context list` rows are `{status, vault, resource_group, last_used, usage_count}`; `env list` renders `Name/Active/Backend/Vault/Resource Group` rows instead of a hand-rolled line format.
- **`xv config show --format yaml`** serializes the whole `Config` object (like `--format json` always did — `config show` is a resource view, not a list; this documented exception is the one command whose machine output is not the table's row set).
```

Under `### Changed` (breaking machine shapes — the point of unification):

```markdown
- **BREAKING (machine shapes normalized).** Pre-1.0 breaking changes, deliberate and grouped here:
  - **`xv find`**: JSON/YAML output is now the standard row shape — `score` is a two-decimal string (was a raw integer) and `folder`/`groups` are empty strings (were `null`). The TTY output is the shared rounded table; the score bar and UPPERCASE header are gone. `--names-only` unchanged.
  - **`xv audit`**: honors the global `--format` (JSON = one array of `{timestamp, operation, resource, caller, status}` rows). `--raw` is deprecated to a hidden alias that warns and implies `--format json`; its old per-entry documents with `---` separators (and rich fields like `correlation_id`/`properties`) are no longer emitted. The contextual `Vault:`/`Secret:` lines moved to stderr so `xv audit --format json | jq` sees pure JSON, and the human timestamp is now full-date (`%Y-%m-%d %H:%M:%S`).
  - **`xv file list --format csv`**: columns now match the table — `Kind,Name,Size,Content-Type,Modified,Groups` (was a snake_case kitchen-sink set with raw byte sizes, etags, and JSON-blob metadata columns). JSON/YAML keep the rich full-fidelity serialization. The human table gains the leading `Kind` column.
- **Counts are plural-aware**: `1 vault`, `3 vaults`, `5 audit log entries` — the `"N noun(s)"` style from the previous pass is gone.
- **`xv config show` human table** renders through the shared formatter (uniform `--columns`/`--no-color` behavior); same for `config show --resolved`.
```

Under `### Fixed`:

```markdown
- **Empty `history`, `find`, and `audit` machine-format output is now valid-empty** (`[]` for JSON, headers-only for CSV) on stdout instead of nothing, so `| jq` works on empty results. Same for empty `context list`/`env list` machine output.
```

Under `### Removed`:

```markdown
- Dead legacy `execute_secret_list` renderer and its `secret_count_label` helper; the `format_table()` free function (all tables now go through `TableFormatter`); the `xv find` score bar.
```

- [ ] **Step 2: README**

In the global CLI flags table (search `### Global CLI flags`), add after the `--format` row:

```markdown
| `--columns <COLS>` | Comma-separated column names for `table`/`plain`/`csv` output, in order (case-insensitive, e.g. `--columns Name,Updated`); unknown names error |
```

Update the `xv find` example comment (search `xv find db --format json`):

```markdown
xv find db --format json                 # [{name, score, folder, groups}] — score is a "NN.00" string
xv find db --format csv                  # Name,Score,Folder,Groups
```

Check `rg -n -- '--raw' README.md` — the hits are all `xv get --raw` (a different flag), leave them; if any `xv audit --raw` example exists, change it to `xv audit --format json`.

- [ ] **Step 3: Gates**

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test --lib
cargo test
```
Expected: all clean/green, clippy 0 warnings (the full suite needs Azure creds on this machine; report failures honestly).

- [ ] **Step 4: E2E (read-only against the configured backend)**

```bash
cargo run --quiet -- audit --days 7 --format json 2>/dev/null | python3 -c 'import json,sys; print(type(json.load(sys.stdin)).__name__)'   # list
cargo run --quiet -- find a --format csv 2>/dev/null | head -2                       # Name,Score,Folder,Groups header
cargo run --quiet -- context list --format json 2>/dev/null | python3 -m json.tool | head -3
cargo run --quiet -- env list --format yaml 2>/dev/null | head -5                    # parses as YAML (needs a .xv.toml)
cargo run --quiet -- ls --columns Name,Updated --format table 2>/dev/null | head -4  # exactly two columns
cargo run --quiet -- ls --columns Bogus 2>&1 | tail -1                               # unknown-column error listing available
cargo run --quiet -- find zzz-nope --format json 2>/dev/null                         # []
cargo run --quiet -- config show --format json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print("object" if isinstance(d,dict) else "WRONG")'
cargo run --quiet -- vault list --format table 2>/dev/null | tail -1                 # plural count, e.g. "3 vaults"
```

Capture actual outputs in the task report; record blocked-by-environment cases explicitly.

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add CHANGELOG.md README.md && git commit -m "docs: changelog and README for the renderer unification pass

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec coverage map

| Spec section | Task |
|---|---|
| §1 `--columns` global flag, `Config.runtime_columns`, `TableFormatter` selection (table/plain/csv, case-insensitive, order, unknown-name error, hide-empty interplay, JSON/YAML/Template ignore) | Task 2 |
| §2 `audit` → `AuditRow` + global `--format`, `--raw` hidden deprecated alias implying `--format json` | Task 3 |
| §2 `find` → `FindRow` (2-decimal score string, score bar dropped), envelope normalization, CSV gain, `--names-only` unchanged | Task 4 |
| §2 `context list` → `ContextItem` gains Serialize + `TableFormatter` (`● Current` glyph kept) | Task 5 |
| §2 `env list` → `EnvRow { Name, Active, Backend, Vault, Resource Group }` | Task 5 |
| §2 `config show` human table via `TableFormatter`; `--format json|yaml` whole-`Config` exception | Task 5 |
| §2 `file list` CSV = table column set + leading `Kind`; JSON/YAML rich serialization kept | Task 6 |
| §2 `format_table()` free fn deleted after migrations (stragglers: `config show --resolved`, `parse`, dead legacy history — all handled) | Task 5 |
| §3 machine valid-empty for `history`, `find`, `audit` (human behavior unchanged) | Task 6 (history), Task 4 (find), Task 3 (audit) |
| §4 plural-aware `count_label` (singular + plural), all Phase A adopters, `empty_state_message` unchanged | Task 1 |
| §5 legacy `execute_secret_list` + `secret_count_label` (+ its test) deleted | Task 7 |
| Decisions: machine shapes normalize now, changelog-documented | Task 8 + Global Constraints |
| Decisions: `vault share list --fmt` NOT removed | Global Constraints (no task touches it) |
| Testing: unit (`--columns` projection/case/unknown/hide-empty interplay, plural `count_label`) | Tasks 1-2 |
| Testing: behavioral commands + `config show --format json` object check | Task 8 Step 4 (+ per-task verify steps) |
| Testing: gates (`fmt --check`, `clippy --all-targets` 0 warnings, `test --lib`, full `test`) | Task 8 Step 3 |
