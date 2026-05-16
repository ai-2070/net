//! Full-page node detail. Reached by pressing `[Enter]` on a
//! cursored node row in LIST (or a cursored peer on NET.MAP).
//! `[Esc]` returns to the originating tab.
//!
//! Layout:
//! - top panel (PEER SNAPSHOT): every `PeerSnapshot` field —
//!   health / RTT / maintenance / inventory axes
//! - bottom panel (PLACEMENT): daemons placed on this node +
//!   chains the node holds, both projected from the live
//!   snapshot at render time
//!
//! Snapshots `PeerSnapshot` at focus time so the upper body
//! stays stable across a subsequent tick; placement / daemon
//! data is read live so the page reflects the cluster as it
//! evolves.

use std::collections::BTreeSet;

use net_sdk::deck::{
    MaintenanceMirrorSnapshot, MeshOsSnapshot, PeerHealthSnapshot, PeerSnapshot,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme;

/// Snapshot of the focused peer + its id. The page owns its
/// copy of the `PeerSnapshot` so a subsequent tick under the
/// focused id doesn't shift the upper body the operator is
/// reading.
#[derive(Clone, Debug)]
pub struct NodeFocusEntry {
    pub id: u64,
    pub label: Option<String>,
    pub peer: PeerSnapshot,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &NodeFocusEntry,
    live: &MeshOsSnapshot,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(16), Constraint::Min(0)])
        .split(area);
    render_peer_panel(frame, rows[0], entry);
    render_placement_panel(frame, rows[1], entry, live);
}

fn render_peer_panel(frame: &mut Frame<'_>, area: Rect, entry: &NodeFocusEntry) {
    let id_label = match entry.label.as_deref() {
        Some(label) => format!("0x{:x}.{}", entry.id, label),
        None => format!("0x{:x}", entry.id),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NODE", theme::green_hi()),
            Span::styled(format!("    {id_label}"), theme::text()),
            Span::styled("    [Esc] back", theme::dim()),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Two columns: identity / health on the left, inventory
    // axes on the right. The page has the body width to spread
    // the fields out — no more stacked single-column shape
    // from the prior modal.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    render_identity_column(frame, cols[0], entry, &id_label);
    render_inventory_column(frame, cols[1], entry);
}

fn render_identity_column(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &NodeFocusEntry,
    id_label: &str,
) {
    let (health_text, health_style) = match entry.peer.health {
        Some(PeerHealthSnapshot::Healthy) => ("Healthy", theme::green()),
        Some(PeerHealthSnapshot::Degraded) => ("Degraded", theme::amber()),
        Some(PeerHealthSnapshot::Unreachable) => ("Unreachable", theme::red()),
        _ => ("—", theme::chrome()),
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // id
            Constraint::Length(1), // health
            Constraint::Length(1), // rtt
            Constraint::Length(1), // maintenance
            Constraint::Length(1), // version
            Constraint::Length(1), // fork_of
            Constraint::Min(0),    // pad
        ])
        .split(area);

    frame.render_widget(kv("id        ", id_label, theme::green_hi()), rows[0]);
    frame.render_widget(kv("health    ", health_text, health_style), rows[1]);
    frame.render_widget(
        kv(
            "rtt       ",
            &entry
                .peer
                .rtt_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "—".to_string()),
            theme::text(),
        ),
        rows[2],
    );
    frame.render_widget(
        kv(
            "maint     ",
            &maint_label(entry.peer.maintenance),
            theme::cyan(),
        ),
        rows[3],
    );
    frame.render_widget(
        kv(
            "version   ",
            entry.peer.software_version.as_deref().unwrap_or("—"),
            theme::text(),
        ),
        rows[4],
    );
    frame.render_widget(
        kv(
            "fork_of   ",
            &entry
                .peer
                .forked_from
                .map(|id| format!("0x{id:x}"))
                .unwrap_or_else(|| "—".to_string()),
            theme::text(),
        ),
        rows[5],
    );
}

fn render_inventory_column(frame: &mut Frame<'_>, area: Rect, entry: &NodeFocusEntry) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // cpu
            Constraint::Length(1), // mem
            Constraint::Length(1), // disk
            Constraint::Length(1), // sat
            Constraint::Length(1), // caps header
            Constraint::Min(0),    // caps list
        ])
        .split(area);

    frame.render_widget(
        kv(
            "cpu_1m    ",
            &entry
                .peer
                .cpu_load_1m
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "—".to_string()),
            theme::text(),
        ),
        rows[0],
    );
    frame.render_widget(
        kv(
            "memory    ",
            &fmt_used_total(entry.peer.mem_used_bytes, entry.peer.mem_total_bytes),
            theme::text(),
        ),
        rows[1],
    );
    frame.render_widget(
        kv(
            "disk      ",
            &fmt_used_total(entry.peer.disk_used_bytes, entry.peer.disk_total_bytes),
            theme::text(),
        ),
        rows[2],
    );
    let (sat_text, sat_style) = match entry.peer.saturation_trend {
        Some(s) if s >= 0.8 => (format!("{s:.2}"), theme::red()),
        Some(s) if s >= 0.5 => (format!("{s:.2}"), theme::amber()),
        Some(s) => (format!("{s:.2}"), theme::green()),
        None => ("—".to_string(), theme::chrome()),
    };
    frame.render_widget(kv("sat       ", &sat_text, sat_style), rows[3]);

    // Capabilities — one per line (page has the vertical room
    // for it now). Empty → dim "—".
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "  caps      ",
            theme::chrome(),
        )])),
        rows[4],
    );
    if entry.peer.capability_set.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "    —",
                theme::dim(),
            )])),
            rows[5],
        );
    } else {
        let caps_lines: Vec<Line> = entry
            .peer
            .capability_set
            .iter()
            .map(|c| {
                Line::from(vec![
                    Span::styled("    · ", theme::chrome()),
                    Span::styled(c.clone(), theme::text()),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(caps_lines), rows[5]);
    }
}

fn render_placement_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &NodeFocusEntry,
    live: &MeshOsSnapshot,
) {
    let daemons_here: Vec<(u64, &net_sdk::deck::DaemonSnapshot)> = live
        .daemons
        .iter()
        .filter(|(_, d)| d.placement == entry.id)
        .map(|(id, d)| (*id, d))
        .collect();
    let chains_here: Vec<u64> = live
        .replicas
        .iter()
        .filter(|(_, r)| r.holders.contains(&entry.id))
        .map(|(c, _)| *c)
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("PLACEMENT", theme::green_hi()),
            Span::styled(
                format!(
                    "    {} daemons · {} chains held",
                    daemons_here.len(),
                    chains_here.len()
                ),
                theme::chrome(),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    // Daemons on this node.
    let mut daemon_lines: Vec<Line> = Vec::with_capacity(daemons_here.len() + 1);
    daemon_lines.push(Line::from(vec![Span::styled(
        "  DAEMONS",
        theme::chrome(),
    )]));
    if daemons_here.is_empty() {
        daemon_lines.push(Line::from(vec![Span::styled(
            "    none",
            theme::dim(),
        )]));
    } else {
        for (id, d) in &daemons_here {
            daemon_lines.push(Line::from(vec![
                Span::styled("    · ", theme::chrome()),
                Span::styled(format!("daemon.0x{id:x}"), theme::cyan()),
                Span::styled("  ", theme::chrome()),
                Span::styled(d.name.clone(), theme::text()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(daemon_lines), cols[0]);

    // Chains held by this node.
    let mut chain_lines: Vec<Line> = Vec::with_capacity(chains_here.len() + 1);
    chain_lines.push(Line::from(vec![Span::styled(
        "  CHAINS HELD",
        theme::chrome(),
    )]));
    if chains_here.is_empty() {
        chain_lines.push(Line::from(vec![Span::styled(
            "    none",
            theme::dim(),
        )]));
    } else {
        for chain in &chains_here {
            chain_lines.push(Line::from(vec![
                Span::styled("    · ", theme::chrome()),
                Span::styled(format!("chain.0x{chain:x}"), theme::text()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(chain_lines), cols[1]);
}

fn kv<'a>(label: &'a str, value: &'a str, value_style: ratatui::style::Style) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(format!("  {label}"), theme::chrome()),
        Span::styled(value.to_string(), value_style),
    ]))
}

fn maint_label(mirror: Option<MaintenanceMirrorSnapshot>) -> String {
    match mirror {
        Some(MaintenanceMirrorSnapshot::Active) => "active".into(),
        Some(MaintenanceMirrorSnapshot::EnteringMaintenance) => "draining".into(),
        Some(MaintenanceMirrorSnapshot::Maintenance) => "maintenance".into(),
        Some(MaintenanceMirrorSnapshot::ExitingMaintenance) => "exiting".into(),
        Some(MaintenanceMirrorSnapshot::DrainFailed) => "DRAIN-FAILED".into(),
        Some(MaintenanceMirrorSnapshot::Recovery) => "recovery".into(),
        _ => "—".into(),
    }
}

fn fmt_used_total(used: Option<u64>, total: Option<u64>) -> String {
    match (used, total) {
        (Some(u), Some(t)) if t > 0 => {
            let pct = (u * 100 / t).min(999);
            format!("{} / {}  ({pct}%)", fmt_bytes(u), fmt_bytes(t))
        }
        _ => "—".to_string(),
    }
}

fn fmt_bytes(b: u64) -> String {
    const K: u64 = 1 << 10;
    const M: u64 = 1 << 20;
    const G: u64 = 1 << 30;
    const T: u64 = 1 << 40;
    if b >= T {
        format!("{:.1} TB", b as f64 / T as f64)
    } else if b >= G {
        format!("{:.1} GB", b as f64 / G as f64)
    } else if b >= M {
        format!("{:.1} MB", b as f64 / M as f64)
    } else if b >= K {
        format!("{:.1} KB", b as f64 / K as f64)
    } else {
        format!("{b} B")
    }
}

// `BTreeSet<String>` parameter retained for future fan-out
// (e.g. a richer capability detail row); referenced from the
// crate's import set so the type stays in scope.
#[allow(dead_code)]
fn _caps_marker(_caps: &BTreeSet<String>) {}
