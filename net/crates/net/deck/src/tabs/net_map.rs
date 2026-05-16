//! NET.MAP tab — proximity graph + mesh event tail.
//!
//! Top half: a canvas drawing peers at radial positions seeded
//! from the substrate's RTT probe, then refined by a few passes
//! of pairwise repulsion so co-located peers (same RTT band)
//! don't overlap. Edges connect each peer to its k-nearest
//! neighbours after the spread pass.
//!
//! Per-role glyphs:
//!   ◆ NODE        · default
//!   ■ DATAFORTS   · peer carries `dataforts.blob.storage`
//!
//! Title carries live counts: `{n} nodes · {m} edges · {d} dataforts`.
//!
//! Bottom half: MESH.EVENTS — tail of the most recent log records
//! shaped like a `tail -f` feed. Same data the LOGS tab reads;
//! this is the at-a-glance "what's happening across the mesh"
//! view that pairs with the spatial layout above.

use net_sdk::deck::{LogRecord, MeshOsSnapshot, PeerHealthSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Context},
        Block, Borders, Paragraph,
    },
    Frame,
};

use crate::{theme, widgets};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    _tick: u64,
    snapshot: Option<&MeshOsSnapshot>,
    cursor: usize,
    logs: &[LogRecord],
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // graph — gets the slack
            Constraint::Length(7), // MESH.EVENTS panel — title + 4 rows + bottom border
            Constraint::Length(2), // legend
        ])
        .split(area);

    render_graph(frame, rows[0], snapshot, cursor);
    render_events(frame, rows[1], logs);
    render_legend(frame, rows[2]);
}

// ───────────────────────── graph ─────────────────────────

fn render_graph(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&MeshOsSnapshot>,
    cursor: usize,
) {
    // Compute the layout + edge set once per frame. Both
    // `spread_overlaps` and `nearest_edges` are O(n²) in the
    // peer count; doing them once and threading through the
    // title + the canvas avoids paying twice on every paint.
    let layout: Option<(Vec<LiveNode>, Vec<(usize, usize)>)> = snapshot
        .filter(|s| !s.peers.is_empty())
        .map(|s| {
            let nodes = project_live_peers(s);
            let edges = nearest_edges(&nodes, 2);
            (nodes, edges)
        });

    let title_text = match layout.as_ref() {
        Some((peers, edges)) => {
            let n = peers.len();
            let datafort_count = peers
                .iter()
                .filter(|p| p.role == NodeRole::Datafort)
                .count();
            let pos = cursor.min(n.saturating_sub(1)) + 1;
            format!(
                "    {} nodes · {} edges · {} dataforts    {}/{}",
                n,
                edges.len(),
                datafort_count,
                pos,
                n
            )
        }
        None => "    no peers".to_string(),
    };
    let header = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("MESH.PROXIMITY", theme::green_hi()),
        Span::styled(title_text, theme::chrome()),
    ]);
    let title_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(header)
        .title_alignment(Alignment::Left);

    match layout {
        Some((peers, edges)) => {
            let n = peers.len();
            let cursor = cursor.min(n.saturating_sub(1));
            let canvas = Canvas::default()
                .block(title_block)
                .marker(Marker::Braille)
                .x_bounds([-80.0, 80.0])
                .y_bounds([-50.0, 50.0])
                .paint(move |ctx| paint_live_graph(ctx, &peers, &edges, cursor));
            frame.render_widget(canvas, area);
        }
        None => {
            let inner = title_block.inner(area);
            frame.render_widget(title_block, area);
            widgets::empty::render(
                frame,
                inner,
                "no peers reported yet",
                "wire a proximity / health probe",
            );
        }
    }
}

// ───────────────────────── live projection ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeRole {
    Node,
    Datafort,
}

struct LiveNode {
    id: u64,
    label: Option<String>,
    x: f64,
    y: f64,
    health: PeerHealthSnapshot,
    role: NodeRole,
}

fn project_live_peers(snapshot: &MeshOsSnapshot) -> Vec<LiveNode> {
    let mut nodes = radial_layout(snapshot);
    spread_overlaps(&mut nodes);
    nodes
}

fn radial_layout(snapshot: &MeshOsSnapshot) -> Vec<LiveNode> {
    let observed: Vec<u64> = snapshot.peers.values().filter_map(|p| p.rtt_ms).collect();
    let min_rtt = observed.iter().copied().min().unwrap_or(0);
    let max_rtt = observed.iter().copied().max().unwrap_or(0);
    let range = max_rtt.saturating_sub(min_rtt).max(1);

    snapshot
        .peers
        .iter()
        .map(|(id, p)| {
            let angle = angle_for(*id);
            let rtt_ms = p.rtt_ms.unwrap_or(max_rtt);
            let normalized = (rtt_ms.saturating_sub(min_rtt)) as f64 / range as f64;
            let radius_unit = (normalized * 0.8 + 0.15).clamp(0.15, 0.95);
            let radius_x = radius_unit * 72.0;
            let radius_y = radius_unit * 45.0;
            let x = radius_x * angle.cos();
            let y = radius_y * angle.sin();
            let health = p.health.unwrap_or(PeerHealthSnapshot::Healthy);
            let label = crate::nodes::label_for(&format!("0x{:x}", *id), &p.capability_set);
            let role = classify_role(&p.capability_set);
            LiveNode {
                id: *id,
                label,
                x,
                y,
                health,
                role,
            }
        })
        .collect()
}

/// Push overlapping nodes apart with pairwise repulsion. The
/// radial layout puts peers with similar RTT at the same
/// radius, so a region with 5 close peers can clump on a thin
/// arc. A few iterations of repulsion (deterministic — no
/// randomness) thin the clumps out while preserving the
/// overall layout topology.
fn spread_overlaps(nodes: &mut [LiveNode]) {
    const ITERATIONS: usize = 120;
    /// Minimum desired separation between any two nodes, in
    /// canvas units. Sized to keep id labels (`0xXXXX` ≈ 6
    /// columns wide) from clobbering each other.
    const MIN_DIST: f64 = 22.0;
    const STRENGTH: f64 = 0.5;
    /// Canvas-clamp bounds — match the canvas `*_bounds` in
    /// render_graph with a small margin so labels fit.
    const X_MIN: f64 = -76.0;
    const X_MAX: f64 = 60.0; // narrower on the right so the cursor id_label fits
    const Y_MIN: f64 = -47.0;
    const Y_MAX: f64 = 47.0;

    for _ in 0..ITERATIONS {
        let n = nodes.len();
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = nodes[i].x - nodes[j].x;
                let dy = nodes[i].y - nodes[j].y;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < MIN_DIST && dist > 0.001 {
                    let push = (MIN_DIST - dist) * STRENGTH / dist;
                    let pdx = dx * push;
                    let pdy = dy * push;
                    nodes[i].x += pdx;
                    nodes[i].y += pdy;
                    nodes[j].x -= pdx;
                    nodes[j].y -= pdy;
                }
            }
        }
        for n in nodes.iter_mut() {
            n.x = n.x.clamp(X_MIN, X_MAX);
            n.y = n.y.clamp(Y_MIN, Y_MAX);
        }
    }
}

fn classify_role(caps: &std::collections::BTreeSet<String>) -> NodeRole {
    if caps.iter().any(|c| c == "dataforts.blob.storage") {
        NodeRole::Datafort
    } else {
        NodeRole::Node
    }
}

fn angle_for(id: u64) -> f64 {
    let mut s = id.wrapping_add(0x9e37_79b9_7f4a_7c15);
    s = (s ^ (s >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    s ^= s >> 31;
    // Drop the bottom 11 bits and use the 53 mantissa-bit
    // payload directly — the prior `(s as u32) as f64` cast
    // threw away half the entropy and could collide nearby
    // node ids onto identical angles.
    let unit = (s >> 11) as f64 / ((1u64 << 53) as f64);
    unit * std::f64::consts::TAU
}

// ───────────────────────── edges ─────────────────────────

/// For each peer, link to its k nearest neighbours by Euclidean
/// distance in graph coordinates. De-duplicated so an edge
/// (a, b) is the same as (b, a). Uses a HashSet for dedup so
/// large peer sets don't pay O(n²k) on the per-frame
/// `Vec::contains` scan.
fn nearest_edges(peers: &[LiveNode], k: usize) -> Vec<(usize, usize)> {
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (i, a) in peers.iter().enumerate() {
        let mut ranked: Vec<(usize, f64)> = peers
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(j, b)| {
                let dx = a.x - b.x;
                let dy = a.y - b.y;
                (j, dx * dx + dy * dy)
            })
            .collect();
        ranked.sort_by(|(_, da), (_, db)| da.partial_cmp(db).unwrap_or(std::cmp::Ordering::Equal));
        for (j, _) in ranked.into_iter().take(k) {
            let pair = if i < j { (i, j) } else { (j, i) };
            if seen.insert(pair) {
                edges.push(pair);
            }
        }
    }
    edges
}

fn paint_live_graph(
    ctx: &mut Context<'_>,
    peers: &[LiveNode],
    edges: &[(usize, usize)],
    cursor: usize,
) {
    // Edges first so node glyphs sit on top of them.
    for (a, b) in edges {
        let (Some(pa), Some(pb)) = (peers.get(*a), peers.get(*b)) else {
            continue;
        };
        // Sample dots along the segment so the edge reads as a
        // dotted line at terminal resolution.
        let steps = 12;
        for s in 1..steps {
            let t = s as f64 / steps as f64;
            let x = pa.x + (pb.x - pa.x) * t;
            let y = pa.y + (pb.y - pa.y) * t;
            ctx.print(
                x,
                y,
                Line::styled("·", ratatui::style::Style::default().fg(theme::RULE)),
            );
        }
    }

    // Two-pass node paint so the cursored node wins z-order
    // against neighboring gray labels.
    for (i, n) in peers.iter().enumerate() {
        if i == cursor {
            continue;
        }
        let (glyph, color) = glyph_for(n);
        ctx.print(
            n.x,
            n.y,
            Line::styled(
                glyph.to_string(),
                ratatui::style::Style::default().fg(color),
            ),
        );
        ctx.print(
            n.x + 3.0,
            n.y,
            Line::styled(format!("0x{:x}", n.id), theme::chrome()),
        );
    }

    if let Some(n) = peers.get(cursor) {
        let (glyph, _) = glyph_for(n);
        let cursor_style = ratatui::style::Style::default()
            .fg(theme::GREEN_HI)
            .add_modifier(ratatui::style::Modifier::BOLD);
        ctx.print(n.x, n.y, Line::styled(glyph.to_string(), cursor_style));
        ctx.print(n.x - 2.5, n.y, Line::styled("[", cursor_style));
        ctx.print(n.x + 2.5, n.y, Line::styled("]", cursor_style));
        let id_label = match n.label.as_deref() {
            Some(label) => format!("0x{:x}.{label}", n.id),
            None => format!("0x{:x}", n.id),
        };
        ctx.print(n.x + 4.5, n.y, Line::styled(id_label, theme::green_hi()));
    }
}

fn glyph_for(n: &LiveNode) -> (char, ratatui::style::Color) {
    let color = match n.health {
        PeerHealthSnapshot::Healthy => theme::GREEN_HI,
        PeerHealthSnapshot::Degraded => theme::AMBER,
        PeerHealthSnapshot::Unreachable => theme::RED,
        _ => theme::TEXT,
    };
    // Unreachable peers get the hollow diamond the legend
    // advertises so the operator's eye picks them out even at
    // a glance — color alone reads as "amber-but-redder" on a
    // dense map and the legend then doesn't match the canvas.
    let glyph = match (n.role, n.health) {
        (_, PeerHealthSnapshot::Unreachable) => '◇',
        (NodeRole::Node, _) => '◆',
        (NodeRole::Datafort, _) => '■',
    };
    (glyph, color)
}

// ───────────────────────── mesh events ─────────────────────────

fn render_events(frame: &mut Frame<'_>, area: Rect, logs: &[LogRecord]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("MESH.EVENTS", theme::green_hi()),
            Span::styled(format!("    {} records", logs.len()), theme::chrome()),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if logs.is_empty() {
        widgets::empty::render(
            frame,
            inner,
            "no mesh events yet",
            "daemons + admin actions land here as they fire",
        );
        return;
    }

    let take = (inner.height as usize).max(1);
    let start = logs.len().saturating_sub(take);
    let mut lines: Vec<Line> = Vec::with_capacity(take);
    for r in &logs[start..] {
        // Shared `render_event_line` produces the combined
        // `TS  ICON  source  message` shape; the LOGS tab uses
        // the same helper so both surfaces speak the same
        // visual vocabulary.
        lines.push(super::render_event_line(r));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────── legend ─────────────────────────

fn render_legend(frame: &mut Frame<'_>, area: Rect) {
    let legend = Line::from(vec![
        Span::styled("◆ ", theme::green_hi()),
        Span::styled("NODE   ", theme::dim()),
        Span::styled("■ ", theme::green_hi()),
        Span::styled("DATAFORT   ", theme::dim()),
        Span::styled("◆ ", theme::amber()),
        Span::styled("DEGRADED   ", theme::dim()),
        Span::styled("◇ ", theme::red()),
        Span::styled("UNREACHABLE   ", theme::dim()),
        Span::styled("[Enter]", theme::green_hi()),
        Span::styled(" detail", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(legend).alignment(Alignment::Right), area);
}
