use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::Tab, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, current: Tab) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(18), Constraint::Min(0), Constraint::Length(16)])
        .split(area);

    let brand = Line::from(vec![
        Span::styled("DECK", theme::green_hi()),
        Span::styled("  // OPERATOR", theme::chrome()),
    ]);
    frame.render_widget(Paragraph::new(brand), cols[0]);

    let mut spans = Vec::new();
    for (i, tab) in Tab::all().iter().enumerate() {
        let key = format!("[{}] ", i + 1);
        if *tab == current {
            spans.push(Span::styled(key, theme::green()));
            spans.push(Span::styled(tab.label(), theme::green_hi()));
        } else {
            spans.push(Span::styled(key, theme::chrome()));
            spans.push(Span::styled(tab.label(), theme::dim()));
        }
        spans.push(Span::styled("   ", theme::chrome()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), cols[1]);

    let tag = Line::from(vec![
        Span::styled("● ", theme::green()),
        Span::styled("LIVE", theme::green_hi()),
    ]);
    frame.render_widget(Paragraph::new(tag).alignment(Alignment::Right), cols[2]);
}
