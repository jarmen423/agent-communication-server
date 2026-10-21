//! Client-side durable delivery helpers. The hub owns streams and consumers.

pub mod attach;
pub mod config;

pub use attach::{
    attach_inbox, attach_subject, Attach, AttachReply, InboxTarget, ATTACH_PREFIX,
    ATTACH_QUEUE_GROUP,
};
pub use config::{client_inbox_stream, client_tasks_stream};

/// Header JetStream dedupes on (within the stream's duplicate window).
pub const MSG_ID_HEADER: &str = "Nats-Msg-Id";

/// First value of header `name`, matched case-insensitively. (async-nats
/// keys well-known headers as "standard" names when parsed off the wire
/// but as custom ones when built from a `&str`, so `HeaderMap::get(&str)`
/// misses wire-parsed `Nats-*` headers.)
pub fn header<'a>(headers: &'a async_nats::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| AsRef::<str>::as_ref(*k).eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.first())
        .map(|v| v.as_str())
}

/// Durable consumer name of one identity's inbox.
pub fn inbox_consumer_name(identity: &str) -> String {
    format!("inbox-{identity}")
}
