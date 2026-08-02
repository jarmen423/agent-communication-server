//! Chrome renderers — header, footer, help popup.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::App;
use crate::tui::model::ConnState;

/// Render the top header bar.
pub fn render_header(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    // The connection is auto-reconnected transparently by async-nats, so it
    // is effectively always Connected here. Only one state is constructed.
    let nats_color = match app.nats_state {
        ConnState::Connected => Color::Green,
    };

    let api_age = app
        .snapshot_age_secs()
        .map(|s| format!("API {s}s"))
        .unwrap_or_else(|| "API --".to_string());

    let rate = app.total_rate();
    let rate_str = if rate > 0 {
        format!("msgs {rate}/5m")
    } else {
        "msgs 0".to_string()
    };

    let header = Line::from(vec![
        Span::styled(
            " nats-hub ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("│ "),
        Span::styled("NATS ", Style::default().fg(Color::Gray)),
        Span::styled(app.nats_state.label(), Style::default().fg(nats_color)),
        Span::raw(" │ "),
        Span::styled(&api_age, Style::default().fg(Color::Gray)),
        Span::raw(" │ "),
        Span::styled(&rate_str, Style::default().fg(Color::Gray)),
        Span::raw(" │ "),
        Span::styled(
            format!("env {}", app.total_envelopes),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(header).style(Style::default().bg(Color::Rgb(24, 24, 27))),
        area,
    );
}

/// Render the bottom footer bar.
pub fn render_footer(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let focus_label = app.focus.label();

    let stale = if let Some(ref err) = app.snapshot_error {
        format!(" │ ⚠ {err}")
    } else {
        String::new()
    };

    let dropped = if app.feed_dropped > 0 {
        format!(" │ dropped {}", app.feed_dropped)
    } else {
        String::new()
    };

    let footer = Line::from(vec![
        Span::styled(
            format!(" [{focus_label}] "),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" panel  "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" refresh  "),
        Span::styled("/?", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit"),
        Span::styled(
            format!("{stale}{dropped}"),
            Style::default().fg(Color::Yellow),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(footer).style(Style::default().bg(Color::Rgb(24, 24, 27))),
        area,
    );
}

/// Render the help popup overlay.
pub fn render_help(frame: &mut ratatui::Frame, area: Rect) {
    let popup = super::layout::help_popup(area);

    let text = vec![
        Line::from(Span::styled(
            "nats-hub TUI — Keybindings",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  q / Ctrl+C  ", Style::default().fg(Color::Cyan)),
            Span::raw("Quit"),
        ]),
        Line::from(vec![
            Span::styled("  Tab / S-Tab ", Style::default().fg(Color::Cyan)),
            Span::raw("Next / previous panel"),
        ]),
        Line::from(vec![
            Span::styled("  1-4         ", Style::default().fg(Color::Cyan)),
            Span::raw("Jump to Agents/Sessions/Waves/Feed"),
        ]),
        Line::from(vec![
            Span::styled("  j/k / ↑/↓   ", Style::default().fg(Color::Cyan)),
            Span::raw("Move selection"),
        ]),
        Line::from(vec![
            Span::styled("  r           ", Style::default().fg(Color::Cyan)),
            Span::raw("Force API refresh"),
        ]),
        Line::from(vec![
            Span::styled("  /           ", Style::default().fg(Color::Cyan)),
            Span::raw("Feed filter (substring)"),
        ]),
        Line::from(vec![
            Span::styled("  Esc         ", Style::default().fg(Color::Cyan)),
            Span::raw("Clear filter / close popup"),
        ]),
        Line::from(vec![
            Span::styled("  PgUp/PgDn   ", Style::default().fg(Color::Cyan)),
            Span::raw("Feed scroll"),
        ]),
        Line::from(vec![
            Span::styled("  ?           ", Style::default().fg(Color::Cyan)),
            Span::raw("Toggle this help"),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Help "),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}
