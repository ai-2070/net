//! DATAFORTS tab — renders the live `BlobMetricsSnapshot` from
//! the local mesh blob adapter. With samples mode the runtime
//! pre-fills realistic counters; in default mode the adapter
//! is unhooked and the tab shows an empty state explaining
//! that.
//!
//! Layout:
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
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&BlobMetricsSnapshot>) {
    let Some(snap) = snapshot else {
        render_empty(frame, area);
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);
    render_status(frame, rows[0], snap);
    render_body(frame, rows[1], snap);
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

fn render_status(frame: &mut Frame<'_>, area: Rect, snap: &BlobMetricsSnapshot) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("BLOB.ADAPTER", theme::green_hi()),
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
