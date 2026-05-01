use crate::tui::app::{App, Overlay};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

pub fn render_active_overlay(app: &App, frame: &mut Frame) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(frame),
        Overlay::ErrorDetail(msg) => render_error_detail(msg, frame),
        Overlay::History => render_history(app, frame),
        Overlay::Audit => render_audit(app, frame),
    }
}

fn render_help(frame: &mut Frame) {
    let area = centered_rect(60, 70, frame.area());
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = vec![
        Line::from(Span::styled("xv tui — keymap (read-only v0.7)",
            Style::default().add_modifier(Modifier::BOLD))),
        Line::raw(""),
        Line::raw("Navigation:"),
        Line::raw("  h j k l / arrows   move within / between panes"),
        Line::raw("  Tab / Shift-Tab    cycle panes"),
        Line::raw("  /                  live fuzzy filter (secrets pane)"),
        Line::raw(""),
        Line::raw("Actions:"),
        Line::raw("  Space              toggle value reveal"),
        Line::raw("  y                  copy value (with countdown)"),
        Line::raw("  Y                  copy secret name"),
        Line::raw("  R                  refresh — invalidate cache and reload"),
        Line::raw("  H                  history (versions) overlay"),
        Line::raw("  a                  audit log overlay"),
        Line::raw("  e                  expand error toast into modal"),
        Line::raw("  ?                  this help"),
        Line::raw("  q / Esc            quit (or close overlay)"),
        Line::raw(""),
        Line::from(Span::styled("Reserved for v0.8 (write mode):",
            Style::default().fg(Color::Yellow))),
        Line::raw("  c   create new secret"),
        Line::raw("  d   delete current secret"),
        Line::raw("  r   rename / rotate"),
        Line::raw(""),
        Line::raw("Press q or Esc to close."),
    ];
    let p = Paragraph::new(lines).alignment(Alignment::Left)
        .block(Block::default().title("Help (?)").borders(Borders::ALL));
    frame.render_widget(p, area);
}

fn render_error_detail(msg: &str, frame: &mut Frame) {
    let area = centered_rect(60, 40, frame.area());
    frame.render_widget(Clear, area);
    let p = Paragraph::new(msg.to_string())
        .block(Block::default().title("Error").borders(Borders::ALL))
        .style(Style::default().fg(Color::Red));
    frame.render_widget(p, area);
}

pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default().direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ]).split(area);
    Layout::default().direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ]).split(popup[1])[1]
}

fn render_history(app: &App, frame: &mut Frame) {
    let area = centered_rect(60, 50, frame.area());
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = if let Some((v, n)) = app.selected_vault_and_name() {
        if let Some(versions) = app.history.get(&(v, n)) {
            if versions.is_empty() {
                vec![Line::raw("(no versions)")]
            } else {
                versions.iter().map(|p| {
                    Line::raw(format!("{}  enabled={}  {}",
                        p.version,
                        p.enabled,
                        p.updated_on))
                }).collect()
            }
        } else { vec![Line::raw("(loading versions…)")] }
    } else { vec![Line::raw("(no secret selected)")] };
    let p = Paragraph::new(lines).block(
        Block::default().title("History (H) — secret versions").borders(Borders::ALL));
    frame.render_widget(p, area);
}

fn render_audit(app: &App, frame: &mut Frame) {
    let area = centered_rect(60, 50, frame.area());
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = if let Some((v, n)) = app.selected_vault_and_name() {
        let key = (v, Some(n));
        if let Some(events) = app.audit.get(&key) {
            if events.is_empty() {
                vec![Line::raw("(no events)")]
            } else {
                events.iter().map(|e| Line::raw(e.as_str())).collect()
            }
        } else { vec![Line::raw("(loading events…)")] }
    } else { vec![Line::raw("(no secret selected)")] };
    let p = Paragraph::new(lines).block(
        Block::default().title("Audit (a) — recent events").borders(Borders::ALL));
    frame.render_widget(p, area);
}
