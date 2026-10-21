//! Sessions panel renderer.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::app::App;

/// Render the sessions panel.
pub fn render_sessions(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let focused = app.focus == crate::tui::model::PanelFocus::Sessions;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(63, 63, 70))
    };

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|s| {
            let status_color = match s.status.as_str() {
                "active" => Color::Green,
                "closed" => Color::DarkGray,
                _ => Color::Yellow,
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<6}", &s.session_id[..s.session_id.len().min(6)]),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<8}", s.status),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    format!("→ {}", crate::tui::model::truncate_str(&s.worker, 12)),
                    Style::default().fg(Color::Gray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = format!(" Sessions ({}) ", app.sessions.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(title, Style::default().fg(Color::White))),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 40, 48))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.session_sel));
    frame.render_stateful_widget(list, area, &mut state);
}
