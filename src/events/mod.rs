//! Structured progress events (`MessageKind::Event`) for worker execution.
//!
//! Event envelopes use payload `{ "event_type": "...", "data": { ... } }`.
//! Used by `hub-watch` and worker runtimes for real-time observation.

mod display;
mod watch;

pub use display::{event_summary, format_event_line};
pub use watch::{WatchQuery, WatchTarget, resolve_watch_target};

use crate::protocol::Envelope;

/// Known event type strings published by workers.
pub mod event_types {
    pub const STARTED: &str = "started";
    pub const PROGRESS: &str = "progress";
    pub const STDOUT: &str = "stdout";
    pub const MILESTONE: &str = "milestone";
    pub const COMPLETED: &str = "completed";
    pub const ERROR: &str = "error";
}

/// Build a JSON payload for a typed progress event.
pub fn event_payload(event_type: &str, data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "event_type": event_type,
        "data": data,
    })
}

/// Return the `event_type` field from an event envelope, if present.
pub fn event_type(env: &Envelope) -> Option<&str> {
    env.payload
        .get("event_type")
        .and_then(|v| v.as_str())
}

/// Return the nested `data` object, falling back to the full payload.
pub fn event_data<'a>(env: &'a Envelope) -> &'a serde_json::Value {
    env.payload.get("data").unwrap_or(&env.payload)
}
