//! Node-picker modal. Generic peer-selection overlay used by
//! commands that need an operator-picked target (`force_cutover`'s
//! target, future `flush_avoid_lists(scope=peer)`, etc.).
//!
//! Layout:
//! - title with purpose blurb
//! - scrollable list of peers (id.label · health · RTT)
//! - footer bindings
//!
//! On `Enter` the App transitions the modal from `PickNode`
//! into `Confirm` with the picked target baked into the
//! `ConfirmAction` variant.

use net_sdk::deck::{MeshOsSnapshot, PeerHealthSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::{nodes, theme};

/// Why the picker was opened. The headline + the eventual
/// `ConfirmAction` variant the App builds on `Enter` are
/// driven off this enum.
#[derive(Clone, Debug)]
pub enum PickNodePurpose {
    /// Pick the target node for an ICE force-cutover on the
    /// cursored chain. The chain id is carried through the
    /// modal so the dispatch path has both pieces.
    ForceCutoverTarget { chain: u64 },
    /// Pick which holder to evict for an ICE force-evict-replica
    /// on the cursored chain. The candidate set is the chain's
    /// current holders (not the full peer list).
    ForceEvictHolder { chain: u64 },
}

impl PickNodePurpose {
    pub fn headline(&self) -> String {
        match self {
            Self::ForceCutoverTarget { chain } => {
                format!("ICE  pick cutover target for chain.0x{chain:x}")
            }
            Self::ForceEvictHolder { chain } => {
                format!("ICE  pick holder to evict on chain.0x{chain:x}")
            }
        }
    }

    /// One-line hint about what selection does.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::ForceCutoverTarget { .. } => {
                "the chain's elected leader emits RequestPlacement → target on commit"
            }
            Self::ForceEvictHolder { .. } => {
                "the picked holder drops its replica; the chain falls under desired_count"
            }
        }
    }

    /// The set of node IDs this picker is willing to surface,
    /// derived from the snapshot. Cutover offers every peer
    /// minus `this_node`; evict offers only the chain's current
    /// holders.
    pub fn candidates(&self, snapshot: &MeshOsSnapshot, this_node: u64) -> Vec<u64> {
        match self {
            Self::ForceCutoverTarget { .. } => snapshot
                .peers
                .keys()
                .copied()
                .filter(|id| *id != this_node)
                .collect(),
            Self::ForceEvictHolder { chain } => snapshot
                .replicas
                .get(chain)
                .map(|r| r.holders.clone())
                .unwrap_or_default(),
        }
    }
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    purpose: &PickNodePurpose,
    snapshot: &MeshOsSnapshot,
    this_node: u64,
    cursor: usize,
) {
    let peers = purpose.candidates(snapshot, this_node);
    let modal_area = center(area, 64, 22);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::red())
        .title(Line::from(vec![
            Span::styled(" ❄ ", theme::red()),
            Span::styled(
                "ICE  PICK NODE",
                Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // headline
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // peer list
            Constraint::Length(1), // bindings
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            purpose.headline(),
            Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
        )]))
        .alignment(Alignment::Center),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(purpose.hint(), theme::dim())]))
            .alignment(Alignment::Center),
        rows[1],
    );

    // Peer list. Show up to (inner height - bindings) peers
    // around the cursor; scroll if more than fit.
    let list_height = rows[3].height as usize;
    let lines = peer_lines(snapshot, &peers, cursor, list_height);
    frame.render_widget(Paragraph::new(lines), rows[3]);

    let bindings = Line::from(vec![
        Span::styled("[j/k]", theme::green_hi()),
        Span::styled(" cursor    ", theme::dim()),
        Span::styled("[Enter]", theme::red()),
        Span::styled(" select    ", theme::dim()),
        Span::styled("[Esc]", theme::dim()),
        Span::styled(" cancel", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[4],
    );
}

fn peer_lines(
    snapshot: &MeshOsSnapshot,
    peers: &[u64],
    cursor: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if peers.is_empty() {
        return vec![Line::from(vec![Span::styled(
            "no peers to pick from",
            theme::dim(),
        )])];
    }
    let cursor = cursor.min(peers.len() - 1);
    // Centered scroll: keep cursor mid-window where possible.
    let half = height / 2;
    let start = cursor.saturating_sub(half);
    let end = (start + height).min(peers.len());
    let start = end.saturating_sub(height);

    peers[start..end]
        .iter()
        .enumerate()
        .map(|(i, peer_id)| {
            let abs = start + i;
            let is_cursor = abs == cursor;
            let marker = if is_cursor { "▶ " } else { "  " };
            let id_style = if is_cursor {
                theme::green_hi()
            } else {
                theme::text()
            };
            let mut spans = vec![Span::styled(marker, theme::green_hi())];
            spans.extend(nodes::id_spans_styled(&format!("0x{peer_id:x}"), id_style));
            // health badge
            if let Some(p) = snapshot.peers.get(peer_id) {
                let (health_style, health_text) = match p.health {
                    Some(PeerHealthSnapshot::Healthy) => (theme::green(), " · Healthy"),
                    Some(PeerHealthSnapshot::Degraded) => (theme::amber(), " · Degraded"),
                    Some(PeerHealthSnapshot::Unreachable) => (theme::red(), " · Unreachable"),
                    _ => (theme::chrome(), " · —"),
                };
                spans.push(Span::styled(health_text, health_style));
                if let Some(ms) = p.rtt_ms {
                    spans.push(Span::styled(format!("  RTT {ms}ms"), theme::dim()));
                }
            }
            Line::from(spans)
        })
        .collect()
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
