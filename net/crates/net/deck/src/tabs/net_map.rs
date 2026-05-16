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
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    let live_peers: Option<Vec<LiveNode>> = snapshot
        .filter(|s| !s.peers.is_empty())
        .map(project_live_peers);

    let title_text = match live_peers.as_ref() {
        Some(peers) => format!("   {} live nodes", peers.len()),
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
            // Move-by-clone into the closure — the canvas
            // `paint` borrow must outlive the call, and
            // ratatui captures by Fn (not FnOnce).
            let canvas = Canvas::default()
                .block(title_block)
                .marker(Marker::Braille)
                .x_bounds([-80.0, 80.0])
                .y_bounds([-45.0, 70.0])
                .paint(move |ctx| paint_live_graph(ctx, &peers));
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
    // peer sits at radius proportional to its measured RTT
    // from this node, with the angle deterministically hashed
    // from the node id so the layout is stable across ticks.
    // Peers with no RTT sample land at the outer edge so
    // they're visible but read as "unranked." Reflects the
    // real cluster topology Deck has access to today —
    // pairwise distances would need a 2D MDS over the full
    // proximity graph; that's a future refinement.
    let max_rtt_us: u64 = snapshot
        .peers
        .values()
        .filter_map(|p| p.rtt_ms.map(|ms| ms.saturating_mul(1_000)))
        .max()
        .unwrap_or(1);
    snapshot
        .peers
        .iter()
        .map(|(id, p)| {
            let angle = angle_for(*id);
            // RTT samples come through as milliseconds in the
            // snapshot; the demo probe sources microseconds
            // but the snapshot fold stores them as ms. Either
            // way the ratio against the max is what places
            // the peer on its radial.
            let rtt_us = p
                .rtt_ms
                .map(|ms| ms.saturating_mul(1_000))
                .unwrap_or(max_rtt_us);
            let radius_unit = (rtt_us as f64 / max_rtt_us.max(1) as f64).clamp(0.05, 1.0);
            // Canvas extent: ~70 on each axis with a small
            // margin. Multiply by 0.55 to keep most peers
            // inside the visible window even after the
            // y-axis is squished (terminal cells are taller
            // than wide, so a circle reads as a vertical
            // ellipse without compensation).
            let radius_x = radius_unit * 60.0;
            let radius_y = radius_unit * 36.0;
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

fn paint_live_graph(ctx: &mut Context<'_>, peers: &[LiveNode]) {
    // No edges yet — replica-derived adjacency lands in a
    // follow-up slice once the demo seeds `snapshot.replicas`.
    for n in peers {
        let (glyph, color) = glyph_for_health(n.health);
        ctx.print(
            n.x,
            n.y,
            Line::styled(
                glyph.to_string(),
                ratatui::style::Style::default().fg(color),
            ),
        );
        ctx.print(
            n.x + 2.5,
            n.y - 2.0,
            Line::styled(format!("0x{:x}", n.id), theme::dim()),
        );
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
        Span::styled("UNREACHABLE", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(legend).alignment(Alignment::Right), area);
}
