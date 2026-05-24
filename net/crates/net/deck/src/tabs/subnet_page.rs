//! Full-page SUBNET detail. Reached by pressing `[Enter]` on a
//! cursored row in SUBNETS. `[Esc]` returns to the list.
//!
//! Layout:
//! - top panel: subnet identity (id / depth / parent / member
//!   count / local flag)
//! - bottom panel: member node IDs in two columns; rolls up
//!   peer health from `MeshOsSnapshot.peers` so each row reads
//!   `<node_id_hex>  <health>` rather than just the raw id.
//!
//! The list snapshots `members: Vec<u64>` at focus time so the
//! body stays stable across the next tick — the operator's
//! drill-in shouldn't shuffle under them.

use net_sdk::deck::MeshOsSnapshot;
use net_sdk::subnets::SubnetId;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::{theme, widgets};

/// Captured-at-Enter snapshot of the cursored subnet row. App
/// state holds an `Option<SubnetFocusEntry>`; when `Some`, the
/// focus page render runs instead of the SUBNETS list.
#[derive(Clone, Debug)]
pub struct SubnetFocusEntry {
    pub subnet: SubnetId,
    pub members: Vec<u64>,
    pub is_local: bool,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        // Header card is 5 lines high (block borders + 3 rows
        // of identity); the rest is the members table.
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    render_header(frame, chunks[0], focus);
    render_members(frame, chunks[1], focus, snapshot);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, focus: &SubnetFocusEntry) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(widgets::section_title(
            "SUBNET",
            &focus.subnet.to_string(),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let depth = focus.subnet.depth();
    let parent = focus.subnet.parent();
    let parent_str = if depth == 0 {
        "—".to_string()
    } else {
        parent.to_string()
    };
    let local_str = if focus.is_local { "yes" } else { "no" };
    let local_style = if focus.is_local {
        theme::green()
    } else {
        theme::dim()
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("  id:      ", theme::chrome()),
            Span::styled(focus.subnet.to_string(), theme::green_hi()),
        ]),
        Line::from(vec![
            Span::styled("  depth:   ", theme::chrome()),
            Span::styled(format!("{depth}"), theme::text()),
            Span::styled("    parent:   ", theme::chrome()),
            Span::styled(parent_str, theme::text()),
        ]),
        Line::from(vec![
            Span::styled("  members: ", theme::chrome()),
            Span::styled(format!("{}", focus.members.len()), theme::text()),
            Span::styled("    local:    ", theme::chrome()),
            Span::styled(local_str, local_style),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Left).wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_members(
    frame: &mut Frame<'_>,
    area: Rect,
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(widgets::section_title(
            "MEMBERS",
            &format!("{} node{}", focus.members.len(), if focus.members.len() == 1 { "" } else { "s" }),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if focus.members.is_empty() {
        widgets::empty::render(
            frame,
            inner,
            "no peers in this subnet",
            "the subnet appears in the snapshot but no node id was tagged with it",
        );
        return;
    }

    // Build one line per member: `<node_id_hex>   <health>`. The
    // health column rolls up against `snapshot.peers` so the
    // operator sees what they'd see on the NODES tab without
    // pivoting. `—` when the peer isn't in the snapshot (e.g.
    // demo-only fixture data).
    let lines: Vec<Line<'static>> = focus
        .members
        .iter()
        .map(|id| {
            let id_text = format!("  0x{:016x}", id);
            let (health_text, health_style) = match snapshot.peers.get(id) {
                Some(p) => peer_health_chip(p),
                None => ("—".to_string(), theme::dim()),
            };
            Line::from(vec![
                Span::styled(id_text, theme::text()),
                Span::styled("    ", theme::dim()),
                Span::styled(health_text, health_style),
            ])
        })
        .collect();

    let para = Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn peer_health_chip(peer: &net_sdk::deck::PeerSnapshot) -> (String, ratatui::style::Style) {
    // Mirror the green / amber / red ladder NODES uses so the
    // operator reads the column the same here.
    use net_sdk::deck::PeerHealthSnapshot;
    match peer.health {
        Some(PeerHealthSnapshot::Healthy) => ("healthy".to_string(), theme::green()),
        Some(PeerHealthSnapshot::Degraded) => ("degraded".to_string(), theme::amber()),
        Some(PeerHealthSnapshot::Unreachable) => ("unreachable".to_string(), theme::red()),
        None | Some(_) => ("—".to_string(), theme::dim()),
    }
}
