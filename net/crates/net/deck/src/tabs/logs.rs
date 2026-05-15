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
    // Empty ring (fresh runtime, seeder hasn't fired yet) is
    // a real state — render a placeholder so the user sees the
    // tab is wired but waiting.
    let lines: Vec<Line> = match snapshot {
        Some(snap) if !snap.log_ring.is_empty() => {
            project_live_log_ring(snap, total_rows)
        }
        Some(_) => placeholder_waiting(),
        None => generate_fixture_stream(120 + tick as usize),
    };

    let start = lines.len().saturating_sub(total_rows);
    let visible = &lines[start..];
    frame.render_widget(Paragraph::new(visible.to_vec()), inner);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(36)])
        .split(area);

    let (count, source) = match snapshot {
        Some(s) => (s.log_ring.len(), "live snapshot"),
        None => (8_217, "fixture stream"),
    };
    let left = Line::from(vec![
        Span::styled(format!("{count} lines  ·  "), theme::chrome()),
        Span::styled(source, theme::dim()),
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

fn placeholder_waiting() -> Vec<Line<'static>> {
    vec![Line::from(vec![
        Span::styled("  · · ·  ", theme::chrome()),
        Span::styled("waiting for log lines · runtime connected · empty log_ring", theme::dim()),
    ])]
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

fn generate_fixture_stream(count: usize) -> Vec<Line<'static>> {
    // Static rolling fixture; tick just slides the window so the
    // surface feels live in the visual mock.
    let templates: &[(u64, &str, &str, &str, &str)] = &[
        (0, "0xa96f", "scheduler",  "INFO",  "tick t=482·31  pending=0  drift=0.0"),
        (1, "0xeba8", "drift_corr", "WARN",  "drift_correct nodes(3) reflow"),
        (2, "0xbf44", "mikoshi",    "INFO",  "snapshot taken seq=4912 size=12.4KB"),
        (3, "0x6dfb", "gravity",    "INFO",  "gravity_pull 0x285e → 0x6dfb hot=0.71"),
        (4, "0xd4ff", "anti_entr",  "INFO",  "anti-entropy cycle ok · 0 reflows"),
        (5, "0xeba8", "telemetry",  "ERR",   "channel_full drop=23 buffer=8192"),
        (6, "0x82ee", "replica_co", "INFO",  "rebalance chain.0xc1 holders 2→3"),
        (7, "0xa96f", "scheduler",  "INFO",  "placement score 0xab3 → 0.83"),
        (8, "0x372b", "blob_mover", "INFO",  "blob.0x49 0xd4ff → 0x3599 (12.1MB)"),
        (9, "0xbdda", "fork_coord", "ERR",   "fork sentinel mismatch · retry"),
        (10,"0x3599", "blob_mover", "INFO",  "absorb 0x9a3e free=65% open"),
        (11,"0xbf44", "mikoshi",    "INFO",  "control: backpressure_on level=2"),
        (12,"0xe068", "telemetry",  "INFO",  "metric flush 12.4k samples"),
        (13,"0xeba8", "drift_corr", "WARN",  "anchor late by 2.1ms · nudging"),
        (14,"0x6dfb", "gravity",    "INFO",  "cool 0x4b04 rate=0.10 evictable"),
        (15,"0xbf44", "mikoshi",    "INFO",  "process_event seq=4913 latency=38ns"),
        (16,"0xeba8", "telemetry",  "ERR",   "retry budget exhausted · backoff 5s"),
        (17,"0xa96f", "scheduler",  "INFO",  "avoid_list shrink 4→2"),
        (18,"0xbdda", "fork_coord", "WARN",  "lineage walk → 11 hops"),
        (19,"0xd4ff", "anti_entr",  "INFO",  "anti-entropy seq.start"),
        (20,"0xf206", "blob_mover", "INFO",  "pull 0x29 → 0xf206  delta=2.4KB"),
    ];

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let (_, node, daemon, level, msg) = templates[i % templates.len()];
        let secs = 11_400u64 + i as u64;
        let mm = (secs / 60) % 60;
        let ss = secs % 60;
        let ms = ((i.wrapping_mul(41)) % 1000) as u64;
        let ts = format!("{mm:02}:{ss:02}.{ms:03}");
        let (level_style, level_pad) = match level {
            "INFO" => (theme::dim(), "INFO "),
            "WARN" => (theme::amber(), "WARN "),
            "ERR"  => (theme::red(),   "ERR  "),
            _      => (theme::text(),  "?    "),
        };
        let mut spans = vec![
            Span::styled(ts, theme::chrome()),
            Span::styled("  ", theme::chrome()),
            Span::styled(level_pad.to_string(), level_style),
            Span::styled("  ", theme::chrome()),
        ];
        // node refs in the source column render as `id.label` so
        // every log line carries the deployment tag inline.
        spans.extend(nodes::id_spans(node));
        spans.push(Span::styled("/", theme::chrome()));
        spans.push(Span::styled(daemon.to_string(), theme::cyan()));
        spans.push(Span::styled("  ", theme::chrome()));
        spans.push(Span::styled(msg.to_string(), theme::text()));
        out.push(Line::from(spans));
    }
    out
}
