//! Compact NODE card — header + health + cpu + mem/disk bars +
//! capability tags. Shared between the DAEMONS detail panel
//! (placement node of the cursored daemon) and any other tab
//! that wants the same node summary block.
//!
//! Style mirrors the NODE pane used on the DATAFORTS tab so the
//! deck reads consistently across surfaces.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme;
use net_sdk::dataforts::{HEALTH_GATE_CLEAR_THRESHOLD, HEALTH_GATE_EMIT_THRESHOLD};

#[derive(Clone, Debug, Default)]
pub struct NodeCardView {
    pub id: u64,
    pub label: Option<String>,
    pub is_local: bool,
    pub health: Option<&'static str>,
    pub cpu_load_1m: Option<f64>,
    pub mem_used_bytes: Option<u64>,
    pub mem_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub capabilities: Vec<String>,
}

pub fn render(frame: &mut Frame<'_>, area: Rect, view: &NodeCardView) {
    // Use `{:x}` (variable-width, no leading-zero pad) to match
    // every other tab's node-id rendering. Padding to 4 hex
    // digits would show the same id as `0x0001` here while the
    // rest of the deck renders `0x1` — confusing the operator
    // on cross-tab pivots.
    let id_label = match view.label.as_deref() {
        Some(l) => format!("0x{:x}.{l}", view.id),
        None => format!("0x{:x}", view.id),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("NODE", theme::green_hi()),
            Span::styled(format!("    {id_label}"), theme::text()),
            Span::styled(
                if view.is_local {
                    "    local"
                } else {
                    "    remote"
                },
                if view.is_local {
                    theme::cyan()
                } else {
                    theme::dim()
                },
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (health_text, health_style) = match view.health {
        Some("Healthy") => ("Healthy", theme::green()),
        Some("Degraded") => ("Degraded", theme::amber()),
        Some("Unreachable") => ("Unreachable", theme::red()),
        _ => ("—", theme::chrome()),
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(kv("health", health_text, health_style));
    lines.push(kv(
        "cpu_1m",
        &view
            .cpu_load_1m
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "—".to_string()),
        cpu_style(view.cpu_load_1m),
    ));
    lines.push(bar_kv("memory", view.mem_used_bytes, view.mem_total_bytes));
    lines.push(bar_kv("disk", view.disk_used_bytes, view.disk_total_bytes));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  capabilities",
        theme::chrome(),
    )]));
    if view.capabilities.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("      ", theme::chrome()),
            Span::styled("—", theme::chrome()),
        ]));
    } else {
        for cap in &view.capabilities {
            lines.push(Line::from(vec![
                Span::styled("      ", theme::chrome()),
                Span::styled(cap.clone(), theme::text()),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn kv(label: &str, value: &str, value_style: ratatui::style::Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<12}"), theme::chrome()),
        Span::styled(value.to_string(), value_style),
    ])
}

fn bar_kv(label: &str, used: Option<u64>, total: Option<u64>) -> Line<'static> {
    let (ratio, label_value) = match (used, total) {
        (Some(u), Some(t)) if t > 0 => {
            let r = (u as f64 / t as f64).clamp(0.0, 1.0);
            (Some(r), format!("{} / {}", fmt_bytes(u), fmt_bytes(t)))
        }
        _ => (None, "—".to_string()),
    };
    let mut spans = vec![Span::styled(format!("  {label:<12}"), theme::chrome())];
    match ratio {
        Some(r) => {
            let pct = (r * 100.0).round() as u16;
            let color = if r >= HEALTH_GATE_EMIT_THRESHOLD {
                theme::RED
            } else if r >= HEALTH_GATE_CLEAR_THRESHOLD {
                theme::AMBER
            } else {
                theme::GREEN_HI
            };
            spans.push(bar(pct, 12, color));
            spans.push(Span::styled(format!("  {pct:>3}%  "), theme::text()));
            spans.push(Span::styled(label_value, theme::dim()));
        }
        None => spans.push(Span::styled(label_value, theme::chrome())),
    }
    Line::from(spans)
}

fn cpu_style(load: Option<f64>) -> ratatui::style::Style {
    match load {
        Some(v) if v >= 2.0 => theme::red(),
        Some(v) if v >= 1.0 => theme::amber(),
        Some(_) => theme::green(),
        None => theme::chrome(),
    }
}

fn bar(pct: u16, width: u16, color: ratatui::style::Color) -> Span<'static> {
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
