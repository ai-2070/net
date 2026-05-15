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
    min_level: LogLevel,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    render_filter_bar(frame, rows[0], min_level);
    render_log_grid(frame, rows[1], tick, snapshot, min_level);
    render_status(frame, rows[2], snapshot, min_level);
}

fn render_filter_bar(frame: &mut Frame<'_>, area: Rect, min_level: LogLevel) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("LOG.MATRIX", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Active level threshold gets the amber accent when it's
    // suppressing rows; default Info is rendered green to read
    // as "open / unfiltered."
    let (level_text, level_style) = level_chip(min_level);

    let line = Line::from(vec![
        Span::styled("level ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled(level_text, level_style),
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

fn level_chip(min_level: LogLevel) -> (&'static str, ratatui::style::Style) {
    match min_level {
        LogLevel::Debug => ("DEBUG+", theme::green_hi()),
        LogLevel::Info => ("INFO+", theme::green_hi()),
        LogLevel::Warn => ("WARN+", theme::amber()),
        LogLevel::Error => ("ERR", theme::red()),
        _ => ("?", theme::chrome()),
    }
}

/// Numeric rank used for "min level" comparisons. Higher means
/// more severe.
fn level_rank(l: LogLevel) -> u8 {
    match l {
        LogLevel::Debug => 0,
        LogLevel::Info => 1,
        LogLevel::Warn => 2,
        LogLevel::Error => 3,
        _ => 1,
    }
}

fn render_log_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    tick: u64,
    snapshot: Option<&MeshOsSnapshot>,
    min_level: LogLevel,
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
            project_live_log_ring(snap, total_rows, min_level)
        }
        _ => Vec::new(),
    };

    if lines.is_empty() {
        // Distinguish "no logs at all" from "filter is hiding
        // everything" — the latter is easy to miss and a common
        // operator confusion.
        let any_records = snapshot
            .map(|s| !s.log_ring.is_empty())
            .unwrap_or(false);
        let (head, hint) = if any_records {
            (
                "no log lines at this threshold",
                "press [f] to lower the level filter",
            )
        } else {
            (
                "no log lines yet",
                "publish_log() on any registered daemon will appear here",
            )
        };
        crate::widgets::empty::render(frame, inner, head, hint);
        return;
    }

    let start = lines.len().saturating_sub(total_rows);
    let visible = &lines[start..];
    frame.render_widget(Paragraph::new(visible.to_vec()), inner);
}

fn render_status(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&MeshOsSnapshot>,
    min_level: LogLevel,
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(36)])
        .split(area);

    let total = snapshot.map(|s| s.log_ring.len()).unwrap_or(0);
    let shown = snapshot
        .map(|s| {
            s.log_ring
                .iter()
                .filter(|r| level_rank(r.level) >= level_rank(min_level))
                .count()
        })
        .unwrap_or(0);
    let left = Line::from(vec![
        Span::styled(format!("{shown}/{total} lines  ·  "), theme::chrome()),
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
fn project_live_log_ring(
    snapshot: &MeshOsSnapshot,
    capacity: usize,
    min_level: LogLevel,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::with_capacity(snapshot.log_ring.len().min(capacity));
    let min = level_rank(min_level);
    for rec in snapshot.log_ring.iter().filter(|r| level_rank(r.level) >= min) {
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
