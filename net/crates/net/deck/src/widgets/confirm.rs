//! Confirmation-prompt overlay. Every signed admin action
//! flows through this widget — the operator gets a centered
//! box describing what's about to commit, with `[Enter]` and
//! `[Esc]` bindings.
//!
//! Rendered by `App::draw` after the tab content, so it
//! visually sits on top.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

/// Typed description of a pending operator action. Carries
/// the human-readable details for rendering; the dispatch
/// side decodes the variant to know which SDK call to fire.
#[derive(Clone, Debug)]
pub enum ConfirmAction {
    /// Restart every daemon on the given node. Reads from
    /// `admin().restart_all_daemons(node)`.
    RestartAllDaemons {
        node: u64,
        /// Pre-formatted `id.label` for the display string.
        node_display: String,
        /// Optional context — how many daemons live on this
        /// node — so the operator sees the blast radius.
        daemon_count: usize,
    },
    /// Mark the node as not accepting new placements. Reads
    /// from `admin().cordon(node)`. Reversible via `Uncordon`.
    Cordon { node: u64, node_display: String },
    /// Reverse a prior cordon. Reads from
    /// `admin().uncordon(node)`.
    Uncordon { node: u64, node_display: String },
}

impl ConfirmAction {
    /// One-line headline shown bold at the top of the modal.
    pub fn headline(&self) -> String {
        match self {
            Self::RestartAllDaemons { node_display, .. } => {
                format!("restart all daemons on {node_display}")
            }
            Self::Cordon { node_display, .. } => format!("cordon node {node_display}"),
            Self::Uncordon { node_display, .. } => {
                format!("uncordon node {node_display}")
            }
        }
    }

    /// Multi-line detail body. Each Vec entry is one rendered
    /// row.
    pub fn detail(&self) -> Vec<String> {
        match self {
            Self::RestartAllDaemons { daemon_count, .. } => vec![
                format!("affects {daemon_count} daemon(s) on the host node"),
                "each daemon is stopped and re-spawned by the supervisor".to_string(),
                "fires `admin().restart_all_daemons(node)` — signed,".to_string(),
                "lands on the admin chain with the operator's identity".to_string(),
            ],
            Self::Cordon { .. } => vec![
                "stops new placements from landing on this node".to_string(),
                "existing daemons + replicas stay; no eviction".to_string(),
                "reversible via `[C]` (uncordon) without further effect".to_string(),
                "fires `admin().cordon(node)` — signed, audit-logged".to_string(),
            ],
            Self::Uncordon { .. } => vec![
                "re-admits the node to the placement scorer".to_string(),
                "new replicas + daemons may land here on the next pass".to_string(),
                "no-op if the node was never cordoned".to_string(),
                "fires `admin().uncordon(node)` — signed, audit-logged".to_string(),
            ],
        }
    }
}

/// Render the modal centered over `area`. The Clear widget
/// wipes the underlying cells so the modal isn't transparent.
pub fn render(frame: &mut Frame<'_>, area: Rect, action: &ConfirmAction) {
    let modal_area = center(area, 64, 12);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::amber())
        .title(Line::from(vec![
            Span::styled(" ⚠ ", theme::amber()),
            Span::styled(
                "CONFIRM",
                Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
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
            Constraint::Min(0),    // detail
            Constraint::Length(1), // bindings
        ])
        .split(inner);

    let headline = Line::from(vec![
        Span::styled(action.headline(), theme::green_hi()),
    ]);
    frame.render_widget(
        Paragraph::new(headline).alignment(Alignment::Center),
        rows[0],
    );

    let detail_lines: Vec<Line> = action
        .detail()
        .into_iter()
        .map(|s| Line::from(Span::styled(s, theme::text())))
        .collect();
    frame.render_widget(
        Paragraph::new(detail_lines).alignment(Alignment::Center),
        rows[2],
    );

    let bindings = Line::from(vec![
        Span::styled("[Enter]", theme::green_hi()),
        Span::styled(" confirm    ", theme::dim()),
        Span::styled("[Esc]", theme::red()),
        Span::styled(" cancel", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[3],
    );
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
