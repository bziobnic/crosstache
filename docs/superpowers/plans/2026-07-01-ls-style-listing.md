# ls-Style Folder-Aware `xv ls` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the P1 spec (`docs/superpowers/specs/2026-07-01-ls-style-listing-design.md`): `xv ls [FOLDER]` with folder-first ls-style grid output by default, `-l` long mode, `-r` recursive flatten, unchanged machine-output schema.

**Architecture:** Folders stay a client-side view derived from the `folder` tag. A new pure module `src/cli/ls_view.rs` owns scoping (`scope_secrets`) and rendering (`render_grid`, `render_long`); `secret_ops.rs` gains only mode dispatch and parameter threading; `commands.rs` gains the positional arg, `-l`/`-r`, and a `format_explicit` bool on `Config` so an explicit `--format table` keeps the legacy rounded table.

**Tech Stack:** Rust, `clap` derive, `crossterm::terminal::size`, existing `TableFormatter`/`Pagination` helpers. No new dependencies.

## Global Constraints

- Branch: `ls-style-listing` (exists, spec committed). All work happens there.
- Machine-readable output (JSON/YAML/CSV/template, piped auto, `--names-only`) keeps today's flat `SecretSummary` schema — no folder pseudo-entries — scoped to the **recursive subtree** of the requested path.
- Empty scope exits 0: humans get a stderr message via `output` helpers, machine formats get valid-empty stdout (`[]`).
- Untrusted display text (names, groups, notes) must pass through `sanitize_control_chars` before reaching a TTY, matching the existing table renderer's escape-sequence neutralization.
- Every commit: `cargo fmt` first; messages `feat: …`/`docs: …` ending with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Folder paths: trailing `/` stripped then validated with the existing `crate::utils::helpers::validate_folder_path`; bare `/` or empty ⇒ root.

---

### Task 1: `ls_view` module — scoping logic

**Files:**
- Create: `src/cli/ls_view.rs`
- Modify: `src/cli/mod.rs` (add `pub(crate) mod ls_view;` after the `local_ops` line)
- Modify: `src/utils/format.rs` (make `sanitize_control_chars` `pub(crate)`)
- Modify: `src/cli/secret_ops.rs` (move `date_portion_for_display` + its two tests into `ls_view.rs`; re-import)

**Interfaces:**
- Consumes: `crate::secret::manager::SecretSummary` (fields: `name`, `original_name`, `note: Option<String>`, `folder: Option<String>`, `groups: Option<String>`, `updated_on: String`, `enabled`, `content_type`), `crate::utils::format::sanitize_control_chars`.
- Produces (used by Tasks 2, 3, 5):
  - `pub(crate) struct ScopedList { pub folders: Vec<String>, pub secrets: Vec<SecretSummary>, pub subtree: Vec<SecretSummary> }`
  - `pub(crate) fn scope_secrets(secrets: Vec<SecretSummary>, path: &str) -> ScopedList`
  - `pub(crate) fn display_name(s: &SecretSummary) -> &str`
  - `pub(crate) fn date_portion_for_display(timestamp: &str) -> String` (moved here verbatim from `secret_ops.rs`)

- [ ] **Step 1: Write the failing tests**

Create `src/cli/ls_view.rs` containing only the doc header, imports, and tests for now:

```rust
//! ls-style view logic for `xv ls`: folder scoping and grid/long rendering.
//!
//! Folders are a client-side view derived from each secret's hierarchical
//! `folder` tag (e.g. `prod/db`). Nothing here talks to a backend.

use crate::secret::manager::SecretSummary;

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, folder: Option<&str>) -> SecretSummary {
        SecretSummary {
            name: name.to_string(),
            original_name: name.to_string(),
            note: None,
            folder: folder.map(str::to_string),
            groups: None,
            updated_on: "2026-05-17 01:19:00 UTC".to_string(),
            enabled: true,
            content_type: "text/plain".to_string(),
        }
    }

    #[test]
    fn root_scope_partitions_folders_and_root_secrets() {
        let scoped = scope_secrets(
            vec![
                summary("root-a", None),
                summary("db-pass", Some("prod/db")),
                summary("api-key", Some("prod")),
                summary("dev-key", Some("dev")),
            ],
            "",
        );
        assert_eq!(scoped.folders, vec!["dev".to_string(), "prod".to_string()]);
        assert_eq!(scoped.secrets.len(), 1);
        assert_eq!(scoped.secrets[0].name, "root-a");
        assert_eq!(scoped.subtree.len(), 4);
    }

    #[test]
    fn nested_scope_shows_subfolders_and_direct_children() {
        let scoped = scope_secrets(
            vec![
                summary("db-pass", Some("prod/db")),
                summary("api-key", Some("prod")),
                summary("deep", Some("prod/db/replica")),
                summary("dev-key", Some("dev")),
            ],
            "prod",
        );
        assert_eq!(scoped.folders, vec!["db".to_string()]);
        assert_eq!(scoped.secrets.len(), 1);
        assert_eq!(scoped.secrets[0].name, "api-key");
        assert_eq!(scoped.subtree.len(), 3); // api-key, db-pass, deep
    }

    #[test]
    fn folder_prefix_requires_segment_boundary() {
        let scoped = scope_secrets(
            vec![
                summary("a", Some("prod")),
                summary("b", Some("production")),
            ],
            "prod",
        );
        assert_eq!(scoped.subtree.len(), 1);
        assert_eq!(scoped.subtree[0].name, "a");
        assert!(scoped.folders.is_empty());
    }

    #[test]
    fn empty_scope_yields_empty_lists() {
        let scoped = scope_secrets(vec![summary("a", Some("prod"))], "staging");
        assert!(scoped.folders.is_empty());
        assert!(scoped.secrets.is_empty());
        assert!(scoped.subtree.is_empty());
    }

    #[test]
    fn results_are_sorted_by_display_name() {
        let mut zebra = summary("zzz-internal", None);
        zebra.original_name = "aardvark".to_string();
        let scoped = scope_secrets(vec![summary("beta", None), zebra], "");
        assert_eq!(display_name(&scoped.secrets[0]), "aardvark");
        assert_eq!(display_name(&scoped.secrets[1]), "beta");
    }

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
        assert_eq!(date_portion_for_display("yesterday"), "yesterday");
        assert_eq!(date_portion_for_display("2026-5-7 01:19"), "2026-5-7 01:19");
        assert_eq!(date_portion_for_display("N/A"), "N/A");
        assert_eq!(date_portion_for_display(""), "");
    }
}
```

Add `pub(crate) mod ls_view;` to `src/cli/mod.rs` (after the `pub(crate) mod local_ops;` line).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib cli::ls_view`
Expected: compile error — `scope_secrets`, `display_name`, `date_portion_for_display` not found. Correct RED for missing functions.

- [ ] **Step 3: Implement**

Add above the tests module in `src/cli/ls_view.rs`:

```rust
/// The result of scoping a secret list to a folder path.
pub(crate) struct ScopedList {
    /// Immediate child folder segments, sorted, no trailing slash.
    pub folders: Vec<String>,
    /// Secrets whose folder tag equals the path exactly (direct children), sorted.
    pub secrets: Vec<SecretSummary>,
    /// Every secret at or under the path (recursive), sorted. Root path = all.
    pub subtree: Vec<SecretSummary>,
}

/// User-facing name: `original_name` when present, else the (sanitized) `name`.
pub(crate) fn display_name(s: &SecretSummary) -> &str {
    if s.original_name.is_empty() {
        &s.name
    } else {
        &s.original_name
    }
}

/// Partition `secrets` relative to folder `path` ("" = vault root).
/// A secret with folder tag F is in scope when F == path or F starts with
/// "path/" (segment boundary enforced); its next path segment becomes a
/// child folder entry.
pub(crate) fn scope_secrets(secrets: Vec<SecretSummary>, path: &str) -> ScopedList {
    let mut folders = std::collections::BTreeSet::new();
    let mut direct = Vec::new();
    let mut subtree = Vec::new();

    for s in secrets {
        let folder = s.folder.as_deref().unwrap_or("");
        let rel: Option<&str> = if path.is_empty() {
            Some(folder)
        } else if folder == path {
            Some("")
        } else {
            folder
                .strip_prefix(path)
                .and_then(|rest| rest.strip_prefix('/'))
        };
        let Some(rel) = rel else { continue };
        if rel.is_empty() {
            direct.push(s.clone());
        } else if let Some(segment) = rel.split('/').next() {
            folders.insert(segment.to_string());
        }
        subtree.push(s);
    }

    direct.sort_by(|a, b| display_name(a).cmp(display_name(b)));
    subtree.sort_by(|a, b| display_name(a).cmp(display_name(b)));

    ScopedList {
        folders: folders.into_iter().collect(),
        secrets: direct,
        subtree,
    }
}

/// Reduce a backend timestamp like "2026-05-17 01:19:00 UTC" to its date
/// portion for human tables. Values that don't lead with a YYYY-MM-DD token
/// pass through unmodified; machine formats always get the full timestamp.
pub(crate) fn date_portion_for_display(timestamp: &str) -> String {
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

Then three relocations:

1. In `src/utils/format.rs`, change `fn sanitize_control_chars` to `pub(crate) fn sanitize_control_chars` (Tasks 2–3 use it; no call-site changes needed).
2. In `src/cli/secret_ops.rs`, DELETE the `date_portion_for_display` function and its doc comment, and delete its two tests (`date_portion_truncates_standard_timestamp`, `date_portion_passes_through_nonstandard_values`) from the tests module — they now live in `ls_view.rs`.
3. In `src/cli/secret_ops.rs`, where `format_secret_list_rows_for_human` calls the helper, change the call to `crate::cli::ls_view::date_portion_for_display(&secret.updated_on)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cli::ls_view && cargo test --lib cli::secret_ops && cargo test --lib utils::format`
Expected: PASS — including the pre-existing secret_ops note-wrap tests and format sanitization tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/cli/ls_view.rs src/cli/mod.rs src/cli/secret_ops.rs src/utils/format.rs && git commit -m "feat: add ls_view folder scoping for xv ls

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `ls_view` — grid renderer

**Files:**
- Modify: `src/cli/ls_view.rs` (add types + `render_grid` + tests)

**Interfaces:**
- Consumes: Task 1's `ScopedList`, `display_name`, and `crate::utils::format::sanitize_control_chars`.
- Produces (used by Tasks 3, 5):
  - `pub(crate) enum LsEntry { Folder(String), Secret(SecretSummary) }`
  - `pub(crate) fn entries_for_display(scoped: &ScopedList) -> Vec<LsEntry>` (folders first, then direct secrets)
  - `pub(crate) fn render_grid(entries: &[LsEntry], width: usize, color: bool) -> String`

- [ ] **Step 1: Write the failing tests**

Add to the tests module in `src/cli/ls_view.rs`:

```rust
    fn folder(name: &str) -> LsEntry {
        LsEntry::Folder(name.to_string())
    }
    fn secret_entry(name: &str) -> LsEntry {
        LsEntry::Secret(summary(name, None))
    }

    #[test]
    fn grid_fills_column_major_within_width() {
        let entries = vec![
            folder("dev"),
            folder("prod"),
            secret_entry("alpha"),
            secret_entry("beta"),
            secret_entry("gamma-long-name"),
            secret_entry("delta"),
        ];
        // Width 40: expect multiple columns, folders first, trailing slashes.
        let out = render_grid(&entries, 40, false);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() < entries.len(), "should use >1 column:\n{out}");
        assert!(out.starts_with("dev/"), "folders come first:\n{out}");
        assert!(out.contains("prod/"));
        assert!(out.contains("gamma-long-name"));
        for line in &lines {
            assert_eq!(line.trim_end(), *line, "no trailing whitespace");
            assert!(line.chars().count() <= 40, "line exceeds width:\n{out}");
        }
    }

    #[test]
    fn grid_degrades_to_single_column_when_narrow() {
        let entries = vec![
            secret_entry("an-extremely-long-secret-name-beyond-width"),
            secret_entry("short"),
        ];
        let out = render_grid(&entries, 10, false);
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn grid_colors_folders_when_enabled() {
        let entries = vec![folder("prod"), secret_entry("alpha")];
        let out = render_grid(&entries, 80, true);
        assert!(out.contains("\x1b[36mprod/\x1b[0m"), "{out}");
        assert!(!out.contains("\x1b[36malpha"), "secrets uncolored: {out}");
    }

    #[test]
    fn grid_sanitizes_control_characters_in_names() {
        let entries = vec![secret_entry("evil\x1b[31mname")];
        let out = render_grid(&entries, 80, false);
        assert!(!out.contains('\x1b'), "raw ESC must not reach output: {out:?}");
        assert!(out.contains("\\x1B"), "escaped form visible: {out}");
    }

    #[test]
    fn grid_of_nothing_is_empty() {
        assert_eq!(render_grid(&[], 80, false), "");
    }

    #[test]
    fn entries_for_display_orders_folders_before_secrets() {
        let scoped = scope_secrets(
            vec![summary("root-a", None), summary("x", Some("prod"))],
            "",
        );
        let entries = entries_for_display(&scoped);
        assert!(matches!(&entries[0], LsEntry::Folder(f) if f == "prod"));
        assert!(matches!(&entries[1], LsEntry::Secret(s) if s.name == "root-a"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib cli::ls_view`
Expected: compile error — `LsEntry`, `entries_for_display`, `render_grid` not found.

- [ ] **Step 3: Implement**

Add to `src/cli/ls_view.rs` (above the tests module):

```rust
use crate::utils::format::sanitize_control_chars;

/// One row/cell in the ls-style views.
#[derive(Clone)]
pub(crate) enum LsEntry {
    Folder(String),
    Secret(SecretSummary),
}

/// Folders first (already sorted), then direct-child secrets (already sorted).
pub(crate) fn entries_for_display(scoped: &ScopedList) -> Vec<LsEntry> {
    let mut entries: Vec<LsEntry> = scoped
        .folders
        .iter()
        .cloned()
        .map(LsEntry::Folder)
        .collect();
    entries.extend(scoped.secrets.iter().cloned().map(LsEntry::Secret));
    entries
}

fn entry_label(entry: &LsEntry) -> String {
    match entry {
        LsEntry::Folder(name) => format!("{}/", sanitize_control_chars(name)),
        LsEntry::Secret(s) => sanitize_control_chars(display_name(s)),
    }
}

const GRID_GUTTER: usize = 2;
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// ls -C style column-major grid fitted to `width` display columns.
pub(crate) fn render_grid(entries: &[LsEntry], width: usize, color: bool) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let labels: Vec<String> = entries.iter().map(entry_label).collect();
    let lens: Vec<usize> = labels.iter().map(|l| l.chars().count()).collect();
    let n = labels.len();

    // Find the largest column count whose per-column max widths fit.
    let mut cols = n;
    let (rows, col_widths) = loop {
        let rows = n.div_ceil(cols);
        let mut widths: Vec<usize> = Vec::new();
        for c in 0..cols {
            let w = (0..rows)
                .filter_map(|r| lens.get(c * rows + r).copied())
                .max();
            match w {
                Some(w) => widths.push(w),
                None => break, // trailing empty column; stop
            }
        }
        let total: usize =
            widths.iter().sum::<usize>() + GRID_GUTTER * widths.len().saturating_sub(1);
        if total <= width || widths.len() <= 1 {
            break (rows, widths);
        }
        cols = widths.len() - 1;
    };
    let cols = col_widths.len();

    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let idx = c * rows + r;
            let Some(label) = labels.get(idx) else { continue };
            let pad = col_widths[c].saturating_sub(lens[idx]);
            if color && matches!(entries[idx], LsEntry::Folder(_)) {
                line.push_str(CYAN);
                line.push_str(label);
                line.push_str(RESET);
            } else {
                line.push_str(label);
            }
            line.push_str(&" ".repeat(pad + GRID_GUTTER));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cli::ls_view`
Expected: PASS (all Task 1 + Task 2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/cli/ls_view.rs && git commit -m "feat: add ls-style grid renderer for xv ls

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: `ls_view` — long renderer (`-l`)

**Files:**
- Modify: `src/cli/ls_view.rs` (add `render_long` + `truncate_note` + tests)

**Interfaces:**
- Consumes: Task 2's `LsEntry`, `entry_label` internals not required — uses `display_name`, `date_portion_for_display`, `sanitize_control_chars`.
- Produces (used by Task 5): `pub(crate) fn render_long(entries: &[LsEntry], color: bool) -> String`

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
    #[test]
    fn long_listing_aligns_columns_and_marks_folders() {
        let mut with_meta = summary("api-key", None);
        with_meta.groups = Some("team-a".to_string());
        with_meta.note = Some("rotate quarterly".to_string());
        let entries = vec![folder("prod"), LsEntry::Secret(with_meta)];
        let out = render_long(&entries, false);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("NAME"), "header row: {out}");
        assert!(lines[0].contains("UPDATED") && lines[0].contains("GROUPS") && lines[0].contains("NOTE"));
        assert!(lines[1].starts_with("prod/"), "folders first: {out}");
        assert!(lines[1].contains('-'), "folder placeholder columns: {out}");
        assert!(lines[2].starts_with("api-key"));
        assert!(lines[2].contains("2026-05-17"), "date-only: {out}");
        assert!(lines[2].contains("team-a") && lines[2].contains("rotate quarterly"));
        for line in &lines {
            assert_eq!(line.trim_end(), *line, "no trailing whitespace");
        }
    }

    #[test]
    fn long_listing_truncates_multiline_and_overlong_notes() {
        let mut s = summary("a", None);
        s.note = Some(format!("{}\nsecond line", "x".repeat(80)));
        let out = render_long(&[LsEntry::Secret(s)], false);
        assert!(!out.contains("second line"), "only first note line: {out}");
        assert!(out.contains('…'), "ellipsis on truncation: {out}");
        let data_line = out.lines().nth(1).unwrap();
        assert!(data_line.chars().count() < 80 + 40, "note capped: {out}");
    }

    #[test]
    fn long_listing_sanitizes_note_text() {
        let mut s = summary("a", None);
        s.note = Some("bad\x1b]0;title\x07note".to_string());
        let out = render_long(&[LsEntry::Secret(s)], false);
        assert!(!out.contains('\x1b') && !out.contains('\x07'), "{out:?}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib cli::ls_view`
Expected: compile error — `render_long` not found.

- [ ] **Step 3: Implement**

Add above the tests module:

```rust
const LONG_NOTE_MAX: usize = 60;

fn truncate_note(note: &str, max: usize) -> String {
    let first = note.lines().next().unwrap_or("");
    if first.chars().count() <= max {
        first.to_string()
    } else {
        let cut: String = first.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Borderless long listing: NAME  UPDATED  GROUPS  NOTE. Folders render as
/// `name/` with `-` placeholders, mirroring `xv file list`'s <DIR> rows.
pub(crate) fn render_long(entries: &[LsEntry], color: bool) -> String {
    struct Row {
        name: String,
        updated: String,
        groups: String,
        note: String,
        is_folder: bool,
    }
    let rows: Vec<Row> = entries
        .iter()
        .map(|entry| match entry {
            LsEntry::Folder(name) => Row {
                name: format!("{}/", sanitize_control_chars(name)),
                updated: "-".to_string(),
                groups: "-".to_string(),
                note: "-".to_string(),
                is_folder: true,
            },
            LsEntry::Secret(s) => Row {
                name: sanitize_control_chars(display_name(s)),
                updated: date_portion_for_display(&s.updated_on),
                groups: sanitize_control_chars(s.groups.as_deref().unwrap_or("-")),
                note: sanitize_control_chars(&truncate_note(
                    s.note.as_deref().unwrap_or(""),
                    LONG_NOTE_MAX,
                )),
                is_folder: false,
            },
        })
        .collect();

    let name_w = rows
        .iter()
        .map(|r| r.name.chars().count())
        .chain(["NAME".len()])
        .max()
        .unwrap_or(4);
    let updated_w = rows
        .iter()
        .map(|r| r.updated.chars().count())
        .chain(["UPDATED".len()])
        .max()
        .unwrap_or(7);
    let groups_w = rows
        .iter()
        .map(|r| r.groups.chars().count())
        .chain(["GROUPS".len()])
        .max()
        .unwrap_or(6);

    let mut out = String::new();
    let header = format!(
        "{:<name_w$}  {:<updated_w$}  {:<groups_w$}  NOTE",
        "NAME", "UPDATED", "GROUPS"
    );
    out.push_str(header.trim_end());
    out.push('\n');
    for row in rows {
        let padded_name = format!("{:<name_w$}", row.name);
        let name_cell = if color && row.is_folder {
            format!("{CYAN}{padded_name}{RESET}")
        } else {
            padded_name
        };
        let line = format!(
            "{name_cell}  {:<updated_w$}  {:<groups_w$}  {}",
            row.updated, row.groups, row.note
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib cli::ls_view`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && git add src/cli/ls_view.rs && git commit -m "feat: add long-listing renderer for xv ls -l

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: CLI surface — positional path, `-l`, `-r`, `format_explicit`

**Files:**
- Modify: `src/cli/commands.rs:459-491` (the `List` clap variant), `src/cli/commands.rs:1461-1479` (the `Commands::List` dispatch arm), `src/cli/commands.rs:1376-1378` (format resolution)
- Modify: `src/config/settings.rs` (`Config` struct near line 201, its `Default` impl near line 303)
- Modify: `src/cli/secret_ops.rs` (`execute_secret_list_direct` signature ~line 489 — accept and thread the new params; the body threading to display happens in Task 5)

**Interfaces:**
- Consumes: `crate::utils::helpers::validate_folder_path(&str) -> Result<()>` (rejects leading/trailing `/`, empty segments, >50-char segments).
- Produces (Task 5 relies on): `execute_secret_list_direct(path: String, group: Option<String>, all: bool, expiring: Option<String>, expired: bool, no_cache: bool, pagination: Pagination, pager: bool, names_only: bool, long: bool, recursive: bool, config: Config, registry: …)` — `path` already normalized ("" = root); `Config.format_explicit: bool` set for every command.

- [ ] **Step 1: Extend the clap variant**

In `src/cli/commands.rs`, add three fields to `List` (positional first, keep every existing field):

```rust
    /// List secrets in the current vault context (alias: ls)
    #[command(alias = "ls")]
    List {
        /// Folder path to list (e.g. `prod` or `prod/db`). Omit for the vault root.
        #[arg(value_name = "FOLDER")]
        path: Option<String>,
        /// Long listing: name, updated date, groups, note
        #[arg(short = 'l', long)]
        long: bool,
        /// List every secret in scope recursively (flatten folders)
        #[arg(short = 'r', long)]
        recursive: bool,
        // ... existing fields unchanged: group, all, expiring, expired,
        // no_cache, page, page_size, pager, names_only ...
    },
```

- [ ] **Step 2: Add `format_explicit` to Config and set it at dispatch**

`src/config/settings.rs` — add after the `template` field (~line 205):

```rust
    /// True when the user passed an explicit `--format` (not `auto`).
    /// Set in `Cli::execute`, not persisted.
    #[serde(skip)]
    #[tabled(skip)]
    pub format_explicit: bool,
```

and add `format_explicit: false,` to the `Config` `Default` impl (near the `no_color: false,` line at ~303).

`src/cli/commands.rs:1376-1378` — extend the resolution block:

```rust
        let resolved = self.format.resolve_for_stdout();
        config.runtime_output_format = resolved;
        config.format_explicit = !matches!(self.format, OutputFormat::Auto);
        config.output_json = matches!(resolved, OutputFormat::Json);
```

(Keep the surrounding lines exactly as they are; only the `format_explicit` line is new.)

- [ ] **Step 3: Normalize the path in the dispatch arm**

Replace the `Commands::List` arm (`src/cli/commands.rs:1461-1479`) with:

```rust
            Commands::List {
                path,
                long,
                recursive,
                group,
                all,
                expiring,
                expired,
                no_cache,
                page,
                page_size,
                pager,
                names_only,
            } => {
                let pagination = crate::utils::pagination::Pagination::from_args(page, page_size)?;
                let pager = pager.map(PagerWhen::wants_pager).unwrap_or(false);
                let path = match path {
                    Some(raw) => {
                        let trimmed = raw.trim_end_matches('/').to_string();
                        if !trimmed.is_empty() {
                            crate::utils::helpers::validate_folder_path(&trimmed)?;
                        }
                        trimmed
                    }
                    None => String::new(),
                };
                crate::cli::secret_ops::execute_secret_list_direct(
                    path, group, all, expiring, expired, no_cache, pagination, pager,
                    names_only, long, recursive, config, registry,
                )
                .await
            }
```

- [ ] **Step 4: Extend `execute_secret_list_direct`'s signature**

In `src/cli/secret_ops.rs`, add the three parameters to BOTH functions so this task compiles standalone while Task 5 supplies the behavior:

- `execute_secret_list_direct` (~line 489): add `path: String` as the first parameter and `long: bool, recursive: bool` after `names_only`.
- `display_cached_secret_list` (~line 395): add `path: &str, long: bool, recursive: bool` after `all`.
- Update every `display_cached_secret_list` call site: pass `&path, long, recursive` from `execute_secret_list_direct`; pass `"", false, false` from the legacy `execute_secret_list` path if it calls it.
- Leave `display_cached_secret_list`'s BODY unchanged in this task — it ignores the new parameters until Task 5 (Rust does not warn on unused function parameters, so the build stays clean).

Grep to find all call sites:

```bash
grep -n 'display_cached_secret_list' src/cli/secret_ops.rs
```

- [ ] **Step 5: Verify**

Run: `cargo check`
Expected: clean (warnings about unused parameters are acceptable ONLY if the compiler emits none — Rust doesn't warn on unused fn params, so expect silence).

Run: `cargo run --quiet -- ls --help 2>&1 | head -20`
Expected: shows `[FOLDER]` positional, `-l, --long`, `-r, --recursive`.

Run: `cargo run --quiet -- ls 'bad//path' 2>&1 | head -2; echo "exit=$?"`
Expected: the `validate_folder_path` error ("empty folder names"), non-zero exit.

Run: `cargo test --lib`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
cargo fmt && git add src/cli/commands.rs src/config/settings.rs src/cli/secret_ops.rs && git commit -m "feat: add xv ls folder path, -l/-r flags, and format_explicit plumbing

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Wire the ls view into `display_cached_secret_list`

**Files:**
- Modify: `src/cli/secret_ops.rs:394-486` (`display_cached_secret_list` body; signature already extended in Task 4)

**Interfaces:**
- Consumes: `ls_view::{scope_secrets, entries_for_display, render_grid, render_long, display_name, LsEntry, ScopedList}`, `config.format_explicit`, `crossterm::terminal::size`.
- Produces: final user-facing behavior. Mode matrix (human formats only — `Table | Plain | Raw`):
  - `long == true` → `render_long`
  - else `config.format_explicit == true` → legacy P0 table path (unchanged rendering, scoped data)
  - else → `render_grid`
  - Machine formats (`Json | Yaml | Csv | Template`) and `--names-only`: recursive subtree, flat, schema unchanged.

- [ ] **Step 1: Replace the body of `display_cached_secret_list`**

```rust
#[allow(clippy::too_many_arguments)]
pub(crate) fn display_cached_secret_list(
    secrets: Vec<crate::secret::manager::SecretSummary>,
    group: Option<String>,
    all: bool,
    path: &str,
    long: bool,
    recursive: bool,
    pagination: Pagination,
    pager: bool,
    vault_name: &str,
    config: &Config,
    names_only: bool,
) -> Result<()> {
    use crate::cli::ls_view::{self, LsEntry};
    use crate::utils::format::TableFormatter;
    use crate::utils::pagination::{paginate_slice, pagination_footer_text};
    use std::fmt::Write as _;

    let filtered = filter_secret_summaries_for_display(secrets, group.as_deref(), all);
    let scoped = ls_view::scope_secrets(filtered, path);

    // Pipe-friendly modes: flat recursive subtree, unchanged schema.
    if names_only {
        for s in &scoped.subtree {
            println!("{}", ls_view::display_name(s));
        }
        return Ok(());
    }

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );

    if !human_table_like {
        let page = paginate_slice(&scoped.subtree, pagination);
        let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
        let output = formatter.format_table(&page.items)?;
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    let mut output = String::new();
    output.push('\n');
    // Color only for styled table/grid; plain/raw must not emit ANSI escapes
    let color = !config.no_color && fmt == OutputFormat::Table;
    if color {
        let _ = writeln!(output, "\x1b[36mVault: {}\x1b[0m", vault_name);
    } else {
        let _ = writeln!(output, "Vault: {}", vault_name);
    }
    output.push('\n');

    if scoped.subtree.is_empty() {
        let msg = if !path.is_empty() {
            format!("No secrets found in folder '{path}'.")
        } else if all {
            "No secrets found in vault.".to_string()
        } else {
            "No enabled secrets found in vault. Use --all to show disabled secrets.".to_string()
        };
        output.push_str(&output::format_line(
            output::Level::Info,
            &msg,
            output::should_use_rich_stdout(),
        ));
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    // Legacy rounded table only on explicit --format table|plain|raw.
    if config.format_explicit && !long {
        let table_secrets = if recursive {
            &scoped.subtree
        } else {
            &scoped.secrets
        };
        let page = paginate_slice(table_secrets, pagination);
        let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
        let display_rows = format_secret_list_rows_for_human(&page.items);
        output.push_str(&formatter.format_table(&display_rows)?);
        output.push('\n');
        let _ = writeln!(
            output,
            "{} in vault '{}'",
            secret_count_label(
                page.items.len(),
                page.total_items,
                None,
                page.page_size.is_some(),
            ),
            vault_name
        );
        if let Some(footer) = pagination_footer_text(&page, "secret", fmt) {
            output.push('\n');
            output.push_str(&footer);
        }
        crate::utils::pager::print_output(&output, pager)?;
        return Ok(());
    }

    // ls-style grid / long listing.
    let entries: Vec<LsEntry> = if recursive {
        scoped.subtree.iter().cloned().map(LsEntry::Secret).collect()
    } else {
        ls_view::entries_for_display(&scoped)
    };
    let folder_count = if recursive { 0 } else { scoped.folders.len() };
    let secret_count = entries.len() - folder_count;

    let page = paginate_slice(&entries, pagination);
    let rendered = if long {
        ls_view::render_long(&page.items, color)
    } else {
        let width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80);
        ls_view::render_grid(&page.items, width, color)
    };
    output.push_str(&rendered);
    output.push('\n');
    let mut count_line = secret_count_label(
        page.items
            .iter()
            .filter(|e| matches!(e, LsEntry::Secret(_)))
            .count(),
        secret_count,
        None,
        page.page_size.is_some(),
    );
    if folder_count > 0 {
        let _ = write!(count_line, ", {} folder(s)", folder_count);
    }
    let _ = writeln!(output, "{} in vault '{}'", count_line, vault_name);
    if let Some(footer) = pagination_footer_text(&page, "entry", fmt) {
        output.push('\n');
        output.push_str(&footer);
    }
    crate::utils::pager::print_output(&output, pager)?;
    Ok(())
}
```

Notes for the implementer:
- `LsEntry` needs `Clone` for `paginate_slice` — if `paginate_slice` requires `Clone` (check its bound), add `#[derive(Clone)]`-compatible manual derives: `LsEntry` contains `SecretSummary` which is `Clone`, so add `Clone` to the `LsEntry` enum derive in `ls_view.rs`.
- `execute_secret_list_direct` already passes `&path, long, recursive` (Task 4). Verify the expiring/expired filter path (per-secret fetch, ~lines 524-567 pre-P1) feeds its filtered list through this same function so expiry composes with scoping.
- Do not touch the machine-format branch beyond swapping `filtered` → `scoped.subtree`.

- [ ] **Step 2: Run the full lib suite**

Run: `cargo test --lib`
Expected: PASS. Also `cargo clippy --all-targets` clean.

- [ ] **Step 3: Behavioral verification against the real vault (read-only)**

```bash
cargo run --quiet -- --format table ls | head -8      # legacy table (explicit format)
cargo run --quiet -- ls --format json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(type(d).__name__, len(d))'
cargo run --quiet -- ls --names-only | head -3
```
Expected: legacy table unchanged from P0; JSON prints `list <N>` (flat array, unchanged schema); names-only one per line. The grid itself needs a TTY — defer visual grid checks to Task 6's e2e (which creates folder-tagged fixtures).

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add src/cli/secret_ops.rs src/cli/ls_view.rs && git commit -m "feat: folder-aware ls-style output for xv ls

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Docs, gates, and end-to-end verification with folder fixtures

**Files:**
- Modify: `CHANGELOG.md` (extend the existing `## Unreleased` section — create it after `# Changelog` if a release consumed it — with an `### Added` block)
- Modify: `README.md` (the `xv list` / listing section: document `xv ls [FOLDER]`, `-l`, `-r`, grid default, `--format table` for the legacy table)

**Interfaces:**
- Consumes: everything from Tasks 1-5.
- Produces: release notes + verified branch.

- [ ] **Step 1: CHANGELOG entry**

Add under `## Unreleased` (create the section if absent):

```markdown
### Added

- **`xv ls` is folder-aware and ls-styled.** The default TTY output is now a multi-column name grid with folders listed first (`prod/`), derived from each secret's `folder` tag. `xv ls prod` lists inside a folder, `xv ls -l` is a borderless long listing (name, updated date, groups, note), `xv ls -r` flattens recursively, and the previous rounded table remains available via explicit `--format table`. Piped/machine output (`--format json|yaml|csv`, `--names-only`) keeps the flat schema unchanged, scoped to the requested subtree.
```

- [ ] **Step 2: README**

Find the listing docs (`grep -n 'xv list\|xv ls' README.md | head`) and update the section to show:

```markdown
xv ls                  # grid of folders (prod/) and root secrets
xv ls prod             # inside a folder
xv ls prod -l          # long listing: name, updated, groups, note
xv ls -r               # every secret, flattened
xv ls --format table   # the classic table
```

- [ ] **Step 3: E2E with folder fixtures (real vault, cleaned up after)**

Create two throwaway secrets, verify, delete them. These are writes to the configured vault — use unmistakable names and ALWAYS run the cleanup:

```bash
cargo run --quiet -- set xv-p1-e2e-alpha --value dummy --folder xv-p1-e2e/sub
cargo run --quiet -- set xv-p1-e2e-beta  --value dummy --folder xv-p1-e2e
cargo run --quiet -- ls | head -5                                  # non-TTY → JSON; just confirms no crash
cargo run --quiet -- ls xv-p1-e2e --format json | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))'   # expect 2 (subtree)
cargo run --quiet -- ls xv-p1-e2e --names-only                     # both names
cargo run --quiet -- ls xv-p1-e2e/ --names-only                    # trailing slash tolerated
cargo run --quiet -- ls xv-p1-e2e-nonexistent; echo "exit=$?"      # stderr message, exit=0
script -q /dev/null cargo run --quiet -- ls | head -12             # pseudo-TTY: grid with xv-p1-e2e/ folder entry
script -q /dev/null cargo run --quiet -- ls xv-p1-e2e -l | head -6 # long listing with sub/ row
cargo run --quiet -- delete xv-p1-e2e-alpha --force
cargo run --quiet -- delete xv-p1-e2e-beta --force
```

Capture actual outputs. (macOS `script` invocation: `script -q /dev/null <cmd>` allocates a pty so auto-format resolves to the grid.) If any check fails, fix before committing; if vault writes are impossible (auth), report the gap honestly instead of skipping silently.

- [ ] **Step 4: Full gates**

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test --lib
cargo test
```
Expected: all clean/green (full `cargo test` needs the Azure creds available on this machine).

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md README.md && git commit -m "docs: document folder-aware ls-style xv ls

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Spec coverage map

| Spec section | Task |
|---|---|
| §1 command surface (positional, `-l`, `-r`, flag composition) | Task 4 |
| §2 scoping model + empty-scope semantics | Task 1 (logic), Task 5 (messages/exit) |
| §3a grid | Task 2 (render), Task 5 (wiring, width, color) |
| §3b long | Task 3 (render), Task 5 (wiring) |
| §3c legacy table + `format_explicit` | Task 4 (plumbing), Task 5 (branch) |
| §4 machine formats + `--names-only` subtree scope | Task 5 |
| §5 pagination/pager | Task 5 |
| §6 module structure | Tasks 1-3 |
| §7 interactions (expiry-before-scope, cache untouched) | Task 5 notes |
| Testing (unit + manual) | per-task steps + Task 6 e2e |
