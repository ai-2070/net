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
pub fn render(frame: &mut Frame<'_>, area: Rect, current: Tab, in_focus: bool, toast: Option<&str>) {
    // Active toast hijacks the footer row — confirmation
    // messages need to be visible against the binding hints.
    if let Some(msg) = toast {
        let line = Line::from(vec![Span::styled(format!("  {msg}"), theme::green_hi())]);
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let line = if in_focus {
        focus_chips()
    } else {
        tab_chips(current)
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Chips shown while the operator is on the NODE page (Enter
/// drilldown). Most cluster actions don't apply at this scope, so
/// the row strips down to navigation + the exit hint.
fn focus_chips() -> Line<'static> {
    let mut spans = base_nav();
    spans.extend([
        chip_key("Esc"),
        chip_desc(" back   "),
        chip_key("?"),
        chip_desc(" help   "),
        chip_key("q"),
        chip_desc(" quit"),
    ]);
    Line::from(spans)
}

fn tab_chips(current: Tab) -> Line<'static> {
    let mut spans = base_nav();
    match current {
        Tab::NetMap => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" detail   "),
            ]);
        }
        Tab::List => {
            spans.extend([
                chip_key("Enter"),
                chip_desc(" detail   "),
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
        Tab::Dataforts => {
            spans.extend([
                chip_key("B"),
                chip_desc(" blobs   "),
            ]);
        }
        Tab::Daemon => {
            spans.extend([
                chip_key("W/S"),
                chip_desc(" group   "),
                chip_key("r"),
                chip_desc(" restart   "),
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
        chip_key("1-9"),
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
