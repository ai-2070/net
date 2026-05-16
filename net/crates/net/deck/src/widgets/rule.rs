use ratatui::{layout::Rect, text::Line, widgets::Paragraph, Frame};

use crate::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let line = "─".repeat(area.width as usize);
    frame.render_widget(Paragraph::new(Line::styled(line, theme::rule())), area);
}
