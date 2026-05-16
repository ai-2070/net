use net_sdk::deck::{DaemonLifecycleSnapshot, DaemonSnapshot, LogLevel, LogRecord, MeshOsSnapshot};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::DaemonCursor,
    lineage::{self, GroupKind as LiveGroupKind, LiveGroup, LiveMember, MemberRole as LiveRole},
    nodes, theme, widgets,
    widgets::node_card::NodeCardView,
};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    snapshot: Option<&MeshOsSnapshot>,
    cursor: DaemonCursor,
    local_node: &NodeCardView,
    logs: &[LogRecord],
) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(area);

    let groups = snapshot.map(|s| lineage::group_daemons(&s.daemons));
    let has_groups = groups.as_ref().map(|g| !g.is_empty()).unwrap_or(false);

    if has_groups {
        let groups = groups.unwrap();
        render_list(frame, cols[0], &groups, cursor);
        render_detail(frame, cols[1], &groups, cursor, snapshot, local_node, logs);
    } else {
        render_empty_list(frame, cols[0]);
        render_empty_detail(frame, cols[1]);
    }
}

// ───────────────────────── empty-state panels ─────────────────────────

fn render_empty_list(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMONS", theme::green_hi()),
            Span::styled("   0 registered", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no daemons registered yet",
        "register via the MeshOsDaemonSdk",
    );
}

fn render_empty_detail(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMON", theme::green_hi()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::empty::render(
        frame,
        inner,
        "no daemon selected",
        "details appear here once a daemon registers",
    );
}

// ───────────────────────── live list (lineage tree) ─────────────────────────

fn render_list(frame: &mut Frame<'_>, area: Rect, groups: &[LiveGroup<'_>], cursor: DaemonCursor) {
    let total: usize = groups.iter().map(|g| g.members.len()).sum();
    let n_groups = groups.len();
    // Daemon cursor is two-level (group + member within group).
    // Clamp against the live shape so the chip stays coherent
    // when groups churn under the cursor.
    let g_pos = cursor.group.min(n_groups.saturating_sub(1));
    let m_total = groups.get(g_pos).map(|g| g.members.len()).unwrap_or(0);
    let m_pos = cursor.member.min(m_total.saturating_sub(1));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMONS", theme::green_hi()),
            Span::styled(
                format!("   {total} live · {n_groups} groups"),
                theme::chrome(),
            ),
            Span::styled(
                format!(
                    "    grp {}/{n_groups} · mbr {}/{m_total}",
                    g_pos + 1,
                    m_pos + 1
                ),
                theme::dim(),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(group_header(group));
        let n = group.members.len();
        for (mi, member) in group.members.iter().enumerate() {
            let last = mi + 1 == n;
            let is_cursor = gi == cursor.group && mi == cursor.member;
            lines.push(member_line(group.kind, member, last, is_cursor));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn group_header(group: &LiveGroup<'_>) -> Line<'static> {
    let (tag, tag_color) = match group.kind {
        LiveGroupKind::Solo => ("STANDALONE", theme::TEXT_DIM),
        LiveGroupKind::Replica => ("REPLICA", theme::GREEN_HI),
        LiveGroupKind::Fork { .. } => ("FORK", theme::AMBER),
        LiveGroupKind::Standby => ("STANDBY", theme::CYAN),
    };
    let detail = match group.kind {
        LiveGroupKind::Solo => format!("· {}", group.display_name),
        LiveGroupKind::Replica => {
            format!("· {} · {} members", group.display_name, group.members.len())
        }
        LiveGroupKind::Fork { parent_seq } => {
            format!(
                "· {} · parent @ seq={} · {} forks",
                group.display_name,
                parent_seq,
                group.members.len()
            )
        }
        LiveGroupKind::Standby => {
            let warm = group.members.len().saturating_sub(1);
            format!("· {} · 1 active + {} warm", group.display_name, warm)
        }
    };
    Line::from(vec![
        Span::styled("┌─ ", theme::rule()),
        Span::styled(
            tag,
            ratatui::style::Style::default()
                .fg(tag_color)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(" ", theme::chrome()),
        Span::styled(detail, theme::chrome()),
    ])
}

fn member_line(
    kind: LiveGroupKind,
    member: &LiveMember<'_>,
    last: bool,
    is_cursor: bool,
) -> Line<'static> {
    let connector = if last { "│  └─ " } else { "│  ├─ " };
    let cursor = if is_cursor { "▶ " } else { "  " };

    let (glyph, role_text, role_color) = role_repr(member.role, kind);

    let cursor_color = if is_cursor {
        theme::GREEN_HI
    } else {
        theme::CHROME
    };
    let id_style = if is_cursor {
        theme::green_hi()
    } else {
        theme::text()
    };

    let (health_color, health_text) = health_repr(member.daemon);

    Line::from(vec![
        Span::styled(connector, theme::rule()),
        Span::styled(cursor, ratatui::style::Style::default().fg(cursor_color)),
        Span::styled(glyph, ratatui::style::Style::default().fg(role_color)),
        Span::raw(" "),
        Span::styled(format!("{:<10}", short_id(member.id)), id_style),
        Span::raw(" "),
        Span::styled(
            format!("{:<12}", role_text),
            ratatui::style::Style::default().fg(role_color),
        ),
        Span::raw("  "),
        Span::styled(
            health_text,
            ratatui::style::Style::default().fg(health_color),
        ),
    ])
}

fn role_repr(
    role: LiveRole,
    _kind: LiveGroupKind,
) -> (&'static str, String, ratatui::style::Color) {
    match role {
        LiveRole::Solo => ("◆", "solo".to_string(), theme::TEXT),
        LiveRole::Replica(i) => ("□", format!("m[{i}] idle"), theme::GREEN_HI),
        LiveRole::Fork(i) => ("┝", format!("fork[{i}]"), theme::AMBER),
        LiveRole::StandbyActive => ("●", "active".to_string(), theme::GREEN_HI),
        LiveRole::StandbyWarm(i) => ("○", format!("warm[{i}]"), theme::CYAN),
    }
}

fn health_repr(d: &DaemonSnapshot) -> (ratatui::style::Color, &'static str) {
    use net_sdk::deck::DaemonHealthSnapshot;
    match d.health {
        Some(DaemonHealthSnapshot::Healthy) => (theme::GREEN, "Healthy"),
        Some(DaemonHealthSnapshot::Degraded { .. }) => (theme::AMBER, "Degraded"),
        Some(DaemonHealthSnapshot::Unhealthy) => (theme::RED, "Unhealthy"),
        None => (theme::CHROME, "Unknown"),
        _ => (theme::CHROME, "Unknown"),
    }
}

fn short_id(id: u64) -> String {
    format!("0x{id:x}")
}

// ───────────────────────── live detail (right pane) ─────────────────────────

fn render_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    groups: &[LiveGroup<'_>],
    cursor: DaemonCursor,
    snapshot: Option<&MeshOsSnapshot>,
    local_node: &NodeCardView,
    logs: &[LogRecord],
) {
    let Some((group, member)) = groups
        .get(cursor.group)
        .and_then(|g| g.members.get(cursor.member).map(|m| (g, m)))
    else {
        render_empty_detail(frame, area);
        return;
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMON ", theme::green_hi()),
            Span::styled(format!("{} ", short_id(member.id)), theme::text()),
            Span::styled(format!("· {}", group.display_name), theme::cyan()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Resolve the NODE card for the daemon's placement: local
    // gets the synthesized view passed in; remote peers come
    // from `snapshot.peers`. `placement_card_h` sizes itself to
    // 5 fixed lines (id/health/cpu/mem/disk) + 1 blank + 1 caps
    // header + N caps + 2 borders.
    let placement_view = resolve_placement_view(member.daemon, snapshot, local_node);
    let card_h = (placement_view.capabilities.len().clamp(1, 6) as u16 + 9).min(14);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),      // facts
            Constraint::Length(card_h), // placement NODE card
            Constraint::Min(0),         // log tail
            Constraint::Length(3),      // controls
        ])
        .split(inner);

    render_facts(frame, rows[0], group, member);
    widgets::node_card::render(frame, rows[1], &placement_view);
    render_log_tail(frame, rows[2], member.id, logs);
    render_controls(frame, rows[3]);
}

fn resolve_placement_view(
    daemon: &DaemonSnapshot,
    snapshot: Option<&MeshOsSnapshot>,
    local_node: &NodeCardView,
) -> NodeCardView {
    if daemon.placement == local_node.id {
        return local_node.clone();
    }
    let Some(snap) = snapshot else {
        return NodeCardView {
            id: daemon.placement,
            label: nodes::label_of(&format!("0x{:x}", daemon.placement)).map(|s| s.to_string()),
            ..NodeCardView::default()
        };
    };
    let Some((_, peer)) = snap.peers.iter().find(|(id, _)| **id == daemon.placement) else {
        return NodeCardView {
            id: daemon.placement,
            label: nodes::label_of(&format!("0x{:x}", daemon.placement)).map(|s| s.to_string()),
            ..NodeCardView::default()
        };
    };
    NodeCardView {
        id: daemon.placement,
        label: nodes::label_for(&format!("0x{:x}", daemon.placement), &peer.capability_set),
        is_local: false,
        health: match peer.health {
            Some(net_sdk::deck::PeerHealthSnapshot::Healthy) => Some("Healthy"),
            Some(net_sdk::deck::PeerHealthSnapshot::Degraded) => Some("Degraded"),
            Some(net_sdk::deck::PeerHealthSnapshot::Unreachable) => Some("Unreachable"),
            _ => None,
        },
        cpu_load_1m: peer.cpu_load_1m,
        mem_used_bytes: peer.mem_used_bytes,
        mem_total_bytes: peer.mem_total_bytes,
        disk_used_bytes: peer.disk_used_bytes,
        disk_total_bytes: peer.disk_total_bytes,
        capabilities: peer.capability_set.iter().cloned().collect(),
    }
}

fn render_facts(frame: &mut Frame<'_>, area: Rect, group: &LiveGroup<'_>, member: &LiveMember<'_>) {
    let d = member.daemon;
    let group_line = match group.kind {
        LiveGroupKind::Solo => "standalone · no group".to_string(),
        LiveGroupKind::Replica => format!(
            "ReplicaGroup · {} · {} members",
            group.display_name,
            group.members.len()
        ),
        LiveGroupKind::Fork { parent_seq } => format!(
            "ForkGroup · {} · parent @ seq={parent_seq} · {} forks",
            group.display_name,
            group.members.len()
        ),
        LiveGroupKind::Standby => {
            let warm = group.members.len().saturating_sub(1);
            format!(
                "StandbyGroup · {} · 1 active + {warm} warm",
                group.display_name
            )
        }
    };
    let role_line = match member.role {
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
    let (_hc, health_text) = health_repr(d);

    let mut placement_spans = vec![Span::styled("placement  ", theme::chrome())];
    placement_spans.extend(nodes::id_spans(&format!("0x{:x}", d.placement)));
    placement_spans.push(Span::styled(
        format!(" · saturation {:.2}", d.saturation),
        theme::text(),
    ));

    let lines = vec![
        kv("identity   ", format!("ent.{}", short_id(member.id))),
        kv("lineage    ", group_line),
        kv("role       ", role_line),
        kv("kind       ", group.display_name.clone()),
        kv("lifecycle  ", lifecycle_line),
        kv("health     ", health_text.to_string()),
        Line::from(placement_spans),
        kv("restart    ", format!("{:?}", d.restart_state)),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

/// Tail of log lines scoped to the cursored daemon. Pulls from
/// the deck's streaming `LogsTail` (filter by `daemon_id`) and
/// renders the most recent N lines.
fn render_log_tail(frame: &mut Frame<'_>, area: Rect, daemon_id: u64, logs: &[LogRecord]) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(
                "LOG.TAIL  ",
                ratatui::style::Style::default().fg(theme::GREEN_HI),
            ),
            Span::styled(format!("daemon {}", short_id(daemon_id)), theme::cyan()),
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

fn fmt_ts(ts_ms: u64) -> String {
    // Render as HH:MM:SS.mmm derived from unix-ms. No TZ
    // conversion — operators correlate against the same clock
    // the substrate stamps.
    let total_s = ts_ms / 1000;
    let ms = ts_ms % 1000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = (total_s / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

fn render_controls(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![Span::styled(
            "CONTROLS  ",
            theme::chrome(),
        )]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled("[w/s] ", theme::green_hi()),
        Span::styled("member   ", theme::dim()),
        Span::styled("[W/S] ", theme::green_hi()),
        Span::styled("group    ", theme::dim()),
        Span::styled("[r] ", theme::green_hi()),
        Span::styled("restart   ", theme::dim()),
        Span::styled("[d] ", theme::green_hi()),
        Span::styled("drain", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(line), inner);
}

fn kv(k: &'static str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(k, theme::chrome()),
        Span::styled(v, theme::text()),
    ])
}

use super::format_age_ms as format_age;
