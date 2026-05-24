//! NODES tab — full-height table of every peer in the cluster.
//! Used to share its area with a DAEMONS panel; the daemons
//! surface now lives on its own tab so this view is nodes-only.

use net_sdk::deck::MeshOsSnapshot;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::{nodes, theme, widgets};

/// Local-node row included in the NODES table alongside the
/// snapshot's remote peers. The substrate's `snapshot.peers`
/// map only contains *remote* peers — local probes never
/// sample self — so the App synthesizes this row to keep the
/// operator's own node visible alongside everyone else.
pub struct LocalNodeRow<'a> {
    pub id: net_sdk::deck::NodeId,
    pub peer: &'a net_sdk::deck::PeerSnapshot,
    pub local_maintenance: &'a net_sdk::deck::MaintenanceStateSnapshot,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&MeshOsSnapshot>,
    cursor: usize,
    local: Option<LocalNodeRow<'_>>,
) {
    let has_peers = snapshot.map(|s| !s.peers.is_empty()).unwrap_or(false);
    let has_local = local.is_some();
    if has_peers || has_local {
        if let Some(s) = snapshot {
            render_live_nodes_table(frame, area, s, cursor, local);
        }
    } else {
        render_empty_nodes_table(frame, area);
    }
}

/// Render the NODES-shaped table against a caller-supplied
/// iterator of `(node_id, peer)` rows. Used by both the
/// top-level NODES tab and the SUBNET focus page's MEMBERS
/// panel so each shows the same columns / styling / colors.
///
/// `title_spans` is rendered as the block title — caller picks
/// the section name + counters. `snapshot` is still passed in
/// so the daemon-count column can roll up against every
/// daemon's `placement` field. `local_id` + `local_maintenance`
/// let the helper render `self` in the RTT column + map the
/// local node's richer `MaintenanceStateSnapshot` to the
/// shared `MaintenanceMirrorSnapshot` chip vocabulary.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_nodes_view(
    frame: &mut Frame<'_>,
    area: Rect,
    title_spans: Vec<Span<'static>>,
    nodes_iter: &[(u64, &net_sdk::deck::PeerSnapshot)],
    snapshot: &MeshOsSnapshot,
    cursor: usize,
    local_id: Option<u64>,
    local_maintenance_mirror: Option<net_sdk::deck::MaintenanceMirrorSnapshot>,
) {
    use net_sdk::deck::{MaintenanceMirrorSnapshot, PeerHealthSnapshot};

    let total = nodes_iter.len();
    let body_h = (area.height as usize).saturating_sub(2).saturating_sub(1);
    let effective_cursor = cursor.min(total.saturating_sub(1));
    let (start, end, hidden_above, hidden_below) =
        super::scroll_window(total, body_h, effective_cursor);

    let mut title_spans = title_spans;
    if hidden_above > 0 {
        title_spans.push(Span::styled(
            format!("    ▲ {hidden_above} more"),
            theme::dim(),
        ));
    }
    if hidden_below > 0 {
        title_spans.push(Span::styled(
            format!("    ▼ {hidden_below} more"),
            theme::dim(),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(title_spans))
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

    // Pre-aggregate daemon → peer placement counts once so the
    // DAEMONS column isn't O(peers × daemons) per frame.
    let mut daemon_counts: std::collections::HashMap<u64, usize> =
        std::collections::HashMap::with_capacity(total);
    for d in snapshot.daemons.values() {
        *daemon_counts.entry(d.placement).or_insert(0) += 1;
    }

    let mut table_rows: Vec<Row> = Vec::with_capacity(end.saturating_sub(start));
    for (offset, (peer_id, p)) in nodes_iter[start..end].iter().enumerate() {
        let i = start + offset;
        let peer_id = *peer_id;
        let is_local_row = Some(peer_id) == local_id;
        let is_cursor = i == effective_cursor;
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
        let rtt_text = if is_local_row {
            "self".to_string()
        } else {
            match p.rtt_ms {
                Some(ms) => format!("{ms}ms"),
                None => "—".to_string(),
            }
        };
        let cpu_text = match p.cpu_load_1m {
            Some(load) => format!("{load:.2}"),
            None => "—".to_string(),
        };
        let mem_text = match (p.mem_used_bytes, p.mem_total_bytes) {
            (Some(used), Some(total)) if total > 0 => {
                format!("{}%", percent_u64(used, total))
            }
            _ => "—".to_string(),
        };
        let disk_text = match (p.disk_used_bytes, p.disk_total_bytes) {
            (Some(used), Some(total)) if total > 0 => {
                format!("{}%", percent_u64(used, total))
            }
            _ => "—".to_string(),
        };
        let (sat_text, sat_style) = match p.saturation_trend {
            Some(s) if s < 0.5 => (format!("{:.2}", s), theme::green()),
            Some(s) if s < 0.8 => (format!("{:.2}", s), theme::amber()),
            Some(s) => (format!("{:.2}", s), theme::red()),
            None => ("—".to_string(), theme::chrome()),
        };
        let mem_style = pressure_style(p.mem_used_bytes, p.mem_total_bytes);
        let disk_style = pressure_style(p.disk_used_bytes, p.disk_total_bytes);
        let daemon_count = daemon_counts.get(&peer_id).copied().unwrap_or(0);
        let maintenance = if is_local_row {
            local_maintenance_mirror
        } else {
            p.maintenance
        };
        let maint_style;
        let maint_text = match maintenance {
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
            Constraint::Length(22), // NODE
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
    let selected = effective_cursor
        .checked_sub(start)
        .filter(|s| start + *s < end);
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

/// Map the local node's `MaintenanceStateSnapshot` (state machine
/// with timestamps) onto the `MaintenanceMirrorSnapshot` chip
/// vocabulary used by the peer row. Public so the SUBNET focus
/// page builds the same chip when the local node belongs to the
/// drilled subnet.
pub(crate) fn local_maintenance_mirror(
    local_maintenance: &net_sdk::deck::MaintenanceStateSnapshot,
) -> net_sdk::deck::MaintenanceMirrorSnapshot {
    use net_sdk::deck::{MaintenanceMirrorSnapshot, MaintenanceStateSnapshot};
    match local_maintenance {
        MaintenanceStateSnapshot::Active => MaintenanceMirrorSnapshot::Active,
        MaintenanceStateSnapshot::EnteringMaintenance { .. } => {
            MaintenanceMirrorSnapshot::EnteringMaintenance
        }
        MaintenanceStateSnapshot::Maintenance { .. } => MaintenanceMirrorSnapshot::Maintenance,
        MaintenanceStateSnapshot::ExitingMaintenance { .. } => {
            MaintenanceMirrorSnapshot::ExitingMaintenance
        }
        MaintenanceStateSnapshot::DrainFailed { .. } => MaintenanceMirrorSnapshot::DrainFailed,
        MaintenanceStateSnapshot::Recovery { .. } => MaintenanceMirrorSnapshot::Recovery,
        _ => MaintenanceMirrorSnapshot::Active,
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
    local: Option<LocalNodeRow<'_>>,
) {
    use net_sdk::deck::PeerHealthSnapshot;

    // Walk a single iterator that prepends the local node so
    // every cursor-aware downstream (`cursored_node` in
    // `app.rs`, clamp / step / cursor_to_bottom) treats the
    // table as `[local, ...peers]` consistently.
    let local_peer = local.as_ref().map(|r| (r.id, r.peer));
    let nodes_iter: Vec<(u64, &net_sdk::deck::PeerSnapshot)> = local_peer
        .into_iter()
        .chain(snapshot.peers.iter().map(|(id, p)| (*id, p)))
        .collect();
    let total = nodes_iter.len();
    let healthy = nodes_iter
        .iter()
        .filter(|(_, p)| matches!(p.health, Some(PeerHealthSnapshot::Healthy)))
        .count();
    let degraded = nodes_iter
        .iter()
        .filter(|(_, p)| matches!(p.health, Some(PeerHealthSnapshot::Degraded)))
        .count();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("NODES", theme::green_hi()),
        Span::styled(
            format!("    {total} live · {healthy} healthy · {degraded} degraded"),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ];
    let local_id = local.as_ref().map(|r| r.id);
    let local_maintenance_mirror = local
        .as_ref()
        .map(|r| local_maintenance_mirror(r.local_maintenance));
    render_nodes_view(
        frame,
        area,
        title_spans,
        &nodes_iter,
        snapshot,
        cursor,
        local_id,
        local_maintenance_mirror,
    );
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

/// Color a percentage-style pressure value (used/total) green
/// Integer-percent of `used / total`, computed in u128 so a
/// large `used` (close to `u64::MAX / 100`) doesn't silently
/// overflow under release-mode wrapping arithmetic. Capped at
/// 999 to leave room for the rare over-100% case (drift between
/// usage reporting and the cap) without distorting the column.
fn percent_u64(used: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let pct = (used as u128) * 100 / (total as u128);
    pct.min(999) as u64
}

/// when comfortable, amber under load, red at capacity. Same
/// thresholds the dataforts health gate uses (85% / 95%).
fn pressure_style(used: Option<u64>, total: Option<u64>) -> ratatui::style::Style {
    use net_sdk::dataforts::{HEALTH_GATE_CLEAR_THRESHOLD, HEALTH_GATE_EMIT_THRESHOLD};
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => {
            let ratio = u as f64 / t as f64;
            if ratio >= HEALTH_GATE_EMIT_THRESHOLD {
                theme::red()
            } else if ratio >= HEALTH_GATE_CLEAR_THRESHOLD {
                theme::amber()
            } else {
                theme::text()
            }
        }
        _ => theme::chrome(),
    }
}
