use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::Tab, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, current: Tab) {
    // Brand on the left, tabs filling the rest. The "● LIVE"
    // chip the right side used to carry duplicated the status
    // bar's live indicator — dropped so the tab strip can fit
    // all 10 tabs without truncating on a 120-col terminal.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    let brand = Line::from(vec![Span::styled("DECK  ", theme::green_hi())]);
    frame.render_widget(Paragraph::new(brand), cols[0]);

    // Tab key glyphs: 1..=9 then 0 (for the 10th tab; matches
    // `KeyCode::Char('0') => Tab::Blobs` in App::on_key).
    let key_for = |i: usize| -> String {
        if i < 9 {
            format!("[{}] ", i + 1)
        } else {
            "[0] ".to_string()
        }
    };

    let mut spans = Vec::new();
    for (i, tab) in Tab::all().iter().enumerate() {
        let key = key_for(i);
        if *tab == current {
            spans.push(Span::styled(key, theme::green()));
            spans.push(Span::styled(tab.label(), theme::green_hi()));
        } else {
            spans.push(Span::styled(key, theme::chrome()));
            spans.push(Span::styled(tab.label(), theme::dim()));
        }
        // Single-space gap between tabs. The visual rhythm
        // comes from the `[N]` prefix in green; extra spaces
        // were redundant + overflowing past 10 tabs.
        spans.push(Span::raw(" "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), cols[1]);
}
