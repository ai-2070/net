//! Empty-state placeholder. Reused by every tab that reads
//! from the snapshot — when its source collection is empty
//! (default mode without `--features samples`, or a real
//! cluster that hasn't reported anything yet), the tab
//! renders this centered prompt instead of fixture data.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::theme;

/// Render a centered "no data" prompt with a contextual hint.
/// `headline` is the operator-facing description; `hint`
/// suggests what to do or wait for.
pub fn render(frame: &mut Frame<'_>, area: Rect, headline: &str, hint: &str) {
    // Vertically center: split area into three regions, place
    // text in the middle.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    let lines = vec![
        Line::from(Span::styled(
            format!("· · · {headline} · · ·"),
            theme::dim(),
        )),
        Line::from(""),
        Line::from(Span::styled(hint, theme::chrome())),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rows[1]);
}
