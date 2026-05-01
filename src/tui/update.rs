use crate::tui::app::{App, Overlay, Pane};
use crate::tui::message::Message;
use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug)]
pub enum Command {
    LoadVaults,
    LoadSecrets { vault: String },
    LoadValue { vault: String, name: String },
    LoadHistory { vault: String, name: String },
    LoadAudit { vault: String, name: Option<String> },
    CopyToClipboard(String),
    Quit,
}

pub fn update(app: &mut App, msg: Message) -> Vec<Command> {
    let mut cmds = Vec::new();
    match msg {
        Message::KeyPress(k) => cmds.extend(handle_key(app, k)),
        Message::VaultsLoaded(vs) => {
            app.vaults = vs;
            app.vaults_loading = false;
            if !app.vaults.is_empty() {
                app.vault_state.select(Some(0));
                let vault_name = app.selected_vault().map(str::to_string);
                if let Some(name) = vault_name {
                    app.secrets_loading = true;
                    cmds.push(Command::LoadSecrets { vault: name });
                }
            }
        }
        Message::SecretsLoaded { vault, secrets } => {
            app.secrets_by_vault.insert(vault, secrets);
            app.secrets_loading = false;
            if !app.filtered_secrets().is_empty() {
                app.secret_state.select(Some(0));
            }
        }
        Message::ValueLoaded { vault, name, value } => {
            app.values.insert((vault, name), value);
            app.value_loading = false;
        }
        Message::HistoryLoaded { vault, name, versions } => {
            app.history.insert((vault, name), versions);
        }
        Message::AuditLoaded { vault, name, events } => {
            app.audit.insert((vault, name), events);
        }
        Message::Tick => {
            cmds.extend(tick_clipboard(app));
            tick_toast(app);
            cmds.extend(tick_value_debounce(app));
        }
        Message::Error(e) => {
            app.toast = Some(crate::tui::app::Toast {
                message: e.to_string(),
                code: Some(e.code().to_string()),
                ticks_left: 50,
            });
            app.vaults_loading = false;
            app.secrets_loading = false;
            app.value_loading = false;
        }
        Message::Quit => {
            app.quit = true;
            cmds.push(Command::Quit);
        }
    }
    cmds
}

fn handle_key(app: &mut App, key: KeyEvent) -> Vec<Command> {
    let mut cmds = Vec::new();
    // Filter mode intercepts most keys.
    if app.secret_filter_active {
        match key.code {
            KeyCode::Esc => { app.secret_filter_active = false; app.secret_filter.clear(); app.secret_state.select(Some(0)); }
            KeyCode::Enter => { app.secret_filter_active = false; }
            KeyCode::Backspace => { app.secret_filter.pop(); app.secret_state.select(Some(0)); }
            KeyCode::Char(c) => { app.secret_filter.push(c); app.secret_state.select(Some(0)); }
            _ => {}
        }
        return cmds;
    }
    // Overlay-aware keys
    if !matches!(app.overlay, Overlay::None) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            app.overlay = Overlay::None;
        }
        return cmds;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => cmds.push(Command::Quit),
        KeyCode::Char('?') => app.overlay = Overlay::Help,
        KeyCode::Char('/') => {
            if app.pane == crate::tui::app::Pane::Secrets {
                app.secret_filter_active = true;
                app.secret_filter.clear();
            }
        }
        KeyCode::Tab => app.pane = next_pane(app.pane),
        KeyCode::BackTab => app.pane = prev_pane(app.pane),
        KeyCode::Char('j') | KeyCode::Down => cmds.extend(move_cursor(app, 1)),
        KeyCode::Char('k') | KeyCode::Up => cmds.extend(move_cursor(app, -1)),
        KeyCode::Char('h') | KeyCode::Left => app.pane = prev_pane(app.pane),
        KeyCode::Char('l') | KeyCode::Right => app.pane = next_pane(app.pane),
        // Tasks 5-10 wire Space y Y R H a c d r e here.
        _ => {}
    }
    cmds
}

fn move_cursor(app: &mut App, delta: i32) -> Vec<Command> {
    let mut cmds = Vec::new();
    match app.pane {
        Pane::Vaults => {
            move_list(&mut app.vault_state, app.vaults.len(), delta);
            app.secret_state.select(Some(0));
            let vault_name = app.selected_vault().map(str::to_string);
            if let Some(name) = vault_name {
                if !app.secrets_by_vault.contains_key(&name) {
                    app.secrets_loading = true;
                    cmds.push(Command::LoadSecrets { vault: name });
                }
            }
        }
        Pane::Secrets => {
            let n = app.filtered_secrets().len();
            move_list(&mut app.secret_state, n, delta);
            // Task 6 wires value-fetch debounce here.
        }
        Pane::Detail => {}
    }
    cmds
}

fn move_list(state: &mut ratatui::widgets::ListState, len: usize, delta: i32) {
    if len == 0 { return; }
    let cur = state.selected().unwrap_or(0) as i32;
    let new = (cur + delta).rem_euclid(len as i32) as usize;
    state.select(Some(new));
}

fn next_pane(p: Pane) -> Pane { match p { Pane::Vaults => Pane::Secrets, Pane::Secrets => Pane::Detail, Pane::Detail => Pane::Vaults } }
fn prev_pane(p: Pane) -> Pane { match p { Pane::Vaults => Pane::Detail, Pane::Secrets => Pane::Vaults, Pane::Detail => Pane::Secrets } }

fn tick_clipboard(app: &mut App) -> Vec<Command> {
    let mut cmds = Vec::new();
    if let Some(n) = app.clipboard_countdown {
        if n <= 1 {
            app.clipboard_countdown = None;
            cmds.push(Command::CopyToClipboard(String::new())); // clear
        } else {
            app.clipboard_countdown = Some(n - 1);
        }
    }
    cmds
}

fn tick_toast(app: &mut App) {
    if let Some(t) = app.toast.as_mut() {
        if t.ticks_left == 0 { app.toast = None; } else { t.ticks_left -= 1; }
    }
}

fn tick_value_debounce(app: &mut App) -> Vec<Command> {
    let mut cmds = Vec::new();
    if let Some((v, n, ticks)) = app.value_debounce.clone() {
        if ticks == 0 {
            app.value_debounce = None;
            app.value_loading = true;
            cmds.push(Command::LoadValue { vault: v, name: n });
        } else {
            app.value_debounce = Some((v, n, ticks - 1));
        }
    }
    cmds
}
