use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{nodes, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(area);
    render_list(frame, cols[0]);
    render_detail(frame, cols[1]);
}

// ───────────────────────── lineage model (visual mock) ─────────────────────────

#[derive(Clone, Copy)]
enum GroupKind {
    Standalone,
    Replica { seed: &'static str, lb: &'static str },
    Fork { parent: &'static str, seq: u32 },
    Standby { seed: &'static str },
}

#[derive(Clone, Copy)]
enum MemberRole {
    Solo,
    Replica(u8, &'static str),         // index, state ("idle" / "processing" / …)
    Fork(u8),                          // fork index
    Active(u64),                       // active · seq=N
    Standby(u64),                      // standby · synced_through=N
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum Health {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Copy)]
struct Member {
    id: &'static str,
    health: Health,
    role: MemberRole,
}

struct Group {
    kind: GroupKind,
    name: &'static str,          // e.g. "gravity"
    members: &'static [Member],
}

// Static fixture. Cursor points at the first replica member so the
// detail pane can render its group context.
const GROUPS: &[Group] = &[
    Group {
        kind: GroupKind::Standalone,
        name: "mikoshi",
        members: &[Member {
            id: "0x69",
            health: Health::Healthy,
            role: MemberRole::Solo,
        }],
    },
    Group {
        kind: GroupKind::Replica {
            seed: "0xab12",
            lb: "round-robin",
        },
        name: "gravity",
        members: &[
            Member { id: "0xc2", health: Health::Healthy, role: MemberRole::Replica(0, "idle") },
            Member { id: "0xf1", health: Health::Healthy, role: MemberRole::Replica(1, "idle") },
            Member { id: "0xa9", health: Health::Healthy, role: MemberRole::Replica(2, "processing") },
        ],
    },
    Group {
        kind: GroupKind::Fork {
            parent: "0xabcd",
            seq: 42,
        },
        name: "drift_corr",
        members: &[
            Member { id: "0xab3", health: Health::Degraded, role: MemberRole::Fork(0) },
            Member { id: "0xab4", health: Health::Healthy,  role: MemberRole::Fork(1) },
            Member { id: "0xab5", health: Health::Healthy,  role: MemberRole::Fork(2) },
        ],
    },
    Group {
        kind: GroupKind::Standby { seed: "0xee7b" },
        name: "anti_entr",
        members: &[
            Member { id: "0xae9", health: Health::Healthy, role: MemberRole::Active(102) },
            Member { id: "0xb02", health: Health::Healthy, role: MemberRole::Standby(98) },
            Member { id: "0xd11", health: Health::Healthy, role: MemberRole::Standby(101) },
        ],
    },
];

// Cursor (visual only — selecting another member would just move ▶).
const CURSOR_GROUP: usize = 1;
const CURSOR_MEMBER: usize = 0;

// ───────────────────────── lineage tree (left pane) ─────────────────────────

fn render_list(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMONS", theme::green_hi()),
            Span::styled("   grouped by lineage", theme::chrome()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (gi, group) in GROUPS.iter().enumerate() {
        if gi > 0 {
            lines.push(Line::raw(""));
        }
        lines.extend(group_header(group));
        let n = group.members.len();
        for (mi, member) in group.members.iter().enumerate() {
            let last = mi + 1 == n;
            let is_cursor = gi == CURSOR_GROUP && mi == CURSOR_MEMBER;
            lines.push(member_line(group, member, last, is_cursor));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn group_header(group: &Group) -> Vec<Line<'static>> {
    let (tag, tag_color) = match group.kind {
        GroupKind::Standalone => ("STANDALONE", theme::TEXT_DIM),
        GroupKind::Replica { .. } => ("REPLICA",    theme::GREEN_HI),
        GroupKind::Fork { .. } => ("FORK",          theme::AMBER),
        GroupKind::Standby { .. } => ("STANDBY",    theme::CYAN),
    };
    let detail = match group.kind {
        GroupKind::Standalone => format!("· {}", group.name),
        GroupKind::Replica { seed, lb } => {
            format!("· {} · seed {seed} · {lb}", group.name)
        }
        GroupKind::Fork { parent, seq } => {
            format!("· {} · parent {parent} @ seq={seq}", group.name)
        }
        GroupKind::Standby { seed } => {
            format!("· {} · seed {seed}", group.name)
        }
    };
    vec![Line::from(vec![
        Span::styled("┌─ ", theme::rule()),
        Span::styled(tag, ratatui::style::Style::default().fg(tag_color).add_modifier(ratatui::style::Modifier::BOLD)),
        Span::styled(" ", theme::chrome()),
        Span::styled(detail, theme::chrome()),
    ])]
}

fn member_line(
    group: &Group,
    member: &Member,
    last: bool,
    is_cursor: bool,
) -> Line<'static> {
    let connector = if last { "│  └─ " } else { "│  ├─ " };
    let cursor = if is_cursor { "▶ " } else { "  " };

    // Role glyph + role text are colored by the group's lineage palette.
    let (role_glyph, role_text, role_color) = role_repr(member.role, group.kind);

    let cursor_color = if is_cursor { theme::GREEN_HI } else { theme::CHROME };
    let id_style = if is_cursor { theme::green_hi() } else { theme::text() };

    let health_color = match member.health {
        Health::Healthy => theme::GREEN,
        Health::Degraded => theme::AMBER,
        Health::Unhealthy => theme::RED,
    };
    let health_text = match member.health {
        Health::Healthy => "Healthy",
        Health::Degraded => "Degraded",
        Health::Unhealthy => "Unhealthy",
    };

    Line::from(vec![
        Span::styled(connector, theme::rule()),
        Span::styled(cursor, ratatui::style::Style::default().fg(cursor_color)),
        Span::styled(role_glyph, ratatui::style::Style::default().fg(role_color)),
        Span::raw(" "),
        Span::styled(format!("{:<6}", member.id), id_style),
        Span::raw(" "),
        Span::styled(format!("{:<10}", role_text), ratatui::style::Style::default().fg(role_color)),
        Span::raw("  "),
        Span::styled(health_text, ratatui::style::Style::default().fg(health_color)),
    ])
}

fn role_repr(role: MemberRole, kind: GroupKind) -> (&'static str, String, ratatui::style::Color) {
    match role {
        MemberRole::Solo => ("◆", "solo".to_string(), theme::TEXT),
        MemberRole::Replica(i, state) => {
            let glyph = match state {
                "processing" => "▣", // filled = currently routed
                _ => "□",            // hollow = idle
            };
            (glyph, format!("m[{i}] {state}"), theme::GREEN_HI)
        }
        MemberRole::Fork(i) => {
            let _ = kind;
            ("┝", format!("fork[{i}]"), theme::AMBER)
        }
        MemberRole::Active(seq) => ("●", format!("active s={seq}"), theme::GREEN_HI),
        MemberRole::Standby(synced) => ("○", format!("warm  s={synced}"), theme::CYAN),
    }
}

// ───────────────────────── detail (right pane) ─────────────────────────

fn render_detail(frame: &mut Frame<'_>, area: Rect) {
    // Selected member, by cursor coordinates.
    let group = &GROUPS[CURSOR_GROUP];
    let member = &group.members[CURSOR_MEMBER];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(format!("{} ", theme::SECTION_PREFIX), theme::green()),
            Span::styled("DAEMON ", theme::green_hi()),
            Span::styled(format!("{} ", member.id), theme::text()),
            Span::styled(format!("· {}", group.name), theme::cyan()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),  // facts
            Constraint::Length(7),  // group siblings panel
            Constraint::Length(6),  // saturation history
            Constraint::Min(0),     // log tail + controls
        ])
        .split(inner);

    render_facts(frame, rows[0], group, member);
    render_group_panel(frame, rows[1], group);
    render_saturation(frame, rows[2]);
    render_log_tail(frame, rows[3]);
}

fn render_facts(frame: &mut Frame<'_>, area: Rect, group: &Group, member: &Member) {
    let group_line = match group.kind {
        GroupKind::Standalone => format!("standalone · no group"),
        GroupKind::Replica { seed, lb } => {
            format!("ReplicaGroup · seed {seed} · {lb} · {} members", group.members.len())
        }
        GroupKind::Fork { parent, seq } => {
            format!("ForkGroup · parent {parent} @ seq={seq} · {} forks", group.members.len())
        }
        GroupKind::Standby { seed } => {
            format!("StandbyGroup · seed {seed} · 1 active + {} warm", group.members.len() - 1)
        }
    };
    let role_line = match member.role {
        MemberRole::Solo => "solo · no siblings".to_string(),
        MemberRole::Replica(i, state) => format!("member[{i}] · {state} · interchangeable"),
        MemberRole::Fork(i) => format!("fork[{i}] · independent sibling · per-fork routing"),
        MemberRole::Active(seq) => format!("ACTIVE · processing · seq={seq}"),
        MemberRole::Standby(synced) => format!("STANDBY · warm · synced_through={synced}"),
    };
    let mut placement_spans = vec![Span::styled("placement  ", theme::chrome())];
    placement_spans.extend(nodes::id_spans("0xbf44"));
    placement_spans.push(Span::styled(" · score 0.91 · stable", theme::text()));

    let lines = vec![
        kv("identity   ", format!("ent.{} · ed25519:k7xRq…9pwn", member.id)),
        kv("lineage    ", group_line),
        kv("role       ", role_line),
        kv("kind       ", format!("{} · v1.2.0 · sig-verified", group.name)),
        kv("lifecycle  ", "Running · age 2h 14m".into()),
        kv("health     ", match member.health {
            Health::Healthy => "Healthy · last probe 380ms ago".into(),
            Health::Degraded => "Degraded · drift +2.1ms vs anchor".into(),
            Health::Unhealthy => "Unhealthy · failing probes 5/5".into(),
        }),
        kv("capability ", "[compute, gpu:gb300, region:ap-south1]".into()),
        Line::from(placement_spans),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_group_panel(frame: &mut Frame<'_>, area: Rect, group: &Group) {
    let (title, accent) = match group.kind {
        GroupKind::Standalone => ("LINEAGE  (no group)", theme::TEXT_DIM),
        GroupKind::Replica { .. } => ("LINEAGE  REPLICA SIBLINGS", theme::GREEN_HI),
        GroupKind::Fork { .. } => ("LINEAGE  FORK SIBLINGS", theme::AMBER),
        GroupKind::Standby { .. } => ("LINEAGE  STANDBY MEMBERS", theme::CYAN),
    };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled(title, ratatui::style::Style::default().fg(accent)),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (mi, m) in group.members.iter().enumerate() {
        let is_cursor = mi == CURSOR_MEMBER;
        let last = mi + 1 == group.members.len();
        let connector = if last { "└─ " } else { "├─ " };
        let cursor = if is_cursor { "▶" } else { " " };
        let (glyph, role_text, color) = role_repr(m.role, group.kind);
        let id_style = if is_cursor { theme::green_hi() } else { theme::text() };
        lines.push(Line::from(vec![
            Span::styled(connector, theme::rule()),
            Span::styled(cursor, theme::green_hi()),
            Span::raw(" "),
            Span::styled(glyph, ratatui::style::Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(format!("{:<6}", m.id), id_style),
            Span::raw(" "),
            Span::styled(role_text, ratatui::style::Style::default().fg(color)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_saturation(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled("SATURATION  ", theme::chrome()),
            Span::styled("60s window · 1s buckets", theme::dim()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let row0 = "▁ ▁ ▂ ▁ ▂ ▃ ▂ ▃ ▄ ▃ ▄ ▅ ▄ ▆ ▅ ▆ ▇ ▆ ▅ ▄ ▃ ▂ ▃ ▂ ▁ ▂ ▁ ▂ ▁ ▂ ▁ ▁";
    let row1 = "0.00                              0.50                              1.00";
    let label = Line::from(vec![
        Span::styled("p50 ", theme::chrome()),
        Span::styled("0.31  ", theme::green_hi()),
        Span::styled("p99 ", theme::chrome()),
        Span::styled("0.72  ", theme::amber()),
        Span::styled("max ", theme::chrome()),
        Span::styled("0.84", theme::amber()),
    ]);
    let lines = vec![
        Line::styled(row0, theme::green()),
        Line::styled(row1, theme::chrome()),
        label,
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_log_tail(frame: &mut Frame<'_>, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::rule())
        .title(Line::from(vec![
            Span::styled("LOG.TAIL  ", theme::chrome()),
            Span::styled("daemon · INFO+", theme::dim()),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(inner);

    let lines = vec![
        log_line("11:24:01.882", "INFO", "started · member[0] of ReplicaGroup seed=0xab12"),
        log_line("11:24:01.917", "INFO", "lb registered: round-robin slot 0/3"),
        log_line("11:24:02.044", "INFO", "warm cache 12.4 MB ready"),
        log_line("12:48:55.001", "INFO", "routed event #4901 · sat=0.31"),
        log_line("13:37:21.500", "INFO", "sibling 0xa9 took event #4902 (rr)"),
        log_line("13:37:21.880", "INFO", "control: backpressure_on level=2"),
    ];
    frame.render_widget(Paragraph::new(lines), rows[0]);

    let controls = Line::from(vec![
        Span::styled("[r] ", theme::green_hi()),
        Span::styled("restart   ", theme::dim()),
        Span::styled("[d] ", theme::green_hi()),
        Span::styled("drain    ", theme::dim()),
        Span::styled("[s] ", theme::green_hi()),
        Span::styled("scale    ", theme::dim()),
        Span::styled("[p] ", theme::green_hi()),
        Span::styled("promote  ", theme::dim()),
        Span::styled("[k] ", theme::red()),
        Span::styled("kill", theme::dim()),
    ]);
    frame.render_widget(Paragraph::new(controls), rows[1]);
}

fn kv(k: &'static str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(k, theme::chrome()),
        Span::styled(v, theme::text()),
    ])
}

fn log_line(ts: &'static str, level: &'static str, msg: &'static str) -> Line<'static> {
    let level_style = match level {
        "INFO" => theme::dim(),
        "WARN" => theme::amber(),
        "ERR" | "ERROR" => theme::red(),
        _ => theme::text(),
    };
    Line::from(vec![
        Span::styled(ts, theme::chrome()),
        Span::styled("  ", theme::chrome()),
        Span::styled(format!("{level:<5}"), level_style),
        Span::styled("  ", theme::chrome()),
        Span::styled(msg, theme::text()),
    ])
}
