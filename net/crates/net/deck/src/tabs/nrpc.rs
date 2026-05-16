//! NRPC tab — request/response traffic across the mesh.
//!
//! Renders the rolling `NrpcTail` ring as a flat table:
//! caller → callee, method, latency, payload sizes, status.
//! Newest call at the bottom (tail-style). Populated by the
//! `samples-logs` injector today; a real nRPC observer wire-up
//! will replace the injector without changing this consumer.

use ratatui::{
    layout::{Alignment, Constraint, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::{
    streams::{NrpcCall, NrpcStatus},
    theme, widgets,
};

pub fn render(frame: &mut Frame<'_>, area: Rect, calls: &[NrpcCall], paused: bool) {
    if calls.is_empty() {
        render_empty(frame, area);
        return;
    }
    render_table(frame, area, calls, paused);
}

fn render_empty(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NRPC", theme::green_hi()),
            Span::styled("    0 calls observed", theme::chrome()),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no nRPC traffic observed yet",
        "run with --features samples-logs or wire a real nRPC observer",
    );
}

fn render_table(frame: &mut Frame<'_>, area: Rect, calls: &[NrpcCall], paused: bool) {
    let total = calls.len();
    let mut title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("NRPC", theme::green_hi()),
        Span::styled(format!("    {total} recent calls"), theme::chrome()),
    ];
    if paused {
        title_spans.push(Span::styled("    [PAUSED]", theme::amber()));
    }
    let header_line = Line::from(title_spans);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header_line)
        .title_alignment(Alignment::Left);

    let header = Row::new(vec![
        cell_dim("TS"),
        cell_dim("CALLER"),
        cell_dim(""),
        cell_dim("CALLEE"),
        cell_dim("METHOD"),
        cell_dim("LATENCY"),
        cell_dim("REQ"),
        cell_dim("RESP"),
        cell_dim("STATUS"),
    ])
    .height(1);

    // Take the most recent N rows that fit the visible body.
    // Inner height = area.height - 2 (block borders) - 1 (header).
    let take = (area.height as usize).saturating_sub(3).max(1);
    let start = calls.len().saturating_sub(take);
    let visible = &calls[start..];

    let mut rows: Vec<Row> = Vec::with_capacity(visible.len());
    for c in visible {
        let ts = super::fmt_ts_hms_ms(c.ts_ms);
        let caller = format!("0x{:x}", c.caller);
        let callee = format!("0x{:x}", c.callee);
        let (latency_text, latency_style) = format_latency(c.latency_ms);
        let (status_text, status_style) = match &c.status {
            NrpcStatus::Ok => ("Ok".to_string(), theme::green()),
            NrpcStatus::InFlight => ("InFlight".to_string(), theme::cyan()),
            NrpcStatus::Error(reason) => (format!("Err: {reason}"), theme::red()),
            NrpcStatus::Timeout => ("Timeout".to_string(), theme::amber()),
        };
        rows.push(Row::new(vec![
            Cell::from(Span::styled(ts, theme::chrome())),
            Cell::from(Line::from(crate::nodes::id_spans(&caller))),
            Cell::from(Span::styled("→", theme::chrome())),
            Cell::from(Line::from(crate::nodes::id_spans(&callee))),
            Cell::from(Span::styled(c.method.clone(), theme::cyan())),
            Cell::from(Span::styled(latency_text, latency_style)),
            Cell::from(Span::styled(
                super::format_bytes(c.request_bytes as u64),
                theme::dim(),
            )),
            Cell::from(Span::styled(
                super::format_bytes(c.response_bytes as u64),
                theme::dim(),
            )),
            Cell::from(Span::styled(status_text, status_style)),
        ]));
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(13), // ts (HH:MM:SS.mmm)
            Constraint::Length(20), // caller (id.label)
            Constraint::Length(2),  // arrow
            Constraint::Length(20), // callee (id.label)
            Constraint::Length(28), // method
            Constraint::Length(9),  // latency
            Constraint::Length(8),  // req bytes
            Constraint::Length(8),  // resp bytes
            // STATUS as flex so a long error reason
            // ("Err: kinematic singularity", "Err: model
            // overloaded") isn't silently clipped at a fixed
            // width. Sits last so narrow terminals shrink the
            // status tail rather than dropping a column.
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn cell_dim(s: &'static str) -> Cell<'static> {
    Cell::from(Span::styled(s, theme::chrome()))
}

/// Latency colouring tiers:
///   * `< 10ms` green
///   * `10..100ms` amber
///   * `100..1000ms` red (slow but bounded)
///   * `>= 1000ms` bold red (catastrophic — seconds-tier)
///
/// The bold-red tier distinguishes a 240 ms outlier from a 6 s
/// outage; without it both rendered as the same shade of red.
fn format_latency(ms: u32) -> (String, ratatui::style::Style) {
    let text = if ms < 1_000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1_000.0)
    };
    let style = if ms < 10 {
        theme::green()
    } else if ms < 100 {
        theme::amber()
    } else if ms < 1_000 {
        theme::red()
    } else {
        theme::red_hi()
    };
    (text, style)
}
