//! NRPC tab — request/response traffic across the mesh.
//!
//! Placeholder body. The substrate's nRPC layer carries every
//! typed mesh RPC (request/reply, streaming, fan-out); this tab
//! will eventually render the live call ring: caller, callee,
//! method, latency, status code. For now it shows the empty-
//! state hint while the observer plumbing lands.

use ratatui::{
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Block, Borders},
    Frame,
};

use crate::{theme, widgets};

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NRPC", theme::green_hi()),
            Span::styled("    0 calls in flight", theme::chrome()),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no nRPC traffic observed yet",
        "wire an nRPC observer to populate the call ring",
    );
}
