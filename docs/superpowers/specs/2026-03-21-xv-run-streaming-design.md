# `xv run` Streaming Masking — Design Spec

## Overview

Fix the `xv run` masking path to stream output line-by-line instead of buffering the entire child process output in memory. The `--no-masking` path is already correct (uses `Command::status()` with inherited stdio).

## Problem

The masking path uses `Command::output()`, which buffers all of stdout and stderr in memory before masking. Long-running commands with large output can use unbounded memory.

## Solution

Replace `Command::output()` with `Command::spawn()` + `BufReader::read_until(b'\n')` on piped stdout/stderr. Two OS threads read the streams concurrently to prevent deadlocks (a child can block if either pipe buffer fills).

## Execution Flow

1. Spawn child with `Stdio::piped()` for both stdout and stderr (same as today)
2. Take ownership of `child.stdout` and `child.stderr` handles
3. Inside the `else` (masking) branch, wrap `secret_values` in `Arc::new(secret_values)` for sharing across threads
4. Spawn thread 1: `BufReader::new(stdout).read_until(b'\n')` loop → `from_utf8_lossy` → `mask_secrets()` → `print!`
5. Spawn thread 2: `BufReader::new(stderr).read_until(b'\n')` loop → `from_utf8_lossy` → `mask_secrets()` → `eprint!`
6. `child.wait()` for exit status. Both `child.wait()` and thread joins are independently safe in either order — threads reach EOF only after the child exits and the OS closes the child's pipe write-ends. `wait()` first is conventional to avoid zombie accumulation.
7. Join both threads (ignore `Err` from panics — child exit code is already captured)
8. Flush stdout and stderr (`std::io::stdout().flush()` / `std::io::stderr().flush()`) — required because `std::process::exit()` does not flush stdio buffers
9. Drop `env_vars`, `uri_values` (zeroization). The main thread's `Arc` is the last reference after threads have joined — dropping it triggers `Zeroizing::drop` on each secret value.
10. `std::process::exit(child_exit_code)`

## File Changes

- **Modified:** `src/cli/secret_ops.rs` — replace the `else` branch of the `if no_masking` block in `execute_secret_run`
- **No changes:** `src/cli/helpers.rs` (`mask_secrets` reused as-is)
- **No changes:** `--no-masking` path, secret fetching, env injection, URI resolution

## Key Details

### Thread Safety

`secret_values: Vec<Zeroizing<String>>` is moved into an `Arc::new()` inside the `else` (masking) branch and shared read-only across two threads via `Arc::clone()`. `Zeroizing<String>` is `Send + Sync`, so `Arc<Vec<Zeroizing<String>>>` is safe to share. The `Arc` is constructed only in the masking path — the `--no-masking` path never touches it.

### Line Buffering

`BufReader::read_until(b'\n', &mut buf)` reads until `\n` (inclusive) or EOF. The buffer is converted via `String::from_utf8_lossy()` and passed to `mask_secrets()`. Output is printed with `print!`/`eprint!` (newline already in buffer). This matches standard CLI line-buffering behavior.

For UTF-8 safety, use `read_until(b'\n', &mut Vec<u8>)` rather than `.lines()`. Convert each chunk with `String::from_utf8_lossy()` before masking. This preserves the same behavior as the current code (replace invalid bytes with `U+FFFD`) rather than silently dropping all remaining output after the first invalid byte sequence. Print with `print!`/`eprint!` (not `println!`/`eprintln!`) since the buffer includes the trailing `\n`.

### Zeroization

Same pattern as the current code. Sequence: (1) `child.wait()` reaps the process, (2) `join()` both threads — their `Arc` clones drop, ref count goes to 1, (3) drop `env_vars` and `uri_values`, (4) drop the main thread's `Arc` — ref count hits 0, `Zeroizing::drop` zeroes each secret value's memory. If a thread panics, its `Arc` clone may not drop — secret memory is not zeroed for that ref, but the process exits immediately after via `std::process::exit()`, so the OS reclaims all memory. This is acceptable.

### `mask_secrets` Compatibility

`mask_secrets(text: &str, secrets: &[Zeroizing<String>])` takes a slice. Inside each thread, `&*arc_clone` (or just `&arc_clone` via `Deref`) gives `&Vec<Zeroizing<String>>` which auto-derefs to `&[Zeroizing<String>]`. No signature change needed.

### No New Dependencies

Uses only `std::sync::Arc`, `std::thread`, `std::io::BufReader`, `std::io::BufRead` — all in the standard library.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Spawn fails | Same error as today: `"Failed to execute command '{cmd}': {e}"` |
| Thread panics | `join()` returns `Err`; proceed to exit with child's code. Secret memory not zeroed for that thread's Arc ref (acceptable — process exits immediately) |
| `read_until` I/O error | Thread exits its loop; remaining output on that stream is lost. Child continues on the other stream. |
| Child exits before all output read | Threads drain remaining buffered output, then exit |

## Testing

- `#[cfg(test)]` subprocess tests in `secret_ops.rs` (no Azure credentials needed):
  - Spawn `echo "hello SECRET world"` with masking, verify stdout contains `[MASKED]`
  - Spawn a shell command writing to both stdout and stderr, verify both streams are masked
- Existing `mask_secrets` unit tests in `helpers.rs` unchanged
- `mask_secrets` is already unit-tested independently; the new tests focus on the threading/streaming plumbing

### Known Limitations

- `mask_secrets` operates per-line: a secret containing `\n` would not be masked across lines. This is accepted — secrets virtually never contain newlines.
- `status.code()` returns `None` on Unix when child is killed by signal; falls back to exit code 1 (same as current code)
- stdout/stderr interleaving: two threads write to separate fds independently. Lines from the two streams may interleave in the terminal. This matches normal subprocess behavior.

## Out of Scope

- Chunk-buffered overlap window for secrets spanning line boundaries (secrets rarely contain newlines)
- Async Tokio IO migration (unnecessary complexity for this fix)
- PTY emulation for preserving terminal colors
- Changes to `--no-masking` path
