//! SUBNETS tab — cursored, scrollable rollup of every subnet
//! the local mesh knows about.
//!
//! Columns: `▶` cursor marker, SUBNET (dotted id), PARENT
//! (parent's dotted id; `—` for depth-0), DEPTH, MEMBERS
//! (count), HEALTH (`healthy/total` of member peers rolled up
//! against the current `MeshOsSnapshot`), AGG (`yes/—` for
//! subnets backed by a known aggregator source), LOCAL
//! (`yes/—`).

use std::collections::HashSet;
use std::sync::Arc;

use net_sdk::deck::{DeckClient, MeshOsSnapshot, SubnetRollup};
use net_sdk::subnets::SubnetId;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::{theme, widgets};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    deck: &Arc<DeckClient>,
    snapshot: &MeshOsSnapshot,
    aggregator_subnets: &HashSet<SubnetId>,
    cursor: usize,
) {
    let local = deck.local_subnet();
    let rollups = deck.subnets_with_members(None);
    // Under `--features demo` the cluster harness boots N nodes
    // flat under `SubnetId::GLOBAL`; substitute the fixture so
    // the panel shows a realistic multi-region tree.
    #[cfg(feature = "demo")]
    let (local, rollups) = if local.is_none() && rollups.is_empty() {
        crate::demo::fixtures::subnets()
    } else {
        (local, rollups)
    };
    if local.is_none() && rollups.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(
            frame,
            area,
            local,
            &rollups,
            snapshot,
            aggregator_subnets,
            cursor,
        );
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(widgets::section_title("SUBNETS", "no mesh attached"));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no mesh handle wired into the deck",
        "the in-process runtime doesn't carry a MeshNode today — \
         attaches when remote-attach lands or the CLI plumbs one in.",
    );
}

#[allow(clippy::too_many_arguments)]
fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    local: Option<SubnetId>,
    rollups: &[SubnetRollup],
    snapshot: &MeshOsSnapshot,
    aggregator_subnets: &HashSet<SubnetId>,
    cursor: usize,
) {
    let shown = rollups.len();
    let pos = if shown == 0 {
        0
    } else {
        cursor.min(shown - 1) + 1
    };
    let body_h = (area.height as usize).saturating_sub(2).saturating_sub(1);
    let effective_cursor = cursor.min(shown.saturating_sub(1));
    let (start, end, hidden_above, hidden_below) =
        super::scroll_window(shown, body_h, effective_cursor);

    let peer_total: usize = rollups.iter().map(|r| r.members.len()).sum();
    let local_str = local
        .map(|s| s.to_string())
        .unwrap_or_else(|| "—".to_string());
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("SUBNETS", theme::green_hi()),
        Span::styled(
            format!(
                "    local: {local_str} · {buckets} known · {peers} peers",
                buckets = rollups.len(),
                peers = peer_total
            ),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{shown}"), theme::dim()),
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
        Cell::from(Span::styled(" ", theme::chrome())),
        Cell::from(Span::styled("SUBNET", theme::chrome())),
        Cell::from(Span::styled("PARENT", theme::chrome())),
        Cell::from(Span::styled("DEPTH", theme::chrome())),
        Cell::from(Span::styled("MEMBERS", theme::chrome())),
        Cell::from(Span::styled("HEALTH", theme::chrome())),
        Cell::from(Span::styled("AGG", theme::chrome())),
        Cell::from(Span::styled("LOCAL", theme::chrome())),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(end.saturating_sub(start));
    for (offset, rollup) in rollups[start..end].iter().enumerate() {
        let i = start + offset;
        let is_cursor = i == effective_cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let subnet_style = if is_cursor {
            theme::green_hi()
        } else {
            theme::text()
        };

        let parent_text = if rollup.subnet.depth() == 0 {
            "—".to_string()
        } else {
            rollup.subnet.parent().to_string()
        };

        let (health_text, health_style) = health_rollup(&rollup.members, snapshot);
        let (agg_text, agg_style) = if aggregator_subnets.contains(&rollup.subnet) {
            ("yes".to_string(), theme::green())
        } else {
            ("—".to_string(), theme::dim())
        };

        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(rollup.subnet.to_string(), subnet_style)),
            Cell::from(Span::styled(parent_text, theme::dim())),
            Cell::from(Span::styled(
                format!("{}", rollup.subnet.depth()),
                theme::text(),
            )),
            Cell::from(Span::styled(
                format!("{}", rollup.members.len()),
                theme::text(),
            )),
            Cell::from(Span::styled(health_text, health_style)),
            Cell::from(Span::styled(agg_text, agg_style)),
            Cell::from(Span::styled(
                if rollup.is_local { "yes" } else { "—" },
                if rollup.is_local {
                    theme::green()
                } else {
                    theme::dim()
                },
            )),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor marker
            Constraint::Length(10), // subnet
            Constraint::Length(8),  // parent
            Constraint::Length(6),  // depth
            Constraint::Length(8),  // members
            Constraint::Length(8),  // health
            Constraint::Length(5),  // agg
            Constraint::Min(0),     // local
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

/// Roll up `members` against `snapshot.peers` into a
/// `(healthy/total, style)` chip. `—` when none of the members
/// appear in the snapshot (common under demo fixtures + when
/// the deck has no mesh wired). The style ladder mirrors what
/// NODES uses for its rollup column.
fn health_rollup(
    members: &[u64],
    snapshot: &MeshOsSnapshot,
) -> (String, ratatui::style::Style) {
    use net_sdk::deck::PeerHealthSnapshot;
    let mut total = 0u32;
    let mut healthy = 0u32;
    let mut degraded = 0u32;
    let mut unreachable = 0u32;
    for id in members {
        let Some(peer) = snapshot.peers.get(id) else {
            continue;
        };
        total += 1;
        match peer.health {
            Some(PeerHealthSnapshot::Healthy) => healthy += 1,
            Some(PeerHealthSnapshot::Degraded) => degraded += 1,
            Some(PeerHealthSnapshot::Unreachable) => unreachable += 1,
            _ => {}
        }
    }
    if total == 0 {
        return ("—".to_string(), theme::dim());
    }
    let text = format!("{healthy}/{total}");
    let style = if unreachable > 0 {
        theme::red()
    } else if degraded > 0 || healthy < total {
        theme::amber()
    } else {
        theme::green()
    };
    (text, style)
}
