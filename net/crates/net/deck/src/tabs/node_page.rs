//! Full-page node detail. Reached by pressing `[Enter]` on a
//! cursored node row in LIST (or a cursored peer on NET.MAP).
//! `[Esc]` returns to the originating tab.
//!
//! Layout:
//! - top panel (NODE):
//!   - left column: identity (id / health / rtt / maint /
//!     version / fork_of)
//!   - right column: resource bars (cpu / memory / disk / sat)
//!   - bottom row: capabilities as a horizontal flow
//! - bottom panel (PLACEMENT): daemons placed on this node +
//!   chains the node holds, side-by-side, read live
//!
//! Snapshots `PeerSnapshot` at focus time so the body stays
//! stable across a subsequent tick; placement / daemon data
//! is read live so the page reflects the cluster as it
//! evolves.

use net_sdk::deck::{
    MaintenanceMirrorSnapshot, MeshOsSnapshot, PeerHealthSnapshot, PeerSnapshot,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme;

/// Snapshot of the focused peer + its id.
#[derive(Clone, Debug)]
pub struct NodeFocusEntry {
    pub id: u64,
    pub label: Option<String>,
    pub peer: PeerSnapshot,
}

/// Minimal datafort view rendered on the NODE page when the
/// focused node advertises `dataforts.blob.storage`. For the
/// local datafort the deck populates the adapter list; for a
/// remote datafort the list is empty and the panel shows just
/// the cap badges (the deck has no remote-adapter probe today).
#[derive(Clone, Debug, Default)]
pub struct DatafortView {
    pub is_local: bool,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub overflow_enabled: bool,
    pub overflow_active: bool,
    pub adapters: Vec<DatafortAdapterRow>,
}

#[derive(Clone, Debug)]
pub struct DatafortAdapterRow {
    pub id: String,
    pub disk_used_bytes: u64,
    pub disk_capacity_bytes: u64,
    pub overflow_enabled: bool,
    pub overflow_active: bool,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &NodeFocusEntry,
    live: &MeshOsSnapshot,
    datafort: Option<&DatafortView>,
) {
    // NODE panel grows with the capability tree so a node with
    // deep capability subtrees (sensor.radar.shortwave …) still
    // gets the full tree drawn instead of being clipped. Clamp to
    // leave room for PLACEMENT + the optional DATAFORT section.
    let cap_lines = count_cap_lines(&entry.peer.capability_set);
    let needed = 2 /* borders */ + 6 /* main rows */ + 1 /* spacer */ + cap_lines + 1;
    let datafort_h: u16 = match datafort {
        Some(v) if v.is_local && !v.adapters.is_empty() => {
            // 1 status row + 1 row per adapter + 2 borders
            (v.adapters.len() as u16 + 3).min(10)
        }
        Some(_) => 4, // remote: 2 content rows + 2 borders
        None => 0,
    };
    let panel_h = (needed as u16)
        .max(12)
        .min(area.height.saturating_sub(8 + datafort_h));
    let constraints: Vec<Constraint> = if datafort_h > 0 {
        vec![
            Constraint::Length(panel_h),     // NODE
            Constraint::Length(datafort_h),  // DATAFORT
            Constraint::Min(0),              // PLACEMENT
            Constraint::Length(2),           // hint row + spacer
        ]
    } else {
        vec![
            Constraint::Length(panel_h),
            Constraint::Min(0),
            Constraint::Length(2),
        ]
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    render_peer_panel(frame, rows[0], entry);
    let hint_row;
    if let Some(v) = datafort {
        render_datafort_panel(frame, rows[1], entry, v);
        render_placement_panel(frame, rows[2], entry, live);
        hint_row = Rect { height: 1, ..rows[3] };
    } else {
        render_placement_panel(frame, rows[1], entry, live);
        hint_row = Rect { height: 1, ..rows[2] };
    }
    render_back_hint(frame, hint_row);
}

fn render_datafort_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &NodeFocusEntry,
    view: &DatafortView,
) {
    let (disk_used, disk_total) = match (view.disk_used_bytes, view.disk_total_bytes) {
        (Some(u), Some(t)) => (u, t),
        _ => (0u64, 0u64),
    };
    let ratio = if disk_total == 0 {
        0.0
    } else {
        (disk_used as f64 / disk_total as f64).clamp(0.0, 1.0)
    };
    let pct = (ratio * 100.0) as u16;
    let bar_color = pressure_color(ratio);
    let overflow_chip = if view.overflow_active {
        Span::styled("    ACTIVE", theme::amber())
    } else if view.overflow_enabled {
        Span::styled("    enabled", theme::green())
    } else {
        Span::styled("    off", theme::dim())
    };
    let role = if view.is_local { "local" } else { "remote" };
    let role_style = if view.is_local { theme::cyan() } else { theme::dim() };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DATAFORT", theme::green_hi()),
            Span::styled(format!("    {}", role), role_style),
            Span::styled("    overflow", theme::chrome()),
            overflow_chip,
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let disk_label = if disk_total > 0 {
        format!("{} / {}", fmt_bytes(disk_used), fmt_bytes(disk_total))
    } else {
        "—".to_string()
    };
    lines.push(Line::from(vec![
        Span::styled("  disk      ", theme::chrome()),
        bar_span(pct, 16, bar_color),
        Span::styled(format!("  {pct:>3}%  "), theme::text()),
        Span::styled(disk_label, theme::dim()),
    ]));

    if view.is_local {
        if view.adapters.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "  adapters  —",
                theme::chrome(),
            )]));
        } else {
            for a in &view.adapters {
                let r = if a.disk_capacity_bytes == 0 {
                    0.0
                } else {
                    (a.disk_used_bytes as f64 / a.disk_capacity_bytes as f64).clamp(0.0, 1.0)
                };
                let p = (r * 100.0) as u16;
                let chip = if a.overflow_active {
                    Span::styled("  ACTIVE", theme::amber())
                } else if a.overflow_enabled {
                    Span::styled("  enabled", theme::green())
                } else {
                    Span::styled("  off", theme::dim())
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<12}", a.id), theme::text()),
                    bar_span(p, 12, pressure_color(r)),
                    Span::styled(format!(" {p:>3}%  ", ), theme::dim()),
                    Span::styled(
                        format!(
                            "{} / {}",
                            fmt_bytes(a.disk_used_bytes),
                            fmt_bytes(a.disk_capacity_bytes)
                        ),
                        theme::dim(),
                    ),
                    chip,
                ]));
            }
        }
    } else {
        // Remote: surface the cap tags the peer advertises.
        let caps: Vec<&String> = entry
            .peer
            .capability_set
            .iter()
            .filter(|c| c.starts_with("dataforts."))
            .collect();
        let chips: Vec<Span> = if caps.is_empty() {
            vec![Span::styled("  caps      —", theme::chrome())]
        } else {
            let mut v = vec![Span::styled("  caps      ", theme::chrome())];
            let mut first = true;
            for c in caps {
                if !first {
                    v.push(Span::styled("  ·  ", theme::chrome()));
                }
                v.push(Span::styled(c.clone(), theme::text()));
                first = false;
            }
            v
        };
        lines.push(Line::from(chips));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_back_hint(frame: &mut Frame<'_>, area: Rect) {
    let hint = Line::from(vec![
        Span::styled("[Esc]", theme::green_hi()),
        Span::styled(" back", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(hint).alignment(Alignment::Right),
        area,
    );
}

// ───────────────────────── peer panel ─────────────────────────

fn render_peer_panel(frame: &mut Frame<'_>, area: Rect, entry: &NodeFocusEntry) {
    let id_label = match entry.label.as_deref() {
        Some(label) => format!("0x{:x}.{}", entry.id, label),
        None => format!("0x{:x}", entry.id),
    };
    let (health_glyph, health_style) = health_dot(entry.peer.health);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NODE", theme::green_hi()),
            Span::styled("    ", theme::chrome()),
            Span::styled(
                id_label.clone(),
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("    ", theme::chrome()),
            Span::styled(health_glyph, health_style),
            Span::styled(" ", theme::chrome()),
            Span::styled(health_label(entry.peer.health), health_style),
        ]))
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Top of inner: two columns (identity + resources). The
    // capability tree takes the remaining height so deep
    // capability namespaces render in full instead of as a
    // truncated single-line tag flow.
    let stack = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // identity + resources
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // caps tree
        ])
        .split(inner);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(stack[0]);

    render_identity_column(frame, cols[0], entry, &id_label);
    render_resources_column(frame, cols[1], entry);
    render_caps_section(frame, stack[2], entry);
}

fn render_identity_column(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &NodeFocusEntry,
    id_label: &str,
) {
    let (health_text, health_style) = health_label_styled(entry.peer.health);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // id
            Constraint::Length(1), // health
            Constraint::Length(1), // rtt
            Constraint::Length(1), // maint
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

fn render_resources_column(frame: &mut Frame<'_>, area: Rect, entry: &NodeFocusEntry) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // cpu_1m
            Constraint::Length(1), // memory
            Constraint::Length(1), // disk
            Constraint::Length(1), // sat
            Constraint::Min(0),    // pad
        ])
        .split(area);

    // CPU 1-min load — no bar (load is not bounded to a
    // percentage without core count), just the value with a
    // "spike if > 1.0" amber accent.
    let cpu_style = match entry.peer.cpu_load_1m {
        Some(v) if v >= 2.0 => theme::red(),
        Some(v) if v >= 1.0 => theme::amber(),
        Some(_) => theme::green(),
        None => theme::chrome(),
    };
    let cpu_text = entry
        .peer
        .cpu_load_1m
        .map(|v| format!("{v:.2}"))
        .unwrap_or_else(|| "—".to_string());
    frame.render_widget(kv("cpu_1m    ", &cpu_text, cpu_style), rows[0]);

    // Memory + disk: bar + percent + raw bytes label.
    render_bar_row(
        frame,
        rows[1],
        "memory    ",
        entry.peer.mem_used_bytes,
        entry.peer.mem_total_bytes,
    );
    render_bar_row(
        frame,
        rows[2],
        "disk      ",
        entry.peer.disk_used_bytes,
        entry.peer.disk_total_bytes,
    );

    // Saturation — already a 0..1 ratio.
    render_sat_row(frame, rows[3], entry.peer.saturation_trend);
}

/// One resource line: `label  bar  NN%  used / total`.
fn render_bar_row(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    used: Option<u64>,
    total: Option<u64>,
) {
    let (ratio, label_value) = match (used, total) {
        (Some(u), Some(t)) if t > 0 => {
            let r = (u as f64 / t as f64).clamp(0.0, 1.0);
            let labeled = format!("{} / {}", fmt_bytes(u), fmt_bytes(t));
            (Some(r), labeled)
        }
        _ => (None, "—".to_string()),
    };
    let mut spans = vec![Span::styled(format!("  {label}"), theme::chrome())];
    match ratio {
        Some(r) => {
            let pct = (r * 100.0) as u16;
            let color = pressure_color(r);
            spans.push(bar_span(pct, 16, color));
            spans.push(Span::styled(format!("  {pct:>3}%  "), pct_style(r)));
            spans.push(Span::styled(label_value, theme::dim()));
        }
        None => {
            spans.push(Span::styled(label_value, theme::chrome()));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_sat_row(frame: &mut Frame<'_>, area: Rect, sat: Option<f32>) {
    let mut spans = vec![Span::styled("  sat       ", theme::chrome())];
    match sat {
        Some(s) => {
            let r = (s as f64).clamp(0.0, 1.0);
            let pct = (r * 100.0) as u16;
            let color = pressure_color(r);
            spans.push(bar_span(pct, 16, color));
            spans.push(Span::styled(format!("  {s:.2}"), pct_style(r)));
        }
        None => spans.push(Span::styled("—", theme::chrome())),
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ───────────────────────── capability tree ─────────────────────────
//
// Capabilities are dot-separated namespaces (e.g.
// `sensor.radar.shortwave`). Rendered as a tree grouped by the
// shared prefix so deep namespaces read clearly. A subtree that
// is a single linear chain (no branching) collapses inline as
// `parent.child.grandchild`; a subtree that branches gets one
// indented row per child:
//
//     compute.daemon
//     greedy.cache
//     sensor.
//       lidar
//       radar.
//         shortwave
//         longwave
//       temp.cel

struct CapNode {
    name: String,
    children: Vec<CapNode>,
}

fn render_caps_section(frame: &mut Frame<'_>, area: Rect, entry: &NodeFocusEntry) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        "  caps",
        theme::chrome(),
    )]));

    let tree = build_cap_tree(&entry.peer.capability_set);
    if tree.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("      ", theme::chrome()),
            Span::styled("—", theme::chrome()),
        ]));
    } else {
        for node in &tree {
            push_cap_lines(node, 0, &mut lines);
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn push_cap_lines(node: &CapNode, depth: usize, lines: &mut Vec<Line<'static>>) {
    // 6-col gutter to align under the "caps" label, then 2 cols
    // per depth level for the tree indent.
    let indent = format!("      {}", "  ".repeat(depth));
    if is_chain(node) {
        lines.push(Line::from(vec![
            Span::styled(indent, theme::chrome()),
            Span::styled(chain_path(node), theme::text()),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(indent, theme::chrome()),
            Span::styled(format!("{}.", node.name), theme::cyan()),
        ]));
        for child in &node.children {
            push_cap_lines(child, depth + 1, lines);
        }
    }
}

fn build_cap_tree<'a, I>(caps: I) -> Vec<CapNode>
where
    I: IntoIterator<Item = &'a String>,
{
    let mut sorted: Vec<&String> = caps.into_iter().collect();
    sorted.sort();
    let mut roots: Vec<CapNode> = Vec::new();
    for cap in sorted {
        let parts: Vec<&str> = cap.split('.').filter(|s| !s.is_empty()).collect();
        insert_cap(&mut roots, &parts);
    }
    roots
}

fn insert_cap(siblings: &mut Vec<CapNode>, parts: &[&str]) {
    let Some((head, rest)) = parts.split_first() else {
        return;
    };
    if let Some(child) = siblings.iter_mut().find(|c| c.name == *head) {
        insert_cap(&mut child.children, rest);
    } else {
        let mut node = CapNode {
            name: (*head).to_string(),
            children: Vec::new(),
        };
        insert_cap(&mut node.children, rest);
        siblings.push(node);
    }
}

fn is_chain(node: &CapNode) -> bool {
    match node.children.len() {
        0 => true,
        1 => is_chain(&node.children[0]),
        _ => false,
    }
}

fn chain_path(node: &CapNode) -> String {
    if node.children.is_empty() {
        node.name.clone()
    } else {
        format!("{}.{}", node.name, chain_path(&node.children[0]))
    }
}

fn count_cap_lines<'a, I>(caps: I) -> usize
where
    I: IntoIterator<Item = &'a String>,
{
    let tree = build_cap_tree(caps);
    // 1 line for the "caps" header, + N for the tree (or 1 for
    // the "—" empty marker).
    let body = if tree.is_empty() {
        1
    } else {
        tree.iter().map(count_cap_node_lines).sum()
    };
    1 + body
}

fn count_cap_node_lines(node: &CapNode) -> usize {
    if is_chain(node) {
        1
    } else {
        1 + node.children.iter().map(count_cap_node_lines).sum::<usize>()
    }
}

// ───────────────────────── placement panel ─────────────────────────

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
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(1), // vertical divider
            Constraint::Percentage(50),
        ])
        .split(inner);

    let divider_lines: Vec<Line> = (0..cols[1].height)
        .map(|_| Line::from(Span::styled("│", theme::rule())))
        .collect();
    frame.render_widget(Paragraph::new(divider_lines), cols[1]);

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
    frame.render_widget(Paragraph::new(chain_lines), cols[2]);
}

// ───────────────────────── helpers ─────────────────────────

fn kv<'a>(label: &'a str, value: &'a str, value_style: ratatui::style::Style) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(format!("  {label}"), theme::chrome()),
        Span::styled(value.to_string(), value_style),
    ]))
}

fn health_dot(h: Option<PeerHealthSnapshot>) -> (&'static str, ratatui::style::Style) {
    match h {
        Some(PeerHealthSnapshot::Healthy) => ("●", theme::green()),
        Some(PeerHealthSnapshot::Degraded) => ("●", theme::amber()),
        Some(PeerHealthSnapshot::Unreachable) => ("○", theme::red()),
        _ => ("·", theme::chrome()),
    }
}

fn health_label(h: Option<PeerHealthSnapshot>) -> &'static str {
    match h {
        Some(PeerHealthSnapshot::Healthy) => "Healthy",
        Some(PeerHealthSnapshot::Degraded) => "Degraded",
        Some(PeerHealthSnapshot::Unreachable) => "Unreachable",
        _ => "—",
    }
}

fn health_label_styled(
    h: Option<PeerHealthSnapshot>,
) -> (&'static str, ratatui::style::Style) {
    let (_, style) = health_dot(h);
    (health_label(h), style)
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

/// Pressure-band color: green steady, amber at ≥0.85, red at
/// ≥0.95. Matches the dataforts health-gate thresholds the
/// rest of the deck uses.
fn pressure_color(ratio: f64) -> ratatui::style::Color {
    if ratio >= 0.95 {
        theme::RED
    } else if ratio >= 0.85 {
        theme::AMBER
    } else {
        theme::GREEN_HI
    }
}

fn pct_style(ratio: f64) -> ratatui::style::Style {
    if ratio >= 0.95 {
        theme::red()
    } else if ratio >= 0.85 {
        theme::amber()
    } else {
        theme::text()
    }
}

/// Filled `━` + empty `·` bar.
fn bar_span(pct: u16, width: u16, color: ratatui::style::Color) -> Span<'static> {
    let pct = pct.min(100);
    let filled = ((pct as u32 * width as u32) / 100) as usize;
    let empty = width as usize - filled;
    let mut s = String::with_capacity(width as usize);
    for _ in 0..filled {
        s.push('━');
    }
    for _ in 0..empty {
        s.push('·');
    }
    Span::styled(s, ratatui::style::Style::default().fg(color))
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
