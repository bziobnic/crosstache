use crate::tui::app::{App, Pane};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

pub fn view(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(20), Constraint::Min(20), Constraint::Length(40)])
        .split(chunks[0]);

    render_vaults(app, frame, panes[0]);
    render_secrets(app, frame, panes[1]);
    render_detail(app, frame, panes[2]);
    render_status(app, frame, chunks[1]);
    if app.toast.is_some() { render_toast(app, frame, chunks[2]); }
    crate::tui::overlays::render_active_overlay(app, frame);
}

fn border_for(app: &App, pane: Pane) -> Style {
    if app.pane == pane { Style::default().fg(Color::Cyan) }
    else { Style::default().fg(Color::DarkGray) }
}

fn render_vaults(app: &App, frame: &mut Frame, area: Rect) {
    let title = if app.vaults_loading { "Vaults (loading…)" } else { "Vaults" };
    let items: Vec<ListItem> = app.vaults.iter().map(|v| ListItem::new(v.name.as_str())).collect();
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL).border_style(border_for(app, Pane::Vaults)))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut s = app.vault_state.clone();
    frame.render_stateful_widget(list, area, &mut s);
}

fn render_secrets(app: &App, frame: &mut Frame, area: Rect) {
    let title = if app.secret_filter_active {
        format!("Secrets (filter: /{})", app.secret_filter)
    } else if !app.secret_filter.is_empty() {
        format!("Secrets (filter: {})", app.secret_filter)
    } else if app.secrets_loading {
        "Secrets (loading…)".to_string()
    } else { "Secrets".to_string() };
    let items: Vec<ListItem> = app.filtered_secrets().iter().map(|s| {
        let d = if s.original_name.is_empty() { s.name.as_str() } else { s.original_name.as_str() };
        ListItem::new(d)
    }).collect();
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL).border_style(border_for(app, Pane::Secrets)))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut s = app.secret_state.clone();
    frame.render_stateful_widget(list, area, &mut s);
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(s) = app.selected_secret() {
        let display = if s.original_name.is_empty() { s.name.as_str() } else { s.original_name.as_str() };
        lines.push(Line::raw(format!("name: {display}")));
        let value_line = if app.value_revealed {
            if let Some((v, n)) = app.selected_vault_and_name() {
                if let Some(val) = app.values.get(&(v, n)) {
                    format!("value: {}", val.as_str())
                } else if app.value_loading {
                    "value: (loading…)".to_string()
                } else { "value: (press Space)".to_string() }
            } else { "value: ()".to_string() }
        } else { "value: ●●●●●●●●".to_string() };
        lines.push(Line::raw(value_line));
        if let Some(g) = &s.groups { lines.push(Line::raw(format!("groups: {g}"))); }
        if let Some(f) = &s.folder { lines.push(Line::raw(format!("folder: {f}"))); }
        lines.push(Line::raw(format!("updated: {}", s.updated_on)));
    } else {
        lines.push(Line::raw("(no secret selected)"));
    }
    let p = Paragraph::new(lines)
        .block(Block::default().title("Detail").borders(Borders::ALL).border_style(border_for(app, Pane::Detail)));
    frame.render_widget(p, area);
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = app.selected_vault() { parts.push(format!("vault: {v}")); }
    if let Some(s) = app.selected_vault().and_then(|v| app.secrets_by_vault.get(v)) {
        parts.push(format!("{} secrets", s.len()));
    }
    if let Some(n) = app.clipboard_countdown {
        parts.push(format!("clipboard: {}s", (n + 9) / 10));
    }
    parts.push("?:help q:quit".to_string());
    let p = Paragraph::new(parts.join("  ·  ")).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(p, area);
}

fn render_toast(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(t) = &app.toast {
        let line = format!("⚠ {}{} (e: details, Esc: dismiss)",
            t.code.as_deref().map(|c| format!("[{c}] ")).unwrap_or_default(),
            t.message);
        let p = Paragraph::new(line).style(Style::default().fg(Color::Yellow));
        frame.render_widget(p, area);
    }
}
