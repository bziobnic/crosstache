//! Unified TTY-aware output module.
//!
//! Provides convenience functions for all user-facing CLI output with
//! automatic TTY detection: emoji prefixes and crossterm colors in
//! interactive terminals, plain-text prefixes when piped/redirected.

use crossterm::style::Stylize;
use std::io::{IsTerminal, Write};
use std::sync::OnceLock;

/// Output severity / category level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Success,
    Error,
    Warn,
    Info,
    Hint,
    Step,
}

// ── TTY detection (cached) ──────────────────────────────────────────

static IS_TTY_STDOUT: OnceLock<bool> = OnceLock::new();
static IS_TTY_STDERR: OnceLock<bool> = OnceLock::new();

/// Returns `true` if stdout is an interactive terminal (cached).
pub fn is_tty() -> bool {
    *IS_TTY_STDOUT.get_or_init(|| std::io::stdout().is_terminal())
}

/// Returns `true` if stderr is an interactive terminal (cached).
pub fn is_tty_stderr() -> bool {
    *IS_TTY_STDERR.get_or_init(|| std::io::stderr().is_terminal())
}

/// Returns `true` when stdout is a TTY **and** `NO_COLOR` is not set.
pub fn should_use_rich_stdout() -> bool {
    is_tty() && std::env::var_os("NO_COLOR").is_none()
}

/// Returns `true` when stderr is a TTY **and** `NO_COLOR` is not set.
pub fn should_use_rich_stderr() -> bool {
    is_tty_stderr() && std::env::var_os("NO_COLOR").is_none()
}

// ── Core formatter ──────────────────────────────────────────────────

/// Format a single output line with the appropriate prefix.
///
/// When `rich` is `true` the line gets an emoji prefix and crossterm
/// ANSI colours.  When `false` it uses a plain-text tag such as
/// `[ok]` or `[error]`.
pub fn format_line(level: Level, msg: &str, rich: bool) -> String {
    if rich {
        format_rich(level, msg)
    } else {
        format_plain(level, msg)
    }
}

fn format_rich(level: Level, msg: &str) -> String {
    match level {
        Level::Success => format!("\u{2705} {}", msg.green()),
        Level::Error => format!("\u{274c} {}", msg.red()),
        Level::Warn => format!("\u{26a0}\u{fe0f} {}", msg.yellow()),
        Level::Info => format!("\u{2139}\u{fe0f} {}", msg.cyan()),
        Level::Hint => format!("\u{1f4a1} {}", msg.dark_grey()),
        Level::Step => format!("\u{25b6} {}", msg.white().bold()),
    }
}

fn format_plain(level: Level, msg: &str) -> String {
    let tag = match level {
        Level::Success => "[ok]",
        Level::Error => "[error]",
        Level::Warn => "[warn]",
        Level::Info => "[info]",
        Level::Hint => "[hint]",
        Level::Step => "::",
    };
    format!("{tag} {msg}")
}

// ── Convenience printers ────────────────────────────────────────────

/// Print a success message to stdout.
pub fn success(msg: &str) {
    let line = format_line(Level::Success, msg, should_use_rich_stdout());
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Print an error message to stderr.
pub fn error(msg: &str) {
    let line = format_line(Level::Error, msg, should_use_rich_stderr());
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Print a warning message to stdout.
pub fn warn(msg: &str) {
    let line = format_line(Level::Warn, msg, should_use_rich_stdout());
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Print an informational message to stdout.
pub fn info(msg: &str) {
    let line = format_line(Level::Info, msg, should_use_rich_stdout());
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Print a hint message to stdout.
pub fn hint(msg: &str) {
    let line = format_line(Level::Hint, msg, should_use_rich_stdout());
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// Print a step message to stdout.
pub fn step(msg: &str) {
    let line = format_line(Level::Step, msg, should_use_rich_stdout());
    let _ = writeln!(std::io::stdout(), "{line}");
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Plain (pipe) mode ───────────────────────────────────────────

    #[test]
    fn test_format_success_no_tty() {
        let out = format_line(Level::Success, "done", false);
        assert_eq!(out, "[ok] done");
    }

    #[test]
    fn test_format_error_no_tty() {
        let out = format_line(Level::Error, "fail", false);
        assert_eq!(out, "[error] fail");
    }

    #[test]
    fn test_format_warn_no_tty() {
        let out = format_line(Level::Warn, "careful", false);
        assert_eq!(out, "[warn] careful");
    }

    #[test]
    fn test_format_info_no_tty() {
        let out = format_line(Level::Info, "note", false);
        assert_eq!(out, "[info] note");
    }

    #[test]
    fn test_format_hint_no_tty() {
        let out = format_line(Level::Hint, "tip", false);
        assert_eq!(out, "[hint] tip");
    }

    #[test]
    fn test_format_step_no_tty() {
        let out = format_line(Level::Step, "building", false);
        assert_eq!(out, ":: building");
    }

    // ── Rich (TTY) mode ─────────────────────────────────────────────

    #[test]
    fn test_format_success_tty() {
        let out = format_line(Level::Success, "done", true);
        assert!(out.contains('\u{2705}'), "should contain checkmark emoji");
        assert!(out.contains("done"), "should contain the message");
    }

    #[test]
    fn test_format_step_tty() {
        let out = format_line(Level::Step, "building", true);
        assert!(
            out.contains('\u{25b6}'),
            "should contain play-button emoji"
        );
        assert!(out.contains("building"), "should contain the message");
    }
}
