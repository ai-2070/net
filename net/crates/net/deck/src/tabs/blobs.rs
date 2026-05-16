//! BLOBS tab — chunk-level inventory of the local blob
//! adapter. Sourced from `MeshBlobAdapter::list(...)` polled
//! at the deck's tick cadence (see `streams::spawn_blobs_poll`).
//! Newest-touched first.
//!
//! Granularity per the substrate's `BlobAdapter::list` doc:
//! one row per content-hash in the adapter's refcount table.
//! A `BlobRef::Manifest` blob shows up as N rows (one per
//! chunk); the substrate doesn't track logical-blob → chunk
//! association in a queryable index today.

use net_sdk::dataforts::BlobInventoryEntry;
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
    entries: &[BlobInventoryEntry],
    cursor: usize,
    search: &str,
    search_editing: bool,
) {
    if entries.is_empty() && search.is_empty() && !search_editing {
        render_empty(frame, area);
        return;
    }
    render_table(frame, area, entries, cursor, search, search_editing);
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("BLOBS", theme::green_hi()),
            Span::styled("    0 chunks", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no blob chunks indexed",
        "wire a `MeshBlobAdapter` + store blobs",
    );
}

fn render_table(
    frame: &mut Frame<'_>,
    area: Rect,
    entries: &[BlobInventoryEntry],
    cursor: usize,
    search: &str,
    search_editing: bool,
) {
    let needle = search.to_ascii_lowercase();
    // Filter entries the same way the search needle would
    // match in a hash-prefix sense (substring is more
    // forgiving for operators typing a fragment).
    let visible: Vec<&BlobInventoryEntry> = entries
        .iter()
        .filter(|e| needle.is_empty() || e.hash_hex.contains(&needle))
        .collect();
    let total = entries.len();
    let shown = visible.len();
    // When the filter narrows to zero rows, surface "0/0" in
    // the chip and a one-line hint below — the prior
    // saturating-sub left the chip showing "1/0" against an
    // empty body.
    let pos = if shown == 0 {
        0
    } else {
        cursor.min(shown - 1) + 1
    };

    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("BLOBS", theme::green_hi()),
        Span::styled(format!("    {shown}/{total} chunks"), theme::chrome()),
        Span::styled(format!("    {pos}/{shown}"), theme::dim()),
    ];
    append_search_chip(&mut title_spans, search, search_editing);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(title_spans))
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim(" "),
        cell_dim("HASH (truncated)"),
        cell_dim("REF"),
        cell_dim("PIN"),
        cell_dim("FIRST SEEN"),
        cell_dim("LAST TOUCH"),
    ])
    .height(1);

    let now_ms = unix_now_ms();
    let mut rows: Vec<Row> = Vec::with_capacity(shown);
    let effective_cursor = cursor.min(shown.saturating_sub(1));
    for (i, e) in visible.iter().enumerate() {
        let is_cursor = i == effective_cursor;
        let marker = if is_cursor { "▶" } else { " " };
        // Render a 16-char hash window — full hex is 64 chars
        // which would dominate the row. Operators search for
        // a prefix to disambiguate.
        let hash_short = if e.hash_hex.len() > 16 {
            format!(
                "{}…{}",
                &e.hash_hex[..8],
                &e.hash_hex[e.hash_hex.len() - 8..]
            )
        } else {
            e.hash_hex.clone()
        };
        let hash_style = if is_cursor {
            theme::green_hi()
        } else {
            theme::text()
        };
        let ref_style = if e.refcount == 0 {
            theme::dim()
        } else {
            theme::green()
        };
        let pin_text = if e.pinned { "PIN" } else { " · " };
        let pin_style = if e.pinned {
            theme::amber()
        } else {
            theme::chrome()
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(marker, theme::green_hi())),
            Cell::from(Span::styled(hash_short, hash_style)),
            Cell::from(Span::styled(format!("{:>3}", e.refcount), ref_style)),
            Cell::from(Span::styled(pin_text, pin_style)),
            Cell::from(Span::styled(
                format_relative(e.first_seen_unix_ms, now_ms),
                theme::dim(),
            )),
            Cell::from(Span::styled(
                format_relative(e.last_seen_unix_ms, now_ms),
                theme::text(),
            )),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // cursor
            Constraint::Length(19), // hash window
            Constraint::Length(4),  // ref
            Constraint::Length(4),  // pin
            Constraint::Length(11), // first seen
            Constraint::Min(0),     // last touch
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(2);
    frame.render_widget(table, area);
    // Entries exist but the search matched none: tell the
    // operator their filter is in play instead of leaving an
    // empty body that reads as "no chunks indexed".
    if shown == 0 && total > 0 {
        let inner = area.inner(ratatui::layout::Margin {
            vertical: 2,
            horizontal: 2,
        });
        let hint = Line::from(Span::styled(
            format!("no chunks match \"{search}\" — {total} hidden by filter"),
            theme::dim(),
        ));
        frame.render_widget(
            ratatui::widgets::Paragraph::new(hint).alignment(Alignment::Left),
            inner,
        );
    }
}

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

/// Substring match against the hash hex column. Search needle
/// is already lowercased.
pub(crate) fn record_matches(rec: &BlobInventoryEntry, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    rec.hash_hex.contains(needle_lower)
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

use super::unix_now_ms;

fn format_relative(then_ms: u64, now_ms: u64) -> String {
    let delta = now_ms.saturating_sub(then_ms) / 1_000;
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else {
        format!("{}h ago", delta / 3_600)
    }
}
