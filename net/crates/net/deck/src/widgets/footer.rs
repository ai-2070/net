use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::Tab, theme};

/// Render the bottom hint row. Each tab gets only the chips that
/// are actually wired on that tab — operators don't see options
/// they can't use. Common keys (tab/jump/cursor/help/quit) are
/// always present.
/// Focus context — which kind of page the operator is on, so
/// the footer can advertise the right action keys.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusKind {
    None,
    Node,
    Daemon,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    current: Tab,
    focus: FocusKind,
    toast: Option<&str>,
) {
    // Active toast hijacks the footer row — confirmation
    // messages need to be visible against the binding hints.
    if let Some(msg) = toast {
        let line = Line::from(vec![Span::styled(format!("  {msg}"), theme::green_hi())]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let line = match focus {
        FocusKind::Node => node_focus_chips(),
        FocusKind::Daemon => daemon_focus_chips(),
        FocusKind::None => tab_chips(current),
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Chips for the NODE page — mirrors the NODES tab's admin
/// actions so an operator on the focused page sees the same
/// keys they'd use from the list.
fn node_focus_chips() -> Line<'static> {
    let mut spans = base_nav();
    spans.extend([
        chip_key("Enter"),
        chip_desc(" daemon   "),
        chip_key("l"),
        chip_desc(" logs   "),
        chip_key("c/C"),
        chip_desc(" cordon   "),
        chip_key("d"),
        chip_desc(" drain   "),
        chip_key("m/M"),
        chip_desc(" maint   "),
        chip_key("a"),
        chip_desc(" avoid   "),
        chip_key("i"),
        chip_desc(" inval   "),
        chip_key("Esc"),
        chip_desc(" back   "),
        chip_key("q"),
        chip_desc(" quit"),
    ]);
    Line::from(spans)
}

/// Chips for the DAEMON page — restart-all on the placement
/// host + ICE force-restart on the daemon.
fn daemon_focus_chips() -> Line<'static> {
    let mut spans = base_nav();
    spans.extend([
        chip_key("Enter"),
        chip_desc(" drill   "),
        chip_key("l"),
        chip_desc(" logs   "),
        chip_key("r"),
        chip_desc(" restart   "),
        chip_key("R"),
        chip_desc(" ICE restart   "),
        chip_key("Esc"),
        chip_desc(" back   "),
        chip_key("q"),
        chip_desc(" quit"),
    ]);
    Line::from(spans)
}

fn tab_chips(current: Tab) -> Line<'static> {
    let mut spans = base_nav();
    match current {
        Tab::NetMap => {
            spans.extend([chip_key("Enter"), chip_desc(" detail   ")]);
        }
        Tab::Nodes => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" detail   "),
                chip_key("l"),
                chip_desc(" logs   "),
                chip_key("c/C"),
                chip_desc(" cordon   "),
                chip_key("d"),
                chip_desc(" drain   "),
                chip_key("m/M"),
                chip_desc(" maint   "),
                chip_key("a"),
                chip_desc(" avoid   "),
                chip_key("i"),
                chip_desc(" inval   "),
            ]);
        }
        Tab::Daemons => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" host node   "),
                chip_key("l"),
                chip_desc(" logs   "),
                chip_key("r"),
                chip_desc(" restart   "),
                chip_key("R"),
                chip_desc(" ICE restart   "),
            ]);
        }
        Tab::Dataforts => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" detail   "),
                chip_key("b"),
                chip_desc(" blobs   "),
                chip_key("l"),
                chip_desc(" logs   "),
            ]);
        }
        Tab::Groups => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" host node   "),
                chip_key("W/S"),
                chip_desc(" group   "),
                chip_key("r"),
                chip_desc(" restart   "),
                chip_key("R"),
                chip_desc(" ICE restart   "),
            ]);
        }
        Tab::Logs => {
            spans.extend([
                chip_key("f"),
                chip_desc(" filter   "),
                chip_key("p"),
                chip_desc(" pause   "),
                chip_key("/"),
                chip_desc(" search   "),
                chip_key("e"),
                chip_desc(" export   "),
            ]);
        }
        Tab::Audit => {
            spans.extend([
                chip_key("f"),
                chip_desc(" filter   "),
                chip_key("n"),
                chip_desc(" limit   "),
                chip_key("/"),
                chip_desc(" search   "),
                chip_key("e"),
                chip_desc(" export   "),
            ]);
        }
        Tab::Replicas | Tab::Migrations => {
            // No lowercase per-tab actions; admin actions are
            // ICE-only and surface via the help overlay.
        }
        Tab::Nrpc => {
            spans.extend([chip_key("p"), chip_desc(" pause   ")]);
        }
        Tab::Failures => {
            spans.extend([
                chip_key("/"),
                chip_desc(" search   "),
                chip_key("e"),
                chip_desc(" export   "),
            ]);
        }
        Tab::Blobs => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" detail   "),
                chip_key("/"),
                chip_desc(" search   "),
                chip_key("e"),
                chip_desc(" export   "),
            ]);
        }
    }
    // `[B]` triggers a 5 s nRPC burst against the demo's
    // requester loops. Surfaces only under the `demo` feature
    // so non-demo builds don't carry a hint for a keybinding
    // that has no effect.
    if cfg!(feature = "demo") {
        spans.extend([chip_key("B"), chip_desc(" bench   ")]);
    }
    spans.extend([
        chip_key("?"),
        chip_desc(" help   "),
        chip_key("q"),
        chip_desc(" quit"),
    ]);
    Line::from(spans)
}

fn base_nav() -> Vec<Span<'static>> {
    vec![
        chip_key("◂▸"),
        chip_desc(" tab   "),
        chip_key("0-9"),
        chip_desc(" jump   "),
        chip_key("↑↓"),
        chip_desc(" cursor   "),
    ]
}

fn chip_key(s: &'static str) -> Span<'static> {
    Span::styled(s, theme::green())
}

fn chip_desc(s: &'static str) -> Span<'static> {
    Span::styled(s, theme::dim())
}
