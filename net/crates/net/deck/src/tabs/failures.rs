//! FAILURES tab — renders the streaming failure tail (Phase 4).
//! Records come from the executor's failure ring (dispatcher
//! rejections, constraint-violation drops, drain failures).
//! Newest first. Each row carries the seq, age, source token,
//! and the operator-readable reason.

use net_sdk::deck::FailureRecord;
use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect, records: &[FailureRecord], cursor: usize) {
    if records.is_empty() {
        render_empty(frame, area);
    } else {
        render_table(frame, area, records, cursor);
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("FAILURES", theme::green_hi()),
            Span::styled("    0 records", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no failures recorded",
        "executor rejections / drain failures / constraint drops will appear here",
    );
}

fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    records: &[FailureRecord],
    cursor: usize,
) {
    let total = records.len();
    let pos = cursor.min(total.saturating_sub(1)) + 1;
    let header_line = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("FAILURES", theme::green_hi()),
        Span::styled(format!("    {total} records"), theme::chrome()),
        Span::styled(format!("    {pos}/{total}"), theme::dim()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("SEQ"),
        cell_dim("WHEN"),
        cell_dim("SOURCE"),
        cell_dim("REASON"),
    ])
    .height(1);

    let now_ms = unix_now_ms();
    let mut rows: Vec<Row> = Vec::with_capacity(total);
    // Newest first — failures matter most at the head. Cursor
    // indexes the visible (post-reverse) order, so cursor=0
    // points at the freshest record.
    for (i, rec) in records.iter().rev().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let when = format_relative(rec.recorded_at_ms, now_ms);
        // Replay-derived records carry `seq = 0` and are dim to
        // distinguish them from live executor records.
        let seq_text = if rec.seq == 0 {
            "  —".to_string()
        } else {
            format!("{:>5}", rec.seq)
        };
        let source_style = if is_cursor { theme::amber() } else { theme::amber() };
        let reason_style = if is_cursor { theme::green_hi() } else { theme::text() };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(seq_text, theme::dim())),
            Cell::from(Span::styled(when, theme::text())),
            Cell::from(Span::styled(rec.source.clone(), source_style)),
            Cell::from(Span::styled(rec.reason.clone(), reason_style)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(5),  // SEQ
            Constraint::Length(9),  // WHEN
            Constraint::Length(24), // SOURCE
            Constraint::Min(0),     // REASON
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

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn format_relative(recorded_at_ms: u64, now_ms: u64) -> String {
    let delta = now_ms.saturating_sub(recorded_at_ms) / 1_000;
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else {
        format!("{}h ago", delta / 3_600)
    }
}
