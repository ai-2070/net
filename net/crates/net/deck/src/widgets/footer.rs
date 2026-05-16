use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect, toast: Option<&str>) {
    // Active toast hijacks the footer row — confirmation
    // messages need to be visible against the binding hints.
    if let Some(msg) = toast {
        let line = Line::from(vec![Span::styled(format!("  {msg}"), theme::green_hi())]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let line = Line::from(vec![
        Span::styled("◂▸", theme::green()),
        Span::styled(" tab   ", theme::dim()),
        Span::styled("1-9", theme::green()),
        Span::styled(" jump   ", theme::dim()),
        Span::styled("j/k", theme::green()),
        Span::styled(" cursor   ", theme::dim()),
        Span::styled("c/C", theme::green()),
        Span::styled(" cordon   ", theme::dim()),
        Span::styled("d", theme::green()),
        Span::styled(" drain   ", theme::dim()),
        Span::styled("m/M", theme::green()),
        Span::styled(" maint   ", theme::dim()),
        Span::styled("a", theme::green()),
        Span::styled(" avoid   ", theme::dim()),
        Span::styled("i", theme::green()),
        Span::styled(" inval   ", theme::dim()),
        Span::styled("r", theme::green()),
        Span::styled(" restart   ", theme::dim()),
        Span::styled("e", theme::green()),
        Span::styled(" export   ", theme::dim()),
        Span::styled("q", theme::green()),
        Span::styled(" quit", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
