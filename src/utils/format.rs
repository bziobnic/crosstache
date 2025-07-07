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
use std::io::{stdout, Write};
use tabled::{
    settings::{object::Rows, Alignment, Color, Modify, Padding, Style, Width},
    Table, Tabled,
};

/// Output format options
#[derive(Debug, Clone, PartialEq)]
pub enum OutputFormat {
    Table,
    Json,
    Yaml,
    Csv,
    Raw,
}

/// Color theme for console output
#[derive(Debug, Clone)]
pub struct ColorTheme {
    pub header: CrosstermColor,
    pub success: CrosstermColor,
    pub warning: CrosstermColor,
    pub error: CrosstermColor,
    pub info: CrosstermColor,
    pub accent: CrosstermColor,
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self {
            header: CrosstermColor::Blue,
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
    theme: ColorTheme,
    format: OutputFormat,
    no_color: bool,
}

impl TableFormatter {
    /// Create a new table formatter
    pub fn new(format: OutputFormat, no_color: bool) -> Self {
        Self {
            theme: ColorTheme::default(),
            format,
            no_color,
        }
    }

    /// Create a formatted table from data
    pub fn format_table<T: Tabled>(&self, data: &[T]) -> Result<String> {
        if data.is_empty() {
            return Ok("No data to display".to_string());
        }

        match self.format {
            OutputFormat::Table => self.format_as_table(data),
            OutputFormat::Json => self.format_as_json(data),
            OutputFormat::Yaml => self.format_as_yaml(data),
            OutputFormat::Csv => self.format_as_csv(data),
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
            table.with(Modify::new(Rows::first()).with(Color::FG_BLUE));
        }

        // Auto-adjust width to terminal
        if let Ok((width, _)) = size() {
            table.with(Width::wrap(width as usize));
        }

        Ok(table.to_string())
    }

    /// Format data as JSON
    fn format_as_json<T: Tabled>(&self, _data: &[T]) -> Result<String> {
        // Note: This is a simplified implementation
        // In a real implementation, you'd need to serialize T to JSON
        Ok("JSON output not yet implemented".to_string())
    }

    /// Format data as YAML
    fn format_as_yaml<T: Tabled>(&self, _data: &[T]) -> Result<String> {
        // Note: This is a simplified implementation
        Ok("YAML output not yet implemented".to_string())
    }

    /// Format data as CSV
    fn format_as_csv<T: Tabled>(&self, _data: &[T]) -> Result<String> {
        // Note: This is a simplified implementation
        Ok("CSV output not yet implemented".to_string())
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
            format!("=== {} ===", title)
        } else {
            format!("=== {} ===", title.with(self.theme.header).bold())
        };

        println!("{}", styled_title);
        Ok(())
    }

    /// Print a success message
    pub fn print_success(&self, message: &str) -> Result<()> {
        let styled_message = if self.no_color {
            format!("✓ {}", message)
        } else {
            format!("✓ {}", message.with(self.theme.success))
        };

        println!("{}", styled_message);
        Ok(())
    }

    /// Print a warning message
    pub fn print_warning(&self, message: &str) -> Result<()> {
        let styled_message = if self.no_color {
            format!("⚠ {}", message)
        } else {
            format!("⚠ {}", message.with(self.theme.warning))
        };

        println!("{}", styled_message);
        Ok(())
    }

    /// Print an error message
    pub fn print_error(&self, message: &str) -> Result<()> {
        let styled_message = if self.no_color {
            format!("✗ {}", message)
        } else {
            format!("✗ {}", message.with(self.theme.error))
        };

        eprintln!("{}", styled_message);
        Ok(())
    }

    /// Print an info message
    pub fn print_info(&self, message: &str) -> Result<()> {
        let styled_message = if self.no_color {
            format!("ℹ {}", message)
        } else {
            format!("ℹ {}", message.with(self.theme.info))
        };

        println!("{}", styled_message);
        Ok(())
    }

    /// Format key-value pairs
    pub fn format_key_value_pairs(&self, pairs: &[(&str, &str)]) -> String {
        let max_key_length = pairs.iter().map(|(key, _)| key.len()).max().unwrap_or(0);

        pairs
            .iter()
            .map(|(key, value)| {
                let formatted_key = if self.no_color {
                    format!("{:width$}", key, width = max_key_length)
                } else {
                    format!(
                        "{:width$}",
                        key.with(self.theme.accent).bold(),
                        width = max_key_length
                    )
                };
                format!("{}: {}", formatted_key, value)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Print a separator line
    pub fn print_separator(&self) -> Result<()> {
        if let Ok((width, _)) = size() {
            let line = "─".repeat(width as usize);
            if self.no_color {
                println!("{}", line);
            } else {
                println!("{}", line.with(self.theme.info));
            }
        } else {
            println!("{}", "─".repeat(80));
        }
        Ok(())
    }

    /// Clear the screen
    pub fn clear_screen(&self) -> Result<()> {
        execute!(stdout(), Clear(ClearType::All))?;
        Ok(())
    }

    /// Print a banner with border
    pub fn print_banner(&self, title: &str, subtitle: Option<&str>) -> Result<()> {
        let width = if let Ok((w, _)) = size() {
            (w as usize).min(80)
        } else {
            80
        };

        let border = "═".repeat(width);
        let title_line = format!("║ {:^width$} ║", title, width = width - 4);

        if self.no_color {
            println!("╔{}╗", border);
            println!("{}", title_line);
            if let Some(sub) = subtitle {
                let subtitle_line = format!("║ {:^width$} ║", sub, width = width - 4);
                println!("{}", subtitle_line);
            }
            println!("╚{}╝", border);
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

/// Progress indicator for long-running operations
pub struct ProgressIndicator {
    message: String,
    no_color: bool,
}

impl ProgressIndicator {
    /// Create a new progress indicator
    pub fn new(message: String, no_color: bool) -> Self {
        Self { message, no_color }
    }

    /// Start the progress indicator
    pub fn start(&self) -> Result<()> {
        if self.no_color {
            print!("{} ... ", self.message);
        } else {
            print!("{} ... ", self.message.clone().with(CrosstermColor::Cyan));
        }
        stdout().flush()?;
        Ok(())
    }

    /// Finish the progress indicator with success
    pub fn finish_success(&self, result_message: Option<&str>) -> Result<()> {
        let message = result_message.unwrap_or("Done");
        if self.no_color {
            println!("✓ {}", message);
        } else {
            println!("✓ {}", message.with(CrosstermColor::Green));
        }
        Ok(())
    }

    /// Finish the progress indicator with error
    pub fn finish_error(&self, error_message: Option<&str>) -> Result<()> {
        let message = error_message.unwrap_or("Failed");
        if self.no_color {
            println!("✗ {}", message);
        } else {
            println!("✗ {}", message.with(CrosstermColor::Red));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tabled::Tabled;

    #[derive(Tabled)]
    struct TestData {
        name: String,
        value: String,
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

        let formatter = TableFormatter::new(OutputFormat::Table, true);
        let result = formatter.format_table(&data);
        assert!(result.is_ok());
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
}

/// Convenience function for formatting a table with default settings
pub fn format_table(mut table: Table, no_color: bool) -> String {
    table
        .with(Style::rounded())
        .with(Modify::new(Rows::first()).with(Alignment::center()))
        .with(Padding::new(1, 1, 0, 0));

    if !no_color {
        table.with(Modify::new(Rows::first()).with(Color::FG_BLUE));
    }

    table.to_string()
}
