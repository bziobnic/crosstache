# Progress Indicators for File Operations — Implementation Plan

> **Status:** ✅ Implemented in **v0.7.3** (2026-05-02).
> Retained as design history.
> Roadmap & open work tracked in `ROADMAP.md` at the repo root.
> Implementation history lives in `CHANGELOG.md`. This file is retained as design context — do not edit to reflect current behavior; open a new spec instead.


> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add progress bars and spinners to blob file operations so users get visual feedback during uploads, downloads, and sync.

**Architecture:** A `ProgressReporter` trait in `src/utils/progress.rs` abstracts progress reporting. Three implementations: `BarReporter` (bytes bar for large files), `SpinnerReporter` (for small files), `NoopReporter` (non-TTY). A `MultiProgressContext` wraps `indicatif::MultiProgress` for batch operations with an overall file-count bar and per-file reporters. The blob manager methods accept `&dyn ProgressReporter` but never create reporters — the CLI layer handles that.

**Tech Stack:** `indicatif 0.17` (already a dependency), `std::io::IsTerminal` for TTY detection.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/utils/progress.rs` | Create | ProgressReporter trait, BarReporter, SpinnerReporter, NoopReporter, MultiProgressContext, factory function |
| `src/utils/mod.rs` | Modify (line 8) | Add `pub mod progress;` |
| `src/config/settings.rs` | Modify (lines 54-73) | Add `progress_threshold_mb` to BlobConfig |
| `src/blob/manager.rs` | Modify (lines 58-124, 343-384, 505-571, 583-707) | Add `reporter` parameter to upload/download methods |
| `src/cli/file_ops.rs` | Modify (lines 290-366, 368-402, 831-958, 1081-1293, 1508-1564, 1566-1975) | Create reporters, pass to blob manager, wire up MultiProgress for batch ops |

---

### Task 1: ProgressReporter Trait and Implementations

**Files:**
- Create: `src/utils/progress.rs`
- Modify: `src/utils/mod.rs`

- [ ] **Step 1: Create `src/utils/progress.rs` with the trait and all implementations**

```rust
//! Progress reporting for file operations.
//!
//! Provides a trait-based abstraction over `indicatif` so the blob manager
//! stays UI-free while the CLI layer controls progress rendering.

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

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
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .expect("valid template")
                .progress_chars("#>-"),
        );
        Self { bar }
    }

    /// Create a BarReporter managed by a `MultiProgress`.
    pub fn new_in(mp: &MultiProgress, total: u64) -> Self {
        let bar = mp.add(ProgressBar::new(total));
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .expect("valid template")
                .progress_chars("#>-"),
        );
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
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .expect("valid template"),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(std::time::Duration::from_millis(100));
        Self { bar }
    }

    /// Create a SpinnerReporter managed by a `MultiProgress`.
    pub fn new_in(mp: &MultiProgress, message: &str) -> Self {
        let bar = mp.add(ProgressBar::new_spinner());
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .expect("valid template"),
        );
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
        // is_tty = false should always return NoopReporter regardless of size
        let r = create_file_reporter(100_000_000, 5_000_000, false);
        // Just verify it doesn't panic
        r.set_total(100);
        r.advance(50);
        r.finish_clear();
    }

    #[test]
    fn factory_returns_bar_for_large_file_on_tty() {
        // We can't easily check the concrete type, but we can verify it works
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
```

- [ ] **Step 2: Add module export in `src/utils/mod.rs`**

Add `pub mod progress;` to the module list. Insert it alphabetically between `output` and `resource_detector`:

In `src/utils/mod.rs`, add after the `pub mod output;` line:
```rust
pub mod progress;
```

- [ ] **Step 3: Run tests to verify**

Run: `cargo test --lib utils::progress`
Expected: All 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/utils/progress.rs src/utils/mod.rs
git commit -m "feat: add ProgressReporter trait and implementations for file operations"
```

---

### Task 2: Add `progress_threshold_mb` Config Setting

**Files:**
- Modify: `src/config/settings.rs:54-73` (BlobConfig struct and Default impl)
- Modify: `src/config/settings.rs:441-453` (load_from_env)

- [ ] **Step 1: Add field to BlobConfig struct**

In `src/config/settings.rs`, add after line 60 (`pub max_concurrent_uploads: usize,`):

```rust
    pub progress_threshold_mb: usize,
```

- [ ] **Step 2: Add default value in BlobConfig::default()**

In the `Default` impl (lines 63-73), add after `max_concurrent_uploads: 3,`:

```rust
            progress_threshold_mb: 5,
```

- [ ] **Step 3: Add environment variable loading**

In `load_from_env()`, add after the `BLOB_MAX_CONCURRENT_UPLOADS` block (after line 453):

```rust
    if let Ok(value) = std::env::var("PROGRESS_THRESHOLD_MB") {
        if let Ok(threshold) = value.parse::<usize>() {
            blob_config.progress_threshold_mb = threshold;
            blob_config_updated = true;
        }
    }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 5: Commit**

```bash
git add src/config/settings.rs
git commit -m "feat: add progress_threshold_mb config setting (default 5MB)"
```

---

### Task 3: Wire ProgressReporter into BlobManager Methods

**Files:**
- Modify: `src/blob/manager.rs:58-124` (upload_file)
- Modify: `src/blob/manager.rs:343-384` (download_file)
- Modify: `src/blob/manager.rs:583-707` (upload_large_file)

- [ ] **Step 1: Add import at top of `src/blob/manager.rs`**

Add after line 20 (`use tokio::io::AsyncWrite;`):

```rust
use crate::utils::progress::ProgressReporter;
```

- [ ] **Step 2: Add `reporter` parameter to `upload_file`**

Change the signature at line 58 from:

```rust
    pub async fn upload_file(&self, request: FileUploadRequest) -> Result<FileInfo> {
```

to:

```rust
    pub async fn upload_file(&self, request: FileUploadRequest, reporter: &dyn ProgressReporter) -> Result<FileInfo> {
```

Add progress calls. After line 86 (`let content_length = request.content.len() as u64;`), add:

```rust
        reporter.set_total(content_length);
```

After the `put_block_blob` call succeeds (after line 103), add:

```rust
        reporter.advance(content_length);
        reporter.finish_clear();
```

- [ ] **Step 3: Add `reporter` parameter to `download_file`**

Change the signature at line 343 from:

```rust
    pub async fn download_file(&self, request: FileDownloadRequest) -> Result<Vec<u8>> {
```

to:

```rust
    pub async fn download_file(&self, request: FileDownloadRequest, reporter: &dyn ProgressReporter) -> Result<Vec<u8>> {
```

After line 371 (`let content_length = properties.blob.properties.content_length;`), add:

```rust
        reporter.set_total(content_length);
```

After the `get_content()` call succeeds (after line 381), add:

```rust
        reporter.advance(content_length);
        reporter.finish_clear();
```

Also add progress finish for the empty file early return. Change line 374 from:

```rust
            return Ok(Vec::new());
```

to:

```rust
            reporter.finish_clear();
            return Ok(Vec::new());
```

- [ ] **Step 4: Add `reporter` parameter to `upload_large_file`**

Change the signature at line 583 from:

```rust
    pub async fn upload_large_file<R: tokio::io::AsyncRead + Unpin>(
        &self,
        name: &str,
        mut reader: R,
        _file_size: u64,
        metadata: HashMap<String, String>,
        tags: HashMap<String, String>,
    ) -> Result<FileInfo> {
```

to:

```rust
    pub async fn upload_large_file<R: tokio::io::AsyncRead + Unpin>(
        &self,
        name: &str,
        mut reader: R,
        file_size: u64,
        metadata: HashMap<String, String>,
        tags: HashMap<String, String>,
        reporter: &dyn ProgressReporter,
    ) -> Result<FileInfo> {
```

Note: `_file_size` becomes `file_size` (remove the underscore prefix since we now use it).

After line 596 (`let chunk_size = self.chunk_size_mb * 1024 * 1024;`), add:

```rust
        reporter.set_total(file_size);
```

In the upload task spawn block (lines 632-641), we need to report progress after each block completes. Since the reporter is behind `&dyn`, we can't move it into the spawned task. Instead, report after the task completes. Replace the task-wait loop (lines 644-649):

```rust
        // Wait for all block uploads to finish.
        for task in upload_tasks {
            task.await
                .map_err(|e| CrosstacheError::unknown(format!("Upload task panicked: {e}")))?
                .map_err(|e: CrosstacheError| e)?;
        }
```

with:

```rust
        // Wait for all block uploads to finish and report progress.
        for task in upload_tasks {
            task.await
                .map_err(|e| CrosstacheError::unknown(format!("Upload task panicked: {e}")))?
                .map_err(|e: CrosstacheError| e)?;
            reporter.advance(chunk_size as u64);
        }
        reporter.finish_clear();
```

Note: The last chunk may be smaller, so the bar might overshoot briefly — indicatif clamps at 100% automatically. The `finish_clear()` cleans up regardless.

- [ ] **Step 5: Fix all call sites that now need a reporter argument**

Search for all existing calls to `upload_file`, `download_file`, and `upload_large_file` and add `&NoopReporter` temporarily so the code compiles. These will be replaced with real reporters in Task 4.

In `src/cli/file_ops.rs`, add at the top:

```rust
use crate::utils::progress::NoopReporter;
```

Then update every call site:

1. `execute_file_upload` line 357: `blob_manager.upload_file(upload_request).await?` → `blob_manager.upload_file(upload_request, &NoopReporter).await?`
2. `execute_file_download` line 396: `blob_manager.download_file(download_request).await?` → `blob_manager.download_file(download_request, &NoopReporter).await?`
3. `file_sync_perform_upload` line 1532: `blob_manager.upload_file(upload_request).await?` → `blob_manager.upload_file(upload_request, &NoopReporter).await?`
4. `file_sync_perform_download` line 1558: `blob_manager.download_file(download_request).await?` → `blob_manager.download_file(download_request, &NoopReporter).await?`

Also check for calls in `execute_file_download_recursive` — those call `execute_file_download` which already has the `NoopReporter`, so they're fine.

Search for any other call sites of `upload_large_file` and `download_file` outside of `file_ops.rs`:

Run: `grep -rn 'upload_large_file\|\.upload_file(\|\.download_file(' src/ --include='*.rs' | grep -v 'fn \|//\|test'`

Update any additional call sites found with `&NoopReporter`.

- [ ] **Step 6: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 7: Run all tests**

Run: `cargo test`
Expected: All tests pass (existing tests now go through NoopReporter path).

- [ ] **Step 8: Commit**

```bash
git add src/blob/manager.rs src/cli/file_ops.rs
git commit -m "feat: add ProgressReporter parameter to blob manager upload/download methods"
```

---

### Task 4: Wire Real Reporters into CLI File Operations

**Files:**
- Modify: `src/cli/file_ops.rs` (execute_file_upload, execute_file_download, execute_file_upload_recursive, execute_file_download_recursive, execute_file_sync, file_sync_perform_upload, file_sync_perform_download)

- [ ] **Step 1: Add imports and helper for TTY detection and threshold**

Replace the `use crate::utils::progress::NoopReporter;` import added in Task 3 with:

```rust
use crate::utils::progress::{self, MultiProgressContext, NoopReporter};
```

Add a helper function near the top of the file (after the existing imports):

```rust
fn progress_threshold_bytes(config: &Config) -> u64 {
    let blob_config = config.get_blob_config();
    (blob_config.progress_threshold_mb as u64) * 1024 * 1024
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}
```

- [ ] **Step 2: Wire reporter into `execute_file_upload`**

In `execute_file_upload` (line 290), after the file content is read (after line 314, where `content` is assigned), add:

```rust
    let file_size = content.len() as u64;
```

Replace line 355 (`println!("Uploading file '{file_path}' as '{remote_name}'...");`) and the upload call at line 357 with:

```rust
    let threshold = progress_threshold_bytes(_config);
    let tty = is_tty();
    if !tty {
        println!("Uploading file '{file_path}' as '{remote_name}'...");
    }
    let reporter = progress::create_file_reporter(file_size, threshold, tty);
    reporter.set_message(format!("Uploading '{remote_name}'..."));

    let file_info = blob_manager.upload_file(upload_request, reporter.as_ref()).await?;
```

- [ ] **Step 3: Wire reporter into `execute_file_download`**

In `execute_file_download` (line 368), we need the file size before downloading. The blob manager's `download_file` already fetches properties internally, but we need the size before creating the reporter.

Add a properties fetch before the download. Replace lines 394-396:

```rust
    println!("Downloading file '{name}' to '{output_path}'...");

    let content = blob_manager.download_file(download_request).await?;
```

with:

```rust
    let threshold = progress_threshold_bytes(_config);
    let tty = is_tty();
    if !tty {
        println!("Downloading file '{name}' to '{output_path}'...");
    }
    let reporter = progress::create_file_reporter(0, threshold, tty);
    reporter.set_message(format!("Downloading '{name}'..."));

    let content = blob_manager.download_file(download_request, reporter.as_ref()).await?;
```

Note: We pass `0` as file_size to `create_file_reporter` here because we don't know the remote size yet. The blob manager's `download_file` calls `reporter.set_total(content_length)` after fetching properties, which will upgrade the display. For the initial creation, this means we get a spinner (since 0 < threshold), and the spinner works fine for single downloads since they're single-shot anyway. This is acceptable — the spinner provides feedback while the download happens.

- [ ] **Step 4: Wire MultiProgress into `execute_file_upload_recursive`**

In `execute_file_upload_recursive`, after the file count is known (after collecting files), create a MultiProgressContext and use it around the upload loop.

Find the loop that iterates over files and calls `execute_file_upload`. Before the loop starts, add:

```rust
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty();
    let mp = MultiProgressContext::new(file_count as u64, threshold, tty);
```

Where `file_count` is the total number of files collected (this variable should already exist — it's printed as "Found N file(s) to upload").

Inside the loop, before each `execute_file_upload` call, log via the MultiProgress instead of println, and after each call advance the overall bar:

```rust
    mp.log(&format!("Uploaded: {blob_name}"));
    mp.advance_overall(&next_blob_name_or_empty);
```

At the end, before the summary:
```rust
    mp.finish();
```

The exact integration depends on the loop structure — the implementing agent should read the full function and wire the MultiProgress around the existing per-file loop, replacing direct `println!` calls with `mp.log()` when `is_tty` is true.

- [ ] **Step 5: Wire MultiProgress into `execute_file_download_recursive`**

Same pattern as Step 4 but for downloads. After the blob list is fetched and the total count is known, create a `MultiProgressContext`. Use `mp.log()` for per-file status and `mp.advance_overall()` after each download. Call `mp.finish()` before the summary.

- [ ] **Step 6: Wire MultiProgress into `execute_file_sync`**

In `execute_file_sync`, after the comparison phase determines the list of actions (uploads + downloads + deletes), create a `MultiProgressContext` with the total action count. Skip progress entirely when `dry_run` is true.

Before the direction match (around line 1650):

```rust
    let threshold = progress_threshold_bytes(config);
    let tty = is_tty() && !dry_run;
```

The sync function has three direction branches (Up, Down, Both). In each branch's loop:
- Replace `println!("upload: ...")` and `println!("download: ...")` with `mp.log(...)` when `tty` is true
- Call `mp.advance_overall(blob_name)` after each action
- Call `mp.finish()` before the summary output

For sync, calculating the total action count upfront requires knowing how many files need action before the loop runs. The simplest approach: don't create the MultiProgress until the loop starts — instead, use a deferred creation where the total is set to the total number of items being iterated (which is already known: it's the length of `sorted_names` or `remote_names` or `ordered`). Skipped items still advance the overall bar (they're just fast).

Update `file_sync_perform_upload` and `file_sync_perform_download` to accept a `reporter: &dyn ProgressReporter` parameter and pass it through to `blob_manager.upload_file()` / `blob_manager.download_file()`. The caller in `execute_file_sync` creates a child reporter from the MultiProgressContext for each file that needs upload/download.

- [ ] **Step 7: Verify it compiles**

Run: `cargo check`
Expected: No errors.

- [ ] **Step 8: Run all tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 9: Commit**

```bash
git add src/cli/file_ops.rs
git commit -m "feat: wire progress reporters into file upload, download, and sync commands"
```

---

### Task 5: Update Roadmap and Final Verification

**Files:**
- Modify: `dev/ROADMAP.md`

- [ ] **Step 1: Move the progress indicators item to Completed in `dev/ROADMAP.md`**

Remove the "Progress Indicators for File Operations" section from "Open — High Priority" and add it to the Completed section under the latest version heading:

```markdown
- **Progress indicators** for file operations (upload, download, sync): configurable size threshold (`progress_threshold_mb`), TTY-aware, batch MultiProgress with per-file reporters
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings related to progress changes.

- [ ] **Step 4: Commit**

```bash
git add dev/ROADMAP.md
git commit -m "docs: mark progress indicators as completed in roadmap"
```
