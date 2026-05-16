//! DATAFORTS tab — renders the live blob-adapter set. Each
//! registered adapter shows up as a row in the top list with
//! its core metrics summarized; the cursored adapter drives
//! the detail body below (disk gauge + health-gate verdict +
//! Store/Fetch/GC + Overflow panels).
//!
//! Layout:
//! - adapter list (height = N rows + header, capped)
//! - status bar: capacity / used / disk-ratio / health-gate
//! - left column: store + fetch + GC counters
//! - right column: overflow counter family (per-reason)

use net_sdk::dataforts::{
    evaluate_health_gate, BlobMetricsSnapshot, HealthGateAction,
    HEALTH_GATE_CLEAR_THRESHOLD, HEALTH_GATE_EMIT_THRESHOLD,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::{theme, widgets};

/// One row of the multi-adapter list. The deck snapshots each
/// registered adapter's metrics at the frame boundary so the
/// list + detail body agree on what the adapter looked like.
#[derive(Clone)]
pub struct AdapterEntry {
    pub id: String,
    pub metrics: BlobMetricsSnapshot,
}

pub fn render(frame: &mut Frame<'_>, area: Rect, entries: &[AdapterEntry], cursor: usize) {
    if entries.is_empty() {
        render_empty(frame, area);
        return;
    }
    let cursor = cursor.min(entries.len().saturating_sub(1));
    // List height: header + one row per adapter, capped at 8
    // rows so the detail body keeps reasonable real estate on
    // tall clusters.
    let visible_rows = entries.len().min(8);
    let list_height = (visible_rows as u16) + 3; // header + borders
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(list_height), // adapter list
            Constraint::Length(3),           // status bar
            Constraint::Min(0),              // body
        ])
        .split(area);
    render_adapter_list(frame, rows[0], entries, cursor);
    render_status(frame, rows[1], &entries[cursor].metrics, &entries[cursor].id);
    render_body(frame, rows[2], &entries[cursor].metrics);
}

fn render_adapter_list(
    frame: &mut Frame<'_>,
    area: Rect,
    entries: &[AdapterEntry],
    cursor: usize,
) {
    let total = entries.len();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("ADAPTERS", theme::green_hi()),
        Span::styled(
            format!("    {total} registered"),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line);
    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("ADAPTER"),
        cell_dim("DISK"),
        cell_dim("GATE"),
        cell_dim("STORED"),
        cell_dim("FETCHED"),
        cell_dim("BYTES"),
        cell_dim("OVERFLOW"),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let id_style = if is_cursor { theme::green_hi() } else { theme::text() };
        let m = &e.metrics;
        let disk_pct = if m.disk_capacity_bytes == 0 {
            0
        } else {
            ((m.disk_used_bytes * 100) / m.disk_capacity_bytes).min(999)
        };
        let (gate_text, gate_style) = gate_chip(m.disk_used_bytes, m.disk_capacity_bytes);
        let overflow_active = m.overflow.active;
        let (of_text, of_style) = if overflow_active {
            ("ACTIVE", theme::amber())
        } else {
            ("idle", theme::dim())
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(e.id.clone(), id_style)),
            Cell::from(Span::styled(format!("{disk_pct:>3}%"), theme::text())),
            Cell::from(Span::styled(gate_text, gate_style)),
            Cell::from(Span::styled(format!("{:>6}", m.blobs_stored_total), theme::text())),
            Cell::from(Span::styled(format!("{:>6}", m.blobs_fetched_total), theme::text())),
            Cell::from(Span::styled(fmt_bytes(m.bytes_stored_total), theme::dim())),
            Cell::from(Span::styled(of_text, of_style)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(20), // adapter id
            Constraint::Length(5),  // disk %
            Constraint::Length(10), // gate
            Constraint::Length(7),  // stored
            Constraint::Length(8),  // fetched
            Constraint::Length(11), // bytes
            Constraint::Min(8),     // overflow
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

fn gate_chip(used: u64, cap: u64) -> (&'static str, ratatui::style::Style) {
    let gate = evaluate_health_gate(used, cap, false);
    match gate {
        HealthGateAction::Emit => ("UNHEALTHY", theme::red()),
        HealthGateAction::Clear | HealthGateAction::Unchanged => ("healthy", theme::green()),
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DATAFORTS", theme::green_hi()),
            Span::styled("    no adapter wired", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no blob adapter attached to this runtime",
        "run with --features samples for a pre-populated fixture, or wire a MeshBlobAdapter",
    );
}

fn render_status(frame: &mut Frame<'_>, area: Rect, snap: &BlobMetricsSnapshot, adapter_id: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("BLOB.ADAPTER", theme::green_hi()),
            Span::styled(format!("    {adapter_id}"), theme::amber()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let used = snap.disk_used_bytes;
    let cap = snap.disk_capacity_bytes;
    let ratio = if cap == 0 {
        0.0
    } else {
        used as f64 / cap as f64
    };
    let pct = (ratio * 100.0) as u16;
    let pct_clamped = pct.min(100);

    let (bar_color, bar_label, bar_style) = bar_style_for(ratio);
    let gate = evaluate_health_gate(used, cap, false);
    let (gate_text, gate_style) = match gate {
        HealthGateAction::Emit => ("UNHEALTHY", theme::red()),
        HealthGateAction::Clear | HealthGateAction::Unchanged => ("healthy", theme::green()),
    };

    let line = Line::from(vec![
        Span::styled("disk ", theme::chrome()),
        bar(pct_clamped, 28, bar_color),
        Span::styled(format!("  {pct_clamped}% "), bar_style),
        Span::styled(
            format!("({} / {})    ", fmt_bytes(used), fmt_bytes(cap)),
            theme::dim(),
        ),
        Span::styled("gate ", theme::chrome()),
        Span::styled(gate_text, gate_style),
        Span::styled(
            format!(
                "    thresholds emit≥{:.0}% clear≤{:.0}%    ",
                HEALTH_GATE_EMIT_THRESHOLD * 100.0,
                HEALTH_GATE_CLEAR_THRESHOLD * 100.0
            ),
            theme::dim(),
        ),
        Span::styled(bar_label, bar_style),
    ]);
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_body(frame: &mut Frame<'_>, area: Rect, snap: &BlobMetricsSnapshot) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    render_io_panel(frame, cols[0], snap);
    render_overflow_panel(frame, cols[1], snap);
}

fn render_io_panel(frame: &mut Frame<'_>, area: Rect, snap: &BlobMetricsSnapshot) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("STORE / FETCH / GC", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        kv("blobs_stored_total", snap.blobs_stored_total),
        kv("blobs_fetched_total", snap.blobs_fetched_total),
        kv_bytes("bytes_stored_total", snap.bytes_stored_total),
        kv("gc_swept_total", snap.gc_swept_total),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_overflow_panel(frame: &mut Frame<'_>, area: Rect, snap: &BlobMetricsSnapshot) {
    let o = &snap.overflow;
    let (active_text, active_style) = if o.active {
        ("ACTIVE", theme::amber())
    } else {
        ("idle", theme::dim())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("OVERFLOW", theme::green_hi()),
            Span::styled("    ", theme::chrome()),
            Span::styled(active_text, active_style),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        kv("pushes_admitted_total", o.pushes_admitted_total),
        kv("push_errors_total", o.push_errors_total),
        kv_bytes("pushed_bytes_total", o.pushed_bytes_total),
        kv("rejected_no_target", o.rejected_no_target_total),
        kv("rejected_no_storage_cap", o.rejected_no_storage_cap_total),
        kv("rejected_not_participating", o.rejected_not_participating_total),
        kv("rejected_sender_not_overflowing", o.rejected_sender_not_overflowing_total),
        kv("rejected_unhealthy", o.rejected_unhealthy_total),
        kv("rejected_scope_mismatch", o.rejected_scope_mismatch_total),
        kv("rejected_insufficient_disk", o.rejected_insufficient_disk_total),
        kv("high_water_triggered", o.high_water_triggered_total),
        kv("low_water_cleared", o.low_water_cleared_total),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn kv(label: &'static str, value: u64) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<32}"), theme::chrome()),
        Span::styled(format!("{value}"), value_style(value)),
    ])
}

fn kv_bytes(label: &'static str, value: u64) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<32}"), theme::chrome()),
        Span::styled(fmt_bytes(value), value_style(value)),
    ])
}

/// Zeros dim; non-zero brighter so the operator's eye lands on
/// counters that have actual activity behind them.
fn value_style(value: u64) -> ratatui::style::Style {
    if value == 0 {
        theme::dim()
    } else {
        theme::text()
    }
}

fn bar_style_for(ratio: f64) -> (ratatui::style::Color, &'static str, ratatui::style::Style) {
    if ratio >= HEALTH_GATE_EMIT_THRESHOLD {
        (theme::RED, "EMIT", theme::red())
    } else if ratio >= HEALTH_GATE_CLEAR_THRESHOLD {
        (theme::AMBER, "WATCH", theme::amber())
    } else {
        (theme::GREEN_HI, "STEADY", theme::dim())
    }
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

/// Human-readable bytes — KB/MB/GB/TB at the closest power of
/// 1024. Single-decimal precision keeps the column stable
/// without losing too much resolution.
fn fmt_bytes(b: u64) -> String {
    const K: u64 = 1 << 10;
    const M: u64 = 1 << 20;
    const G: u64 = 1 << 30;
    const T: u64 = 1 << 40;
    if b >= T {
        format!("{:.1} TB", b as f64 / T as f64)
    } else if b >= G {
        format!("{:.1} GB", b as f64 / G as f64)
    } else if b >= M {
        format!("{:.1} MB", b as f64 / M as f64)
    } else if b >= K {
        format!("{:.1} KB", b as f64 / K as f64)
    } else {
        format!("{b} B")
    }
}
