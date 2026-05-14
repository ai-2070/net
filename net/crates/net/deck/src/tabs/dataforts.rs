use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect, tick: u64) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    render_summary(frame, rows[0]);
    render_pool(frame, rows[1], tick);
}

fn render_summary(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("MESH.STORAGE", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled("pressure ", theme::chrome()),
        bar(63, 30, theme::GREEN_HI),
        Span::styled("  63% ", theme::green_hi()),
        Span::styled("· STEADY    ", theme::dim()),
        Span::styled("watermark ", theme::chrome()),
        Span::styled("high·85 low·30    ", theme::text()),
        Span::styled("recall every ", theme::chrome()),
        Span::styled("1.4s", theme::text()),
    ]);
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_pool(frame: &mut Frame<'_>, area: Rect, tick: u64) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    render_nodes(frame, cols[0]);
    render_events(frame, cols[1], tick);
}

fn render_nodes(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("STORAGE.POOL", theme::green_hi()),
            Span::styled("    5 nodes · 892 GB cap", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(inner);

    let nodes: [(&str, u16, &str); 5] = [
        ("node.0x7af3", 76, "evictable"),
        ("node.0x2c91", 69, "steady"),
        ("node.0xeb29", 57, "steady"),
        ("node.0xfbb1", 76, "evictable"),
        ("node.0x9a3e", 37, "absorbing"),
    ];
    for (i, (label, pct, tag)) in nodes.iter().enumerate() {
        let tag_color = match *tag {
            "evictable" => theme::AMBER,
            "absorbing" => theme::CYAN,
            _ => theme::TEXT_DIM,
        };
        let line = Line::from(vec![
            Span::styled(format!("{label}  "), theme::dim()),
            bar(*pct, 40, theme::GREEN_HI),
            Span::styled(format!("  {pct}%  "), theme::text()),
            Span::styled(*tag, ratatui::style::Style::default().fg(tag_color)),
        ]);
        frame.render_widget(Paragraph::new(line), rows[i]);
    }
}

fn render_events(frame: &mut Frame<'_>, area: Rect, tick: u64) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("RECENT.EVENTS", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let base = tick / 2;
    let entries: [(u64, &str, &str, &str); 7] = [
        (base + 0,  "cool",   "0x3457", "rate 0.06 · evictable"),
        (base + 4,  "cool",   "0x8366", "rate 0.11 · evictable"),
        (base + 8,  "cool",   "0xd93b", "rate 0.09 · evictable"),
        (base + 13, "cool",   "0xa43f", "rate 0.17 · evictable"),
        (base + 17, "cool",   "0x4b04", "rate 0.10 · evictable"),
        (base + 21, "absorb", "0x9a3e", "free 65%  · open"),
        (base + 25, "pull",   "0xd4ff", "blob 0x29 → 0x7af3"),
    ];
    let lines: Vec<Line> = entries
        .iter()
        .map(|(t, kind, blob, detail)| {
            let kind_color = match *kind {
                "cool" => theme::CYAN,
                "absorb" => theme::GREEN_HI,
                "pull" => theme::AMBER,
                _ => theme::TEXT,
            };
            Line::from(vec![
                Span::styled(fmt_ts(*t), theme::chrome()),
                Span::styled("  [", theme::chrome()),
                Span::styled(*kind, ratatui::style::Style::default().fg(kind_color)),
                Span::styled("]  ", theme::chrome()),
                Span::styled(*blob, theme::text()),
                Span::styled("  ", theme::chrome()),
                Span::styled(*detail, theme::dim()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn bar(pct: u16, width: u16, color: ratatui::style::Color) -> Span<'static> {
    let pct = pct.min(100);
    let filled = ((pct as u32 * width as u32) / 100) as usize;
    let empty = width as usize - filled;
    let mut s = String::with_capacity(width as usize);
    for _ in 0..filled {
        s.push('━');
    }
    for _ in 0..empty {
        s.push('·');
    }
    Span::styled(s, ratatui::style::Style::default().fg(color))
}

fn fmt_ts(t: u64) -> String {
    let mm = (t / 60) % 60;
    let ss = t % 60;
    let ms = (t.wrapping_mul(41)) % 1000;
    format!("{mm:02}:{ss:02}.{ms:03}")
}
