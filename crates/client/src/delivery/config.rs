//! Client-side stream names (environment overrides, then defaults).

pub const DEFAULT_INBOX_STREAM: &str = "HUB_INBOX";
pub const DEFAULT_TASKS_STREAM: &str = "HUB_TASKS";

/// Client-side names (agents don't read the hub's config file): env
/// `NATS_HUB_INBOX_STREAM` / `NATS_HUB_TASKS_STREAM`, else the defaults.
/// The attach reply from hub-server overrides the inbox stream.
pub fn client_inbox_stream() -> String {
    env_name("NATS_HUB_INBOX_STREAM", DEFAULT_INBOX_STREAM)
}

/// See [`client_inbox_stream`].
pub fn client_tasks_stream() -> String {
    env_name("NATS_HUB_TASKS_STREAM", DEFAULT_TASKS_STREAM)
}

fn env_name(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| crate::protocol::valid_identity(v))
        .unwrap_or_else(|| default.to_string())
}
