pub mod blob_detail;
pub mod cluster_picker;
pub mod confirm;
pub mod empty;
pub mod export;
pub mod export_done;
pub mod footer;
pub mod help;
pub mod node_card;
pub mod param_input;
pub mod pick_node;
pub mod rule;
pub mod status_bar;
pub mod tab_bar;

use ratatui::layout::Rect;

/// Centered sub-rect of `area` with the given width / height,
/// clamped to fit so the modal never tries to render outside
/// the parent's bounds. Shared across every modal widget so
/// the centering math has a single source of truth.
pub fn center(area: Rect, width: u16, height: u16) -> Rect {
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
