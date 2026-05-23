//! GATEWAYS tab — projects the local mesh's `SubnetGateway`
//! state as a header line + an export-table table.
//!
//! Reads through `DeckClient::gateway_stats` +
//! `DeckClient::gateway_exports`. When no gateway is installed
//! (no `ChannelConfigRegistry` on the mesh, or no `MeshNode`
//! wired into the deck), renders the "no gateway attached"
//! empty state.
//!
//! Phase A of `SCALING_SUBNET_SPEC.md`.

use std::sync::Arc;

use net_sdk::deck::{DeckClient, GatewayStats};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, deck: &Arc<DeckClient>) {
    match deck.gateway_stats() {
        Some(stats) => {
            let exports = deck.gateway_exports();
            render_table(frame, area, &stats, &exports);
        }
        None => render_empty(frame, area),
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("GATEWAYS", theme::green_hi()),
            Span::styled("    no gateway installed", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no SubnetGateway installed on the local mesh",
        "call MeshNode::set_channel_configs(...) — \
         the gateway is built lazily alongside the channel registry.",
    );
}

fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: &GatewayStats,
    exports: &[(u16, Vec<net_sdk::subnets::SubnetId>)],
) {
    let title = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("GATEWAYS", theme::green_hi()),
        Span::styled(
            format!(
                "    local: {local} · forwarded: {fwd} · dropped: {drp} · {peers} peer-subnets · {exp} export rules",
                local = stats.local_subnet,
                fwd = stats.forwarded,
                drp = stats.dropped,
                peers = stats.peer_subnets.len(),
                exp = stats.export_rules
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
        Cell::from(Span::styled("CHANNEL HASH", theme::chrome())),
        Cell::from(Span::styled("TARGETS", theme::chrome())),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(exports.len());
    if exports.is_empty() {
        // No export rules — keep the table block visible with a
        // placeholder row so the operator knows the empty state
        // is real (not a render bug).
        rows.push(Row::new(vec![
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled(
                "no export rules; SubnetLocal / ParentVisible / Global only",
                theme::dim(),
            )),
        ]));
    } else {
        for (channel_hash, targets) in exports {
            let target_text = targets
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            rows.push(Row::new(vec![
                Cell::from(Span::styled(
                    format!("{channel_hash:#06x}"),
                    theme::text(),
                )),
                Cell::from(Span::styled(target_text, theme::text())),
            ]));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(14), // channel hash
            Constraint::Min(0),     // targets
        ],
    )
    .header(header)
    .block(block);
    frame.render_widget(table, area);
}
