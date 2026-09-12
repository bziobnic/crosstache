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

// ─── disclosure boundary: the reveal keystroke ─────────────────────────────

/// Plaintext planted in the TUI's fetched-value cache.
const TUI_CANARY: &str = "disclosure-canary-7f3e";

/// An app with one vault and one untyped secret selected, whose value has
/// already been fetched (the state `Space` puts the TUI into after
/// `Message::ValueLoaded`), but which has not been revealed yet.
fn app_with_fetched_value() -> App {
    use crosstache::secret::domain::SecretSummary;
    use crosstache::vault::models::VaultSummary;

    let mut app = empty_app();
    app.vaults = vec![VaultSummary {
        name: "default".to_string(),
        location: "local".to_string(),
        resource_group: String::new(),
        status: "Active".to_string(),
        created_at: String::new(),
    }];
    app.vault_state.select(Some(0));
    app.secrets_by_vault.insert(
        "default".to_string(),
        vec![SecretSummary {
            name: "LEAKY".to_string(),
            original_name: "LEAKY".to_string(),
            note: None,
            folder: None,
            groups: None,
            updated_on: String::new(),
            enabled: true,
            expires_on: None,
            content_type: String::new(),
            tags: std::collections::HashMap::new(),
        }],
    );
    app.secret_state.select(Some(0));
    app.values.insert(
        ("default".to_string(), "LEAKY".to_string()),
        zeroize::Zeroizing::new(TUI_CANARY.to_string()),
    );
    app
}

fn render(app: &App) -> String {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| view(app, f)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>()
}

/// Boundary: the TUI reveal keystroke (`Space`). Masked before, plaintext
/// after — and nothing else in the frame ever carries the value.
#[test]
fn boundary_tui_reveal_renders_value() {
    let mut app = app_with_fetched_value();

    // Before: the value is fetched and sitting in `app.values`, but the
    // frame shows only the mask.
    let masked = render(&app);
    assert!(
        !masked.contains(TUI_CANARY),
        "the TUI rendered a fetched value before the reveal keystroke:\n{masked}"
    );
    assert!(
        masked.contains("value: ●●●●●●●●"),
        "the detail pane must show the mask before reveal:\n{masked}"
    );

    // The reveal keystroke itself, through the real update loop.
    let _ = crosstache::tui::update::update(
        &mut app,
        crosstache::tui::message::Message::KeyPress(key(crossterm::event::KeyCode::Char(' '))),
    );
    assert!(app.value_revealed, "Space must toggle the reveal flag");
    let revealed = render(&app);
    assert!(
        revealed.contains(&format!("value: {TUI_CANARY}")),
        "the reveal keystroke must render the value:\n{revealed}"
    );

    // ...and toggling back re-masks it.
    let _ = crosstache::tui::update::update(
        &mut app,
        crosstache::tui::message::Message::KeyPress(key(crossterm::event::KeyCode::Char(' '))),
    );
    let remasked = render(&app);
    assert!(
        !remasked.contains(TUI_CANARY),
        "a second Space must re-mask the value:\n{remasked}"
    );
}
