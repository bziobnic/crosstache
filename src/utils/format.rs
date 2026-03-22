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
    settings::{object::Rows, Alignment, Color, Modify, Padding, Style, Width},
    Table, Tabled,
};

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

/// Table formatter with color support
pub struct TableFormatter {
    _theme: ColorTheme,
    format: OutputFormat,
    no_color: bool,
    template: Option<String>,
}

impl TableFormatter {
    /// Create a new table formatter
    pub fn new(format: OutputFormat, no_color: bool, template: Option<String>) -> Self {
        Self {
            _theme: ColorTheme::default(),
            format: format.resolve_for_stdout(),
            no_color,
            template,
        }
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
                OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw => Ok(
                    "No results found. If this is unexpected, check your vault permissions or filter criteria."
                        .to_string(),
                ),
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
        let mut table = Table::new(data);

        // Apply styling
        table
            .with(Style::rounded())
            .with(Modify::new(Rows::first()).with(Alignment::center()))
            .with(Padding::new(1, 1, 0, 0));

        // Apply color if enabled
        if !self.no_color {
            table.with(Modify::new(Rows::first()).with(Color::FG_CYAN));
        }

        // Auto-adjust width to terminal
        if let Ok((width, _)) = size() {
            table.with(Width::wrap(width as usize));
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
        let mut output = String::new();
        // Header row from Tabled trait
        let headers = T::headers();
        let header_row: Vec<String> = headers.iter().map(|h| csv_escape(h.as_ref())).collect();
        output.push_str(&header_row.join(","));
        output.push('\n');
        // Data rows
        for item in data {
            let fields = item.fields();
            let row: Vec<String> = fields.iter().map(|f| csv_escape(f.as_ref())).collect();
            output.push_str(&row.join(","));
            output.push('\n');
        }
        Ok(output)
    }

    /// Format data as plain text
    fn format_as_plain<T: Tabled>(&self, data: &[T]) -> Result<String> {
        let mut table = Table::new(data);
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

/// Escape a value for CSV output (RFC 4180)
fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
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

/// Convenience function for formatting a table with default settings
pub fn format_table(mut table: Table, no_color: bool) -> String {
    table
        .with(Style::rounded())
        .with(Modify::new(Rows::first()).with(Alignment::center()))
        .with(Padding::new(1, 1, 0, 0));

    if !no_color {
        table.with(Modify::new(Rows::first()).with(Color::FG_CYAN));
    }

    table.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use tabled::Tabled;

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

        let formatter = TableFormatter::new(OutputFormat::Table, true, None);
        let result = formatter.format_table(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_json_is_valid_array() {
        let data: Vec<TestData> = vec![];
        let formatter = TableFormatter::new(OutputFormat::Json, true, None);
        let out = formatter.format_table(&data).expect("format");
        assert_eq!(out.trim(), "[]");
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
            TestData { name: "secret1".to_string(), value: "abc123".to_string(), status: "active".to_string() },
            TestData { name: "secret2".to_string(), value: "xyz789".to_string(), status: "inactive".to_string() },
        ];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("export {{Name}}={{Value}}".to_string()));
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "export secret1=abc123\nexport secret2=xyz789");
    }

    #[test]
    fn test_template_case_insensitive() {
        let data = vec![TestData { name: "mykey".to_string(), value: "myval".to_string(), status: "active".to_string() }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("{{name}} {{NAME}} {{Name}}".to_string()));
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "mykey mykey mykey");
    }

    #[test]
    fn test_template_unknown_field_left_as_is() {
        let data = vec![TestData { name: "key".to_string(), value: "val".to_string(), status: "ok".to_string() }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("{{Name}}: {{nonexistent}}".to_string()));
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "key: {{nonexistent}}");
    }

    #[test]
    fn test_template_missing_template_flag_errors() {
        let data = vec![TestData { name: "key".to_string(), value: "val".to_string(), status: "ok".to_string() }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, None);
        let result = formatter.format_table(&data);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--template"), "Error should mention --template flag");
    }

    #[test]
    fn test_template_empty_data_returns_empty() {
        let data: Vec<TestData> = vec![];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("{{Name}}".to_string()));
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_template_whitespace_in_braces() {
        let data = vec![TestData { name: "key".to_string(), value: "val".to_string(), status: "ok".to_string() }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("{{ Name }} = {{  Value  }}".to_string()));
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "key = val");
    }

    #[test]
    fn test_template_multiple_fields() {
        let data = vec![TestData { name: "db_pass".to_string(), value: "secret".to_string(), status: "active".to_string() }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("{{Name}}={{Value}} ({{Status}})".to_string()));
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

        let data = vec![MultiWordData { secret_name: "api-key".to_string(), created_by: "admin".to_string() }];
        let formatter = TableFormatter::new(OutputFormat::Template, true, Some("{{Secret Name}} by {{Created By}}".to_string()));
        let result = formatter.format_table(&data).unwrap();
        assert_eq!(result, "api-key by admin");
    }
}
