//! Help overlay — lists every keybinding the deck knows
//! about, grouped by purpose. Opened with `?`; dismissed
//! with `Esc` / `q` / `?`.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

struct Binding {
    keys: &'static str,
    desc: &'static str,
}

/// Routine bindings — navigation + cursor + dismissals.
const NAVIGATION: &[Binding] = &[
    Binding { keys: "1-8",       desc: "jump to tab" },
    Binding { keys: "Tab / ◂▸",  desc: "cycle tab" },
    Binding { keys: "j / k",     desc: "cursor down / up" },
    Binding { keys: "g / G",     desc: "cursor to top / bottom" },
    Binding { keys: "J / K",     desc: "DAEMON: next / prev group" },
    Binding { keys: "f",         desc: "AUDIT/LOGS: cycle filter (ICE-only or min level)" },
    Binding { keys: "n",         desc: "AUDIT: cycle row limit (none/25/100)" },
    Binding { keys: "?",         desc: "toggle this help overlay" },
    Binding { keys: "q / Esc",   desc: "quit (or close modal)" },
    Binding { keys: "Ctrl-C",    desc: "quit" },
];

const ADMIN: &[Binding] = &[
    Binding { keys: "c",   desc: "LIST: cordon (cursored node)" },
    Binding { keys: "C",   desc: "LIST: uncordon" },
    Binding { keys: "d",   desc: "LIST: drain (5min window)" },
    Binding { keys: "m",   desc: "LIST: enter maintenance" },
    Binding { keys: "M",   desc: "LIST: exit maintenance" },
    Binding { keys: "a",   desc: "LIST: clear avoid list" },
    Binding { keys: "i",   desc: "LIST: invalidate placement" },
    Binding { keys: "D",   desc: "LIST: drop all replicas on node" },
    Binding { keys: "r",   desc: "DAEMON: restart all daemons (on host)" },
];

const ICE: &[Binding] = &[
    Binding { keys: "F",   desc: "LIST: ICE freeze cluster (60s ttl)" },
    Binding { keys: "T",   desc: "LIST: ICE thaw cluster" },
    Binding { keys: "A",   desc: "LIST: ICE flush avoid lists (global)" },
    Binding { keys: "R",   desc: "DAEMON: ICE force-restart (bypass backoff)" },
    Binding { keys: "K",   desc: "MIGRATIONS: ICE kill migration" },
    Binding { keys: "E",   desc: "REPLICAS: ICE force-evict first holder" },
    Binding { keys: "O",   desc: "REPLICAS: ICE force-cutover (pick target)" },
];

const MODAL: &[Binding] = &[
    Binding { keys: "Enter / Space",  desc: "confirm pending action" },
    Binding { keys: "Esc / q",        desc: "cancel pending action" },
];

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let modal_area = center(area, 78, 38);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(Line::from(vec![
            Span::styled(" ? ", theme::green()),
            Span::styled(
                "DECK KEYBINDINGS",
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
            Constraint::Length(11), // navigation
            Constraint::Length(11), // admin
            Constraint::Length(9),  // ice
            Constraint::Length(4),  // modal
            Constraint::Min(0),     // footer
        ])
        .split(inner);

    render_section(frame, rows[0], "NAVIGATION", NAVIGATION);
    render_section(frame, rows[1], "ADMIN  (signed, audit-logged)", ADMIN);
    render_section(frame, rows[2], "ICE  (break-glass, red modal)", ICE);
    render_section(frame, rows[3], "MODAL", MODAL);

    let footer = Line::from(vec![
        Span::styled("[?] / [Esc] ", theme::green_hi()),
        Span::styled("close help", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(footer).alignment(Alignment::Center),
        rows[4],
    );
}

fn render_section(frame: &mut Frame<'_>, area: Rect, title: &str, bindings: &[Binding]) {
    let mut lines: Vec<Line> = Vec::with_capacity(bindings.len() + 1);
    lines.push(Line::from(vec![Span::styled(
        format!("── {title} ──"),
        theme::chrome(),
    )]));
    for b in bindings {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<15}", b.keys), theme::green_hi()),
            Span::styled(b.desc.to_string(), theme::text()),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
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
