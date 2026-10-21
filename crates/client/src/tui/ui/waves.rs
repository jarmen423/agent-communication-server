//! Waves panel renderer.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::app::App;

/// Render the waves panel.
pub fn render_waves(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let focused = app.focus == crate::tui::model::PanelFocus::Waves;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(63, 63, 70))
    };

    let items: Vec<ListItem> = app
        .waves
        .iter()
        .map(|w| {
            let status_color = match w.status.as_str() {
                "running" | "spawning" => Color::Yellow,
                "merged" | "completed" => Color::Green,
                "failed" => Color::Red,
                _ => Color::Gray,
            };
            let progress = if w.total > 0 {
                format!(" {}/{}", w.done, w.total)
            } else {
                String::new()
            };
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<10}", crate::tui::model::truncate_str(&w.wave_id, 10)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<10}", w.status),
                    Style::default().fg(status_color),
                ),
                Span::styled(progress, Style::default().fg(Color::Gray)),
                Span::raw(" "),
                Span::styled(
                    crate::tui::model::truncate_str(&w.goal, 20),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = format!(" Waves ({}) ", app.waves.len());
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
    state.select(Some(app.wave_sel));
    frame.render_stateful_widget(list, area, &mut state);
}
