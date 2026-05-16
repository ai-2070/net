use net_sdk::deck::{MeshOsSnapshot, PeerHealthSnapshot};
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
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    let live_peers: Option<Vec<LiveNode>> = snapshot
        .filter(|s| !s.peers.is_empty())
        .map(project_live_peers);

    let title_text = match live_peers.as_ref() {
        Some(peers) => {
            let n = peers.len();
            let pos = cursor.min(n.saturating_sub(1)) + 1;
            format!("   {n} live nodes    {pos}/{n}")
        }
        None => "   no peers".to_string(),
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

    match live_peers {
        Some(peers) => {
            let n = peers.len();
            let cursor = cursor.min(n.saturating_sub(1));
            // Move-by-clone into the closure — the canvas
            // `paint` borrow must outlive the call, and
            // ratatui captures by Fn (not FnOnce).
            let canvas = Canvas::default()
                .block(title_block)
                .marker(Marker::Braille)
                .x_bounds([-80.0, 80.0])
                .y_bounds([-45.0, 70.0])
                .paint(move |ctx| paint_live_graph(ctx, &peers, cursor));
            frame.render_widget(canvas, rows[0]);
        }
        None => {
            let inner = title_block.inner(rows[0]);
            frame.render_widget(title_block, rows[0]);
            widgets::empty::render(
                frame,
                inner,
                "no peers reported yet",
                "wire a proximity / health probe — or run with --features samples",
            );
        }
    }

    render_legend(frame, rows[1]);
}

// ───────────────────────── live projection ─────────────────────────

struct LiveNode {
    id: u64,
    x: f64,
    y: f64,
    health: PeerHealthSnapshot,
}

fn project_live_peers(snapshot: &MeshOsSnapshot) -> Vec<LiveNode> {
    // Radial layout off the substrate's proximity probe: each
    // peer sits at a radius derived from its measured RTT from
    // this node, with the angle deterministically hashed from
    // the node id so the layout is stable across ticks.
    //
    // Normalization is min-max across the observed RTT range,
    // not max-only. One outlier (a far-away peer) under
    // max-only normalization compresses every closer peer into
    // the central 20% of the canvas; min-max spreads the
    // range across the full radial extent so the cluster
    // topology actually reads.
    let observed: Vec<u64> = snapshot
        .peers
        .values()
        .filter_map(|p| p.rtt_ms)
        .collect();
    let min_rtt = observed.iter().copied().min().unwrap_or(0);
    let max_rtt = observed.iter().copied().max().unwrap_or(0);
    let range = max_rtt.saturating_sub(min_rtt).max(1);

    snapshot
        .peers
        .iter()
        .map(|(id, p)| {
            let angle = angle_for(*id);
            // Peers with no RTT sample land at the outer edge
            // so they're visible but read as "unranked."
            let rtt_ms = p.rtt_ms.unwrap_or(max_rtt);
            let normalized = (rtt_ms.saturating_sub(min_rtt)) as f64 / range as f64;
            // Floor at 0.15 so the closest peer isn't pinned to
            // the origin (would collide with future "this node"
            // marker + obscures the relative positions of other
            // close peers); ceiling at 0.95 leaves headroom for
            // labels at the outer edge.
            let radius_unit = (normalized * 0.8 + 0.15).clamp(0.15, 0.95);
            // Canvas extent: ~70 on each axis with a small
            // margin. Y squished vs X because terminal cells
            // are taller than wide; without compensation a
            // mathematical circle reads as a vertical ellipse.
            let radius_x = radius_unit * 65.0;
            let radius_y = radius_unit * 38.0;
            let x = radius_x * angle.cos();
            let y = radius_y * angle.sin();
            let health = p.health.unwrap_or(PeerHealthSnapshot::Healthy);
            LiveNode {
                id: *id,
                x,
                y,
                health,
            }
        })
        .collect()
}

/// Deterministic angular position (radians) for a node id.
/// splitmix64 → 32-bit unit fraction → `0..2π`. Stable across
/// renders so the graph doesn't jitter, and well-distributed
/// for sparse inputs (the demo node ids are 16-bit).
fn angle_for(id: u64) -> f64 {
    let mut s = id.wrapping_add(0x9e37_79b9_7f4a_7c15);
    s = (s ^ (s >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    s ^= s >> 31;
    let unit = (s as u32) as f64 / u32::MAX as f64;
    unit * std::f64::consts::TAU
}

fn paint_live_graph(ctx: &mut Context<'_>, peers: &[LiveNode], cursor: usize) {
    // Labels only on the cursored node — every-node labels
    // collide at small radii. Operators reach detail via
    // `Enter` → NodeDetail modal anyway. The cursored node
    // also gets a brighter glyph + a `[` bracket marker so the
    // selection is visible at a glance.
    for (i, n) in peers.iter().enumerate() {
        let is_cursor = i == cursor;
        let (glyph, color) = glyph_for_health(n.health);
        let glyph_style = if is_cursor {
            ratatui::style::Style::default()
                .fg(theme::GREEN_HI)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            ratatui::style::Style::default().fg(color)
        };
        ctx.print(n.x, n.y, Line::styled(glyph.to_string(), glyph_style));
        if is_cursor {
            // Bracket the selection.
            ctx.print(
                n.x - 2.5,
                n.y,
                Line::styled("[", ratatui::style::Style::default().fg(theme::GREEN_HI)),
            );
            ctx.print(
                n.x + 2.5,
                n.y,
                Line::styled("]", ratatui::style::Style::default().fg(theme::GREEN_HI)),
            );
            ctx.print(
                n.x + 4.5,
                n.y - 2.0,
                Line::styled(format!("0x{:x}", n.id), theme::green_hi()),
            );
        }
    }
}

fn glyph_for_health(h: PeerHealthSnapshot) -> (char, ratatui::style::Color) {
    match h {
        PeerHealthSnapshot::Healthy => ('◆', theme::GREEN_HI),
        PeerHealthSnapshot::Degraded => ('◆', theme::AMBER),
        PeerHealthSnapshot::Unreachable => ('◇', theme::RED),
        _ => ('◆', theme::TEXT),
    }
}

fn render_legend(frame: &mut Frame<'_>, area: Rect) {
    let legend = Line::from(vec![
        Span::styled("◆ ", theme::green_hi()),
        Span::styled("HEALTHY   ", theme::dim()),
        Span::styled("◆ ", theme::amber()),
        Span::styled("DEGRADED   ", theme::dim()),
        Span::styled("◇ ", theme::red()),
        Span::styled("UNREACHABLE   ", theme::dim()),
        Span::styled("[Enter]", theme::green_hi()),
        Span::styled(" detail", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(legend).alignment(Alignment::Right), area);
}
