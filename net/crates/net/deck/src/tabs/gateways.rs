//! GATEWAYS tab — projects the local mesh's `SubnetGateway`
//! export table as a cursored, scrollable per-channel rollup.
//!
//! Five columns: `▶` cursor marker, `CHANNEL` (resolved name or
//! `—` for an unknown hash), `VIS` (declared visibility tier),
//! `HASH` (16-bit wire hash), `TARGETS` (subnet IDs the rule
//! exports to), `REACH` (sum of known node counts across those
//! targets — what the SUBNETS panel computes per row, but
//! rolled up across this rule's targets so the operator sees
//! the rule's blast radius at a glance).
//!
//! Cursor + scrolling mirror the BLOBS pattern:
//! `tabs::scroll_window` picks the visible window around the
//! cursor; the header chips show `▲ N more` / `▼ N more` when
//! rows are hidden above / below. `TableState::with_selected`
//! highlights the current row.

use std::collections::HashMap;
use std::sync::Arc;

use net_sdk::deck::{DeckClient, GatewayStats, SubnetRollup};
use net_sdk::subnets::{SubnetId, Visibility};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::{theme, widgets};

/// Resolved row consumed by the table. Built from the
/// substrate's `(channel_hash, targets)` tuples joined with
/// `DeckClient::channels()` (for name + visibility) and the
/// SUBNETS rollup (for REACH). Demo builds get this shape
/// directly from the fixture.
#[derive(Clone, Debug)]
pub(crate) struct ExportRow {
    pub channel_hash: u16,
    pub channel_name: Option<String>,
    pub visibility: Option<Visibility>,
    pub targets: Vec<SubnetId>,
    pub reach: u64,
}

pub fn render(frame: &mut Frame<'_>, area: Rect, deck: &Arc<DeckClient>, cursor: usize) {
    let real_stats = deck.gateway_stats();
    let rows = if real_stats.is_some() {
        resolve_rows(deck)
    } else {
        Vec::new()
    };

    // Demo fallback — when no real gateway is wired, swap in
    // the fixture data so the panel renders.
    #[cfg(feature = "demo")]
    let (stats, rows) = match real_stats {
        Some(s) => (Some(s), rows),
        None => {
            let (fs, fr) = crate::demo::fixtures::gateways();
            (
                Some(fs),
                fr.into_iter()
                    .map(|r| ExportRow {
                        channel_hash: r.channel_hash,
                        channel_name: r.channel_name,
                        visibility: r.visibility,
                        targets: r.targets,
                        reach: r.reach,
                    })
                    .collect(),
            )
        }
    };
    #[cfg(not(feature = "demo"))]
    let (stats, rows) = (real_stats, rows);

    match stats {
        Some(stats) => render_table(frame, area, &stats, &rows, cursor),
        None => render_empty(frame, area),
    }
}

/// Build the resolved row set from the real substrate. The
/// channels registry + subnets rollup are pulled once and
/// joined against the raw `(hash, targets)` exports.
fn resolve_rows(deck: &Arc<DeckClient>) -> Vec<ExportRow> {
    let raw = deck.gateway_exports();
    if raw.is_empty() {
        return Vec::new();
    }
    // Hash → (name, visibility) lookup. Built once per render.
    // `channel_wire_hash` re-queries the registry per channel;
    // the channel count is small in practice (operator
    // tooling), so the per-row cost is negligible.
    let mut meta: HashMap<u16, (String, Visibility)> = HashMap::new();
    for (name, vis) in deck.channels() {
        if let Some(h) = deck.channel_wire_hash(&name) {
            meta.insert(h, (name, vis));
        }
    }
    let rollup_members: HashMap<SubnetId, u64> = deck
        .subnets_with_members(None)
        .into_iter()
        .map(|r: SubnetRollup| (r.subnet, r.members.len() as u64))
        .collect();
    raw.into_iter()
        .map(|(hash, targets)| {
            let reach: u64 = targets
                .iter()
                .map(|s| rollup_members.get(s).copied().unwrap_or(0))
                .sum();
            let (name, vis) = match meta.get(&hash) {
                Some((n, v)) => (Some(n.clone()), Some(*v)),
                None => (None, None),
            };
            ExportRow {
                channel_hash: hash,
                channel_name: name,
                visibility: vis,
                targets,
                reach,
            }
        })
        .collect()
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(widgets::section_title("GATEWAYS", "no gateway installed"));
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
    rows: &[ExportRow],
    cursor: usize,
) {
    let shown = rows.len();
    let pos = if shown == 0 {
        0
    } else {
        cursor.min(shown - 1) + 1
    };
    // Body height accounts for the block's top/bottom borders
    // (2 cells) + the header row (1 cell).
    let body_h = (area.height as usize).saturating_sub(2).saturating_sub(1);
    let effective_cursor = cursor.min(shown.saturating_sub(1));
    let (start, end, hidden_above, hidden_below) =
        super::scroll_window(shown, body_h, effective_cursor);

    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("GATEWAYS", theme::green_hi()),
        Span::styled(
            format!(
                "    local: {local} · fwd: {fwd} · drop: {drp} · {peers} peers · {exp} rules",
                local = stats.local_subnet,
                fwd = stats.forwarded,
                drp = stats.dropped,
                peers = stats.peer_subnets.len(),
                exp = stats.export_rules,
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
        Cell::from(Span::styled("CHANNEL", theme::chrome())),
        Cell::from(Span::styled("VIS", theme::chrome())),
        Cell::from(Span::styled("HASH", theme::chrome())),
        Cell::from(Span::styled("TARGETS", theme::chrome())),
        Cell::from(Span::styled("REACH", theme::chrome())),
    ])
    .height(1);

    let mut body: Vec<Row> = Vec::with_capacity(end.saturating_sub(start));
    if rows.is_empty() {
        body.push(Row::new(vec![
            Cell::from(Span::styled(" ", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled(
                "no export rules; SubnetLocal / ParentVisible / Global only",
                theme::dim(),
            )),
            Cell::from(Span::styled("—", theme::dim())),
        ]));
    } else {
        for (offset, row) in rows[start..end].iter().enumerate() {
            let i = start + offset;
            let is_cursor = i == effective_cursor;
            let marker = if is_cursor { "▶" } else { " " };
            let name_text = row
                .channel_name
                .clone()
                .unwrap_or_else(|| "—".to_string());
            let name_style = if row.channel_name.is_none() {
                theme::dim()
            } else if is_cursor {
                theme::green_hi()
            } else {
                theme::text()
            };
            let vis_text = match row.visibility {
                Some(Visibility::Global) => "global",
                Some(Visibility::ParentVisible) => "parent",
                Some(Visibility::Exported) => "exported",
                Some(Visibility::SubnetLocal) => "local",
                None => "—",
            };
            let target_text = row
                .targets
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            body.push(Row::new(vec![
                Cell::from(Span::styled(marker, theme::green_hi())),
                Cell::from(Span::styled(name_text, name_style)),
                Cell::from(Span::styled(vis_text, theme::cyan())),
                Cell::from(Span::styled(
                    format!("{:#06x}", row.channel_hash),
                    theme::dim(),
                )),
                Cell::from(Span::styled(target_text, theme::text())),
                Cell::from(Span::styled(format!("{}", row.reach), theme::text())),
            ]));
        }
    }

    let table = Table::new(
        body,
        [
            Constraint::Length(2),  // cursor marker
            Constraint::Length(28), // channel name
            Constraint::Length(9),  // visibility tier
            Constraint::Length(8),  // hash
            Constraint::Min(20),    // targets list
            Constraint::Length(7),  // reach
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
