#![cfg(feature = "tui")]

use crosstache::config::Config;
use crosstache::tui::app::{App, Overlay};
use crosstache::tui::view::view;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn empty_app() -> App {
    let mut app = App::new(Config::default());
    app.vaults_loading = false;
    app
}

#[test]
fn empty_app_renders_three_panes_and_status() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let app = empty_app();
    terminal.draw(|f| view(&app, f)).unwrap();
    let dump = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(dump.contains("Vaults"));
    assert!(dump.contains("Secrets"));
    assert!(dump.contains("Detail"));
}

#[test]
fn help_overlay_renders_when_active() {
    let mut app = empty_app();
    app.overlay = Overlay::Help;
    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| view(&app, f)).unwrap();
    let dump = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(dump.contains("keymap") || dump.contains("Help"));
}
