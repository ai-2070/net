//! SUBNETS tab — projects the local mesh node's known subnets
//! as `(subnet, depth, member_count)` rows. Reads through
//! `DeckClient::local_subnet` + `DeckClient::known_subnets`;
//! when no `MeshNode` is wired into the deck the panel renders
//! its "no mesh attached" empty state.
//!
//! Phase A of `SCALING_SUBNET_SPEC.md`. Reuse the existing
//! `deck/src/tabs/` table conventions (`Block::default` +
//! `Row`/`Cell` + cursor-aware highlighting via theme helpers).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use net_sdk::deck::DeckClient;
use net_sdk::subnets::SubnetId;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, deck: &Arc<DeckClient>) {
    let local = deck.local_subnet();
    let known = deck.known_subnets();
    if local.is_none() && known.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(frame, area, local, &known);
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("SUBNETS", theme::green_hi()),
            Span::styled("    no mesh attached", theme::chrome()),
        ]));
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
    local: Option<SubnetId>,
    known: &[(u64, SubnetId)],
) {
    // Group peers by subnet for the member-count column.
    let mut buckets: BTreeMap<u32, BTreeSet<u64>> = BTreeMap::new();
    for (node, subnet) in known {
        buckets.entry(subnet.raw()).or_default().insert(*node);
    }
    let total = buckets.len() + if local.is_some() { 1 } else { 0 };
    let local_str = local
        .map(|s| s.to_string())
        .unwrap_or_else(|| "—".to_string());
    let title = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("SUBNETS", theme::green_hi()),
        Span::styled(
            format!(
                "    local: {local_str} · {buckets} known · {peers} peers",
                buckets = buckets.len(),
                peers = known.len()
            ),
            theme::chrome(),
        ),
    ]);
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

    let mut rows: Vec<Row> = Vec::with_capacity(total);
    // Render every bucket, ascending by subnet raw bits.
    let mut all_raw: BTreeSet<u32> = buckets.keys().copied().collect();
    if let Some(s) = local {
        all_raw.insert(s.raw());
    }
    for raw in all_raw {
        let subnet = SubnetId::from_raw(raw);
        let members = buckets.get(&raw).map(BTreeSet::len).unwrap_or(0);
        let is_local = local == Some(subnet);
        let subnet_style = if is_local {
            theme::green_hi()
        } else {
            theme::text()
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(subnet.to_string(), subnet_style)),
            Cell::from(Span::styled(format!("{}", subnet.depth()), theme::text())),
            Cell::from(Span::styled(format!("{members}"), theme::text())),
            Cell::from(Span::styled(
                if is_local { "yes" } else { "—" },
                if is_local { theme::green() } else { theme::dim() },
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
