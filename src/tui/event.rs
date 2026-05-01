use crate::tui::message::Message;
use tokio::sync::mpsc::Sender;

pub fn spawn_event_reader(tx: Sender<Message>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        loop {
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
