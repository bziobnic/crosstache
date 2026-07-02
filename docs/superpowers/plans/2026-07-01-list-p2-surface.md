# List Command P2 Surface-Consistency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Phase A of the P2 spec (`docs/superpowers/specs/2026-07-01-list-p2-surface-design.md`): unify list-command flags (`--format`/`--fmt`/`--pager`/`--names-only`), add a global `--no-color`, and standardize empty-states and counts through a new `src/utils/list_output.rs` helper module.

**Architecture:** Small pure conventions module + per-command mechanical edits. No renderer restructuring, no machine-output schema changes except the bug-fix class: empty machine output becomes valid-empty (`[]`) on stdout instead of a stderr-only message.

**Tech Stack:** Rust, `clap` derive, existing `TableFormatter`/`Pagination`/`output` helpers. No new dependencies.

## Global Constraints

- Branch: `list-p2-surface` (exists, spec committed).
- Machine output shapes are byte-identical after this phase EXCEPT: empty machine-format output for `vault list`, `vault share list`, and `file list` becomes valid-empty (`[]`/format-appropriate) on stdout (bug fix per spec).
- Human empty-states go to stderr via `crate::utils::output::info`; counts appear on stdout for human formats only; machine formats never carry counts or info messages on stdout.
- `--fmt` on `vault export/import`, `env pull`, `parse` is untouched (file formats, not output rendering).
- Every commit: `cargo fmt` first; messages `feat:`/`fix:`/`docs:` ending with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Line numbers cited below may drift a few lines — locate anchors by symbol/string, not absolute position.

---

### Task 1: `src/utils/list_output.rs` conventions module

**Files:**
- Create: `src/utils/list_output.rs`
- Modify: `src/utils/mod.rs` (register `pub mod list_output;` alongside the other modules)

**Interfaces:**
- Consumes: nothing.
- Produces (all later tasks rely on these exact signatures):
  - `pub fn empty_state_message(noun_plural: &str, scope: Option<&str>) -> String` — `("vaults", None)` → `"No vaults found."`; `("secrets", Some("folder 'prod'"))` → `"No secrets found in folder 'prod'."`
  - `pub fn count_label(displayed: usize, total: usize, noun: &str, scope: Option<&str>, paginated: bool) -> String` — `(3, 3, "vault", None, false)` → `"3 vault(s)"`; `(10, 42, "secret", Some("vault 'kv'"), true)` → `"Showing 10 of 42 secret(s) in vault 'kv'"`

- [ ] **Step 1: Write the failing tests**

Create `src/utils/list_output.rs`:

```rust
//! Shared wording for list-command empty-states and count lines.
//!
//! Every list-style command routes its human empty-state and count text
//! through these helpers so the wording cannot drift per-command again.
//! Streams are the caller's job: empty-states go to stderr via
//! `output::info`, counts go to stdout on human formats only.

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
    fn count_unpaginated_unscoped() {
        assert_eq!(count_label(3, 3, "vault", None, false), "3 vault(s)");
    }

    #[test]
    fn count_unpaginated_scoped() {
        assert_eq!(
            count_label(65, 65, "secret", Some("vault 'kv'"), false),
            "65 secret(s) in vault 'kv'"
        );
    }

    #[test]
    fn count_paginated() {
        assert_eq!(
            count_label(10, 42, "secret", Some("vault 'kv'"), true),
            "Showing 10 of 42 secret(s) in vault 'kv'"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib utils::list_output`
Expected: compile error — `empty_state_message` / `count_label` not found.

- [ ] **Step 3: Implement**

Add above the tests module:

```rust
/// "No <nouns> found[ in <scope>]." — scope is pre-formatted, e.g. "vault 'kv'".
pub fn empty_state_message(noun_plural: &str, scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("No {noun_plural} found in {scope}."),
        None => format!("No {noun_plural} found."),
    }
}

/// "N <noun>(s)[ in <scope>]", or "Showing X of Y <noun>(s)[ in <scope>]"
/// when paginated. Matches the pre-existing "(s)" pluralization style.
pub fn count_label(
    displayed: usize,
    total: usize,
    noun: &str,
    scope: Option<&str>,
    paginated: bool,
) -> String {
    let base = if paginated {
        format!("Showing {displayed} of {total} {noun}(s)")
    } else {
        format!("{displayed} {noun}(s)")
    };
    match scope {
        Some(scope) => format!("{base} in {scope}"),
        None => base,
    }
}
```

Register in `src/utils/mod.rs`: add `pub mod list_output;` in alphabetical position among the existing `pub mod` lines.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib utils::list_output`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/utils/list_output.rs src/utils/mod.rs && git commit -m "feat: add shared list empty-state and count wording helpers

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Global `--no-color` flag

**Files:**
- Modify: `src/cli/commands.rs` — `Cli` struct (global args block, near the `format`/`template` fields ~line 122-190) and the dispatch resolution block (search for `config.format_explicit =`, ~line 1385)
- Modify: `README.md` — global-options table (the one listing `--format`, `--credential-type`, `--template`, `--env`)

**Interfaces:**
- Consumes: `Config.no_color` (exists; env `NO_COLOR` already sets it in `load_from_env`).
- Produces: `xv --no-color <any command>` forces `config.no_color = true` (highest priority in the CLI > env > file hierarchy).

- [ ] **Step 1: Add the flag**

In the `Cli` struct, after the `template` field:

```rust
    /// Disable colored output (same effect as the NO_COLOR env var)
    #[arg(long, global = true, hide = should_hide_options())]
    pub no_color: bool,
```

In the dispatch resolution block (where `config.runtime_output_format` and `config.format_explicit` are assigned), add:

```rust
        if self.no_color {
            config.no_color = true;
        }
```

- [ ] **Step 2: README**

Add a row to the global-options table (next to `--format`):

```markdown
| `--no-color` | Disable colored output (same effect as the `NO_COLOR` env var) |
```

- [ ] **Step 3: Verify**

Run: `cargo check`
Expected: clean.
Run: `cargo run --quiet -- --no-color context list 2>/dev/null | grep -c $'\x1b'; true`
Expected: `0`.
Run: `cargo run --quiet -- --format table ls 2>/dev/null | grep -c $'\x1b'; true` (no `--no-color`)
Expected: non-zero (color still on by default) — confirms the flag defaults off.

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/commands.rs README.md && git commit -m "feat: add global --no-color flag

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: `xv vault list` — drop local `--format`, add `--names-only`, conventions

**Files:**
- Modify: `src/cli/commands.rs` — `VaultCommands::List` variant (~line 936: the `format: OutputFormat` field)
- Modify: `src/cli/vault_ops.rs` — the `VaultCommands::List` dispatch arm (~line 168) and `execute_vault_list` (~line 369; it has a cached branch and a fresh branch, each with its own empty-check and table rendering)

**Interfaces:**
- Consumes: Task 1's `empty_state_message` / `count_label`; global `config.runtime_output_format` (already TTY-resolved at dispatch).
- Produces: `xv vault list` honoring only the global `--format`; `--names-only` printing one vault name per line.

- [ ] **Step 1: Variant changes**

Delete from the `VaultCommands::List` variant:

```rust
        /// Output format (default: auto = table on TTY, json for pipes/redirects)
        #[arg(long, value_enum, default_value = "auto")]
        format: OutputFormat,
```

Add (after `resource_group`):

```rust
        /// Print one name per line, no headers, no ANSI. Pipe-friendly.
        /// Overrides --format and disables auto-format-resolution.
        #[arg(long)]
        names_only: bool,
```

- [ ] **Step 2: Rework `execute_vault_list`**

Update the dispatch arm to stop passing `format` and pass `names_only`. Change `execute_vault_list`'s signature: remove `format: OutputFormat`, add `names_only: bool`. Inside:

- Replace `let output_format = format.resolve_for_stdout();` with `let output_format = config.runtime_output_format;`.
- In BOTH the cached and fresh branches, apply this shared rendering (extract a local helper inside `vault_ops.rs` to avoid duplicating it):

```rust
fn render_vault_list(
    vaults: &[crate::vault::models::VaultSummary],
    output_format: crate::utils::format::OutputFormat,
    pagination: crate::utils::pagination::Pagination,
    pager: bool,
    names_only: bool,
    config: &Config,
) -> Result<()> {
    use crate::utils::format::{OutputFormat, TableFormatter};
    use crate::utils::list_output::{count_label, empty_state_message};
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};

    if names_only {
        for v in vaults {
            println!("{}", v.name);
        }
        return Ok(());
    }

    let human_table_like = matches!(
        output_format,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = TableFormatter::new(output_format, config.no_color, config.template.clone());

    if vaults.is_empty() {
        if human_table_like {
            crate::utils::output::info(&empty_state_message("vaults", None));
        } else {
            println!("{}", formatter.format_table(vaults)?);
        }
        return Ok(());
    }

    let page = paginate_slice(vaults, pagination);
    let mut output = formatter.format_table(&page.items)?;
    if human_table_like {
        output.push('\n');
        output.push_str(&count_label(
            page.items.len(),
            page.total_items,
            "vault",
            None,
            page.page_size.is_some(),
        ));
    }
    if let Some(footer) = pagination_footer_text(&page, "vault", output_format) {
        output.push('\n');
        output.push_str(&footer);
    }
    crate::utils::pager::print_output(&output, pager)?;
    Ok(())
}
```

Both branches then reduce to fetching `Vec<VaultSummary>` (cached or fresh, with the existing cache-store behavior kept) and calling `render_vault_list(&vaults, output_format, pagination, pager, names_only, config)`. Check `VaultSummary`'s name field is `name` (see `src/vault/models.rs:235` region) — if the field differs (e.g. `vault_name`), use the actual field.

- [ ] **Step 3: Verify**

Run: `cargo test --lib` — green.
Run: `cargo run --quiet -- vault list --names-only 2>/dev/null | head -3` — one name per line.
Run: `cargo run --quiet -- vault list --format json 2>/dev/null | python3 -c 'import json,sys; print(type(json.load(sys.stdin)).__name__)'` — `list` (global format flag now serves vault list).
Run: `cargo run --quiet -- vault list 2>&1 >/dev/null | head -1` with a resource group that has no vaults if available; otherwise skip and note — empty-state to stderr.

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/commands.rs src/cli/vault_ops.rs && git commit -m "feat: unify vault list format flag, add --names-only and standard count

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `xv vault share list` — deprecate `--fmt`, fix machine empty-state, add count

**Files:**
- Modify: `src/cli/commands.rs` — `VaultShareCommands::List` variant (~line 1104: the `-f/--fmt` field)
- Modify: `src/cli/vault_ops.rs` — the `VaultShareCommands::List` arm (~line 1173-1226)

**Interfaces:**
- Consumes: Task 1 helpers; `config.runtime_output_format`.
- Produces: global `--format` drives output; hidden `--fmt` warns for one release.

- [ ] **Step 1: Variant change**

Replace:

```rust
        /// Output format
        #[arg(
            short = 'f',
            long = "fmt",
            default_value = "auto",
            id = "share_list_format"
        )]
        format: crate::utils::format::OutputFormat,
```

with:

```rust
        /// Deprecated: use the global --format
        #[arg(long = "fmt", hide = true, id = "share_list_format")]
        format: Option<crate::utils::format::OutputFormat>,
```

(The `-f` short is removed outright; `--fmt json` keeps working with a warning.)

- [ ] **Step 2: Rework the arm**

At the top of the `VaultShareCommands::List` arm body, resolve the format:

```rust
            let fmt = match format {
                Some(f) => {
                    crate::utils::output::warn("--fmt is deprecated; use the global --format");
                    f.resolve_for_stdout()
                }
                None => config.runtime_output_format,
            };
            let human_table_like = matches!(
                fmt,
                crate::utils::format::OutputFormat::Table
                    | crate::utils::format::OutputFormat::Plain
                    | crate::utils::format::OutputFormat::Raw
            );
```

Then replace the body's rendering half (from `if roles.is_empty()` to the end of the arm) with:

```rust
            let formatter = crate::utils::format::TableFormatter::new(
                fmt,
                config.no_color,
                config.template.clone(),
            );

            if roles.is_empty() {
                if human_table_like {
                    output::info(&format!(
                        "No access assignments found for vault '{vault_name}'"
                    ));
                } else {
                    println!("{}", formatter.format_table(&paged.items)?);
                }
            } else {
                let table_output = formatter.format_table(&paged.items)?;
                let mut output = String::new();
                if human_table_like {
                    let _ = writeln!(output, "Access assignments for vault '{vault_name}':");
                }
                output.push_str(&table_output);
                if human_table_like {
                    output.push('\n');
                    output.push_str(&crate::utils::list_output::count_label(
                        paged.items.len(),
                        paged.total_items,
                        "assignment",
                        None,
                        paged.page_size.is_some(),
                    ));
                }
                if let Some(footer) = pagination_footer_text(&paged, "assignment", fmt) {
                    output.push('\n');
                    output.push_str(&footer);
                }
                crate::utils::pager::print_output(&output, pager)?;
            }
```

(Header gating widens from `== Table` to all human table-like formats, matching `xv share list`. The existing pre-rendering half of the arm — role fetch, `resolve_and_filter_roles`, pagination — stays as-is.)

- [ ] **Step 3: Verify**

Run: `cargo test --lib` — green.
Run: `cargo run --quiet -- vault share list <vault> --format json 2>/dev/null | python3 -c 'import json,sys; json.load(sys.stdin); print("valid")'` — `valid` (use the configured default vault name from `xv context show`).
Run: `cargo run --quiet -- vault share list <vault> --fmt json 2>&1 >/dev/null | head -1` — deprecation warning on stderr.

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/commands.rs src/cli/vault_ops.rs && git commit -m "fix: deprecate vault share list --fmt, emit valid-empty machine output, add count

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `xv file list` — `--pager [WHEN]`, `--names-only`, conventions

**Files:**
- Modify: `src/cli/file.rs` — `FileCommands::List` variant (~line 66-98)
- Modify: `src/cli/file_ops.rs` — the `FileCommands::List` dispatch arm (~line 184), the empty-state (`output::info("No files found")`, ~line 540), and the count block (`Total files:` / `Total: {} directories, {} files`, ~line 662-675)

**Interfaces:**
- Consumes: Task 1 helpers; `crate::cli::commands::PagerWhen`.
- Produces: `--pager [auto|always|never]` matching every other list command; `--names-only` printing one file name per line (recursive semantics).

- [ ] **Step 1: Variant changes**

Replace:

```rust
        /// Use an interactive pager for TTY output
        #[arg(long)]
        pager: bool,
```

with:

```rust
        /// Use an interactive pager for output. Optional WHEN is auto (default
        /// when the flag is given), always, or never. e.g. `--pager` or `--pager auto`.
        #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "auto")]
        pager: Option<crate::cli::commands::PagerWhen>,
```

Add after `recursive`:

```rust
        /// Print one file name per line, no headers, no ANSI. Pipe-friendly.
        /// Lists recursively; directory entries are omitted.
        #[arg(long)]
        names_only: bool,
```

- [ ] **Step 2: Dispatch + execution changes** (`src/cli/file_ops.rs`)

In the `FileCommands::List` arm, convert the pager at the boundary and thread `names_only`:

```rust
            let pager = pager
                .map(crate::cli::commands::PagerWhen::wants_pager)
                .unwrap_or(false);
```

Thread `names_only: bool` into `execute_file_list` (add parameter). Inside `execute_file_list`:

- Where the listing call chooses hierarchical vs recursive (the existing `recursive` argument to the blob listing), pass `recursive || names_only` so names-only always lists the full subtree.
- Immediately after items are fetched (before any display/formatting), add:

```rust
    if names_only {
        for item in &items {
            if let BlobListItem::File(file) = item {
                println!("{}", file.name);
            }
        }
        return Ok(());
    }
```

(Adapt the exact iteration to the local variable/type names; the enum is `BlobListItem::{File, Directory}` as used in `display_file_list_items`.)

- [ ] **Step 3: Conventions** (`src/cli/file_ops.rs`)

Empty-state (~line 540): the current `output::info("No files found")` short-circuits for every format. Gate it:

```rust
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    if items.is_empty() {
        if human_table_like {
            output::info(&crate::utils::list_output::empty_state_message("files", None));
        } else {
            // fall through to the format match, which serializes the empty
            // items list as valid-empty machine output ([] for JSON)
        }
    }
```

Adapt to the function's actual control flow: the goal is human-empty → stderr info and return; machine-empty → continue into the existing `match fmt` so JSON/YAML/CSV serialize the empty list. (If `fmt` is resolved later in the function, hoist the resolution above the empty check.)

Count block (~line 662-675): replace the three-way `Total files:` / `Total: X directories, Y files` writeln block with:

```rust
            output.push('\n');
            let mut count_line = crate::utils::list_output::count_label(
                file_count,
                file_count,
                "file",
                None,
                false,
            );
            if !recursive && dir_count > 0 {
                let _ = write!(count_line, ", {} directory(ies)", dir_count);
            }
            let _ = writeln!(output, "{}", count_line);
```

(`file_count`/`dir_count` already exist at that point; keep them.)

- [ ] **Step 4: Verify**

Run: `cargo test --lib` and `cargo test --test file_commands_tests` (file integration tests exist) — green; report honestly if the integration suite needs storage config not present.
Run: `cargo run --quiet -- file list --pager never 2>&1 | head -3` — parses, lists.
Run: `cargo run --quiet -- file list --names-only 2>/dev/null | head -3` — bare names.
Run: `cargo run --quiet -- file list --prefix xv-definitely-nonexistent --format json 2>/dev/null` — `[]`.
(If no storage account is configured on this machine, record the commands as blocked-by-environment in the report rather than skipping silently.)

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/cli/file.rs src/cli/file_ops.rs && git commit -m "feat: standard --pager and --names-only for file list, conventions wording

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Adopt conventions in `ls`, `history`, `audit`, `find`, `context list`

**Files:**
- Modify: `src/cli/secret_ops.rs` — `display_cached_secret_list` empty branch (~line 432-447) and count line (~line 500-512); history count (`"{} version(s) of '{name}'"`, ~line 765); find empty messages (~line 2046-2057)
- Modify: `src/cli/system_ops.rs` — audit count lines (~line 367 and ~line 475) and audit empty messages (search `"No audit log entries found"`)
- Modify: `src/cli/config_ops.rs` — context list empty (~line 1071-1072)

**Interfaces:**
- Consumes: Task 1 helpers.
- Produces: standardized wording; `xv ls` empty message moves to stderr (spec-mandated behavior change).

- [ ] **Step 1: `xv ls` empty-state to stderr** (`display_cached_secret_list`)

Replace the current empty branch (which pushes the message into the stdout `output` buffer):

```rust
    if scoped.subtree.is_empty() {
        let scope_desc = if !path.is_empty() {
            format!("folder '{path}'")
        } else {
            format!("vault '{vault_name}'")
        };
        let msg = if all {
            crate::utils::list_output::empty_state_message("secrets", Some(&scope_desc))
        } else {
            format!(
                "{} Use --all to show disabled secrets.",
                crate::utils::list_output::empty_state_message("enabled secrets", Some(&scope_desc))
            )
        };
        crate::utils::output::info(&msg);
        return Ok(());
    }
```

Notes: this branch no longer prints the `Vault:` header or anything to stdout — `xv ls > file` on an empty scope produces an empty file. Delete the now-dead `output.push_str(&output::format_line(...))` block. Move this empty check ABOVE the header-composition block (the `let mut output = String::new(); ... writeln!(output, "Vault: ...")` section) so no stdout bytes are emitted first. The wording produced: `No enabled secrets found in folder 'p'. Use --all to show disabled secrets.` — same as today except scope wording for the root case changes from "in vault." to "in vault '<name>'." (richer; changelog-noted).

- [ ] **Step 2: `xv ls` count via helper**

In the grid/long footer block, replace the `secret_count_label(...)` + `" in vault '{}'"` composition with:

```rust
    let mut count_line = crate::utils::list_output::count_label(
        page.items
            .iter()
            .filter(|e| matches!(e, LsEntry::Secret(_)))
            .count(),
        secret_count,
        "secret",
        None,
        page.page_size.is_some(),
    );
    if folder_count > 0 {
        let _ = write!(count_line, ", {} folder(s)", folder_count);
    }
    let _ = writeln!(output, "{} in vault '{}'", count_line, vault_name);
```

And in the legacy-table branch, replace `secret_count_label(...)` similarly with `count_label(page.items.len(), page.total_items, "secret", None, page.page_size.is_some())` inside the existing `"{} in vault '{}'"` writeln. Then delete the now-unused `secret_count_label` function and its tests if nothing else references it (grep first; `filter_secret_summaries_for_display` tests may share the module).

- [ ] **Step 3: history count to stdout, human-gated** (`secret_ops.rs` ~line 765)

The current line prints to stderr unconditionally:

```rust
            output::info(&format!("{} version(s) of '{name}'", versions.len()));
```

Replace with (only in the human path — check the surrounding code: if the function prints the table for all formats, gate on the resolved format):

```rust
            let fmt = config.runtime_output_format;
            if matches!(
                fmt,
                OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
            ) {
                println!(
                    "{} of '{name}'",
                    crate::utils::list_output::count_label(
                        versions.len(),
                        versions.len(),
                        "version",
                        None,
                        false
                    )
                );
            }
```

(Yields `N version(s) of 'name'` on stdout for humans; machine formats stay clean.)

- [ ] **Step 4: audit wording** (`system_ops.rs`)

Both count sites (`~367`, `~475`):

```rust
    output::info(&format!(
        "{}:\n",
        crate::utils::list_output::count_label(logs.len(), logs.len(), "audit log entry", None, false)
    ));
```

(second site uses `events.len()`). Both empty sites: replace the literal with `crate::utils::list_output::empty_state_message("audit log entries", None)` — note the criteria phrase is dropped by design (spec: wording normalized).

- [ ] **Step 5: find + context list wording polish**

- `secret_ops.rs` find empties (~2046-2057): keep the "match" wording (it's search-specific) but add terminal periods so all four variants end with `.` — e.g. `"No secrets match '{p}' in vault '{vault_name}'."` and `"No secrets found across all vaults."` (this one can use `empty_state_message("secrets", Some("any vault"))`? No — keep literal `"No secrets found across all vaults."`; the helper's "in" phrasing doesn't fit "across").
- `config_ops.rs` context list (~1071-1072): change `output::info("No vault contexts found")` to `output::info(&crate::utils::list_output::empty_state_message("vault contexts", None))` and change the following stdout `println!("Hint: ...")` to `output::hint("Use 'xv context use <vault-name>' to create a context")` so the hint also lands on stderr.
- `env list` (`config_ops.rs` ~1337): intentionally UNCHANGED — its "No .xv.toml found from {path}. Create one with: xv context init" message is a config-missing diagnostic (with an actionable path), not a list-empty; it already goes to stderr. Note this in the task report so the spec's adopters table is accounted for.

- [ ] **Step 6: Verify**

Run: `cargo test --lib` — green (fix any tests asserting the old wording; update expectations to the new strings, do not weaken assertions).
Run: `cargo run --quiet -- ls xv-no-such-folder --no-cache --format table > /tmp/p2-ls-empty.out 2>/tmp/p2-ls-empty.err; wc -c < /tmp/p2-ls-empty.out; cat /tmp/p2-ls-empty.err | head -1`
Expected: `0` bytes on stdout; stderr shows `No enabled secrets found in folder 'xv-no-such-folder'. Use --all to show disabled secrets.`
Run: `cargo run --quiet -- history <existing-secret> --format json 2>/dev/null | python3 -c 'import json,sys; json.load(sys.stdin); print("clean json")'` — `clean json` (no count line contaminating stdout).

- [ ] **Step 7: Commit**

```bash
cargo fmt && git add src/cli/secret_ops.rs src/cli/system_ops.rs src/cli/config_ops.rs && git commit -m "feat: standardize list empty-states and counts via list_output helpers

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: CHANGELOG, README, gates, e2e

**Files:**
- Modify: `CHANGELOG.md` (extend `## Unreleased`)
- Modify: `README.md` (only if Task 2's row and existing flag docs need reconciliation — check the `vault list` / `file list` sections for stale `--format`/`--pager` descriptions)

**Interfaces:**
- Consumes: everything from Tasks 1-6.
- Produces: release notes + verified branch.

- [ ] **Step 1: CHANGELOG**

Under `## Unreleased` add:

```markdown
### Added

- **Global `--no-color` flag** (complements the `NO_COLOR` env var and config key).
- **`--names-only` on `vault list` and `file list`** (one name per line, pipe-friendly; `file list --names-only` lists recursively).
- **`file list --pager [auto|always|never]`** matching every other list command (bare `--pager` unchanged).

### Changed

- **List empty-states now go to stderr** for human formats across all list commands (including `xv ls`, whose empty message previously landed on stdout — `xv ls > file` on an empty scope now writes an empty file), and empty-state/count wording is standardized via shared helpers. `xv history`'s count line moved from stderr to stdout (human formats only).
- **`vault share list -f/--fmt` is deprecated**: use the global `--format`. `--fmt` still works with a warning for one release; `-f` is removed. `vault list`'s redundant local `--format` was removed (the identical global flag takes over transparently).

### Fixed

- **Empty machine-format output is now valid-empty** (`[]` for JSON) on stdout for `vault list`, `vault share list`, and `file list`, instead of a stderr-only message that broke `| jq` on empty results.
```

- [ ] **Step 2: Gates**

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test --lib
cargo test
```
Expected: all clean/green (full suite needs the Azure creds on this machine; report failures honestly).

- [ ] **Step 3: E2E (read-only against the real vault)**

```bash
cargo run --quiet -- vault list --names-only | head -3
cargo run --quiet -- vault list --format yaml | head -3
cargo run --quiet -- vault share list "$(cargo run --quiet -- context show 2>/dev/null | grep -o 'kv-[a-z]*' | head -1)" --format json | python3 -m json.tool | head -3
cargo run --quiet -- ls xv-no-such-folder > /tmp/e2e-empty.out 2>/tmp/e2e-empty.err; echo "stdout_bytes=$(wc -c < /tmp/e2e-empty.out)"; head -1 /tmp/e2e-empty.err
cargo run --quiet -- --no-color --format table ls | grep -c $'\x1b'; true
cargo run --quiet -- history azure-client-id | tail -1
```

Capture actual outputs. `xv ls xv-no-such-folder` piped → machine format → expect `[]` on stdout (not zero bytes) — the zero-byte stdout check applies to `--format table` (Task 6 verified it); adjust expectations accordingly and record both.

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add CHANGELOG.md README.md && git commit -m "docs: changelog for list-command surface consistency pass

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec coverage map

| Spec section | Task |
|---|---|
| §1 vault list local `--format` removal + `--names-only` | Task 3 |
| §1 vault share list `--fmt` deprecation | Task 4 |
| §1 file list `--pager [WHEN]` + `--names-only` | Task 5 |
| §1 global `--no-color` | Task 2 |
| §2 conventions module | Task 1 |
| §2 adopters table (ls/vault/file/share/history/audit/find/context) | Tasks 3-6 |
| §2 machine valid-empty (vault list, vault share list, file list) | Tasks 3, 4, 5 |
| §2 `ls` stderr change | Task 6 |
| Testing + gates | per-task steps + Task 7 |
