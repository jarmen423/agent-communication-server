//! TUI layout — ratatui chunk definitions for the dashboard.

use ratatui::layout::{Constraint, Layout, Rect};

/// Top-level vertical split: header / body / footer.
pub fn root_chunks(area: Rect) -> Vec<Rect> {
    Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(10),   // body
        Constraint::Length(1), // footer
    ])
    .split(area)
    .to_vec()
}

/// Body split: top row (3 panels) + feed.
pub fn body_chunks(area: Rect) -> Vec<Rect> {
    Layout::vertical([
        Constraint::Percentage(45), // top row
        Constraint::Percentage(55), // feed
    ])
    .split(area)
    .to_vec()
}

/// Top row: three equal columns.
pub fn top_row_chunks(area: Rect) -> Vec<Rect> {
    Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .split(area)
    .to_vec()
}

/// Help popup centered overlay (60% width, 70% height).
pub fn help_popup(area: Rect) -> Rect {
    let w = (area.width as f32 * 0.6) as u16;
    let h = (area.height as f32 * 0.7) as u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}
