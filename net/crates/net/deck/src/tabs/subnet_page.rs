//! Full-page SUBNET detail. Reached by pressing `[Enter]` on a
//! cursored row in SUBNETS. `[Esc]` returns to the list.
//!
//! Layout:
//! - top panel: subnet identity card (id / depth / parent /
//!   members / local / health)
//! - bottom panel: members rendered through the shared NODES
//!   table view so each row carries the same HEALTH / RTT /
//!   CPU / MEM / DISK / SAT / DAEMONS / MAINT columns the
//!   top-level NODES tab uses. The member cursor is tracked
//!   on the focus entry itself; `Enter` drills into the
//!   NODE focus page for the cursored member.

use net_sdk::deck::{MeshOsSnapshot, PeerSnapshot};
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
    /// Cursor across `members`. Tracked on the entry so it
    /// persists for the lifetime of the focus session (Esc
    /// resets — re-opening starts at 0).
    pub member_cursor: usize,
}

/// View into the local node row supplied by the app so the
/// shared NODES helper renders `self` in the RTT column +
/// maps the local maintenance state to the chip vocabulary.
pub struct LocalMemberRow<'a> {
    pub id: u64,
    pub peer: &'a PeerSnapshot,
    pub local_maintenance: &'a net_sdk::deck::MaintenanceStateSnapshot,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
    local: Option<LocalMemberRow<'_>>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        // Header card: borders + 4 identity rows.
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(area);

    render_header(frame, chunks[0], focus, snapshot);
    render_members(frame, chunks[1], focus, snapshot, local);
}

fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(widgets::section_title("SUBNET", &focus.subnet.to_string()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let depth = focus.subnet.depth();
    let parent_str = if depth == 0 {
        "—".to_string()
    } else {
        focus.subnet.parent().to_string()
    };
    let local_str = if focus.is_local { "yes" } else { "no" };
    let local_style = if focus.is_local {
        theme::green()
    } else {
        theme::dim()
    };
    let (health_text, health_style) =
        super::subnets::health_rollup(&focus.members, snapshot);

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
        Line::from(vec![
            Span::styled("  health:  ", theme::chrome()),
            Span::styled(health_text, health_style),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_members(
    frame: &mut Frame<'_>,
    area: Rect,
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
    local: Option<LocalMemberRow<'_>>,
) {
    if focus.members.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::rule())
            .title(widgets::section_title("MEMBERS", "0 nodes"));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        widgets::empty::render(
            frame,
            inner,
            "no peers in this subnet",
            "the subnet appears in the snapshot but no node id was tagged with it",
        );
        return;
    }

    // Build the (node_id, &PeerSnapshot) row list against the
    // snapshot, using the supplied local row when a member
    // matches `this_node`. Members missing from the snapshot
    // are dropped — they render no NODE row, but the header's
    // health rollup still counts them as `—`.
    let mut nodes_iter: Vec<(u64, &PeerSnapshot)> = Vec::with_capacity(focus.members.len());
    let local_id = local.as_ref().map(|r| r.id);
    for id in &focus.members {
        if Some(*id) == local_id {
            if let Some(r) = local.as_ref() {
                nodes_iter.push((*id, r.peer));
            }
        } else if let Some(p) = snapshot.peers.get(id) {
            nodes_iter.push((*id, p));
        }
    }
    if nodes_iter.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::rule())
            .title(widgets::section_title(
                "MEMBERS",
                &format!("{} not in snapshot", focus.members.len()),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        widgets::empty::render(
            frame,
            inner,
            "subnet members are tagged but absent from the snapshot",
            "common under demo fixtures or when the snapshot hasn't seen those peers yet",
        );
        return;
    }

    let cursor = focus.member_cursor.min(nodes_iter.len() - 1);
    let pos = cursor + 1;
    let title_spans = vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("MEMBERS", theme::green_hi()),
        Span::styled(
            format!("    {} of {} in snapshot", nodes_iter.len(), focus.members.len()),
            theme::chrome(),
        ),
        Span::styled(format!("    {pos}/{}", nodes_iter.len()), theme::dim()),
    ];
    let local_maintenance_mirror = local
        .as_ref()
        .map(|r| super::nodes::local_maintenance_mirror(r.local_maintenance));
    super::nodes::render_nodes_view(
        frame,
        area,
        title_spans,
        &nodes_iter,
        snapshot,
        cursor,
        local_id,
        local_maintenance_mirror,
    );
}

/// Resolve the cursored member's `node_id` from a focus entry
/// against the same snapshot the render path sees. Returns
/// `None` when the cursor lands on a member that isn't in the
/// snapshot (drop-from-table semantics match `render_members`).
pub fn cursored_member_id(
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
    local_id: Option<u64>,
) -> Option<u64> {
    let mut visible: Vec<u64> = Vec::with_capacity(focus.members.len());
    for id in &focus.members {
        if Some(*id) == local_id || snapshot.peers.contains_key(id) {
            visible.push(*id);
        }
    }
    let idx = focus.member_cursor.min(visible.len().saturating_sub(1));
    visible.get(idx).copied()
}

/// Count members the focus page will render as table rows
/// (i.e. those present in the snapshot). Used by the App to
/// clamp `member_cursor` after a snapshot tick changes
/// visibility.
pub fn visible_member_count(
    focus: &SubnetFocusEntry,
    snapshot: &MeshOsSnapshot,
    local_id: Option<u64>,
) -> usize {
    focus
        .members
        .iter()
        .filter(|id| Some(**id) == local_id || snapshot.peers.contains_key(id))
        .count()
}
