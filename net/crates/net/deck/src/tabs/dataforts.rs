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
    pub overflow_enabled: bool,
    /// Capability tags this adapter advertises into the mesh.
    pub capabilities: Vec<String>,
    /// Local host the adapter is running on. Populated from the
    /// runtime's `this_node` view; rendered in the new context
    /// row alongside the adapter's own config.
    pub host: HostNodeView,
}

/// Point-in-time view of the node hosting an adapter. Mirrors
/// the slice of `PeerSnapshot` the deck renders for any peer —
/// kept as its own type so the deck can populate it for the
/// local node, which doesn't appear in `snapshot.peers`.
#[derive(Clone)]
pub struct HostNodeView {
    pub id: u64,
    pub label: Option<&'static str>,
    pub health: Option<&'static str>,
    pub cpu_load_1m: Option<f64>,
    pub mem_used_bytes: Option<u64>,
    pub mem_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub capabilities: Vec<String>,
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
            Constraint::Length(14),          // IO + OVERFLOW counters
            Constraint::Min(0),              // context: config | host
        ])
        .split(area);
    render_adapter_list(frame, rows[0], entries, cursor);
    render_status(frame, rows[1], &entries[cursor].metrics, &entries[cursor].id);
    render_body(frame, rows[2], &entries[cursor].metrics);
    render_context_row(frame, rows[3], &entries[cursor]);
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
        let ratio = if m.disk_capacity_bytes == 0 {
            0.0
        } else {
            (m.disk_used_bytes as f64 / m.disk_capacity_bytes as f64).clamp(0.0, 1.0)
        };
        let disk_pct = (ratio * 100.0) as u16;
        let (bar_color, _, _) = bar_style_for(ratio);
        // DISK column is a `┃━━━━━━━━━┃ NN%` bar + percent.
        // Bar color tracks the health-gate threshold the same
        // way the detail status panel does — green steady →
        // amber watch above 85% → red EMIT at/above 95%.
        let disk_cell = Cell::from(Line::from(vec![
            bar(disk_pct, 10, bar_color),
            Span::styled(format!(" {disk_pct:>3}%"), theme::dim()),
        ]));
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
            disk_cell,
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
            Constraint::Length(16), // disk bar + %
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

// ───────────────────────── context row ─────────────────────────
//
// Side-by-side panel below the IO/OVERFLOW counters. Left side
// is this adapter's config + advertised capabilities; right side
// is the host node's identity, resource snapshot, and capability
// set. Lets an operator read the "what is this adapter" answer
// without bouncing to the NODE page.

fn render_context_row(frame: &mut Frame<'_>, area: Rect, entry: &AdapterEntry) {
    if area.height < 4 {
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    render_adapter_config_panel(frame, cols[0], entry);
    render_host_node_panel(frame, cols[1], &entry.host);
}

fn render_adapter_config_panel(frame: &mut Frame<'_>, area: Rect, entry: &AdapterEntry) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DATAFORT", theme::green_hi()),
            Span::styled(format!("    {}", entry.id), theme::amber()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(ctx_kv("id", &entry.id, theme::text()));
    lines.push(ctx_kv(
        "capacity",
        &fmt_bytes(entry.metrics.disk_capacity_bytes),
        theme::text(),
    ));
    lines.push(ctx_kv(
        "overflow",
        if entry.overflow_enabled {
            if entry.metrics.overflow.active {
                "enabled · ACTIVE"
            } else {
                "enabled · idle"
            }
        } else {
            "off"
        },
        if entry.overflow_enabled {
            if entry.metrics.overflow.active {
                theme::amber()
            } else {
                theme::green()
            }
        } else {
            theme::dim()
        },
    ));
    lines.push(ctx_kv(
        "disk_ratio",
        &format!("{:.2}", entry.metrics.overflow.disk_ratio),
        theme::dim(),
    ));
    lines.push(Line::from(vec![Span::raw("")]));
    lines.push(Line::from(vec![Span::styled(
        "  advertises",
        theme::chrome(),
    )]));
    if entry.capabilities.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("      ", theme::chrome()),
            Span::styled("—", theme::chrome()),
        ]));
    } else {
        for cap in &entry.capabilities {
            lines.push(Line::from(vec![
                Span::styled("      ", theme::chrome()),
                Span::styled(cap.clone(), theme::text()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_host_node_panel(frame: &mut Frame<'_>, area: Rect, host: &HostNodeView) {
    let id_label = match host.label {
        Some(l) => format!("0x{:04x}.{}", host.id, l),
        None => format!("0x{:04x}", host.id),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NODE", theme::green_hi()),
            Span::styled(format!("    {}", id_label), theme::text()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(ctx_kv(
        "health",
        host.health.unwrap_or("—"),
        health_style(host.health),
    ));
    lines.push(ctx_kv(
        "cpu_1m",
        &host
            .cpu_load_1m
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".to_string()),
        cpu_style(host.cpu_load_1m),
    ));
    lines.push(bar_kv(
        "memory",
        host.mem_used_bytes,
        host.mem_total_bytes,
    ));
    lines.push(bar_kv("disk", host.disk_used_bytes, host.disk_total_bytes));
    lines.push(Line::from(vec![Span::raw("")]));
    lines.push(Line::from(vec![Span::styled(
        "  capabilities",
        theme::chrome(),
    )]));
    if host.capabilities.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("      ", theme::chrome()),
            Span::styled("—", theme::chrome()),
        ]));
    } else {
        for cap in &host.capabilities {
            lines.push(Line::from(vec![
                Span::styled("      ", theme::chrome()),
                Span::styled(cap.clone(), theme::text()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn ctx_kv(label: &str, value: &str, value_style: ratatui::style::Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<12}"), theme::chrome()),
        Span::styled(value.to_string(), value_style),
    ])
}

fn bar_kv(label: &str, used: Option<u64>, total: Option<u64>) -> Line<'static> {
    let (ratio, label_value) = match (used, total) {
        (Some(u), Some(t)) if t > 0 => {
            let r = (u as f64 / t as f64).clamp(0.0, 1.0);
            (Some(r), format!("{} / {}", fmt_bytes(u), fmt_bytes(t)))
        }
        _ => (None, "—".to_string()),
    };
    let mut spans = vec![Span::styled(format!("  {label:<12}"), theme::chrome())];
    match ratio {
        Some(r) => {
            let pct = (r * 100.0) as u16;
            let color = if r >= HEALTH_GATE_EMIT_THRESHOLD {
                theme::RED
            } else if r >= HEALTH_GATE_CLEAR_THRESHOLD {
                theme::AMBER
            } else {
                theme::GREEN_HI
            };
            spans.push(bar(pct, 12, color));
            spans.push(Span::styled(format!("  {pct:>3}%  "), theme::text()));
            spans.push(Span::styled(label_value, theme::dim()));
        }
        None => {
            spans.push(Span::styled(label_value, theme::chrome()));
        }
    }
    Line::from(spans)
}

fn health_style(s: Option<&'static str>) -> ratatui::style::Style {
    match s {
        Some("Healthy") => theme::green(),
        Some("Degraded") => theme::amber(),
        Some("Unreachable") => theme::red(),
        _ => theme::chrome(),
    }
}

fn cpu_style(load: Option<f64>) -> ratatui::style::Style {
    match load {
        Some(v) if v >= 2.0 => theme::red(),
        Some(v) if v >= 1.0 => theme::amber(),
        Some(_) => theme::green(),
        None => theme::chrome(),
    }
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
