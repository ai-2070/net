//! Blob detail modal — `[Enter]` on a BLOBS row opens this
//! against a snapshot of the cursored entry. Drilling in
//! lets operators see the full 64-char hash (the row only
//! shows a 16-char window), exact timestamps, and
//! refcount-table state.
//!
//! Read-only. Future slices may add `[D]` drop, `[P]` pin
//! actions here; today the modal is informational.

use net_sdk::dataforts::{BlobInventoryEntry, DEFAULT_RETENTION_FLOOR};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &BlobInventoryEntry,
    host_id: u64,
    host_label: Option<&str>,
) {
    let modal_area = center(area, 80, 21);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(Line::from(vec![
            Span::styled(" ⛁ ", theme::green()),
            Span::styled(
                "BLOB DETAIL",
                Style::default()
                    .fg(theme::GREEN_HI)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // headline
            Constraint::Length(1),  // spacer
            Constraint::Length(1),  // host
            Constraint::Length(1),  // adapter
            Constraint::Length(1),  // hash full
            Constraint::Length(1),  // ref + pin
            Constraint::Length(1),  // first seen
            Constraint::Length(1),  // last seen
            Constraint::Length(1),  // spacer
            Constraint::Length(1),  // age line
            Constraint::Length(1),  // gc-status line
            Constraint::Length(1),  // chunk channel
            Constraint::Min(0),     // notes
            Constraint::Length(1),  // bindings
        ])
        .split(inner);

    // Headline: refcount banner. 0 → dim (quiescent); non-0 →
    // green (live).
    let ref_color = if entry.refcount == 0 {
        theme::dim()
    } else {
        theme::green()
    };
    let headline = Line::from(vec![
        Span::styled("blob.chunk ", theme::chrome()),
        Span::styled(format!("refcount = {}", entry.refcount), ref_color),
        if entry.pinned {
            Span::styled("    [PINNED]", theme::amber())
        } else {
            Span::raw("")
        },
    ]);
    frame.render_widget(
        Paragraph::new(headline).alignment(Alignment::Center),
        rows[0],
    );

    let host_str = match host_label {
        Some(l) => format!("0x{host_id:04x}.{l}"),
        None => format!("0x{host_id:04x}"),
    };
    frame.render_widget(kv("host    ", &host_str), rows[2]);
    frame.render_widget(kv("adapter ", &entry.adapter_id), rows[3]);
    frame.render_widget(kv("hash    ", &entry.hash_hex), rows[4]);
    frame.render_widget(
        kv(
            "ref     ",
            &format!(
                "{}{}",
                entry.refcount,
                if entry.pinned { "  (pinned)" } else { "" }
            ),
        ),
        rows[5],
    );
    frame.render_widget(
        kv("first   ", &fmt_unix_ms(entry.first_seen_unix_ms)),
        rows[6],
    );
    frame.render_widget(
        kv("last    ", &fmt_unix_ms(entry.last_seen_unix_ms)),
        rows[7],
    );

    let now_ms = unix_now_ms();
    let age_first = now_ms.saturating_sub(entry.first_seen_unix_ms);
    let age_last = now_ms.saturating_sub(entry.last_seen_unix_ms);
    let age_line = Line::from(vec![
        Span::styled("  age     ", theme::chrome()),
        Span::styled(
            format!(
                "stored {} ago · last touched {} ago",
                fmt_ms(age_first),
                fmt_ms(age_last),
            ),
            theme::text(),
        ),
    ]);
    frame.render_widget(Paragraph::new(age_line), rows[9]);

    // GC retention status — pure-logic mirror of
    // `should_sweep(entry, now, DEFAULT_RETENTION_FLOOR, false)`.
    // Pinned blobs are protected; live (refcount > 0) ones are
    // protected; quiescent ones age out and become sweep-
    // eligible once `age_first >= DEFAULT_RETENTION_FLOOR`.
    let floor_ms = DEFAULT_RETENTION_FLOOR.as_millis() as u64;
    let (gc_text, gc_style) = if entry.pinned {
        ("pinned — protected from GC".to_string(), theme::amber())
    } else if entry.refcount > 0 {
        (
            format!("live ({}× referenced) — protected", entry.refcount),
            theme::green(),
        )
    } else if age_first >= floor_ms {
        (
            "quiescent past retention floor — GC-eligible".to_string(),
            theme::red(),
        )
    } else {
        let until = floor_ms.saturating_sub(age_first);
        (
            format!(
                "quiescent — GC-eligible in {} (retention floor {})",
                fmt_ms(until),
                fmt_ms(floor_ms),
            ),
            theme::dim(),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  gc      ", theme::chrome()),
            Span::styled(gc_text, gc_style),
        ])),
        rows[10],
    );

    // Chunk channel is `MeshBlobAdapter`'s internal RedEX
    // channel for the hash — operators tracing chunk-level
    // I/O against the adapter's logs grep on this. Computed
    // here from the hash (matches what the adapter would
    // derive); no SDK call needed.
    let channel = format!("blob/{}/{}", &entry.hash_hex[..2], &entry.hash_hex[2..]);
    frame.render_widget(kv("channel ", &channel), rows[11]);

    let notes = Line::from(vec![Span::styled(
        "  chunk-level granularity (BlobAdapter::list); logical-blob view needs substrate BlobRef index",
        theme::dim(),
    )]);
    frame.render_widget(Paragraph::new(notes), rows[12]);

    let bindings = Line::from(vec![
        Span::styled("[Enter]", theme::green_hi()),
        Span::styled(" open host node    ", theme::dim()),
        Span::styled("[Esc]", theme::green_hi()),
        Span::styled(" close", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[13],
    );
}

fn kv<'a>(label: &'a str, value: &'a str) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(format!("  {label}"), theme::chrome()),
        Span::styled(value.to_string(), theme::text()),
    ]))
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1_000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3_600, (s % 3_600) / 60)
    }
}

fn fmt_unix_ms(ts_ms: u64) -> String {
    // Render the raw unix-ms next to a relative-to-now form
    // so operators can correlate with logs that timestamp
    // either way. Wall-clock TZ isn't worth the chrono
    // dependency; ms is unambiguous.
    let now = unix_now_ms();
    let delta = now.saturating_sub(ts_ms);
    format!("{ts_ms} ms unix  ({} ago)", fmt_ms(delta))
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}
