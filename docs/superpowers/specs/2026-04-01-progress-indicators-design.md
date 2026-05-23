# Progress Indicators for File Operations

> **Status:** ✅ Implemented in **v0.7.3** (2026-05-02).
> Retained as design history.
> Roadmap & open work tracked in `ROADMAP.md` at the repo root.
> Implementation history lives in `CHANGELOG.md`. This file is retained as design context — do not edit to reflect current behavior; open a new spec instead.


> Date: 2026-04-01 | Status: Approved

## Overview

Add progress bars and spinners to blob/file operations so users get visual feedback during uploads, downloads, recursive operations, and sync. Uses `indicatif` (already a dependency) with a `ProgressReporter` trait to keep UI concerns out of the blob manager.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Single-file treatment | Bytes bar for large files, spinner for small | Avoids visual noise for quick operations |
| Batch display | Scrolling log + sticky overall bar | Shows per-file completion while tracking overall progress |
| Non-TTY behavior | Suppress progress entirely | Matches existing `auto` format behavior; scripts get clean output |
| Size threshold | Configurable, 5 MB default | Consistent with other tunables (`blob_chunk_size_mb`) |
| Architecture | ProgressReporter trait injection | Clean separation; blob manager stays UI-free |

## Architecture

### ProgressReporter Trait

Located in `src/utils/progress.rs`:

```rust
pub trait ProgressReporter: Send + Sync {
    fn set_total(&self, total: u64);
    fn advance(&self, amount: u64);
    fn set_message(&self, msg: String);
    fn finish_with_message(&self, msg: String);
    fn finish_clear(&self);
}
```

### Implementations

- **`BarReporter`** — wraps `indicatif::ProgressBar`. Bytes-style template: `{spinner} [{bar:40}] {bytes}/{total_bytes} ({eta})`. Used on TTY when file size >= threshold.
- **`SpinnerReporter`** — wraps `indicatif::ProgressBar` in spinner mode. Shows filename and spinner. Used on TTY when file size < threshold.
- **`NoopReporter`** — all methods are no-ops. Used when stdout is not a TTY.

### Factory Function

```rust
pub fn create_file_reporter(file_size: u64, threshold: u64, is_tty: bool) -> Box<dyn ProgressReporter>
```

Returns `BarReporter` if `is_tty && file_size >= threshold`, `SpinnerReporter` if `is_tty && file_size < threshold`, `NoopReporter` otherwise.

### MultiProgressContext

Wraps `indicatif::MultiProgress` for batch operations:

- An overall file-count bar (sticky at bottom), template: `{spinner} [{bar:40}] {pos}/{len} files  {msg}` where `{msg}` is the current filename
- A method to create per-file reporters (bar or spinner based on size vs threshold)
- Completed files log above the bars via `println!` through the MultiProgress

## Configuration

New config key: `progress_threshold_mb`

- **Config file:** `progress_threshold_mb = 5`
- **Environment variable:** `PROGRESS_THRESHOLD_MB`
- **Default:** 5 (MB), converted to bytes internally (`value * 1024 * 1024`)
- **No CLI flag** — display preference, not per-command

Follows the existing pattern of `blob_chunk_size_mb` and `clipboard_timeout`.

## Blob Manager Integration

Methods gain a `reporter: &dyn ProgressReporter` parameter. The manager calls trait methods but never creates reporters.

### Methods Affected

- **`upload_file()`** — `set_total(data.len())`, then `advance()` after the PUT completes (single-shot).
- **`upload_large_file()`** — `set_total(file_size)` upfront, `advance(chunk_size)` after each `put_block()`. Thread-safe via indicatif's built-in atomic counters.
- **`download_file()`** — `set_total(content_length)` from response headers, `advance(bytes.len())` after download completes (single-shot; true streaming is out of scope).
- **`download_file_stream()`** — same advance-after-write pattern.

### Methods NOT Affected

`list_files()`, `delete_file()`, `get_properties()` — fast API calls that don't need progress.

## CLI Layer Integration (file_ops.rs)

TTY detection via `std::io::IsTerminal` on stdout (stable since Rust 1.70).

### Single File Operations

`execute_file_upload` and `execute_file_download`:
- Get file size (local fs for upload, blob properties for download)
- Call `create_file_reporter(file_size, threshold, is_tty)`
- Pass reporter to blob manager method
- Reporter finishes; existing success/error messages print after

### Recursive Upload (`execute_file_upload_recursive`)

- Create `MultiProgressContext` with total file count
- For each file: create per-file reporter from multi-progress, pass to upload, log completion above the bar
- Overall bar advances by 1 after each file
- On finish, clear multi-progress and print existing summary

### Recursive Download (`execute_file_download_recursive`)

Same pattern as recursive upload but with download operations.

### Sync (`execute_file_sync`)

- After comparison phase determines action list, create `MultiProgressContext` with total action count
- Each action (upload/download/delete) advances overall bar
- Upload/download actions get per-file reporters for large files
- `--dry-run` skips progress bars entirely (prints existing dry-run output)

### Non-TTY

`create_file_reporter` returns `NoopReporter`. All existing `println!` output continues unchanged.

## Testing

### Unit Tests (in `src/utils/progress.rs`)

- `NoopReporter` doesn't panic on any method call
- `create_file_reporter` returns correct type based on size/threshold/TTY
- `MultiProgressContext` creates child reporters and tracks overall count

### Integration

- Existing CLI integration tests run without TTY, exercising `NoopReporter` path automatically
- No need to test indicatif rendering; trust the library

## Out of Scope

- True streaming downloads (chunked byte-by-byte for single downloads) — current impl buffers full response
- Progress for `list_files()` pagination
- Progress for delete operations within sync (fast API calls)
