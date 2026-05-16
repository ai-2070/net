//! Node detail modal — `[Enter]` on a NET.MAP node opens this
//! against a snapshot of the cursored peer. Surfaces every
//! `PeerSnapshot` field at once: health, RTT, maintenance,
//! the inventory axes (CPU / mem / disk / saturation /
//! capabilities / software version / fork ancestry).

use std::collections::BTreeSet;

use net_sdk::deck::{MaintenanceMirrorSnapshot, PeerHealthSnapshot, PeerSnapshot};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

/// Snapshot of the cursored peer + its id. Modal owns its
/// copy so the next tick under the cursor doesn't shift the
/// body.
#[derive(Clone, Debug)]
pub struct NodeDetailEntry {
    pub id: u64,
    pub label: Option<String>,
    pub peer: PeerSnapshot,
}

pub fn render(frame: &mut Frame<'_>, area: Rect, entry: &NodeDetailEntry) {
    let modal_area = center(area, 78, 22);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(Line::from(vec![
            Span::styled(" ◆ ", theme::green()),
            Span::styled(
                "NODE DETAIL",
                Style::default()
                    .fg(theme::GREEN_HI)
                    .add_modifier(Modifier::BOLD),
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
            Constraint::Length(1), // spacer
            Constraint::Length(1), // id + label
            Constraint::Length(1), // health + RTT
            Constraint::Length(1), // maintenance
            Constraint::Length(1), // spacer
            Constraint::Length(1), // cpu
            Constraint::Length(1), // mem
            Constraint::Length(1), // disk
            Constraint::Length(1), // saturation
            Constraint::Length(1), // software_version
            Constraint::Length(1), // forked_from
            Constraint::Length(1), // capability_set (truncated)
            Constraint::Min(0),    // pad
            Constraint::Length(1), // bindings
        ])
        .split(inner);

    let (health_text, health_style) = match entry.peer.health {
        Some(PeerHealthSnapshot::Healthy) => ("Healthy", theme::green()),
        Some(PeerHealthSnapshot::Degraded) => ("Degraded", theme::amber()),
        Some(PeerHealthSnapshot::Unreachable) => ("Unreachable", theme::red()),
        _ => ("—", theme::chrome()),
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "peer snapshot",
            Style::default()
                .fg(theme::GREEN_HI)
                .add_modifier(Modifier::BOLD),
        )]))
        .alignment(Alignment::Center),
        rows[0],
    );

    let id_label = match entry.label.as_deref() {
        Some(label) => format!("0x{:x}.{}", entry.id, label),
        None => format!("0x{:x}", entry.id),
    };
    frame.render_widget(kv("id      ", &id_label, theme::green_hi()), rows[2]);
    frame.render_widget(
        kv_two(
            "health  ",
            health_text,
            health_style,
            "rtt     ",
            &entry
                .peer
                .rtt_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "—".to_string()),
            theme::text(),
        ),
        rows[3],
    );
    frame.render_widget(
        kv("maint   ", &maint_label(entry.peer.maintenance), theme::cyan()),
        rows[4],
    );

    frame.render_widget(
        kv(
            "cpu_1m  ",
            &entry
                .peer
                .cpu_load_1m
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "—".to_string()),
            theme::text(),
        ),
        rows[6],
    );
    frame.render_widget(
        kv(
            "memory  ",
            &fmt_used_total(entry.peer.mem_used_bytes, entry.peer.mem_total_bytes),
            theme::text(),
        ),
        rows[7],
    );
    frame.render_widget(
        kv(
            "disk    ",
            &fmt_used_total(entry.peer.disk_used_bytes, entry.peer.disk_total_bytes),
            theme::text(),
        ),
        rows[8],
    );
    let (sat_text, sat_style) = match entry.peer.saturation_trend {
        Some(s) if s >= 0.8 => (format!("{s:.2}"), theme::red()),
        Some(s) if s >= 0.5 => (format!("{s:.2}"), theme::amber()),
        Some(s) => (format!("{s:.2}"), theme::green()),
        None => ("—".to_string(), theme::chrome()),
    };
    frame.render_widget(kv("sat     ", &sat_text, sat_style), rows[9]);
    frame.render_widget(
        kv(
            "version ",
            entry.peer.software_version.as_deref().unwrap_or("—"),
            theme::text(),
        ),
        rows[10],
    );
    frame.render_widget(
        kv(
            "fork_of ",
            &entry
                .peer
                .forked_from
                .map(|id| format!("0x{id:x}"))
                .unwrap_or_else(|| "—".to_string()),
            theme::text(),
        ),
        rows[11],
    );
    frame.render_widget(
        kv(
            "caps    ",
            &fmt_caps(&entry.peer.capability_set),
            theme::dim(),
        ),
        rows[12],
    );

    let bindings = Line::from(vec![
        Span::styled("[Esc / Enter]", theme::dim()),
        Span::styled(" close", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[14],
    );
}

fn kv<'a>(label: &'a str, value: &'a str, value_style: ratatui::style::Style) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(format!("  {label}"), theme::chrome()),
        Span::styled(value.to_string(), value_style),
    ]))
}

fn kv_two<'a>(
    label_a: &'a str,
    value_a: &'a str,
    style_a: ratatui::style::Style,
    label_b: &'a str,
    value_b: &'a str,
    style_b: ratatui::style::Style,
) -> Paragraph<'a> {
    Paragraph::new(Line::from(vec![
        Span::styled(format!("  {label_a}"), theme::chrome()),
        Span::styled(format!("{value_a:<14}"), style_a),
        Span::styled(label_b.to_string(), theme::chrome()),
        Span::styled(value_b.to_string(), style_b),
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

fn fmt_caps(caps: &BTreeSet<String>) -> String {
    if caps.is_empty() {
        return "—".to_string();
    }
    let joined: Vec<&str> = caps.iter().map(|s| s.as_str()).collect();
    let line = joined.join(", ");
    // Cap the rendered width so a 30-capability peer doesn't
    // bleed off the modal. Caller has ~62 chars of value
    // width available after the label.
    if line.len() > 62 {
        format!("{}…  +{} more", &line[..58], caps.len().saturating_sub(1))
    } else {
        line
    }
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
