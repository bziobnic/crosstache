//! Table formatting and output utilities
//!
//! This module provides functionality for formatting and displaying
//! tabular data with color support and various output formats.

use crate::error::Result;
use crossterm::{
    execute,
    style::{Color as CrosstermColor, Stylize},
    terminal::{size, Clear, ClearType},
};
use regex::Regex;
use serde::Serialize;
use std::io::stdout;
use std::io::IsTerminal;
use std::sync::LazyLock;
use tabled::{
    settings::{
        object::{Rows, Segment},
        peaker::PriorityMax,
        Alignment, Color, Format, Modify, Padding, Style, Width,
    },
    Table, Tabled,
};

/// Format a byte count in human-readable units (B, KB, MB, GB, TB), using
/// binary (1024) steps. Whole bytes render without decimals; larger units use
/// two decimal places.
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];

    if bytes == 0 {
        return "0 B".to_string();
    }

    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

/// Visibly escape control characters in untrusted display text so
/// remote-supplied values (blob names, metadata, tags) cannot inject terminal
/// escape sequences into TTY output. Covers C0 controls, DEL, and C1 controls
/// (e.g. U+009B CSI). Newlines and tabs are kept: they only affect layout,
/// not terminal state. Machine-readable formats (JSON/YAML/CSV) are left
/// untouched so scripts see the raw values.
pub(crate) fn sanitize_control_chars(input: &str) -> String {
    fn is_dangerous(c: char) -> bool {
        c.is_control() && c != '\n' && c != '\t'
    }
    if !input.chars().any(is_dangerous) {
        return input.to_string();
    }
    input
        .chars()
        .map(|c| {
            if is_dangerous(c) {
                format!("\\x{:02X}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Regex for template placeholders: {{field_name}} with optional whitespace.
/// Field names may contain word characters and spaces.
static TEMPLATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{\s*([\w\s]+?)\s*\}\}").unwrap());

/// Output format options
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OutputFormat {
    /// Automatic format selection (table on TTY, JSON for pipes/redirects)
    #[default]
    Auto,
    /// Human-readable table format
    Table,
    /// Machine-readable JSON output
    Json,
    /// YAML format for configuration files
    Yaml,
    /// Comma-separated values format
    Csv,
    /// Simple text output
    Plain,
    /// Custom template-based output
    Template,
    /// Raw text without formatting
    Raw,
}

impl OutputFormat {
    /// Resolve auto format based on stdout TTY status.
    pub fn resolve_for_stdout(self) -> Self {
        match self {
            Self::Auto => {
                if std::io::stdout().is_terminal() {
                    Self::Table
                } else {
                    Self::Json
                }
            }
            explicit => explicit,
        }
    }
}

/// Color theme for console output
#[derive(Debug, Clone)]
pub struct ColorTheme {
    pub header: CrosstermColor,
    #[allow(dead_code)]
    pub success: CrosstermColor,
    #[allow(dead_code)]
    pub warning: CrosstermColor,
    #[allow(dead_code)]
    pub error: CrosstermColor,
    pub info: CrosstermColor,
    pub accent: CrosstermColor,
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self {
            header: CrosstermColor::Cyan,
            success: CrosstermColor::Green,
            warning: CrosstermColor::Yellow,
            error: CrosstermColor::Red,
            info: CrosstermColor::Cyan,
            accent: CrosstermColor::Magenta,
        }
    }
}

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

/// Table formatter with color support
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

    /// Validate any configured `--columns` selection against `T`'s headers
    /// without rendering. Call this on empty-result paths that skip
    /// `format_table` (e.g. a human-readable "no results" message) so that
    /// an unknown `--columns` value still errors even when there is no data.
    pub fn validate_columns<T: Tabled>(&self) -> Result<()> {
        let headers: Vec<String> = T::headers().iter().map(|h| h.to_string()).collect();
        self.selected_indices(&headers).map(|_| ())
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

    /// Create a formatted table from data
    pub fn format_table<T: Tabled + Serialize>(&self, data: &[T]) -> Result<String> {
        if data.is_empty() {
            // Machine-readable formats must stay valid (e.g. `[]` for JSON) for pipes/jq.
            return match self.format {
                OutputFormat::Auto => self.format_as_json(data),
                OutputFormat::Json => self.format_as_json(data),
                OutputFormat::Yaml => self.format_as_yaml(data),
                OutputFormat::Csv => self.format_as_csv(data),
                OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw => {
                    // Validate --columns selection even with no rows.
                    let headers: Vec<String> = T::headers().iter().map(|h| h.to_string()).collect();
                    let _ = self.selected_indices(&headers)?;
                    Ok(
                        "No results found. If this is unexpected, check your vault permissions or filter criteria."
                            .to_string(),
                    )
                }
                OutputFormat::Template => self.format_as_template(data),
            };
        }

        match self.format {
            OutputFormat::Auto => self.format_as_json(data),
            OutputFormat::Table => self.format_as_table(data),
            OutputFormat::Json => self.format_as_json(data),
            OutputFormat::Yaml => self.format_as_yaml(data),
            OutputFormat::Csv => self.format_as_csv(data),
            OutputFormat::Plain => self.format_as_plain(data),
            OutputFormat::Template => self.format_as_template(data),
            OutputFormat::Raw => self.format_as_raw(data),
        }
    }

    /// Format data as a styled table
    fn format_as_table<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let mut table = self.build_table(data)?;

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

    /// Format data as JSON
    fn format_as_json<T: Serialize>(&self, data: &[T]) -> Result<String> {
        Ok(serde_json::to_string_pretty(data)?)
    }

    /// Format data as YAML
    fn format_as_yaml<T: Serialize>(&self, data: &[T]) -> Result<String> {
        Ok(serde_yaml::to_string(data)?)
    }

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

    /// Format data as plain text
    fn format_as_plain<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let mut table = self.build_table(data)?;
        // Neutralize terminal escape sequences in untrusted cell content
        table.with(Modify::new(Segment::all()).with(Format::content(sanitize_control_chars)));
        table.with(Style::ascii()).with(Padding::new(1, 1, 0, 0));
        Ok(table.to_string())
    }

    /// Format data using a template with {{field_name}} substitution
    fn format_as_template<T: Tabled>(&self, data: &[T]) -> Result<String> {
        if data.is_empty() {
            return Ok(String::new());
        }

        let template_str = self.template.as_deref().ok_or_else(|| {
            crate::error::CrosstacheError::config(
                "Template format requires --template flag with a format string. Example: --template '{{name}}: {{value}}'".to_string(),
            )
        })?;

        // Build case-insensitive header → index map
        let headers = T::headers();
        let header_map: std::collections::HashMap<String, usize> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| (h.as_ref().to_lowercase(), i))
            .collect();

        // Apply template to each row
        let mut lines = Vec::with_capacity(data.len());
        for item in data {
            let fields = item.fields();
            let line = TEMPLATE_REGEX
                .replace_all(template_str, |caps: &regex::Captures| {
                    let field_name = caps[1].trim().to_lowercase();
                    if let Some(&idx) = header_map.get(&field_name) {
                        fields
                            .get(idx)
                            .map(|f| f.as_ref().to_string())
                            .unwrap_or_default()
                    } else {
                        // Unknown field — leave placeholder as-is
                        caps[0].to_string()
                    }
                })
                .to_string();
            lines.push(line);
        }

        Ok(lines.join("\n"))
    }

    /// Format data as raw text
    fn format_as_raw<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let mut table = Table::new(data);
        table.with(Style::empty());
        Ok(table.to_string())
    }
}

/// Display utilities for various data types
pub struct DisplayUtils {
    theme: ColorTheme,
    no_color: bool,
}

impl DisplayUtils {
    /// Create new display utilities
    pub fn new(no_color: bool) -> Self {
        Self {
            theme: ColorTheme::default(),
            no_color,
        }
    }

    /// Print a section header
    pub fn print_header(&self, title: &str) -> Result<()> {
        let styled_title = if self.no_color {
            format!("=== {title} ===")
        } else {
            format!("=== {} ===", title.with(self.theme.header).bold())
        };

        println!("{styled_title}");
        Ok(())
    }

    /// Format key-value pairs
    pub fn format_key_value_pairs(&self, pairs: &[(&str, &str)]) -> String {
        let max_key_length = pairs.iter().map(|(key, _)| key.len()).max().unwrap_or(0);

        pairs
            .iter()
            .map(|(key, value)| {
                let formatted_key = if self.no_color {
                    format!("{key:max_key_length$}")
                } else {
                    format!(
                        "{:width$}",
                        key.with(self.theme.accent).bold(),
                        width = max_key_length
                    )
                };
                format!("{formatted_key}: {value}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Print a separator line
    pub fn print_separator(&self) -> Result<()> {
        if let Ok((width, _)) = size() {
            let line = "─".repeat(width as usize);
            if self.no_color {
                println!("{line}");
            } else {
                println!("{}", line.with(self.theme.info));
            }
        } else {
            println!("{}", "─".repeat(80));
        }
        Ok(())
    }

    /// Clear the screen
    #[allow(dead_code)]
    pub fn clear_screen(&self) -> Result<()> {
        execute!(stdout(), Clear(ClearType::All))?;
        Ok(())
    }

    /// Print a banner with border
    #[allow(dead_code)]
    pub fn print_banner(&self, title: &str, subtitle: Option<&str>) -> Result<()> {
        let width = if let Ok((w, _)) = size() {
            (w as usize).min(80)
        } else {
            80
        };

        let border = "═".repeat(width);
        let title_line = format!("║ {:^width$} ║", title, width = width - 4);

        if self.no_color {
            println!("╔{border}╗");
            println!("{title_line}");
            if let Some(sub) = subtitle {
                let subtitle_line = format!("║ {:^width$} ║", sub, width = width - 4);
                println!("{subtitle_line}");
            }
            println!("╚{border}╝");
        } else {
            println!("╔{}╗", border.clone().with(self.theme.accent));
            println!("{}", title_line.with(self.theme.header).bold());
            if let Some(sub) = subtitle {
                let subtitle_line = format!("║ {:^width$} ║", sub, width = width - 4);
                println!("{}", subtitle_line.with(self.theme.info));
            }
            println!("╚{}╝", border.with(self.theme.accent));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use tabled::Tabled;

    #[test]
    fn sanitize_escapes_ansi_and_c1_controls() {
        // ANSI color/OSC injection via ESC
        assert_eq!(
            sanitize_control_chars("evil\x1b[31mred\x1b]0;title\x07"),
            "evil\\x1B[31mred\\x1B]0;title\\x07"
        );
        // C1 CSI (U+009B) is a single-char escape sequence on many terminals
        assert_eq!(sanitize_control_chars("a\u{9b}31mb"), "a\\x9B31mb");
        // Newlines and tabs are layout, not terminal state — preserved
        assert_eq!(sanitize_control_chars("a\nb\tc"), "a\nb\tc");
        // Clean strings pass through unchanged
        assert_eq!(
            sanitize_control_chars("plain-name_1.txt"),
            "plain-name_1.txt"
        );
    }

    #[test]
    fn table_output_neutralizes_escape_sequences() {
        let data = vec![TestData {
            name: "blob\x1b]8;;http://evil\x07link".to_string(),
            value: "v".to_string(),
            status: "ok".to_string(),
        }];
        let formatter = TableFormatter::new(OutputFormat::Table, true, None, None);
        let out = formatter.format_table(&data).unwrap();
        assert!(
            !out.contains('\x1b'),
            "table output must not contain raw ESC"
        );
        assert!(
            !out.contains('\x07'),
            "table output must not contain raw BEL"
        );
        assert!(out.contains("\\x1B"), "escaped form should be visible");
    }

    #[derive(Debug, Clone, Serialize, Tabled)]
    struct TestData {
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Value")]
        value: String,
        #[tabled(rename = "Status")]
        status: String,
    }

    #[test]
    fn validate_columns_errs_on_unknown_and_passes_on_known() {
        // Unknown column must error even though no rows are ever built or
        // rendered — this is what empty-state early-returns must call.
        let bad = TableFormatter::new(
            OutputFormat::Table,
            true,
            None,
            Some(vec!["Bogus".to_string()]),
        );
        assert!(bad.validate_columns::<TestData>().is_err());

        // A valid column selection (case-insensitive) passes.
        let good = TableFormatter::new(
            OutputFormat::Table,
            true,
            None,
            Some(vec!["name".to_string(), "Status".to_string()]),
        );
        assert!(good.validate_columns::<TestData>().is_ok());

        // No --columns selection at all also passes (nothing to validate).
        let none = TableFormatter::new(OutputFormat::Table, true, None, None);
        assert!(none.validate_columns::<TestData>().is_ok());
    }

    #[test]
    fn test_table_formatting() {
        let data = vec![
            TestData {
                name: "test1".to_string(),
                value: "value1".to_string(),
                status: "active".to_string(),
            },
            TestData {
                name: "test2".to_string(),
                value: "value2".to_string(),
                status: "inactive".to_string(),
            },
        ];

        let formatter = TableFormatter::new(OutputFormat::Table, true, None, None);
        let result = formatter.format_table(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_json_is_valid_array() {
        let data: Vec<TestData> = vec![];
        let formatter = TableFormatter::new(OutputFormat::Json, true, None, None);
        let out = formatter.format_table(&data).expect("format");
        assert_eq!(out.trim(), "[]");
    }

    #[test]
    fn csv_output_uses_rfc4180_escaping() {
        let data = vec![
            TestData {
                name: "plain".to_string(),
                value: "contains,comma".to_string(),
                status: "active".to_string(),
            },
            TestData {
                name: "quoted".to_string(),
                value: "has \"quotes\"".to_string(),
                status: "line\nbreak".to_string(),
            },
        ];
        let formatter = TableFormatter::new(OutputFormat::Csv, true, None, None);
        let out = formatter.format_table(&data).expect("format");

        assert_eq!(
            out,
            "Name,Value,Status\nplain,\"contains,comma\",active\nquoted,\"has \"\"quotes\"\"\",\"line\nbreak\"\n"
        );
    }

    #[test]
    fn test_key_value_formatting() {
        let display = DisplayUtils::new(true);
        let pairs = vec![
            ("Name", "Test Vault"),
            ("Location", "East US"),
            ("Status", "Active"),
        ];

        let result = display.format_key_value_pairs(&pairs);
        assert!(result.contains("Name"));
        assert!(result.contains("Test Vault"));
    }

    #[test]
    fn test_template_basic_substitution() {
        let data = vec![
            TestData {
                name: "secret1".to_string(),
                value: "abc123".to_string(),
                status: "active".to_string(),
            },
            TestData {
                name: "secret2".to_string(),
                value: "xyz789".to_string(),
                status: "inactive".to_string(),
            },
        ];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("export {{Name}}={{Value}}".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "export secret1=abc123\nexport secret2=xyz789");
    }

    #[test]
    fn test_template_case_insensitive() {
        let data = vec![TestData {
            name: "mykey".to_string(),
            value: "myval".to_string(),
            status: "active".to_string(),
        }];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("{{name}} {{NAME}} {{Name}}".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "mykey mykey mykey");
    }

    #[test]
    fn test_template_unknown_field_left_as_is() {
        let data = vec![TestData {
            name: "key".to_string(),
            value: "val".to_string(),
            status: "ok".to_string(),
        }];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("{{Name}}: {{nonexistent}}".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "key: {{nonexistent}}");
    }

    #[test]
    fn test_template_missing_template_flag_errors() {
        let data = vec![TestData {
            name: "key".to_string(),
            value: "val".to_string(),
            status: "ok".to_string(),
        }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, None, None);
        let result = formatter.format_table(&data);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("--template"),
            "Error should mention --template flag"
        );
    }

    #[test]
    fn test_template_empty_data_returns_empty() {
        let data: Vec<TestData> = vec![];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("{{Name}}".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_template_whitespace_in_braces() {
        let data = vec![TestData {
            name: "key".to_string(),
            value: "val".to_string(),
            status: "ok".to_string(),
        }];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("{{ Name }} = {{  Value  }}".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "key = val");
    }

    #[test]
    fn test_template_multiple_fields() {
        let data = vec![TestData {
            name: "db_pass".to_string(),
            value: "secret".to_string(),
            status: "active".to_string(),
        }];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("{{Name}}={{Value}} ({{Status}})".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "db_pass=secret (active)");
    }

    #[test]
    fn test_template_multi_word_field_name() {
        #[derive(Tabled, Serialize)]
        struct MultiWordData {
            #[tabled(rename = "Secret Name")]
            secret_name: String,
            #[tabled(rename = "Created By")]
            created_by: String,
        }

        let data = vec![MultiWordData {
            secret_name: "api-key".to_string(),
            created_by: "admin".to_string(),
        }];
        let formatter = TableFormatter::new(
            OutputFormat::Template,
            true,
            Some("{{Secret Name}} by {{Created By}}".to_string()),
            None,
        );
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "api-key by admin");
    }

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
        let formatter = TableFormatter::new(OutputFormat::Table, true, None, None);
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
        let formatter = TableFormatter::new(OutputFormat::Plain, true, None, None);
        let out = formatter.format_table(&data).unwrap();
        assert!(
            !out.contains("Value"),
            "plain output hides empty columns too:\n{out}"
        );
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
        let formatter = TableFormatter::new(OutputFormat::Table, true, None, None);
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
        let csv = TableFormatter::new(OutputFormat::Csv, true, None, None)
            .format_table(&data)
            .unwrap();
        assert!(csv.contains("Value"), "CSV keeps the full schema:\n{csv}");
        let json = TableFormatter::new(OutputFormat::Json, true, None, None)
            .format_table(&data)
            .unwrap();
        assert!(
            json.contains("\"value\""),
            "JSON keeps the full schema:\n{json}"
        );
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

    #[test]
    fn columns_validated_even_when_table_data_is_empty() {
        let formatter = TableFormatter::new(
            OutputFormat::Table,
            true,
            None,
            Some(vec!["Bogus".to_string()]),
        );
        let empty: Vec<ColRow> = vec![];
        let result = formatter.format_table(&empty);
        assert!(
            result.is_err(),
            "unknown column must error even with no rows"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Bogus"), "{msg}");
    }
}
