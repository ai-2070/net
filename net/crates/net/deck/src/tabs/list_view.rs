use net_sdk::deck::{DaemonHealthSnapshot, DaemonLifecycleSnapshot, MeshOsSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{lineage, nodes, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    // Live nodes table when snapshot has peers; fixture
    // table otherwise (no cluster, fresh runtime, or no
    // probes installed).
    let has_live_peers = snapshot.map(|s| !s.peers.is_empty()).unwrap_or(false);
    if has_live_peers {
        render_live_nodes_table(frame, rows[0], snapshot.unwrap());
    } else {
        render_nodes_table(frame, rows[0]);
    }

    let has_live_daemons = snapshot.map(|s| !s.daemons.is_empty()).unwrap_or(false);
    if has_live_daemons {
        render_live_daemons_table(frame, rows[1], snapshot.unwrap());
    } else {
        render_daemons_table(frame, rows[1]);
    }
}

fn render_live_nodes_table(frame: &mut Frame<'_>, area: Rect, snapshot: &MeshOsSnapshot) {
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
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("NODES", theme::green_hi()),
        Span::styled(
            format!(
                "    {total} live · {healthy} healthy · {degraded} degraded"
            ),
            theme::chrome(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim("NODE"),
        cell_dim("HEALTH"),
        cell_dim("RTT"),
        cell_dim("DAEMONS"),
        cell_dim("MAINT"),
    ])
    .height(1);

    let mut table_rows: Vec<Row> = Vec::with_capacity(snapshot.peers.len());
    for (peer_id, p) in &snapshot.peers {
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
        // Count daemons on this node.
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
            Cell::from(Line::from(nodes::id_spans(&format!("0x{peer_id:x}")))),
            Cell::from(Span::styled(health_text, health_style)),
            Cell::from(Span::styled(rtt_text, theme::text())),
            Cell::from(Span::styled(format!("{daemon_count:>3}"), theme::text())),
            Cell::from(Span::styled(maint_text, maint_style)),
        ]));
    }

    let table = Table::new(
        table_rows,
        [
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

fn render_nodes_table(frame: &mut Frame<'_>, area: Rect) {
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("NODES", theme::green_hi()),
        Span::styled("    17 total   14 healthy   2 degraded   1 maintenance", theme::chrome()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line);

    let header = Row::new(vec![
        cell_dim("NODE"),
        cell_dim("KIND"),
        cell_dim("HEALTH"),
        cell_dim("RTT.P50"),
        cell_dim("SAT"),
        cell_dim("DAEMONS"),
        cell_dim("MAINT"),
    ])
    .height(1);

    // Each row pulls its label from the canonical `nodes` fixture
    // and renders the NODE column as `id.label` (id in text, dot +
    // label in dim chrome). No standalone LABEL column.
    let rows_data: &[(&str, &str, &str, &str, &str, &str, &str)] = &[
        ("0xa96f", "compute",  "Healthy",  "  41µs", "0.42", "  3", "—"),
        ("0xe9b8", "compute",  "Healthy",  "  39µs", "0.51", "  4", "—"),
        ("0xe685", "region",   "Healthy",  "  12µs", "0.18", "  1", "—"),
        ("0xd4ff", "datafort", "Healthy",  "  44µs", "0.66", "  2", "—"),
        ("0x3599", "datafort", "Healthy",  "  47µs", "0.71", "  2", "—"),
        ("0x372b", "compute",  "Healthy",  "  88µs", "0.33", "  6", "—"),
        ("0xeba8", "compute",  "Degraded", " 244µs", "0.91", "  9", "—"),
        ("0x82ee", "compute",  "Healthy",  "  92µs", "0.40", "  3", "—"),
        ("0xbdda", "compute",  "Healthy",  "  85µs", "0.55", "  5", "—"),
        ("0x6dfb", "region",   "Healthy",  "  31µs", "0.22", "  2", "—"),
        ("0x3c81", "compute",  "Maint.",   "  —   ", "0.00", "  0", "drain"),
        ("0xe068", "compute",  "Healthy",  " 162µs", "0.48", "  4", "—"),
        ("0xbf44", "region",   "Healthy",  "  29µs", "0.20", "  1", "—"),
        ("0xf206", "datafort", "Healthy",  " 167µs", "0.62", "  2", "—"),
        ("0xf83d", "compute",  "Healthy",  " 159µs", "0.39", "  3", "—"),
        ("0x6808", "region",   "Degraded", " 451µs", "0.88", "  2", "—"),
        ("0x0fc2", "device",   "Healthy",  "  —   ", "—   ", "  0", "—"),
    ];

    let table_rows: Vec<Row> = rows_data
        .iter()
        .map(|(id, kind, health, rtt, sat, daemons, maint)| {
            let health_style = match *health {
                "Healthy" => theme::green(),
                "Degraded" => theme::amber(),
                "Maint." => theme::cyan(),
                _ => theme::red(),
            };
            let maint_style = if *maint == "—" { theme::chrome() } else { theme::cyan() };
            Row::new(vec![
                Cell::from(Line::from(nodes::id_spans(id))),
                Cell::from(Span::styled(*kind, theme::dim())),
                Cell::from(Span::styled(*health, health_style)),
                Cell::from(Span::styled(*rtt, theme::text())),
                Cell::from(Span::styled(*sat, theme::text())),
                Cell::from(Span::styled(*daemons, theme::text())),
                Cell::from(Span::styled(*maint, maint_style)),
            ])
        })
        .collect();

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(18), // NODE: id.label (e.g. 0xa96f.eu-west-3)
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn render_daemons_table(frame: &mut Frame<'_>, area: Rect) {
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("DAEMONS", theme::green_hi()),
        Span::styled(
            "    52 total · 4 groups · 2 replica · 1 fork · 1 standby",
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
        cell_dim("RST"),
        cell_dim("AGE"),
    ])
    .height(1);

    // ── lineage tag → (label, color) ────────────────────────────────────
    // SOLO            standalone daemon, no group
    // REP m[i]        ReplicaGroup member, index i
    // FORK f[i]@n     ForkGroup fork i at parent seq n
    // STBY active     StandbyGroup active member
    // STBY warm       StandbyGroup standby (warm)
    let rows_data: &[(
        &str, &str, &str, &str, &str, &str, &str, &str, &str,
    )] = &[
        ("0x69",  "mikoshi",    "SOLO",          "0xbf44", "Running",   "Healthy",  "0.31", "0",  "2h 14m"),
        ("0xc2",  "gravity",    "REP  m[0]",     "0x6dfb", "Running",   "Healthy",  "0.42", "0",  "5h 03m"),
        ("0xf1",  "gravity",    "REP  m[1]",     "0x372b", "Running",   "Healthy",  "0.38", "0",  "5h 03m"),
        ("0xa9",  "gravity",    "REP  m[2]",     "0xa96f", "Running",   "Healthy",  "0.55", "0",  "5h 03m"),
        ("0xaa1", "scheduler",  "SOLO",          "0xa96f", "Running",   "Healthy",  "0.12", "0",  "11h 27m"),
        ("0xab3", "drift_corr", "FORK f[0]@42",  "0xeba8", "Running",   "Degraded", "0.91", "2",  "37m"),
        ("0xab4", "drift_corr", "FORK f[1]@42",  "0xbdda", "Running",   "Healthy",  "0.44", "0",  "37m"),
        ("0xab5", "drift_corr", "FORK f[2]@42",  "0x82ee", "Running",   "Healthy",  "0.39", "0",  "37m"),
        ("0xae9", "anti_entr",  "STBY active",   "0xd4ff", "Running",   "Healthy",  "0.55", "0",  "2d 04h"),
        ("0xb02", "anti_entr",  "STBY warm",     "0x3599", "Running",   "Healthy",  "0.04", "0",  "2d 04h"),
        ("0xd11", "anti_entr",  "STBY warm",     "0x82ee", "Running",   "Healthy",  "0.05", "0",  "2d 04h"),
        ("0xc09", "blob_mover", "SOLO",          "0x3599", "Running",   "Healthy",  "0.66", "0",  "8h 21m"),
        ("0xe7b", "fork_coord", "SOLO",          "0xbdda", "Crash-loop","Unhealthy","0.00", "12", "—"),
    ];

    let table_rows: Vec<Row> = rows_data
        .iter()
        .map(|(d, kind, lineage, node, life, health, sat, restarts, age)| {
            let life_style = match *life {
                "Running" => theme::green(),
                "Backoff" => theme::amber(),
                "Crash-loop" => theme::red(),
                _ => theme::dim(),
            };
            let health_style = match *health {
                "Healthy" => theme::green(),
                "Degraded" => theme::amber(),
                "Unhealthy" => theme::red(),
                _ => theme::dim(),
            };
            let restart_style = if *restarts == "0" { theme::dim() } else { theme::amber() };
            let lineage_style = if lineage.starts_with("REP") {
                theme::green_hi()
            } else if lineage.starts_with("FORK") {
                theme::amber()
            } else if lineage.starts_with("STBY") {
                theme::cyan()
            } else {
                theme::dim()
            };
            Row::new(vec![
                Cell::from(Span::styled(*d, theme::text())),
                Cell::from(Span::styled(*kind, theme::cyan())),
                Cell::from(Span::styled(*lineage, lineage_style)),
                Cell::from(Line::from(nodes::id_spans(node))),
                Cell::from(Span::styled(*life, life_style)),
                Cell::from(Span::styled(*health, health_style)),
                Cell::from(Span::styled(*sat, theme::text())),
                Cell::from(Span::styled(*restarts, restart_style)),
                Cell::from(Span::styled(*age, theme::dim())),
            ])
        })
        .collect();

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(6),  // DAEMON
            Constraint::Length(12), // KIND
            Constraint::Length(14), // LINEAGE
            Constraint::Length(18), // NODE: id.label
            Constraint::Length(11), // LIFE
            Constraint::Length(10), // HEALTH
            Constraint::Length(6),  // SAT
            Constraint::Length(4),  // RST
            Constraint::Length(9),  // AGE
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
