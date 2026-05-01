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

fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}

fn ctrl_key(c: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char(c),
        crossterm::event::KeyModifiers::CONTROL,
    )
}

#[test]
fn pressing_q_sets_quit_flag() {
    let mut app = empty_app();
    let _ = crosstache::tui::update::update(
        &mut app,
        crosstache::tui::message::Message::KeyPress(key(crossterm::event::KeyCode::Char('q'))),
    );
    assert!(app.quit, "'q' must set app.quit so the main loop exits");
}

#[test]
fn pressing_esc_sets_quit_flag() {
    let mut app = empty_app();
    let _ = crosstache::tui::update::update(
        &mut app,
        crosstache::tui::message::Message::KeyPress(key(crossterm::event::KeyCode::Esc)),
    );
    assert!(app.quit, "Esc must set app.quit so the main loop exits");
}

#[test]
fn ctrl_c_sets_quit_flag_and_does_not_show_reserved_toast() {
    let mut app = empty_app();
    let _ = crosstache::tui::update::update(
        &mut app,
        crosstache::tui::message::Message::KeyPress(ctrl_key('c')),
    );
    assert!(app.quit, "Ctrl+C must set app.quit");
    assert!(
        app.toast.is_none(),
        "Ctrl+C must not surface the v0.8 reserved-key toast"
    );
}

#[test]
fn plain_c_still_shows_reserved_toast() {
    // Regression guard: Ctrl-C exception must not unintentionally suppress
    // the reserved-key feedback for unmodified 'c'.
    let mut app = empty_app();
    let _ = crosstache::tui::update::update(
        &mut app,
        crosstache::tui::message::Message::KeyPress(key(crossterm::event::KeyCode::Char('c'))),
    );
    let toast = app.toast.expect("plain 'c' should still toast");
    assert!(toast.message.contains("reserved"));
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

#[test]
fn tui_help_works_when_feature_enabled() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_xv"))
        .args(["tui", "--help"])
        .env("XV_NO_PARENT_CONFIG", "1")
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("tui") || stdout.contains("Tui"),
        "tui --help should mention tui: {stdout}"
    );
}
