//! Unified CLI output with TTY-aware formatting
//!
//! All user-facing messages should go through this module to ensure
//! consistent emoji/prefix usage and proper pipe/redirect behavior.

use crossterm::style::{Color as CrosstermColor, Stylize};
use std::io::IsTerminal;
use std::sync::OnceLock;

/// Message severity level
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Success,
    Error,
    Warn,
    Info,
    Hint,
    Step,
}

/// Cached TTY detection result
static STDOUT_IS_TTY: OnceLock<bool> = OnceLock::new();
static STDERR_IS_TTY: OnceLock<bool> = OnceLock::new();

/// Check if stdout is a TTY (cached)
pub fn is_tty() -> bool {
    *STDOUT_IS_TTY.get_or_init(|| std::io::stdout().is_terminal())
}

/// Check if stderr is a TTY (cached)
pub fn is_tty_stderr() -> bool {
    *STDERR_IS_TTY.get_or_init(|| std::io::stderr().is_terminal())
}

/// Whether to use rich (emoji + color) output
fn should_use_rich(is_tty: bool) -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    is_tty
}

/// Whether to use rich output for stdout (use for println! / stdout)
pub fn should_use_rich_stdout() -> bool {
    should_use_rich(is_tty())
}

/// Whether to use rich output for stderr (use for eprintln! / stderr)
pub fn should_use_rich_stderr() -> bool {
    should_use_rich(is_tty_stderr())
}

/// Format a message line for the given level and TTY mode
pub fn format_line(level: Level, msg: &str, rich: bool) -> String {
    if rich {
        match level {
            Level::Success => format!("\u{2705} {}", msg.with(CrosstermColor::Green)),
            Level::Error => format!("\u{274c} {}", msg.with(CrosstermColor::Red)),
            Level::Warn => format!("\u{26a0}\u{fe0f}  {}", msg.with(CrosstermColor::Yellow)),
            Level::Info => format!("\u{2139}\u{fe0f}  {}", msg.with(CrosstermColor::Cyan)),
            Level::Hint => format!("\u{1f4a1} {}", msg.with(CrosstermColor::DarkGrey)),
            Level::Step => format!("\u{25b6} {}", msg.with(CrosstermColor::White).bold()),
        }
    } else {
        match level {
            Level::Success => format!("[ok] {msg}"),
            Level::Error => format!("[error] {msg}"),
            Level::Warn => format!("[warn] {msg}"),
            Level::Info => format!("[info] {msg}"),
            Level::Hint => format!("[hint] {msg}"),
            Level::Step => format!(":: {msg}"),
        }
    }
}

/// Print a success message to stdout
pub fn success(msg: &str) {
    println!(
        "{}",
        format_line(Level::Success, msg, should_use_rich(is_tty()))
    );
}

/// Print an error message to stderr
pub fn error(msg: &str) {
    eprintln!(
        "{}",
        format_line(Level::Error, msg, should_use_rich(is_tty_stderr()))
    );
}

/// Print a warning message to stdout
pub fn warn(msg: &str) {
    println!("{}", format_line(Level::Warn, msg, should_use_rich(is_tty())));
}

/// Print an info message to stdout
pub fn info(msg: &str) {
    println!("{}", format_line(Level::Info, msg, should_use_rich(is_tty())));
}

/// Print a hint message to stdout
pub fn hint(msg: &str) {
    println!("{}", format_line(Level::Hint, msg, should_use_rich(is_tty())));
}

/// Print a step/action message to stdout (e.g., "Rotating secret...")
pub fn step(msg: &str) {
    println!("{}", format_line(Level::Step, msg, should_use_rich(is_tty())));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_success_no_tty() {
        let msg = format_line(Level::Success, "done", false);
        assert_eq!(msg, "[ok] done");
    }

    #[test]
    fn test_format_error_no_tty() {
        let msg = format_line(Level::Error, "failed", false);
        assert_eq!(msg, "[error] failed");
    }

    #[test]
    fn test_format_warn_no_tty() {
        let msg = format_line(Level::Warn, "careful", false);
        assert_eq!(msg, "[warn] careful");
    }

    #[test]
    fn test_format_info_no_tty() {
        let msg = format_line(Level::Info, "note", false);
        assert_eq!(msg, "[info] note");
    }

    #[test]
    fn test_format_hint_no_tty() {
        let msg = format_line(Level::Hint, "try this", false);
        assert_eq!(msg, "[hint] try this");
    }

    #[test]
    fn test_format_step_no_tty() {
        let msg = format_line(Level::Step, "Rotating secret", false);
        assert_eq!(msg, ":: Rotating secret");
    }

    #[test]
    fn test_format_success_tty() {
        let msg = format_line(Level::Success, "done", true);
        assert!(msg.contains("done"));
        assert!(msg.starts_with("\u{2705}"));
    }

    #[test]
    fn test_no_color_env_respected() {
        let msg = format_line(Level::Success, "done", false);
        assert!(!msg.contains("\u{2705}"));
    }
}
