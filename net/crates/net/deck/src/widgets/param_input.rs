//! Param-input modal. A focused text entry for actions whose
//! semantic value is a Duration (drain windows, ICE freeze
//! TTLs) — replaces hard-coded constants with operator-typed
//! values. On Enter the App parses the buffer with
//! `parse_duration` and transitions to a `Confirm` modal
//! carrying the parsed value; out-of-range or unparseable
//! input sets an `error` on the modal so the operator gets
//! immediate feedback without leaving the prompt.

use std::time::Duration;

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme;

/// Why the param-input modal is open. Drives the headline,
/// hint, range bounds, and which `ConfirmAction` the App
/// builds on commit.
#[derive(Clone, Debug)]
pub enum ParamInputPurpose {
    /// Drain window for a routine `Drain` admin commit.
    DrainWindow { node: u64, node_display: String },
    /// TTL for `IceFreezeCluster`. ICE-class.
    IceFreezeTtl,
}

impl ParamInputPurpose {
    pub fn headline(&self) -> String {
        match self {
            Self::DrainWindow { node_display, .. } => format!("drain  {node_display}"),
            Self::IceFreezeTtl => "ICE  freeze cluster".to_string(),
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            Self::DrainWindow { .. } => "window for the placement controller to relocate workload",
            Self::IceFreezeTtl => {
                "global placement freeze — auto-thaws after TTL or on manual thaw"
            }
        }
    }

    /// Pre-filled buffer the operator can accept with Enter.
    pub fn default_buffer(&self) -> &'static str {
        match self {
            Self::DrainWindow { .. } => "5m",
            Self::IceFreezeTtl => "60s",
        }
    }

    /// Inclusive (min, max) bounds for sanity-checking the
    /// parsed value. Way-out-of-range numbers usually mean the
    /// operator typed `5` instead of `5m` or similar.
    pub fn range(&self) -> (Duration, Duration) {
        match self {
            Self::DrainWindow { .. } => (Duration::from_secs(1), Duration::from_secs(60 * 60 * 24)),
            Self::IceFreezeTtl => (Duration::from_secs(5), Duration::from_secs(60 * 30)),
        }
    }

    /// Accent color reflecting the eventual ConfirmAction's
    /// risk level — ICE flows render red, routine flows amber.
    pub fn is_ice(&self) -> bool {
        matches!(self, Self::IceFreezeTtl)
    }
}

/// Parse a `1h30m`/`5m`/`90s`/`120`-style duration. Trailing
/// digits with no unit suffix are treated as seconds (so a
/// bare `60` reads as 60s, which matches how operators muscle-
/// memory the freeze TTL).
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty input".to_string());
    }
    let mut total = Duration::ZERO;
    let mut num: u64 = 0;
    let mut have_digit = false;
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            num = num
                .checked_mul(10)
                .and_then(|n| n.checked_add(d as u64))
                .ok_or_else(|| "value overflows u64 seconds".to_string())?;
            have_digit = true;
        } else if !have_digit {
            return Err(format!("expected digit before '{c}'"));
        } else {
            let unit = match c {
                'h' | 'H' => Duration::from_secs(num.saturating_mul(3600)),
                'm' | 'M' => Duration::from_secs(num.saturating_mul(60)),
                's' | 'S' => Duration::from_secs(num),
                _ => return Err(format!("unknown unit '{c}'; use s / m / h")),
            };
            total = total
                .checked_add(unit)
                .ok_or_else(|| "duration overflows".to_string())?;
            num = 0;
            have_digit = false;
        }
    }
    if have_digit {
        total = total
            .checked_add(Duration::from_secs(num))
            .ok_or_else(|| "duration overflows".to_string())?;
    }
    Ok(total)
}

/// Render the active duration as `Xm Ys` / `Yh Zm` for the
/// preview line so the operator can verify what the buffer
/// parses to without reading the next modal.
pub fn fmt_duration(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    purpose: &ParamInputPurpose,
    buffer: &str,
    error: Option<&str>,
) {
    let modal_area = center(area, 64, 14);
    frame.render_widget(Clear, modal_area);

    let (border_style, accent, marker, banner) = if purpose.is_ice() {
        (theme::red(), theme::red(), " ❄ ", "ICE  PARAMETER")
    } else {
        (theme::amber(), theme::amber(), " · ", "PARAMETER")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![
            Span::styled(marker, accent),
            Span::styled(
                banner,
                Style::default()
                    .fg(accent.fg.unwrap_or_default())
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
            Constraint::Length(1), // input line
            Constraint::Length(1), // preview / error
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // range hint
            Constraint::Length(1), // bindings
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            purpose.headline(),
            Style::default()
                .fg(accent.fg.unwrap_or_default())
                .add_modifier(Modifier::BOLD),
        )]))
        .alignment(Alignment::Center),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(purpose.hint(), theme::dim())]))
            .alignment(Alignment::Center),
        rows[1],
    );

    // Input line: `>  <buffer>_`
    let input = Line::from(vec![
        Span::styled("> ", accent),
        Span::styled(buffer.to_string(), theme::green_hi()),
        Span::styled("_", accent),
    ]);
    frame.render_widget(Paragraph::new(input).alignment(Alignment::Center), rows[3]);

    // Preview or error.
    let preview = match error {
        Some(err) => Line::from(vec![Span::styled(format!("✗ {err}"), theme::red())]),
        None => match parse_duration(buffer) {
            Ok(d) => Line::from(vec![Span::styled(
                format!("= {}", fmt_duration(d)),
                theme::dim(),
            )]),
            Err(_) => Line::from(vec![Span::styled("", theme::dim())]),
        },
    };
    frame.render_widget(
        Paragraph::new(preview).alignment(Alignment::Center),
        rows[4],
    );

    let (min, max) = purpose.range();
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format!(
                "accepts s / m / h  ·  range {} .. {}",
                fmt_duration(min),
                fmt_duration(max)
            ),
            theme::dim(),
        )]))
        .alignment(Alignment::Center),
        rows[6],
    );

    let bindings = Line::from(vec![
        Span::styled("[Enter]", accent),
        Span::styled(" commit    ", theme::dim()),
        Span::styled("[Esc]", theme::dim()),
        Span::styled(" cancel    ", theme::dim()),
        Span::styled("[Backspace]", theme::dim()),
        Span::styled(" erase", theme::dim()),
    ]);
    frame.render_widget(
        Paragraph::new(bindings).alignment(Alignment::Center),
        rows[7],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("2m15s").unwrap(), Duration::from_secs(135));
        // Bare digits read as seconds.
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5x").is_err());
    }
}
