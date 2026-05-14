//! Matrix palette — pulled from the AI 2070 Net site
//! aesthetic: pitch-black background, neon green primary,
//! dim chrome, amber accents for state changes.

use ratatui::style::{Color, Modifier, Style};

/// Neon mesh green. The signature accent.
pub const GREEN: Color = Color::Rgb(154, 255, 0);
/// Bright text used for the active tab, hero numbers, and the
/// "LIVE" tag.
pub const GREEN_HI: Color = Color::Rgb(186, 255, 90);

/// Off-white for body paragraphs + readable text.
pub const TEXT: Color = Color::Rgb(220, 220, 220);
/// Secondary information.
pub const TEXT_DIM: Color = Color::Rgb(140, 140, 140);
/// Dim labels (axis ticks, ▶ section heads, column captions).
pub const CHROME: Color = Color::Rgb(90, 90, 90);
/// Borders + rules + grid lines.
pub const RULE: Color = Color::Rgb(50, 50, 50);

/// State-change accents.
pub const AMBER: Color = Color::Rgb(255, 184, 0);
pub const RED: Color = Color::Rgb(255, 64, 64);
pub const CYAN: Color = Color::Rgb(0, 220, 220);

pub fn green() -> Style {
    Style::default().fg(GREEN)
}
pub fn green_hi() -> Style {
    Style::default().fg(GREEN_HI).add_modifier(Modifier::BOLD)
}
pub fn text() -> Style {
    Style::default().fg(TEXT)
}
pub fn dim() -> Style {
    Style::default().fg(TEXT_DIM)
}
pub fn chrome() -> Style {
    Style::default().fg(CHROME)
}
pub fn rule() -> Style {
    Style::default().fg(RULE)
}
pub fn amber() -> Style {
    Style::default().fg(AMBER)
}
pub fn red() -> Style {
    Style::default().fg(RED)
}
pub fn cyan() -> Style {
    Style::default().fg(CYAN)
}

/// The triangle prefix the site uses for section heads:
/// `▶ MESH.PROXIMITY`.
pub const SECTION_PREFIX: &str = "▶";
