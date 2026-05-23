//! SUBNETS tab — projects the local mesh node's known subnets
//! as `(subnet, depth, member_count)` rows. Reads through
//! `DeckClient::local_subnet` + `DeckClient::known_subnets`;
//! when no `MeshNode` is wired into the deck the panel renders
//! its "no mesh attached" empty state.
//!
//! Phase A of `SCALING_SUBNET_SPEC.md`. Reuse the existing
//! `deck/src/tabs/` table conventions (`Block::default` +
//! `Row`/`Cell` + cursor-aware highlighting via theme helpers).

use std::sync::Arc;

use net_sdk::deck::{DeckClient, SubnetRollup};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::Span,
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, deck: &Arc<DeckClient>) {
    let local = deck.local_subnet();
    let rollups = deck.subnets_with_members(None);
    if local.is_none() && rollups.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(frame, area, local, &rollups);
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

fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    local: Option<net_sdk::subnets::SubnetId>,
    rollups: &[SubnetRollup],
) {
    let peer_total: usize = rollups.iter().map(|r| r.members.len()).sum();
    let local_str = local
        .map(|s| s.to_string())
        .unwrap_or_else(|| "—".to_string());
    let title = widgets::section_title(
        "SUBNETS",
        &format!(
            "local: {local_str} · {buckets} known · {peers} peers",
            buckets = rollups.len(),
            peers = peer_total
        ),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(title)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        Cell::from(Span::styled("SUBNET", theme::chrome())),
        Cell::from(Span::styled("DEPTH", theme::chrome())),
        Cell::from(Span::styled("MEMBERS", theme::chrome())),
        Cell::from(Span::styled("LOCAL", theme::chrome())),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(rollups.len());
    for rollup in rollups {
        let subnet_style = if rollup.is_local {
            theme::green_hi()
        } else {
            theme::text()
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(rollup.subnet.to_string(), subnet_style)),
            Cell::from(Span::styled(
                format!("{}", rollup.subnet.depth()),
                theme::text(),
            )),
            Cell::from(Span::styled(
                format!("{}", rollup.members.len()),
                theme::text(),
            )),
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
            Constraint::Length(20), // subnet
            Constraint::Length(6),  // depth
            Constraint::Length(8),  // members
            Constraint::Min(0),     // local
        ],
    )
    .header(header)
    .block(block);
    frame.render_widget(table, area);
}
