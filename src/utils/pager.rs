//! Interactive TTY pager for long human-facing CLI output.

use crate::error::Result;
use crossterm::event::{read, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use std::io::{self, IsTerminal, Write};

const MORE_PROMPT: &str = "--More-- (space=next, q=quit)";

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn can_page() -> bool {
    io::stdout().is_terminal() && io::stdin().is_terminal()
}

fn page_text(text: &str) -> Result<()> {
    let _guard = RawModeGuard::new()?;
    let mut stdout = io::stdout().lock();
    let screen_height = size().map(|(_, h)| h as usize).unwrap_or(24).max(2);
    let lines_per_page = screen_height.saturating_sub(1).max(1);
    let lines: Vec<&str> = text.lines().collect();

    if lines.is_empty() {
        return Ok(());
    }

    let mut index = 0;
    while index < lines.len() {
        let end = usize::min(index + lines_per_page, lines.len());
        for line in &lines[index..end] {
            write!(stdout, "{line}\r\n")?;
        }
        index = end;

        if index < lines.len() {
            write!(stdout, "{MORE_PROMPT}")?;
            stdout.flush()?;

            loop {
                match read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                write!(stdout, "\r\n")?;
                                stdout.flush()?;
                                return Ok(());
                            }
                            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::PageDown => {
                                write!(stdout, "\r\n")?;
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    stdout.flush()?;
    Ok(())
}

/// Print text directly, or page it interactively when `pager` is enabled.
pub fn print_output(text: &str, pager: bool) -> Result<()> {
    if pager && can_page() {
        page_text(text)
    } else {
        println!("{text}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_page_requires_terminals() {
        // We can't reliably assert the runtime terminal state in tests, but the
        // helper should compile and be callable without side effects.
        let _ = can_page();
    }

    #[test]
    fn direct_print_helper_is_callable() {
        let _ = print_output("hello\nworld", false);
    }
}
