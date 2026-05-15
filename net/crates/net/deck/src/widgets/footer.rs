use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let line = Line::from(vec![
        Span::styled("◂▸", theme::green()),
        Span::styled(" tab    ", theme::dim()),
        Span::styled("1-5", theme::green()),
        Span::styled(" jump    ", theme::dim()),
        Span::styled("j/k", theme::green()),
        Span::styled(" cursor    ", theme::dim()),
        Span::styled("c/C", theme::green()),
        Span::styled(" cordon    ", theme::dim()),
        Span::styled("r", theme::green()),
        Span::styled(" restart    ", theme::dim()),
        Span::styled("q", theme::green()),
        Span::styled(" quit", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
