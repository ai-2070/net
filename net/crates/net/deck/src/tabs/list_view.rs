use net_sdk::deck::{DaemonHealthSnapshot, DaemonLifecycleSnapshot, MeshOsSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{lineage, nodes, theme, widgets};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&MeshOsSnapshot>,
    cursor: usize,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    match snapshot {
        Some(s) if !s.peers.is_empty() => render_live_nodes_table(frame, rows[0], s, cursor),
        _ => render_empty_nodes_table(frame, rows[0]),
    }

    match snapshot {
        Some(s) if !s.daemons.is_empty() => render_live_daemons_table(frame, rows[1], s),
        _ => render_empty_daemons_table(frame, rows[1]),
    }
}

// ───────────────────────── empty-state panels ─────────────────────────

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
        "wire a proximity / health probe — or run with --features samples",
    );
}

fn render_empty_daemons_table(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMONS", theme::green_hi()),
            Span::styled("    0 registered", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no daemons registered yet",
        "register via the MeshOsDaemonSdk — or run with --features samples",
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
            Constraint::Length(8),  // DAEMONS
            Constraint::Length(10), // MAINT
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

// ───────────────────────── live render: daemons ─────────────────────────

fn render_live_daemons_table(frame: &mut Frame<'_>, area: Rect, snapshot: &MeshOsSnapshot) {
    let groups = lineage::group_daemons(&snapshot.daemons);
    let total: usize = groups.iter().map(|g| g.members.len()).sum();
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("DAEMONS", theme::green_hi()),
        Span::styled(
            format!("   {total} live · {} groups", groups.len()),
            theme::chrome(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim("DAEMON"),
        cell_dim("KIND"),
        cell_dim("LINEAGE"),
        cell_dim("NODE"),
        cell_dim("LIFE"),
        cell_dim("HEALTH"),
        cell_dim("SAT"),
        cell_dim("AGE"),
    ])
    .height(1);

    let mut table_rows: Vec<Row> = Vec::with_capacity(total);
    for group in &groups {
        for m in &group.members {
            let d = m.daemon;
            let tag = lineage::lineage_tag(m.role, group.kind);
            let lineage_style = match group.kind {
                lineage::GroupKind::Solo => theme::dim(),
                lineage::GroupKind::Replica => theme::green_hi(),
                lineage::GroupKind::Fork { .. } => theme::amber(),
                lineage::GroupKind::Standby => theme::cyan(),
            };
            let life_style = match d.lifecycle {
                DaemonLifecycleSnapshot::Running => theme::green(),
                DaemonLifecycleSnapshot::Starting | DaemonLifecycleSnapshot::Stopping => {
                    theme::amber()
                }
                DaemonLifecycleSnapshot::Stopped => theme::dim(),
                _ => theme::dim(),
            };
            let (health_style, health_text) = match d.health {
                Some(DaemonHealthSnapshot::Healthy) => (theme::green(), "Healthy"),
                Some(DaemonHealthSnapshot::Degraded { .. }) => (theme::amber(), "Degraded"),
                Some(DaemonHealthSnapshot::Unhealthy) => (theme::red(), "Unhealthy"),
                _ => (theme::chrome(), "—"),
            };
            let life_text = match d.lifecycle {
                DaemonLifecycleSnapshot::Running => "Running",
                DaemonLifecycleSnapshot::Starting => "Starting",
                DaemonLifecycleSnapshot::Stopping => "Stopping",
                DaemonLifecycleSnapshot::Stopped => "Stopped",
                _ => "?",
            };
            table_rows.push(Row::new(vec![
                Cell::from(Span::styled(format!("0x{:x}", m.id), theme::text())),
                Cell::from(Span::styled(group.display_name.clone(), theme::cyan())),
                Cell::from(Span::styled(tag, lineage_style)),
                Cell::from(Line::from(nodes::id_spans(&format!("0x{:x}", d.placement)))),
                Cell::from(Span::styled(life_text, life_style)),
                Cell::from(Span::styled(health_text, health_style)),
                Cell::from(Span::styled(format!("{:.2}", d.saturation), theme::text())),
                Cell::from(Span::styled(format_age(d.age_ms), theme::dim())),
            ]));
        }
    }

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(10), // DAEMON
            Constraint::Length(12), // KIND
            Constraint::Length(14), // LINEAGE
            Constraint::Length(18), // NODE: id.label
            Constraint::Length(9),  // LIFE
            Constraint::Length(10), // HEALTH
            Constraint::Length(6),  // SAT
            Constraint::Length(9),  // AGE
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn format_age(ms: u64) -> String {
    let s = ms / 1_000;
    let m = s / 60;
    let h = m / 60;
    if h > 0 {
        format!("{h}h {:02}m", m % 60)
    } else if m > 0 {
        format!("{m}m {:02}s", s % 60)
    } else {
        format!("{s}s")
    }
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}
