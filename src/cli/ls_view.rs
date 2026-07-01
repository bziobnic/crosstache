//! ls-style view logic for `xv ls`: folder scoping and grid/long rendering.
//!
//! Folders are a client-side view derived from each secret's hierarchical
//! `folder` tag (e.g. `prod/db`). Nothing here talks to a backend.

use crate::secret::manager::SecretSummary;
use crate::utils::format::sanitize_control_chars;

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
            let Some(label) = labels.get(idx) else {
                continue;
            };
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
}
