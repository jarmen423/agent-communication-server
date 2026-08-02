//! TUI UI root — composes all panels into a single draw call.

pub mod agents;
pub mod chrome;
pub mod feed;
pub mod layout;
pub mod sessions;
pub mod waves;

use ratatui::Frame;

use crate::tui::app::App;

/// Draw the full dashboard.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let root = layout::root_chunks(area);

    // Header
    chrome::render_header(frame, root[0], app);

    // Body
    let body = layout::body_chunks(root[1]);
    let top = layout::top_row_chunks(body[0]);

    agents::render_agents(frame, top[0], app);
    sessions::render_sessions(frame, top[1], app);
    waves::render_waves(frame, top[2], app);
    feed::render_feed(frame, body[1], app);

    // Footer
    chrome::render_footer(frame, root[2], app);

    // Help overlay (on top of everything)
    if app.show_help {
        chrome::render_help(frame, area);
    }
}
