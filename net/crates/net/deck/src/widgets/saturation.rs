//! Saturation histogram — shared widget used by the DAEMONS
//! detail panel (placement node's curve) and the NODE page
//! (focused node's curve).
//!
//! The samples are deck-side: `peer.saturation_trend` pushed
//! into a 60-slot ring buffer once per tick. This widget just
//! renders the ring as a vertical bar grid coloured by pressure
//! band, plus p50/p99/max chips above and a `0.0 — 0.5 — 1.0`
//! axis below.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{nodes, theme};

/// Title block + chips + bars + axis. `samples` is the ordered
/// history; the rightmost sample is "now."
pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    node_id_hex: &str,
    samples: &[f32],
) {
    let mut title_spans: Vec<Span> = vec![
        Span::styled(
            format!("{title}  "),
            ratatui::style::Style::default().fg(theme::GREEN_HI),
        ),
        Span::styled("saturation · ", theme::chrome()),
    ];
    title_spans.extend(nodes::id_spans(node_id_hex));
    title_spans.push(Span::styled("  · 60 samples", theme::chrome()));
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(title_spans));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if samples.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                "  no samples yet — node not in snapshot.peers",
                theme::chrome(),
            )]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let mut sorted: Vec<f32> = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = sorted[sorted.len() / 2];
    let p99 = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)];
    let max = sorted.last().copied().unwrap_or(0.0);

    let width = inner.width as usize;
    let take = samples.len().min(width.max(1));
    let start = samples.len() - take;
    let visible = &samples[start..];

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // percentile chips
            Constraint::Min(0),    // bars
            Constraint::Length(1), // axis
        ])
        .split(inner);

    let chips = Line::from(vec![
        Span::styled("  p50 ", theme::chrome()),
        Span::styled(format!("{p50:.2}"), pressure_style(p50)),
        Span::styled("   p99 ", theme::chrome()),
        Span::styled(format!("{p99:.2}"), pressure_style(p99)),
        Span::styled("   max ", theme::chrome()),
        Span::styled(format!("{max:.2}"), pressure_style(max)),
    ]);
    frame.render_widget(Paragraph::new(chips), rows[0]);

    let h = rows[1].height as usize;
    if h > 0 {
        let mut grid_lines: Vec<Line> = Vec::with_capacity(h);
        for row_idx in 0..h {
            let level = (h - row_idx) as f32 / h as f32;
            let mut spans: Vec<Span> = Vec::with_capacity(take + 1);
            spans.push(Span::raw(""));
            for v in visible.iter().copied() {
                let cell = if v >= level {
                    Span::styled("█", pressure_style(v))
                } else {
                    Span::raw(" ")
                };
                spans.push(cell);
            }
            grid_lines.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(grid_lines), rows[1]);
    }

    let axis_w = rows[2].width as usize;
    let mut axis = String::with_capacity(axis_w);
    for i in 0..axis_w {
        if i == 0 {
            axis.push_str("0.0");
        } else if i == axis_w / 2 {
            axis.push_str("0.5");
        } else if i + 3 == axis_w {
            axis.push_str("1.0");
        } else {
            axis.push(' ');
        }
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(axis, theme::chrome()))),
        rows[2],
    );
}

fn pressure_style(v: f32) -> ratatui::style::Style {
    if v >= 0.85 {
        theme::red()
    } else if v >= 0.65 {
        theme::amber()
    } else {
        ratatui::style::Style::default().fg(theme::GREEN_HI)
    }
}
