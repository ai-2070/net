use net_sdk::deck::{LogLevel, MeshOsSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{nodes, theme};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    tick: u64,
    snapshot: Option<&MeshOsSnapshot>,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    render_filter_bar(frame, rows[0]);
    render_log_grid(frame, rows[1], tick, snapshot);
    render_status(frame, rows[2], snapshot);
}

fn render_filter_bar(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("LOG.MATRIX", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled("level ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled("INFO+", theme::green_hi()),
        Span::styled("]   ", theme::chrome()),
        Span::styled("node ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled("*", theme::green_hi()),
        Span::styled("]   ", theme::chrome()),
        Span::styled("daemon ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled("*", theme::green_hi()),
        Span::styled("]   ", theme::chrome()),
        Span::styled("kind ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled("*", theme::green_hi()),
        Span::styled("]   ", theme::chrome()),
        Span::styled("follow ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled("ON", theme::green_hi()),
        Span::styled("]", theme::chrome()),
    ]);
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_log_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    tick: u64,
    snapshot: Option<&MeshOsSnapshot>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let total_rows = inner.height as usize;
    // Live mode: project the snapshot's log_ring into Lines.
    // Empty ring (fresh runtime, no source publishing yet)
    // surfaces a centered "waiting" placeholder so the user
    // sees the tab is wired but idle.
    let _ = tick;
    let lines: Vec<Line> = match snapshot {
        Some(snap) if !snap.log_ring.is_empty() => {
            project_live_log_ring(snap, total_rows)
        }
        _ => Vec::new(),
    };

    if lines.is_empty() {
        crate::widgets::empty::render(
            frame,
            inner,
            "no log lines yet",
            "publish_log() on any registered daemon will appear here",
        );
        return;
    }

    let start = lines.len().saturating_sub(total_rows);
    let visible = &lines[start..];
    frame.render_widget(Paragraph::new(visible.to_vec()), inner);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(36)])
        .split(area);

    let count = snapshot.map(|s| s.log_ring.len()).unwrap_or(0);
    let left = Line::from(vec![
        Span::styled(format!("{count} lines  ·  "), theme::chrome()),
        Span::styled("live snapshot", theme::dim()),
        Span::styled("  ·  ", theme::chrome()),
        Span::styled("0 dropped", theme::green_hi()),
    ]);
    frame.render_widget(Paragraph::new(left), cols[0]);

    let right = Line::from(vec![
        Span::styled("[/] ", theme::green_hi()),
        Span::styled("search   ", theme::dim()),
        Span::styled("[f] ", theme::green_hi()),
        Span::styled("filter   ", theme::dim()),
        Span::styled("[p] ", theme::green_hi()),
        Span::styled("pause", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}

/// Project the live `MeshOsSnapshot.log_ring` into renderable
/// Lines. Each record carries `seq`, `ts_ms`, `level`,
/// `daemon_id`, `node_id`, `message`. Older entries are at the
/// front of the ring; we keep that order and let the caller
/// pick the tail.
fn project_live_log_ring(snapshot: &MeshOsSnapshot, capacity: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::with_capacity(snapshot.log_ring.len().min(capacity));
    for rec in snapshot.log_ring.iter() {
        let (level_style, level_pad) = match rec.level {
            LogLevel::Info  => (theme::dim(),   "INFO "),
            LogLevel::Warn  => (theme::amber(), "WARN "),
            LogLevel::Error => (theme::red(),   "ERR  "),
            _              => (theme::text(),  "?    "),
        };
        let ts = format_ts_ms(rec.ts_ms);
        let mut spans = vec![
            Span::styled(ts, theme::chrome()),
            Span::styled("  ", theme::chrome()),
            Span::styled(level_pad.to_string(), level_style),
            Span::styled("  ", theme::chrome()),
        ];
        // The runtime stamps `node_id` and `daemon_id` on every
        // record. Format `<node>/<daemon>` so the source column
        // still reads even when one or the other is missing.
        match (rec.node_id, rec.daemon_id) {
            (Some(node), Some(daemon)) => {
                spans.extend(nodes::id_spans(&format!("0x{node:x}")));
                spans.push(Span::styled("/", theme::chrome()));
                spans.push(Span::styled(format!("0x{daemon:x}"), theme::cyan()));
            }
            (Some(node), None) => {
                spans.extend(nodes::id_spans(&format!("0x{node:x}")));
            }
            (None, Some(daemon)) => {
                spans.push(Span::styled(format!("daemon.0x{daemon:x}"), theme::cyan()));
            }
            (None, None) => {
                spans.push(Span::styled("—", theme::chrome()));
            }
        }
        spans.push(Span::styled("  ", theme::chrome()));
        spans.push(Span::styled(rec.message.clone(), theme::text()));
        out.push(Line::from(spans));
    }
    out
}

fn format_ts_ms(ts_ms: u64) -> String {
    // wall-clock-relative; show as MM:SS.mmm so the column
    // width stays stable.
    let total_sec = ts_ms / 1_000;
    let mm = (total_sec / 60) % 60;
    let ss = total_sec % 60;
    let ms = ts_ms % 1_000;
    format!("{mm:02}:{ss:02}.{ms:03}")
}
