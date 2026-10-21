//! Terminal formatting for structured progress events.

use crate::events::{event_data, event_type};
use crate::protocol::Envelope;

#[derive(Debug, Clone, Copy)]
enum EventColor {
    Started,
    Progress,
    Stdout,
    Milestone,
    Completed,
    Error,
    Other,
}

impl EventColor {
    fn from_event_type(event_type: &str) -> Self {
        match event_type {
            "started" => Self::Started,
            "progress" => Self::Progress,
            "stdout" => Self::Stdout,
            "milestone" => Self::Milestone,
            "completed" => Self::Completed,
            "error" => Self::Error,
            _ => Self::Other,
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Self::Started => "\x1b[36m",
            Self::Progress => "\x1b[33m",
            Self::Stdout => "\x1b[90m",
            Self::Milestone => "\x1b[35m",
            Self::Completed => "\x1b[32m",
            Self::Error => "\x1b[31m",
            Self::Other => "\x1b[37m",
        }
    }
}

/// Extract a one-line human summary from an event envelope.
pub fn event_summary(env: &Envelope) -> String {
    let kind = event_type(env).unwrap_or("unknown");
    let data = event_data(env);

    match kind {
        "started" => data
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "progress" => data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "stdout" => data
            .get("line")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "milestone" => {
            let name = data
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("milestone");
            let detail = data.get("detail").and_then(|v| v.as_str()).unwrap_or("");
            if detail.is_empty() {
                name.to_string()
            } else {
                format!("{name}: {detail}")
            }
        }
        "completed" => data
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "error" => data
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => serde_json::to_string(data).unwrap_or_default(),
    }
}

/// Format an event envelope as a colorized terminal line.
pub fn format_event_line(env: &Envelope) -> String {
    let kind = event_type(env).unwrap_or("unknown");
    let color = EventColor::from_event_type(kind);
    let ts = env.meta.timestamp.format("%H:%M:%S");
    let summary = event_summary(env);
    let summary_display = if summary.len() > 120 {
        format!("{}…", &summary[..117])
    } else {
        summary
    };

    format!(
        "\x1b[90m[{ts}]\x1b[0m {ansi}{kind:<10}\x1b[0m \x1b[1m{from}\x1b[0m  {channel}  \"{summary}\"",
        ansi = color.ansi(),
        kind = kind,
        from = env.meta.from,
        channel = env.meta.channel,
        summary = summary_display,
    )
}
