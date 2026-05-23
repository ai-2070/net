use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::Tab, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, current: Tab) {
    // Brand on the left, tabs filling the rest. The "● LIVE"
    // chip the right side used to carry duplicated the status
    // bar's live indicator — dropped so the strip can fit
    // every tab on a 120-col terminal.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    let brand = Line::from(vec![Span::styled("DECK  ", theme::green_hi())]);
    frame.render_widget(Paragraph::new(brand), cols[0]);

    // Tab key glyphs: numeric prefixes `[1]`..`[9]` plus `[0]`
    // (LOGS) for the primary slots — mirrors the numeric jump
    // handler in app.rs. The trailing SUBNETS / GATEWAYS /
    // AGGREGATORS / AUDIT group renders with its letter
    // shortcut (`[H]`/`[V]`/`[B]`/`[U]`) so the operator can
    // see the keystroke alongside the label.
    let key_for = |i: usize, tab: Tab| -> String {
        if i < Tab::PRIMARY_COUNT {
            return if i == 9 {
                "[0] ".to_string()
            } else {
                format!("[{}] ", i + 1)
            };
        }
        let letter = match tab {
            Tab::Subnets => 'H',
            Tab::Gateways => 'V',
            Tab::Aggregators => 'B',
            Tab::Audit => 'U',
            // Any future trailing tab without a letter falls
            // through to label-only.
            _ => return String::new(),
        };
        format!("[{letter}] ")
    };

    let tabs: Vec<(String, &'static str)> = Tab::all()
        .iter()
        .enumerate()
        .map(|(i, t)| (key_for(i, *t), t.label()))
        .collect();
    // ASCII so byte length == cell width. Entry width is
    // `[N] LABEL ` — key + label + trailing space.
    let widths: Vec<usize> = tabs.iter().map(|e| e.0.len() + e.1.len() + 1).collect();
    let total_width: usize = widths.iter().sum();
    let avail = cols[1].width as usize;

    let all = Tab::all();
    let current_idx = all
        .iter()
        .position(|t| *t == current)
        // Focused-page-only variants aren't in `all()`; treat
        // them as "no tab highlighted" and start the window at
        // the head.
        .unwrap_or(0);

    let mut spans = Vec::new();
    let push_tab = |spans: &mut Vec<Span<'_>>, i: usize, current_idx: usize| {
        let key = tabs[i].0.clone();
        let label = tabs[i].1;
        if i == current_idx {
            spans.push(Span::styled(key, theme::green()));
            spans.push(Span::styled(label, theme::green_hi()));
        } else {
            spans.push(Span::styled(key, theme::chrome()));
            spans.push(Span::styled(label, theme::dim()));
        }
        spans.push(Span::raw(" "));
    };

    // Fits — render the full strip.
    if total_width <= avail {
        for i in 0..tabs.len() {
            push_tab(&mut spans, i, current_idx);
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), cols[1]);
        return;
    }

    // Doesn't fit — scroll the window so `current` stays
    // visible. Same shape as `tabs::scroll_window` for the
    // vertical lists: pick a window containing `current_idx`,
    // reserve room on each side for a `<N` / `+N` chip iff
    // that side actually has hidden entries.
    let (start, end) = scroll_window_horizontal(&widths, avail, current_idx);
    if start > 0 {
        spans.push(Span::styled(format!("<{start} "), theme::dim()));
    }
    for i in start..end {
        push_tab(&mut spans, i, current_idx);
    }
    let hidden_after = all.len().saturating_sub(end);
    if hidden_after > 0 {
        spans.push(Span::styled(format!("+{hidden_after}"), theme::dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), cols[1]);
}

/// Pick a contiguous window `start..end` of tab indices whose
/// summed `widths` fits within `avail` while keeping
/// `current_idx` visible. Mirrors the vertical scroll_window's
/// 2-pass reservation: chip-cells (`<N` / `+N`) are reserved
/// only when that side actually hides entries.
///
/// `widths[i]` is the cell width of tab `i` (including its
/// trailing space). `avail` is the column count available to
/// the strip. `current_idx` is the active tab — kept inside
/// the returned window.
fn scroll_window_horizontal(
    widths: &[usize],
    avail: usize,
    current_idx: usize,
) -> (usize, usize) {
    let n = widths.len();
    if n == 0 || avail == 0 {
        return (0, 0);
    }
    // Per-side chip cost. `<N ` and `+N` for N up to two
    // digits — `<NN ` is 4 cells, `+NN` is 3 cells. Reserve
    // the worst case so the chip always fits even when the
    // hidden count grows.
    const LEFT_CHIP: usize = 4;
    const RIGHT_CHIP: usize = 3;

    // 2-pass reservation: on each pass decide whether the
    // left / right chips will be needed given the current
    // viewport; tighten the viewport accordingly; recompute.
    let mut left_reserve = 0usize;
    let mut right_reserve = 0usize;
    let cur = current_idx.min(n - 1);
    let (mut start, mut end) = (cur, cur);
    for _ in 0..3 {
        let viewport = avail.saturating_sub(left_reserve + right_reserve);
        if viewport == 0 {
            return (cur, cur + 1);
        }
        // Greedy: place `current` first, then alternately add
        // neighbors on the side with the smaller next-cost.
        // Bias to growing right first when both sides are
        // equally cheap so a head-pinned current tab still
        // reveals its right neighbors (matches the natural
        // reading direction).
        let cur_w = widths[cur];
        if cur_w > viewport {
            return (cur, cur + 1);
        }
        let mut used = cur_w;
        start = cur;
        end = cur + 1;
        loop {
            let can_right = end < n && used + widths[end] <= viewport;
            let can_left = start > 0 && used + widths[start - 1] <= viewport;
            if !can_right && !can_left {
                break;
            }
            // Prefer right unless only left is feasible.
            if can_right && (!can_left || widths[end] <= widths[start - 1]) {
                used += widths[end];
                end += 1;
            } else if can_left {
                start -= 1;
                used += widths[start];
            } else {
                break;
            }
        }
        let need_left = if start > 0 { LEFT_CHIP } else { 0 };
        let need_right = if end < n { RIGHT_CHIP } else { 0 };
        if need_left == left_reserve && need_right == right_reserve {
            return (start, end);
        }
        left_reserve = need_left;
        right_reserve = need_right;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All tabs fit — full window, no chips.
    #[test]
    fn fits_returns_full_range() {
        let widths = vec![10, 10, 10, 10];
        let (start, end) = scroll_window_horizontal(&widths, 40, 2);
        assert_eq!((start, end), (0, 4));
    }

    /// Current at head — window starts at 0, right chip space
    /// reserved when entries are hidden on the right.
    #[test]
    fn current_at_head_window_starts_at_zero() {
        let widths = vec![10, 10, 10, 10, 10];
        let (start, end) = scroll_window_horizontal(&widths, 25, 0);
        assert_eq!(start, 0);
        // Window must contain the current tab.
        assert!(end > 0);
        // Reserved 3 cells for `+N` so we fit at most 2 tabs
        // (each costs 10).
        assert!(end <= 3);
    }

    /// Current in the middle — window slides so cursor stays
    /// visible; both side chips reserve space.
    #[test]
    fn current_in_middle_window_contains_it() {
        let widths = vec![10, 10, 10, 10, 10];
        let (start, end) = scroll_window_horizontal(&widths, 25, 2);
        assert!(start <= 2 && 2 < end, "window {start}..{end} missing cursor");
    }

    /// Current at tail — window slides to the right edge.
    #[test]
    fn current_at_tail_window_ends_at_n() {
        let widths = vec![10, 10, 10, 10, 10];
        let (_start, end) = scroll_window_horizontal(&widths, 25, 4);
        assert_eq!(end, 5);
    }

    /// `avail == 0` returns an empty window (no panic).
    #[test]
    fn zero_avail_yields_empty_window() {
        let widths = vec![10, 10, 10];
        let (start, end) = scroll_window_horizontal(&widths, 0, 1);
        assert_eq!((start, end), (0, 0));
    }

    /// Single tab wider than avail — still returns the cursor's
    /// position as a degenerate window so the renderer can show
    /// at least the active tab (truncated by the terminal if
    /// needed).
    #[test]
    fn single_wide_tab_clamps_to_cursor() {
        let widths = vec![5, 50, 5];
        let (start, end) = scroll_window_horizontal(&widths, 20, 1);
        assert_eq!((start, end), (1, 2));
    }
}
