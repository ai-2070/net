//! DATAFORTS tab — one row per node acting as a blob storage
//! tier. A datafort is any peer advertising
//! `dataforts.blob.storage`, plus the local node. The cursored
//! datafort drives the detail body below: aggregate disk +
//! health-gate, IO / OVERFLOW counters (local only — remote
//! adapter-level state isn't probed across the wire), and a
//! per-datafort breakdown of adapters + node info.
//!
//! The deck reads remote dataforts straight from `snapshot.peers`
//! (capability tags, disk fields, health). No separate
//! adapter-level probe runs across the cluster — peer-side
//! decisions like admission / overflow read the node's own local
//! view, so the deck doesn't need per-remote-adapter telemetry to
//! make routing or health calls.
//!
//! Layout:
//! - dataforts list (height = N rows + header, capped)
//! - status bar: aggregate disk + health-gate verdict
//! - IO + OVERFLOW counters (local; placeholder for remote)
//! - context row: adapters (local) | node info

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

/// One row of the dataforts list — either the local node (with
/// full adapter-level detail) or a remote peer observed to
/// advertise `dataforts.blob.storage`.
#[derive(Clone)]
pub struct DatafortEntry {
    pub id: u64,
    pub label: Option<&'static str>,
    pub is_local: bool,
    pub health: Option<&'static str>,
    pub cpu_load_1m: Option<f64>,
    pub mem_used_bytes: Option<u64>,
    pub mem_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub capabilities: Vec<String>,
    /// Per-adapter rows. Populated only for the local datafort.
    pub adapters: Vec<AdapterEntry>,
}

/// One adapter on the local datafort. Metrics are snapshotted
/// at frame boundary so the list + detail agree.
#[derive(Clone)]
pub struct AdapterEntry {
    pub id: String,
    pub metrics: BlobMetricsSnapshot,
    pub overflow_enabled: bool,
}

pub fn render(frame: &mut Frame<'_>, area: Rect, entries: &[DatafortEntry], cursor: usize) {
    if entries.is_empty() {
        render_empty(frame, area);
        return;
    }
    let cursor = cursor.min(entries.len().saturating_sub(1));
    let visible_rows = entries.len().min(10);
    let list_height = (visible_rows as u16) + 3;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(list_height),
            Constraint::Length(3),  // aggregate status bar
            Constraint::Length(14), // IO + OVERFLOW
            Constraint::Min(0),     // context (adapters | node)
        ])
        .split(area);
    render_datafort_list(frame, rows[0], entries, cursor);
    let cur = &entries[cursor];
    let agg = aggregate_for(cur);
    render_status(frame, rows[1], cur, &agg);
    render_body(frame, rows[2], cur, &agg);
    render_context_row(frame, rows[3], cur);
}

// ───────────────────────── top list ─────────────────────────

fn render_datafort_list(
    frame: &mut Frame<'_>,
    area: Rect,
    entries: &[DatafortEntry],
    cursor: usize,
) {
    let total = entries.len();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("DATAFORTS", theme::green_hi()),
        Span::styled(format!("    {total} reachable"), theme::chrome()),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line);
    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("NODE"),
        cell_dim("ROLE"),
        cell_dim("HEALTH"),
        cell_dim("DISK"),
        cell_dim("ADAPT"),
        cell_dim("OVERFLOW"),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let id_style = if is_cursor { theme::green_hi() } else { theme::text() };

        let id_label = format_id_label(e.id, e.label);
        let role_text = if e.is_local { "local" } else { "remote" };
        let role_style = if e.is_local { theme::cyan() } else { theme::dim() };

        let (health_text, health_style) = health_chip(e.health);
        let (ratio, disk_pct) = match (e.disk_used_bytes, e.disk_total_bytes) {
            (Some(u), Some(t)) if t > 0 => {
                let r = (u as f64 / t as f64).clamp(0.0, 1.0);
                (r, (r * 100.0) as u16)
            }
            _ => (0.0, 0),
        };
        let (bar_color, _, _) = bar_style_for(ratio);
        let disk_cell = Cell::from(Line::from(vec![
            bar(disk_pct, 10, bar_color),
            Span::styled(format!(" {disk_pct:>3}%"), theme::dim()),
        ]));

        let adapt_text = if e.is_local {
            format!("{:>3}", e.adapters.len())
        } else {
            "  —".to_string()
        };
        let overflow_text = overflow_label(e);
        let overflow_style = if overflow_text == "ACTIVE" {
            theme::amber()
        } else if overflow_text == "armed" {
            theme::green()
        } else {
            theme::dim()
        };

        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(id_label, id_style)),
            Cell::from(Span::styled(role_text, role_style)),
            Cell::from(Span::styled(health_text, health_style)),
            disk_cell,
            Cell::from(Span::styled(adapt_text, theme::dim())),
            Cell::from(Span::styled(overflow_text.to_string(), overflow_style)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(20), // node id.label
            Constraint::Length(7),  // role
            Constraint::Length(11), // health
            Constraint::Length(16), // disk bar + %
            Constraint::Length(5),  // adapter count
            Constraint::Min(8),     // overflow
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn overflow_label(e: &DatafortEntry) -> &'static str {
    if e.is_local {
        if e.adapters.iter().any(|a| a.metrics.overflow.active) {
            "ACTIVE"
        } else if e.adapters.iter().any(|a| a.overflow_enabled) {
            "armed"
        } else {
            "off"
        }
    } else if e.capabilities.iter().any(|c| c == "dataforts.blob.overflow") {
        "armed"
    } else {
        "off"
    }
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DATAFORTS", theme::green_hi()),
            Span::styled("    no dataforts reachable", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no datafort reachable from this deck",
        "wire a MeshBlobAdapter locally or join a cluster with a storage peer",
    );
}

// ───────────────────────── status bar ─────────────────────────

fn render_status(
    frame: &mut Frame<'_>,
    area: Rect,
    cur: &DatafortEntry,
    agg: &AggregateView,
) {
    let title_id = format_id_label(cur.id, cur.label);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DATAFORT", theme::green_hi()),
            Span::styled(format!("    {title_id}"), theme::amber()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let used = agg.disk_used;
    let cap = agg.disk_capacity;
    let ratio = if cap == 0 { 0.0 } else { used as f64 / cap as f64 };
    let pct = (ratio * 100.0) as u16;
    let pct_clamped = pct.min(100);
    let (bar_color, bar_label, bar_style) = bar_style_for(ratio);
    let (gate_text, gate_style) = gate_chip(used, cap);

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

fn gate_chip(used: u64, cap: u64) -> (&'static str, ratatui::style::Style) {
    let gate = evaluate_health_gate(used, cap, false);
    match gate {
        HealthGateAction::Emit => ("UNHEALTHY", theme::red()),
        HealthGateAction::Clear | HealthGateAction::Unchanged => ("healthy", theme::green()),
    }
}

// ───────────────────────── body (IO / OVERFLOW) ─────────────────────────

fn render_body(
    frame: &mut Frame<'_>,
    area: Rect,
    cur: &DatafortEntry,
    agg: &AggregateView,
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    if cur.is_local {
        render_io_panel(frame, cols[0], agg);
        render_overflow_panel(frame, cols[1], agg);
    } else {
        render_remote_panel(
            frame,
            cols[0],
            "STORE / FETCH / GC",
            "adapter-level counters live on the host node",
        );
        render_remote_panel(
            frame,
            cols[1],
            "OVERFLOW",
            "remote dataforts surface overflow only via the cap advertisement",
        );
    }
}

fn render_io_panel(frame: &mut Frame<'_>, area: Rect, agg: &AggregateView) {
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
        kv("blobs_stored_total", agg.blobs_stored),
        kv("blobs_fetched_total", agg.blobs_fetched),
        kv_bytes("bytes_stored_total", agg.bytes_stored),
        kv("gc_swept_total", agg.gc_swept),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_overflow_panel(frame: &mut Frame<'_>, area: Rect, agg: &AggregateView) {
    let (active_text, active_style) = if agg.overflow_active {
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
    let o = &agg.overflow;
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

fn render_remote_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    hint: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled(title.to_string(), theme::green_hi()),
            Span::styled("    remote", theme::dim()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            format!("  {hint}"),
            theme::chrome(),
        )]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────── context row ─────────────────────────

fn render_context_row(frame: &mut Frame<'_>, area: Rect, cur: &DatafortEntry) {
    if area.height < 4 {
        return;
    }
    if cur.is_local {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_adapters_panel(frame, cols[0], cur);
        render_node_panel(frame, cols[1], cur);
    } else {
        render_node_panel(frame, area, cur);
    }
}

fn render_adapters_panel(frame: &mut Frame<'_>, area: Rect, cur: &DatafortEntry) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("ADAPTERS", theme::green_hi()),
            Span::styled(
                format!("    {} on this datafort", cur.adapters.len()),
                theme::chrome(),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    if cur.adapters.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  no adapters",
            theme::chrome(),
        )]));
    } else {
        for a in &cur.adapters {
            let m = &a.metrics;
            let ratio = if m.disk_capacity_bytes == 0 {
                0.0
            } else {
                (m.disk_used_bytes as f64 / m.disk_capacity_bytes as f64).clamp(0.0, 1.0)
            };
            let pct = (ratio * 100.0) as u16;
            let (bar_color, _, _) = bar_style_for(ratio);
            let overflow_chip = if m.overflow.active {
                Span::styled("  ACTIVE", theme::amber())
            } else if a.overflow_enabled {
                Span::styled("  armed", theme::green())
            } else {
                Span::styled("  off", theme::dim())
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14}", a.id), theme::text()),
                bar(pct, 10, bar_color),
                Span::styled(format!(" {pct:>3}%  "), theme::dim()),
                Span::styled(
                    format!("{} / {}", fmt_bytes(m.disk_used_bytes), fmt_bytes(m.disk_capacity_bytes)),
                    theme::dim(),
                ),
                overflow_chip,
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_node_panel(frame: &mut Frame<'_>, area: Rect, cur: &DatafortEntry) {
    let id_label = format_id_label(cur.id, cur.label);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NODE", theme::green_hi()),
            Span::styled(format!("    {}", id_label), theme::text()),
            Span::styled(
                if cur.is_local { "    local" } else { "    remote" },
                if cur.is_local { theme::cyan() } else { theme::dim() },
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let (health_text, health_style) = health_chip(cur.health);
    lines.push(ctx_kv("health", health_text, health_style));
    lines.push(ctx_kv(
        "cpu_1m",
        &cur.cpu_load_1m
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".to_string()),
        cpu_style(cur.cpu_load_1m),
    ));
    lines.push(bar_kv("memory", cur.mem_used_bytes, cur.mem_total_bytes));
    lines.push(bar_kv("disk", cur.disk_used_bytes, cur.disk_total_bytes));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  capabilities",
        theme::chrome(),
    )]));
    if cur.capabilities.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("      ", theme::chrome()),
            Span::styled("—", theme::chrome()),
        ]));
    } else {
        for cap in &cur.capabilities {
            lines.push(Line::from(vec![
                Span::styled("      ", theme::chrome()),
                Span::styled(cap.clone(), theme::text()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────── aggregation ─────────────────────────

/// Aggregate view across the cursored datafort's adapters (local)
/// or its peer-level fields (remote). Drives the status bar +
/// IO/OVERFLOW panels.
struct AggregateView {
    disk_used: u64,
    disk_capacity: u64,
    blobs_stored: u64,
    blobs_fetched: u64,
    bytes_stored: u64,
    gc_swept: u64,
    overflow_active: bool,
    overflow: net_sdk::dataforts::OverflowMetricsSnapshot,
}

fn aggregate_for(cur: &DatafortEntry) -> AggregateView {
    if cur.is_local {
        let mut v = AggregateView {
            disk_used: 0,
            disk_capacity: 0,
            blobs_stored: 0,
            blobs_fetched: 0,
            bytes_stored: 0,
            gc_swept: 0,
            overflow_active: false,
            overflow: Default::default(),
        };
        for a in &cur.adapters {
            let m = &a.metrics;
            v.disk_used += m.disk_used_bytes;
            v.disk_capacity += m.disk_capacity_bytes;
            v.blobs_stored += m.blobs_stored_total;
            v.blobs_fetched += m.blobs_fetched_total;
            v.bytes_stored += m.bytes_stored_total;
            v.gc_swept += m.gc_swept_total;
            v.overflow_active |= m.overflow.active;
            sum_overflow(&mut v.overflow, &m.overflow);
        }
        v
    } else {
        AggregateView {
            disk_used: cur.disk_used_bytes.unwrap_or(0),
            disk_capacity: cur.disk_total_bytes.unwrap_or(0),
            blobs_stored: 0,
            blobs_fetched: 0,
            bytes_stored: 0,
            gc_swept: 0,
            overflow_active: false,
            overflow: Default::default(),
        }
    }
}

fn sum_overflow(
    acc: &mut net_sdk::dataforts::OverflowMetricsSnapshot,
    o: &net_sdk::dataforts::OverflowMetricsSnapshot,
) {
    acc.pushes_admitted_total += o.pushes_admitted_total;
    acc.push_errors_total += o.push_errors_total;
    acc.pushed_bytes_total += o.pushed_bytes_total;
    acc.rejected_no_target_total += o.rejected_no_target_total;
    acc.rejected_no_storage_cap_total += o.rejected_no_storage_cap_total;
    acc.rejected_not_participating_total += o.rejected_not_participating_total;
    acc.rejected_sender_not_overflowing_total += o.rejected_sender_not_overflowing_total;
    acc.rejected_unhealthy_total += o.rejected_unhealthy_total;
    acc.rejected_scope_mismatch_total += o.rejected_scope_mismatch_total;
    acc.rejected_insufficient_disk_total += o.rejected_insufficient_disk_total;
    acc.high_water_triggered_total += o.high_water_triggered_total;
    acc.low_water_cleared_total += o.low_water_cleared_total;
}

// ───────────────────────── helpers ─────────────────────────

fn format_id_label(id: u64, label: Option<&'static str>) -> String {
    match label {
        Some(l) => format!("0x{id:04x}.{l}"),
        None => format!("0x{id:04x}"),
    }
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
        None => spans.push(Span::styled(label_value, theme::chrome())),
    }
    Line::from(spans)
}

fn health_chip(s: Option<&'static str>) -> (&'static str, ratatui::style::Style) {
    match s {
        Some("Healthy") => ("Healthy", theme::green()),
        Some("Degraded") => ("Degraded", theme::amber()),
        Some("Unreachable") => ("Unreachable", theme::red()),
        _ => ("—", theme::chrome()),
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
