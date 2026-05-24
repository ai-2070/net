//! AGGREGATORS tab — cursored, scrollable view of the running
//! `AggregatorDaemon`'s buffered summaries. One row per
//! `SummaryAnnouncement`; `j`/`k` + `g`/`G` navigation matches
//! the BLOBS / GATEWAYS / SUBNETS pattern.
//!
//! Columns: `▶` cursor marker, GEN (monotonic tick), KIND
//! (fold-kind ID), SUBNET (source subnet), BUCKETS (flattened
//! `name=count` pairs).
//!
//! Newest-first ordering: the daemon's summary buffer is
//! append-then-evict, so iterating in reverse surfaces the
//! latest tick at the top — what an operator cares about.
//!
//! Phase C of `SCALING_SUBNET_SPEC.md`.

use std::sync::Arc;

use net_sdk::deck::{AggregatorSnapshot, DeckClient, SummaryAnnouncement};
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table, TableState},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, deck: &Arc<DeckClient>, cursor: usize) {
    let snap = deck.aggregator_snapshot();
    // Under `--features demo` no AggregatorDaemon is wired into
    // the cluster harness; substitute the demo fixture.
    #[cfg(feature = "demo")]
    let snap = snap.or_else(|| Some(crate::demo::fixtures::aggregator()));
    match snap {
        None => render_empty(frame, area),
        Some(snap) => render_table(frame, area, &snap, cursor),
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

fn render_table(frame: &mut Frame<'_>, area: Rect, snap: &AggregatorSnapshot, cursor: usize) {
    // Newest-first ordering. Materialize once so the cursor
    // index stays stable across the table render + selection
    // calc.
    let summaries: Vec<&SummaryAnnouncement> = snap.summaries.iter().rev().collect();
    let shown = summaries.len();
    let pos = if shown == 0 {
        0
    } else {
        cursor.min(shown - 1) + 1
    };
    let body_h = (area.height as usize).saturating_sub(2).saturating_sub(1);
    let effective_cursor = cursor.min(shown.saturating_sub(1));
    let (start, end, hidden_above, hidden_below) =
        super::scroll_window(shown, body_h, effective_cursor);

    let kinds: Vec<String> = snap
        .fold_kinds
        .iter()
        .map(|k| format!("{k:#06x}"))
        .collect();
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("AGGREGATORS", theme::green_hi()),
        Span::styled(
            format!(
                "    source: {source} · folds: [{kinds}] · gen: {gen} · every {interval:?}",
                source = snap.source_subnet,
                kinds = kinds.join(", "),
                gen = snap.generation,
                interval = snap.summary_interval,
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
        Cell::from(Span::styled("GEN", theme::chrome())),
        Cell::from(Span::styled("KIND", theme::chrome())),
        Cell::from(Span::styled("SUBNET", theme::chrome())),
        Cell::from(Span::styled("BUCKETS", theme::chrome())),
    ])
    .height(1);

    let mut rows: Vec<Row> = Vec::with_capacity(end.saturating_sub(start));
    if summaries.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(Span::styled(" ", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled("—", theme::dim())),
            Cell::from(Span::styled(
                "no summaries yet; aggregator hasn't ticked",
                theme::dim(),
            )),
        ]));
    } else {
        for (offset, summary) in summaries[start..end].iter().enumerate() {
            let i = start + offset;
            let is_cursor = i == effective_cursor;
            let marker = if is_cursor { "▶" } else { " " };
            let cell_style = if is_cursor {
                theme::green_hi()
            } else {
                theme::text()
            };
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
                Cell::from(Span::styled(marker, theme::green_hi())),
                Cell::from(Span::styled(format!("{}", summary.generation), cell_style)),
                Cell::from(Span::styled(
                    format!("{:#06x}", summary.fold_kind),
                    cell_style,
                )),
                Cell::from(Span::styled(summary.source_subnet.to_string(), cell_style)),
                Cell::from(Span::styled(bucket_text, cell_style)),
            ]));
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor marker
            Constraint::Length(6),  // gen
            Constraint::Length(8),  // kind
            Constraint::Length(12), // subnet
            Constraint::Min(0),     // buckets
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
