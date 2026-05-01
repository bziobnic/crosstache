use crate::tui::app::{App, Overlay};
use ratatui::Frame;

pub fn render_active_overlay(app: &App, _frame: &mut Frame) {
    if matches!(app.overlay, Overlay::None) { return; }
    // Tasks 8 (Help, ErrorDetail) and 10 (History, Audit) fill in each variant.
}
