//! Export-confirmation modal. Pops after `[e]` lands a file
//! on disk so the operator sees the resolved path before
//! returning to the tab — toasts are easy to miss in a
//! busy session, and the path is the actionable bit
//! (operator copies it into the incident write-up).

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

/// Outcome variants the modal renders. The success path
/// surfaces the path + record count; the failure path
/// surfaces the error string so the operator can paste it
/// into a bug report.
#[derive(Clone, Debug)]
pub enum ExportOutcome {
    Ok { tab: String, path: String, count: usize },
    Err { tab: String, message: String },
}

pub fn render(frame: &mut Frame<'_>, area: Rect, outcome: &ExportOutcome) {
    let modal_area = center(area, 78, 11);
    frame.render_widget(Clear, modal_area);

    let (border_style, accent, banner) = match outcome {
        ExportOutcome::Ok { .. } => (theme::green(), theme::GREEN_HI, "EXPORT  ✓"),
        ExportOutcome::Err { .. } => (theme::red(), theme::RED, "EXPORT  ✗"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                banner,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
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
            Constraint::Length(1), // path / err
            Constraint::Length(1), // count / spacer
            Constraint::Length(1), // hint
            Constraint::Min(0),    // pad
            Constraint::Length(1), // bindings
        ])
        .split(inner);

    match outcome {
        ExportOutcome::Ok { tab, path, count } => {
            frame.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    format!("wrote {tab} export"),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                )]))
                .alignment(Alignment::Center),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  path  ", theme::chrome()),
                    Span::styled(path.clone(), theme::green_hi()),
                ])),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  rows  ", theme::chrome()),
                    Span::styled(format!("{count}"), theme::text()),
                ])),
                rows[3],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    "ISO 8601 UTC stamp in the filename — replace `-` between digits with `:` to parse as RFC 3339.",
                    theme::dim(),
                )]))
                .alignment(Alignment::Center),
                rows[4],
            );
        }
        ExportOutcome::Err { tab, message } => {
            frame.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    format!("{tab} export failed"),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                )]))
                .alignment(Alignment::Center),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  err   ", theme::chrome()),
                    Span::styled(message.clone(), theme::red()),
                ])),
                rows[2],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    "nothing was written to disk.",
                    theme::dim(),
                )]))
                .alignment(Alignment::Center),
                rows[4],
            );
        }
    }

    let bindings = Line::from(vec![
        Span::styled("[Esc / Enter]", theme::dim()),
        Span::styled(" close", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[6],
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
