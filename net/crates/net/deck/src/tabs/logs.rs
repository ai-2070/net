use net_sdk::deck::{LogLevel, LogRecord};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{nodes, theme};

/// LOGS tab view state — the filter / search / pause knobs the
/// operator twiddles. Grouped into a struct so `render` doesn't
/// take a 7-argument bag of bools and strings; the App stamps
/// one of these per frame from its own state.
pub struct LogsView<'a> {
    pub min_level: LogLevel,
    pub paused: bool,
    pub search: &'a str,
    pub search_editing: bool,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    tick: u64,
    records: &[LogRecord],
    view: LogsView<'_>,
) {
    // Status bar gets two rows: the chip line on top and a
    // blank spacer below so the search/filter/pause chips
    // don't visually collide with the global footer's
    // tab/jump/cursor row.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);
    render_filter_bar(
        frame,
        rows[0],
        view.min_level,
        view.paused,
        view.search,
        view.search_editing,
    );
    render_log_grid(frame, rows[1], tick, records, view.min_level, view.search);
    let status_row = Rect {
        height: 1,
        ..rows[2]
    };
    render_status(
        frame,
        status_row,
        records,
        view.min_level,
        view.paused,
        view.search,
    );
}

fn render_filter_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    min_level: LogLevel,
    paused: bool,
    search: &str,
    search_editing: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("LOG.MATRIX", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // While editing, the chip row is replaced with the search
    // prompt — the operator's full attention is on the buffer.
    if search_editing {
        let line = Line::from(vec![
            Span::styled("/ ", theme::amber()),
            Span::styled(search.to_string(), theme::green_hi()),
            Span::styled("_", theme::amber()),
            Span::styled("    [Enter] commit  [Esc] cancel", theme::dim()),
        ]);
        frame.render_widget(Paragraph::new(line), inner);
        return;
    }

    // Active level threshold gets the amber accent when it's
    // suppressing rows; default Info is rendered green to read
    // as "open / unfiltered."
    let (level_text, level_style) = level_chip(min_level);
    let (follow_text, follow_style) = if paused {
        ("PAUSED", theme::amber())
    } else {
        ("ON", theme::green_hi())
    };

    let mut spans = vec![
        Span::styled("level ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled(level_text, level_style),
        Span::styled("]   ", theme::chrome()),
        Span::styled("match ", theme::chrome()),
        Span::styled("[", theme::chrome()),
    ];
    if search.is_empty() {
        spans.push(Span::styled("*", theme::green_hi()));
    } else {
        spans.push(Span::styled(format!("/{search}/"), theme::amber()));
    }
    spans.extend([
        Span::styled("]   ", theme::chrome()),
        Span::styled("kind ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled("*", theme::green_hi()),
        Span::styled("]   ", theme::chrome()),
        Span::styled("follow ", theme::chrome()),
        Span::styled("[", theme::chrome()),
        Span::styled(follow_text, follow_style),
        Span::styled("]", theme::chrome()),
    ]);
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn level_chip(min_level: LogLevel) -> (&'static str, ratatui::style::Style) {
    match min_level {
        LogLevel::Debug => ("DEBUG+", theme::green_hi()),
        LogLevel::Info => ("INFO+", theme::green_hi()),
        LogLevel::Warn => ("WARN+", theme::amber()),
        LogLevel::Error => ("ERR", theme::red()),
        _ => ("?", theme::chrome()),
    }
}

/// Numeric rank used for "min level" comparisons. Higher means
/// more severe. The fallback `0` (treats an unknown future
/// variant as the most verbose, NOT as Info) means a new
/// variant lands in the operator's view by default; the
/// previous `1` fallback silently let unknown variants past
/// the Info filter, which is the wrong direction for a
/// `#[non_exhaustive]` enum.
pub(crate) fn level_rank(l: LogLevel) -> u8 {
    match l {
        LogLevel::Debug => 0,
        LogLevel::Info => 1,
        LogLevel::Warn => 2,
        LogLevel::Error => 3,
        _ => 0,
    }
}

fn render_log_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    tick: u64,
    records: &[LogRecord],
    min_level: LogLevel,
    search: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let total_rows = inner.height as usize;
    // Empty ring (fresh runtime, no source publishing yet)
    // surfaces a centered "waiting" placeholder so the user
    // sees the tab is wired but idle.
    let _ = tick;
    let lines: Vec<Line> = if records.is_empty() {
        Vec::new()
    } else {
        project_log_records(records, total_rows, min_level, search)
    };

    if lines.is_empty() {
        // Distinguish "no logs at all" from "filters hiding
        // everything" — the latter is easy to miss and a common
        // operator confusion. When a search is active the hint
        // points at `[/]` first, because that's usually the
        // narrowest filter.
        let (head, hint) = if records.is_empty() {
            (
                "no log lines yet",
                "publish_log() on any registered daemon will appear here",
            )
        } else if !search.is_empty() {
            (
                "no log lines match the current filters",
                "press [/] to edit the search or [f] to lower the level",
            )
        } else {
            (
                "no log lines at this threshold",
                "press [f] to lower the level filter",
            )
        };
        crate::widgets::empty::render(frame, inner, head, hint);
        return;
    }

    let start = lines.len().saturating_sub(total_rows);
    let visible = &lines[start..];
    frame.render_widget(Paragraph::new(visible.to_vec()), inner);
}

fn render_status(
    frame: &mut Frame<'_>,
    area: Rect,
    records: &[LogRecord],
    min_level: LogLevel,
    paused: bool,
    search: &str,
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(36)])
        .split(area);

    let total = records.len();
    let needle = search.to_ascii_lowercase();
    let shown = records
        .iter()
        .filter(|r| level_rank(r.level) >= level_rank(min_level))
        .filter(|r| needle.is_empty() || record_matches(r, &needle))
        .count();
    let (source_text, source_style) = if paused {
        ("frozen snapshot", theme::amber())
    } else {
        ("live snapshot", theme::dim())
    };
    let left = Line::from(vec![
        Span::styled(format!("{shown}/{total} lines  ·  "), theme::chrome()),
        Span::styled(source_text, source_style),
        Span::styled("  ·  ", theme::chrome()),
        Span::styled("0 dropped", theme::green_hi()),
    ]);
    frame.render_widget(Paragraph::new(left), cols[0]);

    let right = Line::from(vec![
        Span::styled("[/] ", theme::green_hi()),
        Span::styled("search   ", theme::dim()),
        Span::styled("[f] ", theme::green_hi()),
        Span::styled("filter   ", theme::dim()),
        Span::styled("[p] ", theme::green_hi()),
        Span::styled("pause", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}

/// Project a slice of `LogRecord` into renderable Lines.
/// Each record carries `seq`, `ts_ms`, `level`, `daemon_id`,
/// `node_id`, `message`. Older entries are at the front of the
/// slice; we keep that order and let the caller pick the tail.
fn project_log_records(
    records: &[LogRecord],
    capacity: usize,
    min_level: LogLevel,
    search: &str,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::with_capacity(records.len().min(capacity));
    let min = level_rank(min_level);
    let needle = search.to_ascii_lowercase();
    for rec in records
        .iter()
        .filter(|r| level_rank(r.level) >= min)
        .filter(|r| needle.is_empty() || record_matches(r, &needle))
    {
        let (level_style, level_pad) = match rec.level {
            LogLevel::Debug => (theme::chrome(), "DEBUG"),
            LogLevel::Info => (theme::dim(), "INFO "),
            LogLevel::Warn => (theme::amber(), "WARN "),
            LogLevel::Error => (theme::red(), "ERR  "),
            _ => (theme::text(), "?    "),
        };
        let ts = format_ts_ms(rec.ts_ms);
        let mut spans = vec![
            Span::styled(ts, theme::chrome()),
            Span::styled("  ", theme::chrome()),
            Span::styled(level_pad.to_string(), level_style),
            Span::styled("  ", theme::chrome()),
        ];
        // The runtime stamps `node_id` and `daemon_id` on every
        // record. Format `<node>/<daemon>` so the source column
        // still reads even when one or the other is missing.
        match (rec.node_id, rec.daemon_id) {
            (Some(node), Some(daemon)) => {
                spans.extend(nodes::id_spans(&format!("0x{node:x}")));
                spans.push(Span::styled("/", theme::chrome()));
                spans.push(Span::styled(format!("0x{daemon:x}"), theme::cyan()));
            }
            (Some(node), None) => {
                spans.extend(nodes::id_spans(&format!("0x{node:x}")));
            }
            (None, Some(daemon)) => {
                spans.push(Span::styled(format!("daemon.0x{daemon:x}"), theme::cyan()));
            }
            (None, None) => {
                spans.push(Span::styled("—", theme::chrome()));
            }
        }
        spans.push(Span::styled("  ", theme::chrome()));
        spans.push(Span::styled(rec.message.clone(), theme::text()));
        out.push(Line::from(spans));
    }
    out
}

/// ASCII case-insensitive substring match. `needle` must
/// already be lowercased by the caller — we lowercase the
/// haystack here. Non-ASCII bytes pass through verbatim which
/// is fine for the operator-facing log messages we render.
fn matches_ci(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    haystack.to_ascii_lowercase().contains(needle_lower)
}

/// Match a log record against the search needle. Covers the
/// message column plus the structured `daemon_id` / `node_id`
/// fields rendered as `0x…` so operators can grep by daemon
/// hex directly — even when the message text doesn't repeat
/// the id (e.g. an auto-generated daemon log emitted via
/// `publish_log` without echoing the id into the message).
pub(crate) fn record_matches(rec: &LogRecord, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if matches_ci(&rec.message, needle_lower) {
        return true;
    }
    if let Some(d) = rec.daemon_id {
        if format!("0x{d:x}").contains(needle_lower) {
            return true;
        }
    }
    if let Some(n) = rec.node_id {
        if format!("0x{n:x}").contains(needle_lower) {
            return true;
        }
    }
    false
}

fn format_ts_ms(ts_ms: u64) -> String {
    // wall-clock-relative; show as HH:MM:SS.mmm so a record
    // an hour old doesn't read identically to one a minute
    // old. The prior MM:SS.mmm form dropped the hours
    // component entirely and made multi-hour log windows
    // ambiguous against the rest of the deck (NET.MAP /
    // DAEMONS / DAEMON.PAGE all stamp `HH:MM:SS.mmm`).
    let total_sec = ts_ms / 1_000;
    let hh = total_sec / 3_600;
    let mm = (total_sec / 60) % 60;
    let ss = total_sec % 60;
    let ms = ts_ms % 1_000;
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03}")
}
