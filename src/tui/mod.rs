//! Read-only Terminal UI for crosstache. Feature-gated on `tui`.
//! See `docs/tui.md` for the user-facing contract.

pub mod app;
pub mod clipboard;
pub mod data;
pub mod event;
pub mod message;
pub mod overlays;
pub mod update;
pub mod view;

use crate::config::Config;
use crate::error::Result;
use app::App;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use message::Message;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};
use update::Command;

pub async fn run_tui(
    config: Config,
    _registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_loop(&mut terminal, config).await;
    teardown_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()
        .map_err(|e| crate::error::CrosstacheError::config(format!("raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| crate::error::CrosstacheError::config(format!("alt screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
        .map_err(|e| crate::error::CrosstacheError::config(format!("terminal: {e}")))
}

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    Ok(())
}

async fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, config: Config) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let mut app = App::new(config.clone());

    let _evt = event::spawn_event_reader(tx.clone());
    let _tick = event::spawn_tick_timer(tx.clone());
    let _initial = data::spawn_load_vaults(config, tx.clone());

    while !app.quit {
        terminal
            .draw(|f| view::view(&app, f))
            .map_err(|e| crate::error::CrosstacheError::config(format!("draw: {e}")))?;
        let Some(msg) = rx.recv().await else { break };
        let cmds = update::update(&mut app, msg);
        for cmd in cmds {
            handle_command(&app, &tx, cmd).await;
        }
    }
    Ok(())
}

async fn handle_command(app: &App, tx: &tokio::sync::mpsc::Sender<Message>, cmd: Command) {
    match cmd {
        Command::Quit => {}
        Command::LoadVaults => {
            let _ = data::spawn_load_vaults(app.config.clone(), tx.clone());
        }
        Command::LoadSecrets { vault } => {
            let _ = data::spawn_load_secrets(app.config.clone(), vault, tx.clone());
        }
        Command::LoadValue { vault, name } => {
            let _ = data::spawn_load_value(app.config.clone(), vault, name, tx.clone());
        }
        Command::LoadHistory { vault, name } => {
            let _ = data::spawn_load_history(app.config.clone(), vault, name, tx.clone());
        }
        Command::LoadAudit { vault, name } => {
            let _ = data::spawn_load_audit(app.config.clone(), vault, name, tx.clone());
        }
        Command::CopyToClipboard(s) => {
            if let Err(e) = clipboard::copy_string(&s) {
                let _ = tx.send(Message::Error(e)).await;
            }
        }
    }
}
