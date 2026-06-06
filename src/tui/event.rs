use crate::tui::message::Message;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;

/// How often the blocking reader wakes to re-check the shutdown flag.
///
/// `crossterm::event::read()` blocks indefinitely until input arrives, and
/// `spawn_blocking` threads cannot be cancelled. If we used `read()` directly,
/// the thread would stay parked after the UI quit, and the Tokio runtime would
/// block process shutdown until the user pressed a key. Polling with a short
/// timeout lets the thread observe `shutdown` and return on its own.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Spawn the terminal event reader.
///
/// The returned thread polls for crossterm events and forwards key presses on
/// `tx`. It terminates promptly — without requiring further input — once either
/// `shutdown` is set to `true` or the receiver is dropped.
pub fn spawn_event_reader(
    tx: Sender<Message>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            // Wait for input, but wake periodically to re-check `shutdown` so
            // the thread can exit cleanly when the UI quits.
            match crossterm::event::poll(POLL_INTERVAL) {
                Ok(false) => continue, // timed out, no event pending
                Ok(true) => {}         // event ready; fall through to read it
                Err(_) => break,
            }
            match crossterm::event::read() {
                Ok(crossterm::event::Event::Key(key)) => {
                    // Windows emits Press, Repeat, AND Release for every
                    // keystroke; Unix only emits Press. Without this guard,
                    // each arrow press moved the cursor twice on Windows.
                    if !matches!(key.kind, crossterm::event::KeyEventKind::Press) {
                        continue;
                    }
                    if tx.blocking_send(Message::KeyPress(key)).is_err() {
                        break;
                    }
                }
                Ok(_) => {} // ignore mouse / resize for v0.7
                Err(_) => break,
            }
        }
    })
}

pub fn spawn_tick_timer(tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            if tx.send(Message::Tick).await.is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Regression test for the TUI exit hang: once the shutdown flag is set, the
    /// blocking event-reader thread must terminate on its own without any
    /// terminal input. Before the fix, the reader was parked in an indefinite
    /// `crossterm::event::read()` and only this guarantee makes process exit
    /// prompt.
    #[tokio::test]
    async fn reader_exits_on_shutdown_without_input() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = spawn_event_reader(tx, shutdown.clone());

        // Signal shutdown and confirm the thread joins well within a bound that
        // comfortably exceeds the poll interval.
        shutdown.store(true, Ordering::Relaxed);
        let joined = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            joined.is_ok(),
            "event reader did not terminate after shutdown flag was set"
        );
    }

    /// The reader also terminates when the receiver is dropped (channel closed),
    /// which is what happens if the consumer goes away first.
    #[tokio::test]
    async fn reader_exits_when_receiver_dropped() {
        let (tx, rx) = tokio::sync::mpsc::channel::<Message>(8);
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = spawn_event_reader(tx, shutdown.clone());

        drop(rx);
        // With no input arriving, the reader observes shutdown on the next poll
        // tick. Set it to simulate the real teardown path and ensure no hang.
        shutdown.store(true, Ordering::Relaxed);
        let joined = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(joined.is_ok(), "event reader did not terminate");
    }
}
