# TODO — Fix `xv tui` exit hang

## Problem

When the user quits the TUI (`q` / `Esc` / `Ctrl-C`), the shell prompt does not
return until the user presses another key.

## Root cause

`src/tui/event.rs::spawn_event_reader` runs an indefinitely-blocking
`crossterm::event::read()` loop inside `tokio::task::spawn_blocking`.
`spawn_blocking` threads cannot be cancelled. On quit, `run_loop` returns and the
mpsc receiver is dropped, but the reader thread stays parked inside `read()`,
which only returns on terminal input. The Tokio runtime blocks process shutdown
waiting for that thread, so the prompt hangs until a keystroke unblocks `read()`.

## Plan

- [x] Branch `fix/tui-exit-hang` created off `main`.
- [x] Step 1: Convert the reader loop to a `crossterm::event::poll(timeout)` +
      shutdown-flag pattern so it exits cleanly without input.
- [x] Step 2: Thread an `Arc<AtomicBool>` shutdown flag from `run_loop` into the
      reader; set it after the event loop exits (before teardown).
- [x] Step 3: Build with `--features tui`.
- [x] Step 4: Add a regression test that the reader thread terminates after the
      shutdown flag is set, without any input.
- [x] Step 5: Run full test suite + clippy.

## Verification

- `cargo build --features tui` succeeds.
- New test: reader `JoinHandle` completes within a bounded time after the
  shutdown flag is set and no events are sent.
- `cargo test --features tui` green; `cargo clippy --features tui` clean.
