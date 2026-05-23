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

    // Tab key glyphs: 1..=9 for the first 9 slots, then `0`
    // for the 10th (LOGS) — mirrors the numeric jump key
    // handler in app.rs. Tabs beyond `PRIMARY_COUNT` (the
    // SUBNETS / GATEWAYS / AGGREGATORS / AUDIT trailing
    // group) render with no key prefix; they're reachable via
    // letter shortcuts (`H`/`V`/`B`/`U`) only.
    let key_for = |i: usize| -> String {
        if i >= Tab::PRIMARY_COUNT {
            String::new()
        } else if i == 9 {
            "[0] ".to_string()
        } else {
            format!("[{}] ", i + 1)
        }
    };

    // First pass — compute the visible cell width each tab
    // entry needs (`[N] LABEL ` = key + label + trailing
    // space). All ASCII so the byte length is the cell width.
    let tabs: Vec<(String, &'static str)> = Tab::all()
        .iter()
        .enumerate()
        .map(|(i, t)| (key_for(i), t.label()))
        .collect();
    let entry_width = |e: &(String, &'static str)| e.0.len() + e.1.len() + 1;
    let total_width: usize = tabs.iter().map(entry_width).sum();
    let avail = cols[1].width as usize;

    // Fits — render the full strip.
    let mut spans = Vec::new();
    if total_width <= avail {
        for (i, tab) in Tab::all().iter().enumerate() {
            let key = &tabs[i].0;
            if *tab == current {
                spans.push(Span::styled(key.clone(), theme::green()));
                spans.push(Span::styled(tab.label(), theme::green_hi()));
            } else {
                spans.push(Span::styled(key.clone(), theme::chrome()));
                spans.push(Span::styled(tab.label(), theme::dim()));
            }
            spans.push(Span::raw(" "));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), cols[1]);
        return;
    }

    // Doesn't fit — reserve room for the trailing `+N` chip
    // and walk forwards until adding the next tab would overrun.
    // The `+N` indicator is dim chrome so the operator notices
    // hidden tabs without it competing with the active label.
    let mut used = 0usize;
    let mut shown = 0usize;
    let all = Tab::all();
    for (i, tab) in all.iter().enumerate() {
        let need = entry_width(&tabs[i]);
        // Reserve `+<count> ` for the chip (max 5 chars at 10
        // tabs total) so the chip always fits.
        let reserve = 5;
        if used + need + reserve > avail {
            break;
        }
        let key = &tabs[i].0;
        if *tab == current {
            spans.push(Span::styled(key.clone(), theme::green()));
            spans.push(Span::styled(tab.label(), theme::green_hi()));
        } else {
            spans.push(Span::styled(key.clone(), theme::chrome()));
            spans.push(Span::styled(tab.label(), theme::dim()));
        }
        spans.push(Span::raw(" "));
        used += need;
        shown += 1;
    }
    let hidden = all.len().saturating_sub(shown);
    if hidden > 0 {
        spans.push(Span::styled(format!("+{hidden}"), theme::dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), cols[1]);
}
