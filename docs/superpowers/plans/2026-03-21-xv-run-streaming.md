# `xv run` Streaming Masking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `xv run` masking path to stream output line-by-line instead of buffering the entire child process output in memory.

**Architecture:** Replace `Command::output()` with `Command::spawn()` + two OS threads reading stdout/stderr via `BufReader::read_until(b'\n')`, masking each line with the existing `mask_secrets()` function, and printing immediately. No new dependencies.

**Tech Stack:** Rust std (`std::thread`, `std::sync::Arc`, `std::io::BufReader`, `std::io::BufRead`)

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/cli/secret_ops.rs` | Modify (lines 1567–1594) | Replace buffered masking with streaming |
| `src/cli/helpers.rs` | No change | `mask_secrets()` reused as-is |

---

### Task 1: Extract streaming helper and write tests

This task adds a `stream_and_mask` helper function and tests it in isolation using subprocess spawning. The helper encapsulates the two-thread streaming pattern so it can be tested independently from the secret-fetching logic.

**Files:**
- Modify: `src/cli/secret_ops.rs` — add `stream_and_mask` helper + `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the `stream_and_mask` helper function**

Add this function near the bottom of `src/cli/secret_ops.rs`, just above line 1597 (before `execute_secret_inject`):

```rust
/// Stream child process stdout/stderr line-by-line, masking secret values in each line.
/// Returns the child's exit code.
///
/// `secret_values` is moved into an `Arc` and shared across two reader threads.
/// After both threads join, this function holds the last `Arc` reference —
/// dropping it triggers `Zeroizing::drop` on each secret value.
fn stream_and_mask(
    mut child: std::process::Child,
    secret_values: Vec<Zeroizing<String>>,
) -> i32 {
    use std::io::{BufRead, BufReader, Write};

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Move secret_values into Arc for sharing across threads.
    // After threads join, the Arc in this function is the last reference.
    let secrets = Arc::new(secret_values);
    let secrets_for_stderr = Arc::clone(&secrets);

    // Thread 1: stream stdout
    let stdout_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
            let line = String::from_utf8_lossy(&buf);
            let masked = mask_secrets(&line, &secrets);
            print!("{}", masked);
            buf.clear();
        }
    });

    // Thread 2: stream stderr
    let stderr_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
            let line = String::from_utf8_lossy(&buf);
            let masked = mask_secrets(&line, &secrets_for_stderr);
            eprint!("{}", masked);
            buf.clear();
        }
    });

    // Wait for child to exit
    let status = child.wait().expect("failed to wait on child");

    // Join threads (they'll finish once child closes pipe write-ends)
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    // Flush before process::exit (which does not flush stdio buffers)
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();

    status.code().unwrap_or(1)
}
```

- [ ] **Step 2: Add test module with streaming masking tests**

Add at the very end of `src/cli/secret_ops.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Helper: run stream_and_mask but redirect its print!/eprint! output to files
    /// so we can verify masking actually happened.
    fn stream_and_mask_to_files(
        mut child: std::process::Child,
        secret_values: Vec<Zeroizing<String>>,
        stdout_file: &std::path::Path,
        stderr_file: &std::path::Path,
    ) -> i32 {
        use std::io::{BufRead, BufReader, Write};
        use std::fs::OpenOptions;

        let stdout_handle = child.stdout.take().expect("stdout was piped");
        let stderr_handle = child.stderr.take().expect("stderr was piped");

        let secrets = Arc::new(secret_values);
        let secrets_for_stderr = Arc::clone(&secrets);

        let stdout_path = stdout_file.to_path_buf();
        let stderr_path = stderr_file.to_path_buf();

        let stdout_thread = std::thread::spawn(move || {
            let mut out = OpenOptions::new().create(true).write(true).open(&stdout_path).unwrap();
            let mut reader = BufReader::new(stdout_handle);
            let mut buf = Vec::new();
            while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
                let line = String::from_utf8_lossy(&buf);
                let masked = mask_secrets(&line, &secrets);
                write!(out, "{}", masked).unwrap();
                buf.clear();
            }
        });

        let stderr_thread = std::thread::spawn(move || {
            let mut out = OpenOptions::new().create(true).write(true).open(&stderr_path).unwrap();
            let mut reader = BufReader::new(stderr_handle);
            let mut buf = Vec::new();
            while reader.read_until(b'\n', &mut buf).unwrap_or(0) > 0 {
                let line = String::from_utf8_lossy(&buf);
                let masked = mask_secrets(&line, &secrets_for_stderr);
                write!(out, "{}", masked).unwrap();
                buf.clear();
            }
        });

        let status = child.wait().expect("failed to wait on child");
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        status.code().unwrap_or(1)
    }

    #[test]
    fn test_stream_and_mask_stdout_masks_secrets() {
        let secret = Zeroizing::new("SUPERSECRET".to_string());
        let secrets = vec![secret];
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        let stderr_path = dir.path().join("stderr.txt");

        let child = Command::new("echo")
            .arg("hello SUPERSECRET world")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn echo");

        let exit_code = stream_and_mask_to_files(child, secrets, &stdout_path, &stderr_path);
        assert_eq!(exit_code, 0);

        let output = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(output.contains("[MASKED]"), "Expected [MASKED] in stdout, got: {}", output);
        assert!(!output.contains("SUPERSECRET"), "Secret should not appear in output");
    }

    #[test]
    fn test_stream_and_mask_both_streams() {
        let secret = Zeroizing::new("TOPSECRET".to_string());
        let secrets = vec![secret];
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        let stderr_path = dir.path().join("stderr.txt");

        let child = Command::new("sh")
            .arg("-c")
            .arg("echo 'stdout TOPSECRET line'; echo 'stderr TOPSECRET line' >&2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask_to_files(child, secrets, &stdout_path, &stderr_path);
        assert_eq!(exit_code, 0);

        let stdout_output = std::fs::read_to_string(&stdout_path).unwrap();
        let stderr_output = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(stdout_output.contains("[MASKED]"), "Expected [MASKED] in stdout");
        assert!(stderr_output.contains("[MASKED]"), "Expected [MASKED] in stderr");
        assert!(!stdout_output.contains("TOPSECRET"), "Secret should not appear in stdout");
        assert!(!stderr_output.contains("TOPSECRET"), "Secret should not appear in stderr");
    }

    #[test]
    fn test_stream_and_mask_exit_code() {
        let secrets = vec![];

        let child = Command::new("sh")
            .arg("-c")
            .arg("exit 42")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask(child, secrets);
        assert_eq!(exit_code, 42);
    }

    #[test]
    fn test_stream_and_mask_large_output_no_oom() {
        // Verify streaming works for output larger than typical pipe buffer (64KB)
        let secret = Zeroizing::new("HIDDEN".to_string());
        let secrets = vec![secret];

        let child = Command::new("sh")
            .arg("-c")
            // Generate ~200KB of output (enough to exceed pipe buffer)
            // Use awk for portability (seq not available in all environments)
            .arg("awk 'BEGIN{for(i=1;i<=3000;i++) print \"line \" i \" contains HIDDEN data\"}'")

            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let exit_code = stream_and_mask(child, secrets);
        assert_eq!(exit_code, 0);
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib stream_and_mask -- --nocapture`
Expected: All 4 tests PASS (`test_stream_and_mask_stdout_masks_secrets`, `test_stream_and_mask_both_streams`, `test_stream_and_mask_exit_code`, `test_stream_and_mask_large_output_no_oom`)

- [ ] **Step 4: Commit**

```bash
git add src/cli/secret_ops.rs
git commit -m "feat: add stream_and_mask helper with tests for xv run streaming"
```

---

### Task 2: Wire up streaming in execute_secret_run

Replace the buffered masking path with a call to the new `stream_and_mask` helper.

**Files:**
- Modify: `src/cli/secret_ops.rs` (lines 1567–1594)

- [ ] **Step 1: Replace the `else` block in `execute_secret_run`**

Replace lines 1567–1594 (the entire `else { ... }` block) with:

```rust
    } else {
        // Stream output line-by-line with masking
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            CrosstacheError::config(format!("Failed to execute command '{}': {}", command[0], e))
        })?;

        // Drop env vars now — they're already set on the child process
        drop(env_vars);
        drop(uri_values);

        // secret_values is moved into stream_and_mask, which wraps it in Arc.
        // After threads join, Arc drop triggers Zeroizing::drop on each secret.
        let exit_code = stream_and_mask(child, secret_values);
        std::process::exit(exit_code);
    }
```

- [ ] **Step 2: Run all tests to verify nothing is broken**

Run: `cargo test --lib`
Expected: All tests PASS (including the new `stream_and_mask` tests)

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

- [ ] **Step 4: Commit**

```bash
git add src/cli/secret_ops.rs
git commit -m "fix: stream xv run masking output line-by-line instead of buffering"
```

---

### Task 3: Manual smoke test

Verify the fix works end-to-end with the actual `xv run` command (requires Azure credentials).

**Files:** None (manual verification only)

- [ ] **Step 1: Build release**

Run: `cargo build`

- [ ] **Step 2: Test with masking (default)**

Run: `cargo run -- run -- echo "test output"` (with a vault that has secrets)
Expected: Command output appears immediately, line by line. Any secret values in output replaced with `[MASKED]`.

- [ ] **Step 3: Test with --no-masking**

Run: `cargo run -- run --no-masking -- echo "test output"`
Expected: Same behavior as before (direct passthrough, no change to this path).

- [ ] **Step 4: Final commit (if any cleanup needed)**

Only if manual testing reveals issues. Otherwise, Task 2's commit is the final one.
