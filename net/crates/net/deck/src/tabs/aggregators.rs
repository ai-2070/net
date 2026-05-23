//! AGGREGATORS tab — projects the running [`AggregatorDaemon`]'s
//! latest summaries as a header + per-summary table.
//!
//! Reads through `DeckClient::aggregator_*` accessors. When no
//! aggregator is installed (most operator binaries — they're
//! queriers, not hosts), renders the "no aggregator wired"
//! empty state.
//!
//! Phase C of `SCALING_SUBNET_SPEC.md`.

use std::sync::Arc;

use net_sdk::deck::{AggregatorSnapshot, DeckClient};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::Span,
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, deck: &Arc<DeckClient>) {
    match deck.aggregator_snapshot() {
        None => render_empty(frame, area),
        Some(snap) => render_table(frame, area, &snap),
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(widgets::section_title("AGGREGATORS", "no aggregator wired"));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no AggregatorDaemon installed on the deck",
        "the deck is a querier — pass an Arc<AggregatorDaemon> to \
         DeckClient::with_aggregator(...) when running an aggregator \
         alongside the deck.",
    );
}

fn render_table(frame: &mut Frame<'_>, area: Rect, snap: &AggregatorSnapshot) {
    let source = snap.source_subnet.to_string();
    let kinds: Vec<String> = snap
        .fold_kinds
        .iter()
        .map(|k| format!("{k:#06x}"))
        .collect();
    let generation = snap.generation;
    let interval = snap.summary_interval;
    let summaries = &snap.summaries;

    let title = widgets::section_title(
        "AGGREGATORS",
        &format!(
            "source: {source} · folds: [{kinds}] · gen: {generation} · every {interval:?} · {count} summaries buffered",
            kinds = kinds.join(", "),
            count = summaries.len(),
        ),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(title)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        Cell::from(Span::styled("GEN", theme::chrome())),
        Cell::from(Span::styled("KIND", theme::chrome())),
        Cell::from(Span::styled("SUBNET", theme::chrome())),
        Cell::from(Span::styled("BUCKETS", theme::chrome())),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(summaries.len());
    if summaries.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled(
                "no summaries yet; aggregator hasn't ticked",
                theme::dim(),
            )),
        ]));
    } else {
        // Render newest-first — operators care about the latest
        // tick. The daemon's buffer is append-then-evict, so the
        // tail is freshest.
        for summary in summaries.iter().rev() {
            let bucket_text = if summary.buckets.is_empty() {
                "—".to_string()
            } else {
                summary
                    .buckets
                    .iter()
                    .map(|(name, count)| format!("{name}={count}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            };
            rows.push(Row::new(vec![
                Cell::from(Span::styled(
                    format!("{}", summary.generation),
                    theme::text(),
                )),
                Cell::from(Span::styled(
                    format!("{:#06x}", summary.fold_kind),
                    theme::text(),
                )),
                Cell::from(Span::styled(
                    summary.source_subnet.to_string(),
                    theme::text(),
                )),
                Cell::from(Span::styled(bucket_text, theme::text())),
            ]));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),  // gen
            Constraint::Length(8),  // kind
            Constraint::Length(12), // subnet
            Constraint::Min(0),     // buckets
        ],
    )
    .header(header)
    .block(block);
    frame.render_widget(table, area);
}
