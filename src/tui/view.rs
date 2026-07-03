use crate::tui::app::{App, Pane};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use std::collections::BTreeMap;

/// Placeholder shown for a secret-kind field's value in the TUI detail
/// pane — the field's presence/name is informational, but its value is
/// never rendered here (masked regardless of the top-level value's
/// reveal state), matching record-types plan Task 11.
const MASKED_FIELD_VALUE: &str = "●●●●●●●●";

/// Placeholder for the top `value:` line on typed records, regardless of
/// reveal state. Showing the raw fetched value there for a typed record
/// means showing the raw envelope JSON — every secret field's value at
/// once — which defeats the masked "Fields" section below it (MINOR
/// review finding on the first cut of Task 11: "TUI top value: line
/// reveals the raw envelope JSON ... undermining the masked Fields
/// section"). Untyped secrets are unaffected — they keep the existing
/// reveal/mask/loading behavior exactly.
const TYPED_RECORD_VALUE_PLACEHOLDER: &str = "value: <typed record — see Fields>";

/// Pure formatter for the top `value:` line, so its branching (typed vs
/// untyped, revealed vs masked vs loading vs no-selection) is unit
/// testable without a real `App`/terminal.
fn value_line(
    is_typed_record: bool,
    revealed: bool,
    has_selection: bool,
    fetched: Option<&str>,
    loading: bool,
) -> String {
    if is_typed_record {
        return TYPED_RECORD_VALUE_PLACEHOLDER.to_string();
    }
    if !has_selection {
        return "value: ()".to_string();
    }
    if !revealed {
        return "value: ●●●●●●●●".to_string();
    }
    match fetched {
        Some(v) => format!("value: {v}"),
        None if loading => "value: (loading…)".to_string(),
        None => "value: (press Space)".to_string(),
    }
}

/// Pure formatter for a record's "Fields" section in the TUI detail pane
/// (record-types plan Task 11): one line per field, sorted by name.
/// Metadata fields render `name: value` plainly; fields named in
/// `secret_names` always render with a masked placeholder value instead of
/// whatever `fields` holds for them (defense in depth — even if a caller
/// accidentally passes a real secret value in `fields` for a secret-kind
/// field, it never reaches the rendered line).
fn record_field_lines(
    record_type: &str,
    fields: &BTreeMap<String, String>,
    secret_names: &[String],
) -> Vec<String> {
    let mut lines = vec![format!("type: {record_type}")];
    for (name, value) in fields {
        if secret_names.iter().any(|s| s == name) {
            lines.push(format!("{name}: {MASKED_FIELD_VALUE}"));
        } else {
            lines.push(format!("{name}: {value}"));
        }
    }
    lines
}

pub fn view(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(20),
            Constraint::Min(20),
            Constraint::Length(40),
        ])
        .split(chunks[0]);

    render_vaults(app, frame, panes[0]);
    render_secrets(app, frame, panes[1]);
    render_detail(app, frame, panes[2]);
    render_status(app, frame, chunks[1]);
    if app.toast.is_some() {
        render_toast(app, frame, chunks[2]);
    }
    crate::tui::overlays::render_active_overlay(app, frame);
}

fn border_for(app: &App, pane: Pane) -> Style {
    if app.pane == pane {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn render_vaults(app: &App, frame: &mut Frame, area: Rect) {
    let title = if app.vaults_loading {
        "Vaults (loading…)"
    } else {
        "Vaults"
    };
    let items: Vec<ListItem> = app
        .vaults
        .iter()
        .map(|v| ListItem::new(v.name.as_str()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_for(app, Pane::Vaults)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut s = app.vault_state;
    frame.render_stateful_widget(list, area, &mut s);
}

fn render_secrets(app: &App, frame: &mut Frame, area: Rect) {
    let title = if app.secret_filter_active {
        format!("Secrets (filter: /{})", app.secret_filter)
    } else if !app.secret_filter.is_empty() {
        format!("Secrets (filter: {})", app.secret_filter)
    } else if app.secrets_loading {
        "Secrets (loading…)".to_string()
    } else {
        "Secrets".to_string()
    };
    let items: Vec<ListItem> = app
        .filtered_secrets()
        .iter()
        .map(|s| {
            let d = if s.original_name.is_empty() {
                s.name.as_str()
            } else {
                s.original_name.as_str()
            };
            ListItem::new(d)
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_for(app, Pane::Secrets)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut s = app.secret_state;
    frame.render_stateful_widget(list, area, &mut s);
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(s) = app.selected_secret() {
        let display = if s.original_name.is_empty() {
            s.name.as_str()
        } else {
            s.original_name.as_str()
        };
        lines.push(Line::raw(format!("name: {display}")));
        let is_typed_record = s.tags.contains_key(crate::records::TYPE_TAG);
        let selection_key = app.selected_vault_and_name();
        let fetched = selection_key.as_ref().and_then(|k| app.values.get(k));
        let line = value_line(
            is_typed_record,
            app.value_revealed,
            selection_key.is_some(),
            fetched.map(|v| v.as_str()),
            app.value_loading,
        );
        lines.push(Line::raw(line));
        if let Some(g) = &s.groups {
            lines.push(Line::raw(format!("groups: {g}")));
        }
        if let Some(f) = &s.folder {
            lines.push(Line::raw(format!("folder: {f}")));
        }
        lines.push(Line::raw(format!("updated: {}", s.updated_on)));

        // Record-types plan Task 11: a "Fields" section for typed records.
        // Metadata fields (f.* tags) are known without fetching the value;
        // secret field NAMES only become known once the value has been
        // fetched (the existing Space-to-reveal detail-fetch path), and
        // their values are always masked regardless of the top `value:`
        // line's reveal state.
        if let Some(record_type) = s.tags.get(crate::records::TYPE_TAG) {
            let mut fields: BTreeMap<String, String> = s
                .tags
                .iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix(crate::records::FIELD_TAG_PREFIX)
                        .map(|f| (f.to_string(), v.clone()))
                })
                .collect();
            let secret_names: Vec<String> = fetched
                .and_then(|val| crate::records::parse_envelope(val.as_str()).ok())
                .map(|envelope| envelope.into_keys().collect())
                .unwrap_or_default();
            for name in &secret_names {
                fields.entry(name.clone()).or_default();
            }

            lines.push(Line::raw(""));
            lines.push(Line::raw("Fields:"));
            for line in record_field_lines(record_type, &fields, &secret_names) {
                lines.push(Line::raw(format!("  {line}")));
            }
        }
    } else {
        lines.push(Line::raw("(no secret selected)"));
    }
    let p = Paragraph::new(lines).block(
        Block::default()
            .title("Detail")
            .borders(Borders::ALL)
            .border_style(border_for(app, Pane::Detail)),
    );
    frame.render_widget(p, area);
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = app.selected_vault() {
        parts.push(format!("vault: {v}"));
    }
    if let Some(s) = app
        .selected_vault()
        .and_then(|v| app.secrets_by_vault.get(v))
    {
        parts.push(format!("{} secrets", s.len()));
    }
    if let Some(n) = app.clipboard_countdown {
        parts.push(format!("clipboard: {}s", n.div_ceil(10)));
    }
    parts.push("?:help q:quit".to_string());
    let p = Paragraph::new(parts.join("  ·  ")).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(p, area);
}

fn render_toast(app: &App, frame: &mut Frame, area: Rect) {
    if let Some(t) = &app.toast {
        let line = format!(
            "⚠ {}{} (e: details, Esc: dismiss)",
            t.code
                .as_deref()
                .map(|c| format!("[{c}] "))
                .unwrap_or_default(),
            t.message
        );
        let p = Paragraph::new(line).style(Style::default().fg(Color::Yellow));
        frame.render_widget(p, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn value_line_typed_record_never_reveals_raw_envelope() {
        // Even "revealed" with a fetched value present, a typed record
        // shows the placeholder, not the raw envelope JSON.
        assert_eq!(
            value_line(true, true, true, Some(r#"{"password":"hunter2"}"#), false),
            TYPED_RECORD_VALUE_PLACEHOLDER,
        );
        // ...and while masked/unrevealed too.
        assert_eq!(
            value_line(true, false, true, None, false),
            TYPED_RECORD_VALUE_PLACEHOLDER,
        );
    }

    #[test]
    fn value_line_untyped_secret_unaffected() {
        assert_eq!(
            value_line(false, false, true, None, false),
            "value: ●●●●●●●●"
        );
        assert_eq!(
            value_line(false, true, true, Some("hunter2"), false),
            "value: hunter2"
        );
        assert_eq!(
            value_line(false, true, true, None, true),
            "value: (loading…)"
        );
        assert_eq!(
            value_line(false, true, true, None, false),
            "value: (press Space)"
        );
        assert_eq!(value_line(false, true, false, None, false), "value: ()");
    }

    #[test]
    fn record_field_lines_includes_type_header() {
        let lines = record_field_lines("login", &BTreeMap::new(), &[]);
        assert_eq!(lines, vec!["type: login".to_string()]);
    }

    #[test]
    fn record_field_lines_renders_metadata_plain() {
        let f = fields(&[("username", "bob"), ("url", "https://example.com")]);
        let lines = record_field_lines("login", &f, &[]);
        assert!(lines.contains(&"username: bob".to_string()));
        assert!(lines.contains(&"url: https://example.com".to_string()));
    }

    #[test]
    fn record_field_lines_masks_secret_field_values() {
        let f = fields(&[("username", "bob"), ("password", "hunter2")]);
        let secret_names = vec!["password".to_string()];
        let lines = record_field_lines("login", &f, &secret_names);
        assert!(lines.contains(&"username: bob".to_string()));
        assert!(lines.contains(&format!("password: {MASKED_FIELD_VALUE}")));
        assert!(
            !lines.iter().any(|l| l.contains("hunter2")),
            "secret value must never appear: {lines:?}"
        );
    }

    #[test]
    fn record_field_lines_masks_even_when_value_is_populated() {
        // Defense in depth: even if the caller passes a real value for a
        // name also listed in secret_names, the rendered line still masks
        // it rather than trusting the caller never to make that mistake.
        let f = fields(&[("totp-seed", "ABCDEF")]);
        let secret_names = vec!["totp-seed".to_string()];
        let lines = record_field_lines("login", &f, &secret_names);
        assert_eq!(
            lines,
            vec![
                "type: login".to_string(),
                format!("totp-seed: {MASKED_FIELD_VALUE}"),
            ]
        );
    }

    #[test]
    fn record_field_lines_sorted_by_name() {
        let f = fields(&[("zeta", "1"), ("alpha", "2")]);
        let lines = record_field_lines("custom", &f, &[]);
        assert_eq!(
            lines,
            vec![
                "type: custom".to_string(),
                "alpha: 2".to_string(),
                "zeta: 1".to_string(),
            ]
        );
    }
}
