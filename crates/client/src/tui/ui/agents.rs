//! Agents panel renderer.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::app::App;
use crate::tui::model::AgentStatus;

/// Status dot + color.
fn status_span(status: AgentStatus) -> Span<'static> {
    match status {
        AgentStatus::Working => Span::styled("● ", Style::default().fg(Color::Green)),
        AgentStatus::Idle => Span::styled("○ ", Style::default().fg(Color::Gray)),
        AgentStatus::Error => Span::styled("✗ ", Style::default().fg(Color::Red)),
        AgentStatus::Offline => Span::styled("· ", Style::default().fg(Color::DarkGray)),
    }
}

fn status_style(status: AgentStatus) -> Style {
    match status {
        AgentStatus::Working => Style::default().fg(Color::Green),
        AgentStatus::Idle => Style::default().fg(Color::Gray),
        AgentStatus::Error => Style::default().fg(Color::Red),
        AgentStatus::Offline => Style::default().fg(Color::DarkGray),
    }
}

/// Render the agents panel into the given area.
pub fn render_agents(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let focused = app.focus == crate::tui::model::PanelFocus::Agents;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Rgb(63, 63, 70))
    };

    let items: Vec<ListItem> = app
        .agents
        .iter()
        .map(|a| {
            let caps = if a.capabilities.is_empty() {
                String::new()
            } else {
                format!(" [{}]", a.capabilities.join(","))
            };
            let line = Line::from(vec![
                status_span(a.status),
                Span::styled(
                    format!("{:<14}", crate::tui::model::truncate_str(&a.identity, 14)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{:<8}", a.status.label()), status_style(a.status)),
                Span::styled(caps, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = format!(" Agents ({}) ", app.agents.len());
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
    state.select(Some(app.agent_sel));
    frame.render_stateful_widget(list, area, &mut state);
}
