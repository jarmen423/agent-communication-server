//! Message feed panel renderer.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::app::App;

/// Kind → accent color.
fn kind_color(kind: &str) -> Color {
    match kind {
        "event" => Color::Cyan,
        "message" => Color::White,
        "status" => Color::Yellow,
        "control" => Color::Magenta,
        "human" => Color::Green,
        _ => Color::Gray,
    }
}

/// Render the message feed panel.
pub fn render_feed(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let focused = app.focus == crate::tui::model::PanelFocus::Feed;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(63, 63, 70))
    };

    let filtered = app.filtered_feed();
    let visible_height = area.height.saturating_sub(2) as usize; // borders
    let total = filtered.len();

    // Auto-scroll to bottom when following; otherwise honor explicit scroll offset
    let max_scroll = total.saturating_sub(visible_height);
    let scroll = if app.feed_follow {
        max_scroll
    } else {
        app.feed_scroll.min(max_scroll)
    };

    let items: Vec<ListItem> = filtered
        .iter()
        .skip(scroll)
        .take(visible_height)
        .map(|line| {
            let ts = line.timestamp.format("%H:%M:%S").to_string();
            let l = Line::from(vec![
                Span::styled(format!("{ts} "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:<8}", line.kind),
                    Style::default().fg(kind_color(&line.kind)),
                ),
                Span::styled(
                    format!("{:<14}", crate::tui::model::truncate_str(&line.from, 14)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<18}", crate::tui::model::truncate_str(&line.channel, 18)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    crate::tui::model::truncate_str(&line.summary, 60),
                    Style::default().fg(Color::Gray),
                ),
            ]);
            ListItem::new(l)
        })
        .collect();

    let filter_hint = if app.filter_editing {
        format!(" Filter: {}█", app.feed_filter)
    } else if !app.feed_filter.is_empty() {
        format!(" Filter: {}", app.feed_filter)
    } else {
        String::new()
    };

    let title = format!(" Feed ({total}){filter_hint} ");
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, Style::default().fg(Color::White))),
    );

    frame.render_widget(list, area);
}
