//! Durable delivery over JetStream (`hub-server --durable`).
//!
//! Three streams *capture* the subjects the hub already uses; nothing about
//! the delivery subjects changes, and live core-NATS subscribers keep
//! working (see `docs/DELIVERY.md`):
//!
//! | Stream        | Subjects                                   | Retention  | Read by |
//! |---------------|--------------------------------------------|------------|---------|
//! | `HUB_INGRESS` | `hub.pub.*.>`, `hub.send.>`                | work-queue | the shared `router` consumer |
//! | `HUB_INBOX`   | `channel.inbox.>`                          | limits     | `inbox-<id>` per identity |
//! | `HUB_TASKS`   | `channel.task.>`, `.wave.>`, `.session.>`  | limits     | `DIRECT.GET` by sequence |
//!
//! Only hub-server creates streams and consumers. Agents *attach*: they ask
//! hub-server for their inbox consumer on `hub.delivery.<id>.inbox`
//! ([`ATTACH_PREFIX`]) and then may only read and ack that one consumer.
//! An agent that could create consumers could aim a push consumer (or a
//! pull/direct-get reply) at another identity's subjects; see
//! [`ingress_rejection`] for the router-side half of that defence.

pub mod attach;
pub mod config;
pub mod setup;

pub use attach::{
    attach_inbox, attach_subject, serve_attach, serve_attach_until, Attach, AttachReply,
    InboxTarget, ATTACH_PREFIX, ATTACH_QUEUE_GROUP,
};
pub use config::{
    client_inbox_stream, client_tasks_stream, DeliveryConfig, StorageKind, StreamLimits,
};

pub use nats_hub_client::delivery::{header, inbox_consumer_name, MSG_ID_HEADER};

/// Prefix of the `Nats-Msg-Id` the router stamps on what it stores. A
/// message on ingress that carries it is a replay of something the hub
/// already stored, never a fresh agent send.
pub const ROUTER_MSG_ID_PREFIX: &str = "hub-router:";

/// Headers JetStream adds to messages it hands out (direct get, message
/// get, republish). None of them is ever set by a real agent send.
const JETSTREAM_EMITTED_HEADERS: &[&str] = &[
    "Nats-Stream",
    "Nats-Sequence",
    "Nats-Subject",
    "Nats-Time-Stamp",
    "Nats-Last-Sequence",
];

/// The `Nats-Msg-Id` the router stores a routed envelope under. Namespaced
/// by the (subject-bound) sender, so one identity can't suppress another's
/// message by reusing its `meta.id` inside the duplicate window.
pub fn router_msg_id(from: &str, id: &str) -> String {
    format!("{ROUTER_MSG_ID_PREFIX}{from}:{id}")
}

/// Why the durable router must drop a message it pulled from ingress, or
/// `None` when it is a genuine agent send.
///
/// JetStream delivers to whatever reply subject a request names, and that
/// delivery is not checked against the requester's publish permissions.
/// An agent allowed to read its own inbox consumer (or `DIRECT.GET` the
/// tasks stream) could name `hub.pub.<someone-else>.<ch>` as the reply and
/// have the server replay a stored envelope *as* that identity. Such a
/// replay is recognisable:
///
/// - a pull-consumer delivery keeps the stored message's original subject
///   (`channel.…`), which is not an ingress subject;
/// - a direct/message get carries `Nats-Stream`/`Nats-Sequence`/… headers;
/// - anything the router stored carries its [`ROUTER_MSG_ID_PREFIX`] id.
pub fn ingress_rejection(
    subject: &str,
    headers: Option<&async_nats::HeaderMap>,
) -> Option<&'static str> {
    let ingress = subject.starts_with("hub.pub.") || subject.starts_with("hub.send.");
    if !ingress {
        return Some("not an ingress subject (replayed stream delivery)");
    }
    let headers = headers?;
    if JETSTREAM_EMITTED_HEADERS
        .iter()
        .any(|h| header(headers, h).is_some())
    {
        return Some("carries JetStream delivery headers (replayed stream message)");
    }
    if header(headers, MSG_ID_HEADER).is_some_and(|v| v.starts_with(ROUTER_MSG_ID_PREFIX)) {
        return Some("carries a router message id (replayed stored envelope)");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genuine_sends_pass() {
        assert_eq!(ingress_rejection("hub.pub.alice.chat", None), None);
        assert_eq!(ingress_rejection("hub.send.chat", None), None);
        let mut h = async_nats::HeaderMap::new();
        h.insert(MSG_ID_HEADER, "alice:1234");
        h.insert("traceparent", "00-abc");
        assert_eq!(ingress_rejection("hub.pub.alice.chat", Some(&h)), None);
    }

    #[test]
    fn replays_are_rejected() {
        // Pull delivery keeps the stored (non-ingress) subject.
        assert!(ingress_rejection("channel.inbox.alice", None).is_some());
        // Direct get stamps stream headers.
        let mut h = async_nats::HeaderMap::new();
        h.insert("Nats-Stream", "HUB_TASKS");
        h.insert("Nats-Sequence", "7");
        assert!(ingress_rejection("hub.pub.bob.task.x", Some(&h)).is_some());
        // Anything the router stored carries its id prefix.
        let mut h = async_nats::HeaderMap::new();
        h.insert(MSG_ID_HEADER, router_msg_id("bob", "1").as_str());
        assert!(ingress_rejection("hub.pub.bob.task.x", Some(&h)).is_some());
    }

    #[test]
    fn wire_parsed_headers_are_found() {
        use std::str::FromStr;
        let mut h = async_nats::HeaderMap::new();
        h.insert(
            async_nats::HeaderName::from_str("Nats-Stream").unwrap(),
            "HUB_TASKS",
        );
        assert_eq!(header(&h, "nats-stream"), Some("HUB_TASKS"));
        assert!(ingress_rejection("hub.pub.bob.x", Some(&h)).is_some());
    }

    #[test]
    fn names() {
        assert_eq!(inbox_consumer_name("bob"), "inbox-bob");
        assert_eq!(router_msg_id("bob", "id-1"), "hub-router:bob:id-1");
    }
}
