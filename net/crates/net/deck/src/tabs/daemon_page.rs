//! Full-page daemon detail. Reached by pressing `[Enter]` on a
//! cursored row in DAEMONS or GROUPS — or by selecting a daemon
//! from a NODE page's PLACEMENT list. `[Esc]` returns to the
//! originating tab.
//!
//! Layout:
//! - FACTS panel (identity / lineage / role / kind / lifecycle
//!   / health / saturation / restart)
//! - GROUP panel: placement node row + sibling daemons. A
//!   linear cursor walks both. `[Enter]` opens whichever the
//!   cursor is on (Node page for placement, Daemon page for a
//!   sibling).
//! - LOG.TAIL filtered to this daemon
//! - bottom `[Esc] back` hint
//!
//! Snapshots `DaemonSnapshot` at focus time so the facts pane
//! stays stable across ticks. Group siblings + log tail read
//! live so the page reflects fleet evolution.

use net_sdk::deck::{
    DaemonHealthSnapshot, DaemonLifecycleSnapshot, DaemonSnapshot, LogRecord, MeshOsSnapshot,
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    lineage::{self, GroupKind as LiveGroupKind, MemberRole as LiveRole},
    nodes, theme,
};

/// Focus state for the Daemon page. Held by `App::daemon_focus`.
#[derive(Clone, Debug)]
pub struct DaemonFocusEntry {
    pub id: u64,
    /// Snapshot of the daemon at focus time. Facts pane reads
    /// from here; group siblings + log tail come from the
    /// live snapshot passed into `render`.
    pub snapshot: DaemonSnapshot,
    /// Linear cursor over the GROUP list: 0 = placement node,
    /// 1..=N = sibling at index N-1.
    pub cursor: usize,
}

/// A row in the GROUP panel — either the placement node or a
/// sibling daemon. The page exposes these so the app layer's
/// Enter handler can resolve the cursor without re-walking
/// lineage.
pub enum GroupRow {
    PlacementNode { id: u64 },
    Sibling { id: u64 },
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &DaemonFocusEntry,
    live: &MeshOsSnapshot,
    logs: &[LogRecord],
) {
    // Cache the lineage grouping for the whole frame — both
    // the facts panel (lineage_info) and the group panel
    // (sibling_role per row) used to recompute it per call,
    // re-walking the full daemon set N+1 times per render.
    let groups = lineage::group_daemons(&live.daemons);
    let rows_total = group_rows_from(entry, &groups);
    let group_h = (rows_total.len() as u16 + 2).clamp(4, 12);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(12),      // FACTS
            Constraint::Length(group_h), // GROUP
            Constraint::Min(0),          // LOG.TAIL
            Constraint::Length(2),       // hint row + spacer
        ])
        .split(area);

    render_facts_panel(frame, rows[0], entry, &groups);
    render_group_panel(frame, rows[1], entry, &groups, &rows_total);
    render_log_tail(frame, rows[2], entry.id, logs);
    let hint_row = Rect {
        height: 1,
        ..rows[3]
    };
    render_back_hint(frame, hint_row);
}

/// Build the GROUP list (placement at index 0, siblings 1..N).
/// Public so the Enter handler can dispatch on the cursor.
pub fn group_rows(entry: &DaemonFocusEntry, live: &MeshOsSnapshot) -> Vec<GroupRow> {
    let groups = lineage::group_daemons(&live.daemons);
    group_rows_from(entry, &groups)
}

fn group_rows_from(entry: &DaemonFocusEntry, groups: &[lineage::LiveGroup<'_>]) -> Vec<GroupRow> {
    let mut out: Vec<GroupRow> = Vec::new();
    out.push(GroupRow::PlacementNode {
        id: entry.snapshot.placement,
    });
    if let Some(group) = groups
        .iter()
        .find(|g| g.members.iter().any(|m| m.id == entry.id))
    {
        for m in &group.members {
            out.push(GroupRow::Sibling { id: m.id });
        }
    }
    out
}

// ───────────────────────── facts panel ─────────────────────────

fn render_facts_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &DaemonFocusEntry,
    groups: &[lineage::LiveGroup<'_>],
) {
    let d = &entry.snapshot;
    let (group_kind, display_name, member_count, role) = lineage_info(entry, groups);

    let title = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("DAEMON", theme::green_hi()),
        Span::styled(
            format!("    {}", short_id(entry.id)),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("    · {}", display_name), theme::cyan()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(title)
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lineage_line = match group_kind {
        LiveGroupKind::Solo => "standalone · no group".to_string(),
        LiveGroupKind::Replica => format!("ReplicaGroup · {display_name} · {member_count} members"),
        LiveGroupKind::Fork { parent_seq } => {
            format!("ForkGroup · {display_name} · parent @ seq={parent_seq} · {member_count} forks")
        }
        LiveGroupKind::Standby => {
            let warm = member_count.saturating_sub(1);
            format!("StandbyGroup · {display_name} · 1 active + {warm} warm")
        }
    };
    let role_line = match role {
        LiveRole::Solo => "solo · no siblings".to_string(),
        LiveRole::Replica(i) => format!("member[{i}] · interchangeable"),
        LiveRole::Fork(i) => format!("fork[{i}] · independent sibling"),
        LiveRole::StandbyActive => "ACTIVE · processing".to_string(),
        LiveRole::StandbyWarm(i) => format!("STANDBY warm[{i}]"),
    };
    let lifecycle_line = match d.lifecycle {
        DaemonLifecycleSnapshot::Running => format!("Running · age {}", format_age(d.age_ms)),
        DaemonLifecycleSnapshot::Starting => "Starting".to_string(),
        DaemonLifecycleSnapshot::Stopping => "Stopping".to_string(),
        DaemonLifecycleSnapshot::Stopped => "Stopped".to_string(),
        _ => "Unknown".to_string(),
    };
    let (health_style, health_text) = match d.health {
        Some(DaemonHealthSnapshot::Healthy) => (theme::green(), "Healthy"),
        Some(DaemonHealthSnapshot::Degraded { .. }) => (theme::amber(), "Degraded"),
        Some(DaemonHealthSnapshot::Unhealthy) => (theme::red(), "Unhealthy"),
        _ => (theme::chrome(), "—"),
    };

    let mut placement_spans = vec![Span::styled("  placement   ", theme::chrome())];
    placement_spans.extend(nodes::id_spans(&format!("0x{:x}", d.placement)));
    placement_spans.push(Span::styled(
        format!(" · saturation {:.2}", d.saturation),
        theme::text(),
    ));

    let lines = vec![
        kv(
            "identity    ",
            &format!("ent.{}", short_id(entry.id)),
            theme::text(),
        ),
        kv("lineage     ", &lineage_line, theme::text()),
        kv("role        ", &role_line, theme::text()),
        kv("kind        ", &display_name, theme::cyan()),
        kv("lifecycle   ", &lifecycle_line, theme::green()),
        kv("health      ", health_text, health_style),
        Line::from(placement_spans),
        kv(
            "restart     ",
            &format!("{:?}", d.restart_state),
            theme::dim(),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn lineage_info(
    entry: &DaemonFocusEntry,
    groups: &[lineage::LiveGroup<'_>],
) -> (LiveGroupKind, String, usize, LiveRole) {
    for g in groups {
        if let Some(m) = g.members.iter().find(|m| m.id == entry.id) {
            return (g.kind, g.display_name.clone(), g.members.len(), m.role);
        }
    }
    // Daemon vanished between focus + render — render with
    // best-effort defaults from the snapshotted DaemonSnapshot.
    (
        LiveGroupKind::Solo,
        entry.snapshot.name.clone(),
        1,
        LiveRole::Solo,
    )
}

// ───────────────────────── group panel ─────────────────────────

fn render_group_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    entry: &DaemonFocusEntry,
    groups: &[lineage::LiveGroup<'_>],
    rows: &[GroupRow],
) {
    let cursor = entry.cursor.min(rows.len().saturating_sub(1));
    let title = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("GROUP", theme::green_hi()),
        Span::styled(
            format!("    {} entries · [Enter] drill", rows.len()),
            theme::chrome(),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶" } else { " " };
        let marker_style = theme::green_hi();
        match r {
            GroupRow::PlacementNode { id } => {
                let mut spans: Vec<Span> = vec![
                    Span::styled(format!("  {marker} ",), marker_style),
                    Span::styled("NODE  ", theme::chrome()),
                ];
                let id_style = if is_cursor {
                    theme::green_hi()
                } else {
                    theme::text()
                };
                spans.extend(nodes::id_spans_styled(&format!("0x{id:x}"), id_style));
                lines.push(Line::from(spans));
            }
            GroupRow::Sibling { id } => {
                let is_self = *id == entry.id;
                let id_style = if is_cursor {
                    theme::green_hi()
                } else if is_self {
                    theme::cyan()
                } else {
                    theme::text()
                };
                let (role_text, role_style) = sibling_role(*id, groups);
                let suffix = if is_self { "  (this daemon)" } else { "" };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {marker} "), marker_style),
                    Span::styled("DAEMON  ", theme::chrome()),
                    Span::styled(short_id(*id), id_style),
                    Span::styled("  ", theme::chrome()),
                    Span::styled(role_text, role_style),
                    Span::styled(suffix, theme::dim()),
                ]));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn sibling_role(
    daemon_id: u64,
    groups: &[lineage::LiveGroup<'_>],
) -> (String, ratatui::style::Style) {
    for g in groups {
        if let Some(m) = g.members.iter().find(|m| m.id == daemon_id) {
            let style = match g.kind {
                LiveGroupKind::Solo => theme::dim(),
                LiveGroupKind::Replica => theme::green(),
                LiveGroupKind::Fork { .. } => theme::amber(),
                LiveGroupKind::Standby => theme::cyan(),
            };
            return (lineage::lineage_tag(m.role, g.kind), style);
        }
    }
    ("—".to_string(), theme::chrome())
}

// ───────────────────────── log tail ─────────────────────────

fn render_log_tail(frame: &mut Frame<'_>, area: Rect, daemon_id: u64, logs: &[LogRecord]) {
    use net_sdk::deck::LogLevel;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("LOG.TAIL", theme::green_hi()),
            Span::styled(format!("  daemon {}", short_id(daemon_id)), theme::cyan()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let filtered: Vec<&LogRecord> = logs
        .iter()
        .filter(|r| r.daemon_id == Some(daemon_id))
        .collect();
    if filtered.is_empty() {
        let lines = vec![Line::from(vec![Span::styled(
            "  no log lines for this daemon yet",
            theme::chrome(),
        )])];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let take = (inner.height as usize).max(1);
    let start = filtered.len().saturating_sub(take);
    let mut lines: Vec<Line> = Vec::with_capacity(take);
    for r in &filtered[start..] {
        let (level_text, level_style) = match r.level {
            LogLevel::Error => ("ERROR", theme::red()),
            LogLevel::Warn => ("WARN ", theme::amber()),
            LogLevel::Info => ("INFO ", theme::green()),
            LogLevel::Debug => ("DEBUG", theme::dim()),
            _ => ("?    ", theme::dim()),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {}  ", fmt_ts(r.ts_ms)), theme::chrome()),
            Span::styled(level_text.to_string(), level_style),
            Span::styled("  ", theme::chrome()),
            Span::styled(r.message.clone(), theme::text()),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_back_hint(frame: &mut Frame<'_>, area: Rect) {
    let hint = Line::from(vec![
        Span::styled("[Esc]", theme::green_hi()),
        Span::styled(" back", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(hint).alignment(Alignment::Right), area);
}

fn kv(label: &str, value: &str, value_style: ratatui::style::Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label}"), theme::chrome()),
        Span::styled(value.to_string(), value_style),
    ])
}

use super::format_age_ms as format_age;
use super::short_id;

fn fmt_ts(ts_ms: u64) -> String {
    let total_s = ts_ms / 1000;
    let ms = ts_ms % 1000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = (total_s / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}
