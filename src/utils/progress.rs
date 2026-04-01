//! Progress reporting for file operations.
//!
//! Provides a trait-based abstraction over `indicatif` so the blob manager
//! stays UI-free while the CLI layer controls progress rendering.

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

// ---------------------------------------------------------------------------
// Private style helpers
// ---------------------------------------------------------------------------

fn bar_style() -> ProgressStyle {
    ProgressStyle::default_bar()
        .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .expect("valid template")
        .progress_chars("#>-")
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .template("{spinner:.green} {msg}")
        .expect("valid template")
}

/// Trait for reporting progress during file operations.
/// All methods are no-ops by default so that callers can pass a `NoopReporter`
/// when progress display is unwanted (e.g., non-TTY or tests).
pub trait ProgressReporter: Send + Sync {
    fn set_total(&self, total: u64);
    fn advance(&self, amount: u64);
    fn set_message(&self, msg: String);
    fn finish_with_message(&self, msg: String);
    fn finish_clear(&self);
}

// ---------------------------------------------------------------------------
// NoopReporter
// ---------------------------------------------------------------------------

/// Does nothing. Used when stdout is not a TTY.
pub struct NoopReporter;

impl ProgressReporter for NoopReporter {
    fn set_total(&self, _total: u64) {}
    fn advance(&self, _amount: u64) {}
    fn set_message(&self, _msg: String) {}
    fn finish_with_message(&self, _msg: String) {}
    fn finish_clear(&self) {}
}

// ---------------------------------------------------------------------------
// BarReporter
// ---------------------------------------------------------------------------

/// Bytes-level progress bar for large files.
pub struct BarReporter {
    bar: ProgressBar,
}

impl BarReporter {
    pub fn new(total: u64) -> Self {
        let bar = ProgressBar::new(total);
        bar.set_style(bar_style());
        Self { bar }
    }

    /// Create a BarReporter managed by a `MultiProgress`.
    pub fn new_in(mp: &MultiProgress, total: u64) -> Self {
        let bar = mp.add(ProgressBar::new(total));
        bar.set_style(bar_style());
        Self { bar }
    }
}

impl ProgressReporter for BarReporter {
    fn set_total(&self, total: u64) {
        self.bar.set_length(total);
    }
    fn advance(&self, amount: u64) {
        self.bar.inc(amount);
    }
    fn set_message(&self, msg: String) {
        self.bar.set_message(msg);
    }
    fn finish_with_message(&self, msg: String) {
        self.bar.finish_with_message(msg);
    }
    fn finish_clear(&self) {
        self.bar.finish_and_clear();
    }
}

// ---------------------------------------------------------------------------
// SpinnerReporter
// ---------------------------------------------------------------------------

/// Spinner for small files — shows activity without byte-level tracking.
pub struct SpinnerReporter {
    bar: ProgressBar,
}

impl SpinnerReporter {
    pub fn new(message: &str) -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(spinner_style());
        bar.set_message(message.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(100));
        Self { bar }
    }

    /// Create a SpinnerReporter managed by a `MultiProgress`.
    pub fn new_in(mp: &MultiProgress, message: &str) -> Self {
        let bar = mp.add(ProgressBar::new_spinner());
        bar.set_style(spinner_style());
        bar.set_message(message.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(100));
        Self { bar }
    }
}

impl ProgressReporter for SpinnerReporter {
    fn set_total(&self, _total: u64) {}
    fn advance(&self, _amount: u64) {}
    fn set_message(&self, msg: String) {
        self.bar.set_message(msg);
    }
    fn finish_with_message(&self, msg: String) {
        self.bar.finish_with_message(msg);
    }
    fn finish_clear(&self) {
        self.bar.finish_and_clear();
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create the appropriate reporter for a single-file operation.
pub fn create_file_reporter(
    file_size: u64,
    threshold_bytes: u64,
    is_tty: bool,
) -> Box<dyn ProgressReporter> {
    if !is_tty {
        return Box::new(NoopReporter);
    }
    if file_size >= threshold_bytes {
        Box::new(BarReporter::new(file_size))
    } else {
        Box::new(SpinnerReporter::new(""))
    }
}

// ---------------------------------------------------------------------------
// MultiProgressContext
// ---------------------------------------------------------------------------

/// Manages an overall file-count bar plus per-file child reporters for batch operations.
pub struct MultiProgressContext {
    mp: MultiProgress,
    overall: ProgressBar,
    threshold_bytes: u64,
    is_tty: bool,
}

impl MultiProgressContext {
    pub fn new(total_files: u64, threshold_bytes: u64, is_tty: bool) -> Self {
        if !is_tty {
            return Self {
                mp: MultiProgress::new(),
                overall: ProgressBar::hidden(),
                threshold_bytes,
                is_tty,
            };
        }
        let mp = MultiProgress::new();
        let overall = mp.add(ProgressBar::new(total_files));
        overall.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files  {msg}")
                .expect("valid template")
                .progress_chars("#>-"),
        );
        Self {
            mp,
            overall,
            threshold_bytes,
            is_tty,
        }
    }

    /// Create a per-file reporter inserted above the overall bar.
    pub fn create_child(&self, file_size: u64, name: &str) -> Box<dyn ProgressReporter> {
        if !self.is_tty {
            return Box::new(NoopReporter);
        }
        if file_size >= self.threshold_bytes {
            Box::new(BarReporter::new_in(&self.mp, file_size))
        } else {
            Box::new(SpinnerReporter::new_in(&self.mp, name))
        }
    }

    /// Log a line above the progress bars (completed file status).
    pub fn log(&self, msg: &str) {
        if self.is_tty {
            let _ = self.mp.println(msg);
        }
    }

    /// Advance the overall bar by one file and set the current file message.
    pub fn advance_overall(&self, current_file: &str) {
        self.overall.inc(1);
        self.overall.set_message(current_file.to_string());
    }

    /// Finish and clear all bars.
    pub fn finish(&self) {
        self.overall.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_reporter_does_not_panic() {
        let r = NoopReporter;
        r.set_total(100);
        r.advance(50);
        r.set_message("hello".into());
        r.finish_with_message("done".into());
        r.finish_clear();
    }

    #[test]
    fn factory_returns_noop_when_not_tty() {
        let r = create_file_reporter(100_000_000, 5_000_000, false);
        r.set_total(100);
        r.advance(50);
        r.finish_clear();
    }

    #[test]
    fn factory_returns_bar_for_large_file_on_tty() {
        let r = create_file_reporter(10_000_000, 5_000_000, true);
        r.set_total(10_000_000);
        r.advance(1_000_000);
        r.finish_clear();
    }

    #[test]
    fn factory_returns_spinner_for_small_file_on_tty() {
        let r = create_file_reporter(1_000, 5_000_000, true);
        r.set_total(0);
        r.advance(0);
        r.finish_clear();
    }

    #[test]
    fn multi_progress_noop_when_not_tty() {
        let ctx = MultiProgressContext::new(10, 5_000_000, false);
        let child = ctx.create_child(100, "test.txt");
        child.set_total(100);
        child.advance(100);
        child.finish_clear();
        ctx.advance_overall("test.txt");
        ctx.log("done: test.txt");
        ctx.finish();
    }
}
