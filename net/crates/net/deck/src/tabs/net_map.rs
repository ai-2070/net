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
    snapshot
        .peers
        .iter()
        .map(|(id, p)| {
            let (x, y) = hash_position(*id);
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

/// Deterministic 2D position for a node id. Two cheap xorshift
/// hashes seeded from low / high halves; the resulting `(x, y)`
/// is stable across renders so the graph doesn't jitter.
fn hash_position(id: u64) -> (f64, f64) {
    let lo = id & 0xffff_ffff;
    let hi = id >> 32;
    let mut a = (lo as u32).wrapping_mul(0x9e37_79b9) ^ 0xdead_beef;
    let mut b = ((hi ^ 0x1234_5678) as u32).wrapping_mul(0x85eb_ca6b) ^ 0xc0ff_ee01;
    a ^= a >> 16;
    a = a.wrapping_mul(0x7feb_352d);
    a ^= a >> 15;
    b ^= b >> 16;
    b = b.wrapping_mul(0xa9d6_d4a3);
    b ^= b >> 15;
    let x = -70.0 + ((a as f64) / (u32::MAX as f64)) * 140.0;
    let y = -38.0 + ((b as f64) / (u32::MAX as f64)) * 100.0;
    (x, y)
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
