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
    MigrationPhaseSnapshot, MigrationSnapshot, NodeId,
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
    this_node: NodeId,
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

    // Resolve any in-flight migration for this daemon up front
    // so the FACTS panel can decide whether to render the
    // right-side migration sub-panel. `Complete` migrations
    // intentionally don't keep the right cell pinned forever —
    // once the orchestrator marks the phase complete the
    // daemon's facts go back to single-column rendering even
    // if the producer hasn't dropped the record from
    // `in_flight_migrations` yet.
    let migration = live
        .in_flight_migrations
        .iter()
        .find(|m| m.daemon_origin == entry.id && m.phase != MigrationPhaseSnapshot::Complete);
    render_facts_panel(frame, rows[0], entry, &groups, migration, this_node);
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
    migration: Option<&MigrationSnapshot>,
    this_node: NodeId,
) {
    // Two side-by-side cells when there's an in-flight migration
    // for this daemon: a DAEMON cell on the left + a MIGRATION
    // cell on the right, each with its own bordered block. No
    // migration → the DAEMON cell takes the full row.
    let cols = if migration.is_some() {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0)])
            .split(area)
    };

    render_daemon_cell(frame, cols[0], entry, groups);
    if let Some(m) = migration {
        render_migration_cell(frame, cols[1], m, this_node);
    }
}

fn render_daemon_cell(
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

    let mut placement_spans = vec![Span::styled("  placement  ", theme::chrome())];
    placement_spans.extend(nodes::id_spans(&format!("0x{:x}", d.placement)));
    placement_spans.push(Span::styled(
        format!(" · saturation {:.2}", d.saturation),
        theme::text(),
    ));

    let lines = vec![
        kv(
            "identity   ",
            &format!("ent.{}", short_id(entry.id)),
            theme::text(),
        ),
        kv("lineage    ", &lineage_line, theme::text()),
        kv("role       ", &role_line, theme::text()),
        kv("kind       ", &display_name, theme::cyan()),
        kv("lifecycle  ", &lifecycle_line, theme::green()),
        kv("health     ", health_text, health_style),
        Line::from(placement_spans),
        kv(
            "restart    ",
            &format!("{:?}", d.restart_state),
            theme::dim(),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Right-side MIGRATION cell — its own bordered block, mirrors
/// the columns MIGRATIONS uses (role / size / phase / prog /
/// retry / age-in-phase / elapsed) so the Daemon page reads as
/// the per-daemon view of the same data. The PROG row renders
/// a horizontal progress bar coloured by phase.
fn render_migration_cell(
    frame: &mut Frame<'_>,
    area: Rect,
    m: &MigrationSnapshot,
    this_node: NodeId,
) {
    let (role_text, role_style) = if this_node == m.target_node {
        ("target", theme::green())
    } else if this_node == m.source_node {
        ("source", theme::cyan())
    } else {
        ("observer", theme::dim())
    };
    let size_text = match m.snapshot_bytes {
        Some(n) => format_bytes(n),
        None => "—".to_string(),
    };
    let (phase_style, phase_text) = match m.phase {
        MigrationPhaseSnapshot::Snapshot => (theme::dim(), "Snapshot"),
        MigrationPhaseSnapshot::Transfer => (theme::cyan(), "Transfer"),
        MigrationPhaseSnapshot::Restore => (theme::cyan(), "Restore"),
        MigrationPhaseSnapshot::Replay => (theme::cyan(), "Replay"),
        MigrationPhaseSnapshot::Cutover => (theme::amber(), "Cutover"),
        MigrationPhaseSnapshot::Complete => (theme::green(), "Complete"),
        _ => (theme::chrome(), "?"),
    };
    let retry_style = if m.retries == 0 {
        theme::dim()
    } else if m.retries < 3 {
        theme::amber()
    } else {
        theme::red()
    };

    // Title shows the migration vector at a glance — operator
    // doesn't have to scan the role + role-style to know
    // direction. Source → target reads left-to-right.
    let title = Line::from(vec![
        Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
        Span::styled("MIGRATION", theme::green_hi()),
        Span::styled(format!("    0x{:x}", m.source_node), theme::cyan()),
        Span::styled("  →  ", theme::chrome()),
        Span::styled(format!("0x{:x}", m.target_node), theme::cyan()),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(title)
        .title_alignment(Alignment::Left);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Compute a bar width that fits the cell. The line is
    // "  prog       <bar>  NN%" — the fixed scaffolding takes
    // 2 (lead) + 11 (label) + 2 (gap) + 4 ("100%") = 19 cells.
    // Anything left over is bar; clamp to a sensible band so
    // the bar still reads as a bar at narrow widths.
    let bar_width = (inner.width as usize)
        .saturating_sub(19)
        .clamp(4, 16);
    let lines = vec![
        kv("role       ", role_text, role_style),
        kv("size       ", &size_text, theme::text()),
        kv("phase      ", phase_text, phase_style),
        progress_bar_line("prog       ", m.progress_pct, phase_style, bar_width),
        kv("retry      ", &format!("{}", m.retries), retry_style),
        kv("age        ", &format_age(m.age_in_phase_ms), theme::text()),
        kv("elapsed    ", &format_age(m.elapsed_ms), theme::dim()),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Inline progress bar: `label  ████████░░░░░░░░  30%`. The
/// caller passes `bar_width` so a narrow daemon page (half of
/// a 60-col layout) shrinks the bar rather than overflowing
/// onto the next row. `None` pct renders a row of `—` instead
/// of an empty bar.
fn progress_bar_line(
    label: &str,
    pct: Option<u8>,
    bar_style: Style,
    bar_width: usize,
) -> Line<'static> {
    match pct {
        Some(p) => {
            let p = p.min(100);
            let filled = (p as usize * bar_width) / 100;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
            Line::from(vec![
                Span::styled(format!("  {label}"), theme::chrome()),
                Span::styled(bar, bar_style),
                Span::styled(format!("  {p}%"), theme::text()),
            ])
        }
        None => Line::from(vec![
            Span::styled(format!("  {label}"), theme::chrome()),
            Span::styled("—".to_string(), theme::dim()),
        ]),
    }
}

use super::format_bytes;

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

use super::fmt_ts_hms_ms as fmt_ts;
use super::format_age_ms as format_age;
use super::short_id;
