//! ls-style view logic for `xv ls`: folder scoping and grid/long rendering.
//!
//! Folders are a client-side view derived from each secret's hierarchical
//! `folder` tag (e.g. `prod/db`). Nothing here talks to a backend.

use crate::secret::manager::SecretSummary;
use crate::utils::format::sanitize_control_chars;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Terminal display width of `s` (Unicode-aware: CJK/full-width chars = 2 columns).
pub(crate) fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Left-align `s` in `w` display columns. `format!("{:<w$}")` pads by char
/// count, which breaks on full-width characters — always pad manually.
pub(crate) fn pad_to(s: &str, w: usize) -> String {
    let pad = w.saturating_sub(display_width(s));
    format!("{s}{}", " ".repeat(pad))
}

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
    let lens: Vec<usize> = labels.iter().map(|l| display_width(l)).collect();
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
        // Safe to jump to widths.len()-1: trailing empty columns mean the
        // measured layout already collapsed to widths.len() effective
        // columns, and intermediate cols values in the same rows bucket
        // (rows = n.div_ceil(cols)) produce identical layouts.
        cols = widths.len() - 1;
    };

    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for (c, col_width) in col_widths.iter().enumerate() {
            let idx = c * rows + r;
            let Some(label) = labels.get(idx) else {
                continue;
            };
            let pad = col_width.saturating_sub(lens[idx]);
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

const LONG_NOTE_MAX: usize = 60;

fn truncate_note(note: &str, max: usize) -> String {
    let first = note.lines().next().unwrap_or("");
    if display_width(first) <= max {
        return first.to_string();
    }
    let budget = max.saturating_sub(1); // reserve one column for the ellipsis
    let mut cut = String::new();
    let mut used = 0usize;
    for ch in first.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        cut.push(ch);
    }
    format!("{cut}…")
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
        .map(|r| display_width(&r.name))
        .chain(["NAME".len()])
        .max()
        .unwrap_or(4);
    let updated_w = rows
        .iter()
        .map(|r| display_width(&r.updated))
        .chain(["UPDATED".len()])
        .max()
        .unwrap_or(7);
    let groups_w = rows
        .iter()
        .map(|r| display_width(&r.groups))
        .chain(["GROUPS".len()])
        .max()
        .unwrap_or(6);

    let mut out = String::new();
    let header = format!(
        "{}  {}  {}  NOTE",
        pad_to("NAME", name_w),
        pad_to("UPDATED", updated_w),
        pad_to("GROUPS", groups_w)
    );
    out.push_str(header.trim_end());
    out.push('\n');
    for row in rows {
        let padded_name = pad_to(&row.name, name_w);
        let name_cell = if color && row.is_folder {
            format!("{CYAN}{padded_name}{RESET}")
        } else {
            padded_name
        };
        let line = format!(
            "{name_cell}  {}  {}  {}",
            pad_to(&row.updated, updated_w),
            pad_to(&row.groups, groups_w),
            row.note
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

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
    fn root_subtree_includes_folder_tagged_secrets() {
        let scoped = scope_secrets(
            vec![summary("root-a", None), summary("tucked", Some("prod/db"))],
            "",
        );
        assert_eq!(
            scoped.subtree.len(),
            2,
            "explicit table renders the subtree — folder-tagged secrets must be present at root"
        );
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
            vec![summary("a", Some("prod")), summary("b", Some("production"))],
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
            assert!(display_width(line) <= 40, "line exceeds width:\n{out}");
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
        assert!(
            !out.contains('\x1b'),
            "raw ESC must not reach output: {out:?}"
        );
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

    #[test]
    fn long_listing_aligns_columns_and_marks_folders() {
        let mut with_meta = summary("api-key", None);
        with_meta.groups = Some("team-a".to_string());
        with_meta.note = Some("rotate quarterly".to_string());
        let entries = vec![folder("prod"), LsEntry::Secret(with_meta)];
        let out = render_long(&entries, false);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("NAME"), "header row: {out}");
        assert!(
            lines[0].contains("UPDATED")
                && lines[0].contains("GROUPS")
                && lines[0].contains("NOTE")
        );
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

    #[test]
    fn display_width_counts_columns_not_chars() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("秘密"), 4); // 2 full-width chars = 4 columns
        assert_eq!(pad_to("秘密", 6), "秘密  ");
        assert_eq!(pad_to("abc", 5), "abc  ");
    }

    #[test]
    fn grid_accounts_for_full_width_characters() {
        let entries = vec![
            secret_entry("数据库密码"),
            secret_entry("api-key"),
            secret_entry("另一个秘密名字"),
        ];
        let out = render_grid(&entries, 20, false);
        for line in out.lines() {
            assert!(display_width(line) <= 20, "line exceeds width:\n{out}");
        }
    }

    #[test]
    fn long_listing_aligns_full_width_names() {
        let entries = vec![secret_entry("秘密"), secret_entry("abcd")];
        let out = render_long(&entries, false);
        let lines: Vec<&str> = out.lines().collect();
        let idx_a = lines[1].find("2026-05-17").unwrap();
        let idx_b = lines[2].find("2026-05-17").unwrap();
        assert_eq!(
            display_width(&lines[1][..idx_a]),
            display_width(&lines[2][..idx_b]),
            "UPDATED column misaligned:\n{out}"
        );
    }

    #[test]
    fn note_truncation_is_width_aware() {
        let mut s = summary("a", None);
        s.note = Some("秘".repeat(60)); // 120 columns wide
        let out = render_long(&[LsEntry::Secret(s)], false);
        let data_line = out.lines().nth(1).unwrap();
        let note_cell = data_line.rsplit("  ").next().unwrap();
        assert!(
            display_width(note_cell) <= LONG_NOTE_MAX,
            "note not width-capped:\n{out}"
        );
        assert!(out.contains('…'));
    }
}
