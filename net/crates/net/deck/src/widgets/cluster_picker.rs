//! Cluster picker modal — opens with `:` from any tab. Lists
//! the always-present `"local"` entry first, then the
//! bookmark store's entries sorted pinned-first.
//!
//! Selecting `local` is a no-op when already active; selecting
//! a remote bookmark today surfaces a toast noting the
//! substrate RPC slice is required (per
//! `DECK_PLAN.md` § Deferred work § Multi-Cluster Switcher).
//! The picker UX exists ahead of the wire layer so operators
//! can manage bookmarks via the `bookmarks.toml` they edit
//! directly; the dial happens when the substrate slot lands.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::{bookmarks::Bookmark, theme};

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    bookmarks: &[Bookmark],
    active_cluster: &str,
    cursor: usize,
) {
    let modal_area = center(area, 64, 20);
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::green())
        .title(Line::from(vec![
            Span::styled(" : ", theme::green()),
            Span::styled(
                "CLUSTER",
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
            Constraint::Length(1), // headline
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // cluster list
            Constraint::Length(1), // bindings
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "pick a cluster context",
            Style::default()
                .fg(theme::GREEN_HI)
                .add_modifier(Modifier::BOLD),
        )]))
        .alignment(Alignment::Center),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "edit `bookmarks.toml` to add / remove / pin entries",
            theme::dim(),
        )]))
        .alignment(Alignment::Center),
        rows[1],
    );

    // Build the picker entries: `local` at index 0, then
    // sorted bookmarks.
    let mut lines: Vec<Line> = Vec::with_capacity(bookmarks.len() + 1);
    let active_idx_for_label = |name: &str| name == active_cluster;
    let max = rows[3].height as usize;
    let total = bookmarks.len() + 1;
    let cursor = cursor.min(total.saturating_sub(1));

    let half = max / 2;
    let start = cursor.saturating_sub(half);
    let end = (start + max).min(total);
    let start = end.saturating_sub(max);

    for i in start..end {
        let is_cursor = i == cursor;
        let marker = if is_cursor { "▶ " } else { "  " };
        if i == 0 {
            // The synthetic local entry.
            let active = active_idx_for_label("local");
            let mut spans = vec![Span::styled(marker, theme::green_hi())];
            let id_style = if is_cursor { theme::green_hi() } else { theme::text() };
            spans.push(Span::styled("local", id_style));
            spans.push(Span::styled("    in-process MeshOS runtime", theme::dim()));
            if active {
                spans.push(Span::styled("    ◀ active", theme::amber()));
            }
            lines.push(Line::from(spans));
        } else {
            let bm = &bookmarks[i - 1];
            let active = active_idx_for_label(&bm.name);
            let mut spans = vec![Span::styled(marker, theme::green_hi())];
            let name_style = if is_cursor { theme::green_hi() } else { theme::text() };
            spans.push(Span::styled(bm.name.clone(), name_style));
            if bm.pinned {
                spans.push(Span::styled("  📌", theme::amber()));
            }
            spans.push(Span::styled(
                format!("    {}", bm.endpoint),
                theme::dim(),
            ));
            if active {
                spans.push(Span::styled("    ◀ active", theme::amber()));
            }
            lines.push(Line::from(spans));
        }
    }
    frame.render_widget(Paragraph::new(lines), rows[3]);

    let bindings = Line::from(vec![
        Span::styled("[j/k]", theme::green_hi()),
        Span::styled(" cursor    ", theme::dim()),
        Span::styled("[Enter]", theme::green_hi()),
        Span::styled(" select    ", theme::dim()),
        Span::styled("[Esc]", theme::dim()),
        Span::styled(" cancel", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[4],
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
