//! DAEMONS tab — flat table of every live daemon in the cluster.
//! Extracted from the bottom panel of the old LIST tab so daemons
//! get their own tab with cursor + Enter→NODE drill-down. The
//! grouped lineage view (replica families, fork groups, standby
//! sets) lives on the GROUPS tab.

use net_sdk::deck::{DaemonHealthSnapshot, DaemonLifecycleSnapshot, MeshOsSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::{lineage, nodes, theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: Option<&MeshOsSnapshot>, cursor: usize) {
    match snapshot {
        Some(s) if !s.daemons.is_empty() => render_live(frame, area, s, cursor),
        _ => render_empty(frame, area),
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
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
        "register via the MeshOsDaemonSdk",
    );
}

fn render_live(frame: &mut Frame<'_>, area: Rect, snapshot: &MeshOsSnapshot, cursor: usize) {
    let groups = lineage::group_daemons(&snapshot.daemons);
    // Flatten group→member into a single ordered list so the
    // scroll window can slice on a flat index. Pairs each member
    // with the group it came from so the row render still has
    // the lineage metadata in hand.
    let flat: Vec<(&lineage::LiveGroup<'_>, &lineage::LiveMember<'_>)> = groups
        .iter()
        .flat_map(|g| g.members.iter().map(move |m| (g, m)))
        .collect();
    let total = flat.len();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let body_h = (area.height as usize)
        .saturating_sub(2)
        .saturating_sub(1);
    let (start, end, hidden_above, hidden_below) = super::scroll_window(total, body_h, cursor);
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("DAEMONS", theme::green_hi()),
        Span::styled(
            format!("    {total} live · {} groups", groups.len()),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ];
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
        cell_dim("DAEMON"),
        cell_dim("KIND"),
        cell_dim("LINEAGE"),
        cell_dim("NODE"),
        cell_dim("STATE"),
        cell_dim("HEALTH"),
        cell_dim("SAT"),
        cell_dim("AGE"),
    ])
    .height(1);

    let mut table_rows: Vec<Row> = Vec::with_capacity(end.saturating_sub(start));
    for (offset, (group, m)) in flat[start..end].iter().enumerate() {
        let row_idx = start + offset;
        {
            let d = m.daemon;
            let is_cursor = row_idx == cursor;
            let marker = if is_cursor { "▶" } else { " " };
            let id_style = if is_cursor {
                theme::green_hi()
            } else {
                theme::text()
            };
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
                Cell::from(Span::styled(marker, theme::green_hi())),
                Cell::from(Span::styled(format!("0x{:x}", m.id), id_style)),
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
            Constraint::Length(2),  // cursor marker
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
    let selected = cursor.checked_sub(start).filter(|s| start + *s < end);
    let mut state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

/// Total daemon count across all groups. Used by the cursor
/// clamp.
pub fn total_daemons(snapshot: &MeshOsSnapshot) -> usize {
    let groups = lineage::group_daemons(&snapshot.daemons);
    groups.iter().map(|g| g.members.len()).sum()
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

use super::format_age_ms as format_age;
