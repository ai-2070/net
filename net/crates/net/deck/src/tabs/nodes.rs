//! NODES tab — full-height table of every peer in the cluster.
//! Used to share its area with a DAEMONS panel; the daemons
//! surface now lives on its own tab so this view is nodes-only.

use net_sdk::deck::MeshOsSnapshot;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{nodes, theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>, cursor: usize) {
    match snapshot {
        Some(s) if !s.peers.is_empty() => render_live_nodes_table(frame, area, s, cursor),
        _ => render_empty_nodes_table(frame, area),
    }
}

// ───────────────────────── empty-state panel ─────────────────────────

fn render_empty_nodes_table(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NODES", theme::green_hi()),
            Span::styled("    0 peers", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no peers reported yet",
        "wire a proximity / health probe",
    );
}

// ───────────────────────── live render: nodes ─────────────────────────

fn render_live_nodes_table(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: &MeshOsSnapshot,
    cursor: usize,
) {
    use net_sdk::deck::{MaintenanceMirrorSnapshot, PeerHealthSnapshot};

    let total = snapshot.peers.len();
    let healthy = snapshot
        .peers
        .values()
        .filter(|p| matches!(p.health, Some(PeerHealthSnapshot::Healthy)))
        .count();
    let degraded = snapshot
        .peers
        .values()
        .filter(|p| matches!(p.health, Some(PeerHealthSnapshot::Degraded)))
        .count();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("NODES", theme::green_hi()),
        Span::styled(
            format!("    {total} live · {healthy} healthy · {degraded} degraded"),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("NODE"),
        cell_dim("HEALTH"),
        cell_dim("RTT"),
        cell_dim("CPU"),
        cell_dim("MEM"),
        cell_dim("DISK"),
        cell_dim("SAT"),
        cell_dim("DAEMONS"),
        cell_dim("MAINT"),
    ])
    .height(1);

    let mut table_rows: Vec<Row> = Vec::with_capacity(snapshot.peers.len());
    for (i, (peer_id, p)) in snapshot.peers.iter().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let id_spans = if is_cursor {
            nodes::id_spans_styled(&format!("0x{peer_id:x}"), theme::green_hi())
        } else {
            nodes::id_spans(&format!("0x{peer_id:x}"))
        };
        let (health_style, health_text) = match p.health {
            Some(PeerHealthSnapshot::Healthy) => (theme::green(), "Healthy"),
            Some(PeerHealthSnapshot::Degraded) => (theme::amber(), "Degraded"),
            Some(PeerHealthSnapshot::Unreachable) => (theme::red(), "Unreachable"),
            None => (theme::chrome(), "—"),
            _ => (theme::chrome(), "?"),
        };
        let rtt_text = match p.rtt_ms {
            Some(ms) => format!("{ms}ms"),
            None => "—".to_string(),
        };
        let cpu_text = match p.cpu_load_1m {
            Some(load) => format!("{load:.2}"),
            None => "—".to_string(),
        };
        let mem_text = match (p.mem_used_bytes, p.mem_total_bytes) {
            (Some(used), Some(total)) if total > 0 => {
                format!("{}%", (used * 100 / total).min(999))
            }
            _ => "—".to_string(),
        };
        let disk_text = match (p.disk_used_bytes, p.disk_total_bytes) {
            (Some(used), Some(total)) if total > 0 => {
                format!("{}%", (used * 100 / total).min(999))
            }
            _ => "—".to_string(),
        };
        // Saturation gets a color: green under 0.5, amber to
        // 0.8, red above. Matches the health-gate hysteresis
        // intuition used elsewhere.
        let (sat_text, sat_style) = match p.saturation_trend {
            Some(s) if s < 0.5 => (format!("{:.2}", s), theme::green()),
            Some(s) if s < 0.8 => (format!("{:.2}", s), theme::amber()),
            Some(s) => (format!("{:.2}", s), theme::red()),
            None => ("—".to_string(), theme::chrome()),
        };
        // Highlight mem/disk into amber/red when approaching
        // host pressure so the operator's eye catches them
        // before the saturation_trend tilts.
        let mem_style = pressure_style(p.mem_used_bytes, p.mem_total_bytes);
        let disk_style = pressure_style(p.disk_used_bytes, p.disk_total_bytes);
        let daemon_count = snapshot
            .daemons
            .values()
            .filter(|d| d.placement == *peer_id)
            .count();
        let maint_style;
        let maint_text = match p.maintenance {
            Some(MaintenanceMirrorSnapshot::Active) | None => {
                maint_style = theme::chrome();
                "—".to_string()
            }
            Some(MaintenanceMirrorSnapshot::EnteringMaintenance) => {
                maint_style = theme::cyan();
                "drain".to_string()
            }
            Some(MaintenanceMirrorSnapshot::Maintenance) => {
                maint_style = theme::cyan();
                "maint".to_string()
            }
            Some(MaintenanceMirrorSnapshot::ExitingMaintenance) => {
                maint_style = theme::cyan();
                "exit".to_string()
            }
            Some(MaintenanceMirrorSnapshot::DrainFailed) => {
                maint_style = theme::red();
                "failed".to_string()
            }
            Some(MaintenanceMirrorSnapshot::Recovery) => {
                maint_style = theme::cyan();
                "recovery".to_string()
            }
            _ => {
                maint_style = theme::chrome();
                "?".to_string()
            }
        };
        table_rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Line::from(id_spans)),
            Cell::from(Span::styled(health_text, health_style)),
            Cell::from(Span::styled(rtt_text, theme::text())),
            Cell::from(Span::styled(cpu_text, theme::text())),
            Cell::from(Span::styled(mem_text, mem_style)),
            Cell::from(Span::styled(disk_text, disk_style)),
            Cell::from(Span::styled(sat_text, sat_style)),
            Cell::from(Span::styled(format!("{daemon_count:>3}"), theme::text())),
            Cell::from(Span::styled(maint_text, maint_style)),
        ]));
    }

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(2),  // cursor marker
            Constraint::Length(18), // NODE: id.label
            Constraint::Length(11), // HEALTH
            Constraint::Length(7),  // RTT
            Constraint::Length(5),  // CPU
            Constraint::Length(5),  // MEM
            Constraint::Length(5),  // DISK
            Constraint::Length(5),  // SAT
            Constraint::Length(8),  // DAEMONS
            Constraint::Length(10), // MAINT
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

/// Color a percentage-style pressure value (used/total) green
/// when comfortable, amber under load, red at capacity. Same
/// thresholds the dataforts health gate uses (85% / 95%).
fn pressure_style(used: Option<u64>, total: Option<u64>) -> ratatui::style::Style {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => {
            let ratio = u as f64 / t as f64;
            if ratio >= 0.95 {
                theme::red()
            } else if ratio >= 0.85 {
                theme::amber()
            } else {
                theme::text()
            }
        }
        _ => theme::chrome(),
    }
}
