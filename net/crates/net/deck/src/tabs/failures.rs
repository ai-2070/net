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

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    records: &[FailureRecord],
    cursor: usize,
    search: &str,
    search_editing: bool,
) {
    if records.is_empty() {
        render_empty(frame, area, search, search_editing);
    } else {
        render_table(frame, area, records, cursor, search, search_editing);
    }
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, search: &str, search_editing: bool) {
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("FAILURES", theme::green_hi()),
        Span::styled("    0 records", theme::chrome()),
    ];
    append_search_chip(&mut title_spans, search, search_editing);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(title_spans));
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
    search: &str,
    search_editing: bool,
) {
    let needle = search.to_ascii_lowercase();
    // Project records to the visible (filtered, reversed) set
    // first, then index the cursor against that. Lets the
    // cursor stay coherent as the operator types.
    let visible: Vec<&FailureRecord> = records
        .iter()
        .rev()
        .filter(|r| needle.is_empty() || record_matches(r, &needle))
        .collect();
    let total = records.len();
    let shown = visible.len();
    // When the filter narrows the set to zero, the cursor chip
    // would render as "1/0" — pin it to "0/0" instead and let
    // the body render its "no matches" hint.
    let pos = if shown == 0 {
        0
    } else {
        cursor.min(shown - 1) + 1
    };
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("FAILURES", theme::green_hi()),
        Span::styled(format!("    {shown}/{total} records"), theme::chrome()),
        Span::styled(format!("    {pos}/{shown}"), theme::dim()),
    ];
    append_search_chip(&mut title_spans, search, search_editing);
    let header_line = Line::from(title_spans);
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
    let mut rows: Vec<Row> = Vec::with_capacity(shown);
    // Clamp the cursor against `visible` for marker placement
    // so a narrowed search never leaves the row indicator
    // floating off the bottom of the table.
    let effective_cursor = cursor.min(shown.saturating_sub(1));
    for (i, rec) in visible.iter().enumerate() {
        let is_cursor = i == effective_cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let when = format_relative(rec.recorded_at_ms, now_ms);
        // Replay-derived records carry `seq = 0` and are dim to
        // distinguish them from live executor records.
        let seq_text = if rec.seq == 0 {
            "  —".to_string()
        } else {
            format!("{:>5}", rec.seq)
        };
        let reason_style = if is_cursor {
            theme::green_hi()
        } else {
            theme::text()
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(seq_text, theme::dim())),
            Cell::from(Span::styled(when, theme::text())),
            Cell::from(Span::styled(rec.source.clone(), theme::amber())),
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
    // Records exist but the active search matches none — render
    // a one-line hint inside the body so the operator isn't
    // staring at an empty table wondering whether their filter
    // is broken or there's genuinely nothing to see.
    if shown == 0 && total > 0 {
        let inner = area.inner(ratatui::layout::Margin {
            vertical: 2,
            horizontal: 2,
        });
        let hint = Line::from(Span::styled(
            format!("no matches for \"{search}\" — {total} records hidden by filter"),
            theme::dim(),
        ));
        frame.render_widget(
            ratatui::widgets::Paragraph::new(hint).alignment(Alignment::Left),
            inner,
        );
    }
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

/// Append the active-search chip / editing prompt to the title
/// row. While editing, the prompt hijacks the row entirely so
/// the operator's typing is front-and-center.
fn append_search_chip(spans: &mut Vec<Span<'static>>, search: &str, search_editing: bool) {
    if search_editing {
        spans.push(Span::styled("    / ", theme::amber()));
        spans.push(Span::styled(search.to_string(), theme::green_hi()));
        spans.push(Span::styled("_", theme::amber()));
        spans.push(Span::styled(
            "    [Enter] commit  [Esc] cancel",
            theme::dim(),
        ));
    } else if !search.is_empty() {
        spans.push(Span::styled(
            format!("    [match /{search}/]"),
            theme::amber(),
        ));
    }
}

/// Substring match across the searchable surface of a failure
/// record: source token + reason. `needle_lower` must already
/// be lowercased. ASCII case-insensitive — no per-call
/// allocation of a lowercased haystack copy.
pub(crate) fn record_matches(rec: &FailureRecord, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    super::audit::ascii_icontains(&rec.source, needle_lower)
        || super::audit::ascii_icontains(&rec.reason, needle_lower)
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
