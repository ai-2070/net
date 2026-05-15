use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::App, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let uptime = app.started.elapsed().as_secs();
    let evt_per_s = 8_300_000u64 + ((app.tick.wrapping_mul(11_113)) % 400_000);
    let p50_ns = 35 + (app.tick.wrapping_mul(3) % 8) as u64;

    // First slot: the binary always spawns an in-process
    // runtime now, so the dot is always green. The label
    // reflects whether the static sample fixture is installed.
    let mode_text = if cfg!(feature = "samples") {
        "LIVE +samples"
    } else {
        "LIVE"
    };

    let left = Line::from(vec![
        Span::styled("● ", theme::green()),
        Span::styled(mode_text, theme::green_hi()),
        Span::raw("   "),
        Span::styled("CODENAME: ", theme::chrome()),
        Span::styled("ATOMIC PLAYBOYS", theme::text()),
        Span::raw("   "),
        Span::styled("EVT/SEC: ", theme::chrome()),
        Span::styled(format!("{:.1}M", evt_per_s as f64 / 1_000_000.0), theme::green_hi()),
        Span::raw("   "),
        Span::styled("P50: ", theme::chrome()),
        Span::styled(format!("{p50_ns}ns"), theme::green_hi()),
        Span::raw("   "),
        Span::styled("UP: ", theme::chrome()),
        Span::styled(format!("{uptime}s"), theme::text()),
    ]);

    let right = Line::from(vec![
        Span::styled("v0.17.0   ", theme::chrome()),
        Span::styled("BUILD: ", theme::chrome()),
        Span::styled("2026.05.14", theme::text()),
        Span::raw("   "),
        Span::styled("SHA: ", theme::chrome()),
        Span::styled("f192df9", theme::text()),
    ]);

    frame.render_widget(Paragraph::new(left), cols[0]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}
