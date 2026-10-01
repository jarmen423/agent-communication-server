//! Live, in-memory metrics collector for nats-hub.
//!
//! Zero dependencies: uses `std::sync::atomic` for counters and hand-renders
//! the Prometheus text exposition format. No web framework, no `prometheus`
//! crate. The collector is **always compiled** (independent of the
//! `storage-surreal` feature) so `hub-server --metrics-addr` works even in
//! `--features no-storage` mode.
//!
//! This is the *hot* observability path: `ControlPlane::handle_send` calls
//! [`MetricsCollector::record`] once per routed envelope. It is a single
//! non-blocking atomic increment per counter — no allocation, no `await` —
//! so it is safe to call on the message-routing hot path.
//!
//! Design note (cardinality): only **bounded** labels are used. `channel_class`
//! is bucketed into 6 values regardless of bus size, and per-kind/per-error
//! counters are fixed-size arrays. Per-agent counters are intentionally
//! excluded (high cardinality) — see `docs/archive/PHASE4_PLAN.md` for the rationale.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::{Envelope, MessageKind};

/// Bounded bucket for a channel name. Keeps the `channel_class` Prometheus
/// label to a fixed set of 6 values no matter how many channels exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelClass {
    /// `inbox.<identity>` — direct messages (router routes `meta.to` here).
    Inbox,
    /// `session.<uuid>` — stateful multi-turn sessions.
    Session,
    /// `wave.<id>` — parallel wave orchestration channels.
    Wave,
    /// `task.<uuid>` — one-shot delegated task channels.
    Task,
    /// `agents.broadcast`, `channel.<name>`, etc. — public broadcast.
    Broadcast,
    /// Anything that doesn't match the patterns above.
    Other,
}

impl ChannelClass {
    /// All variants in a stable order — used for metric label emission.
    pub const ALL: [ChannelClass; 6] = [
        ChannelClass::Broadcast,
        ChannelClass::Inbox,
        ChannelClass::Session,
        ChannelClass::Wave,
        ChannelClass::Task,
        ChannelClass::Other,
    ];

    /// Prometheus label value (lowercase, no spaces).
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelClass::Inbox => "inbox",
            ChannelClass::Session => "session",
            ChannelClass::Wave => "wave",
            ChannelClass::Task => "task",
            ChannelClass::Broadcast => "broadcast",
            ChannelClass::Other => "other",
        }
    }

    /// Index into the fixed-size counter array.
    fn index(&self) -> usize {
        match self {
            ChannelClass::Broadcast => 0,
            ChannelClass::Inbox => 1,
            ChannelClass::Session => 2,
            ChannelClass::Wave => 3,
            ChannelClass::Task => 4,
            ChannelClass::Other => 5,
        }
    }
}

impl ChannelClass {
    /// Classify a channel name into a bounded [`ChannelClass`].
    pub fn classify(channel: &str) -> ChannelClass {
        if channel.starts_with("inbox.") {
            ChannelClass::Inbox
        } else if channel.starts_with("session.") {
            ChannelClass::Session
        } else if channel.starts_with("wave.") {
            ChannelClass::Wave
        } else if channel.starts_with("task.") {
            ChannelClass::Task
        } else if channel.contains('.') || channel == "unknown" {
            // `agents.broadcast`, `channel.<x>`, `unknown`, etc.
            ChannelClass::Broadcast
        } else {
            ChannelClass::Other
        }
    }
}

/// Bounded set of message kinds (matches `MessageKind`).
const KIND_COUNT: usize = 5;

/// In-memory metrics for the control plane router.
///
/// All fields are lock-free atomics — safe for concurrent `record` calls from
/// the routing loop without ever blocking message delivery.
#[derive(Default)]
pub struct MetricsCollector {
    /// Grand total of envelopes routed.
    pub messages_total: AtomicU64,
    /// Per-kind counts, indexed by `kind_index`.
    pub by_kind: [AtomicU64; KIND_COUNT],
    /// Per-channel-class counts, indexed by `ChannelClass::index`.
    pub by_channel_class: [AtomicU64; 6],
    /// Total error events (`event_type == "error"`).
    pub errors_total: AtomicU64,
    /// Storage-mirror writes dropped because the bounded mirror queue was
    /// full (the DB could not keep up). Routing is unaffected.
    pub storage_mirror_dropped: AtomicU64,
    /// Messages that arrived on legacy self-asserted subjects (`hub.send.>`,
    /// bare `hub.register`/`hub.presence`) while bound subjects exist.
    /// Counted whether or not `--require-bound-identity` is on, so an
    /// operator can watch the migration to bound identity drain to zero.
    pub unbound_sends_total: AtomicU64,
}

/// Map a [`MessageKind`] to a stable index into `by_kind`.
fn kind_index(kind: MessageKind) -> usize {
    match kind {
        MessageKind::Message => 0,
        MessageKind::Control => 1,
        MessageKind::Human => 2,
        MessageKind::Status => 3,
        MessageKind::Event => 4,
    }
}

impl MetricsCollector {
    /// Record a single routed envelope. Non-blocking; safe on the hot path.
    pub fn record(&self, env: &Envelope) {
        self.messages_total.fetch_add(1, Ordering::Relaxed);

        let ki = kind_index(env.meta.kind.clone());
        self.by_kind[ki].fetch_add(1, Ordering::Relaxed);

        let channel = env.meta.channel.as_str();
        let ci = ChannelClass::classify(channel).index();
        self.by_channel_class[ci].fetch_add(1, Ordering::Relaxed);

        // Error events: kind == Event and payload.event_type == "error".
        if env.meta.kind == MessageKind::Event {
            let is_error = env
                .payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case("error"))
                .unwrap_or(false);
            if is_error {
                self.errors_total.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Snapshot the current counter values (useful for tests/logging).
    pub fn messages_total(&self) -> u64 {
        self.messages_total.load(Ordering::Relaxed)
    }

    /// Count one storage-mirror write dropped on overflow. Non-blocking.
    pub fn record_mirror_drop(&self) {
        self.storage_mirror_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Storage-mirror writes dropped so far.
    pub fn mirror_dropped(&self) -> u64 {
        self.storage_mirror_dropped.load(Ordering::Relaxed)
    }

    /// Count one message that arrived on a legacy (self-asserted) subject.
    /// Non-blocking; safe on the hot path.
    pub fn record_unbound(&self) {
        self.unbound_sends_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Legacy-subject arrivals so far.
    pub fn unbound_sends(&self) -> u64 {
        self.unbound_sends_total.load(Ordering::Relaxed)
    }

    /// Render the Prometheus text exposition format.
    ///
    /// Emits only bounded labels (`kind`, `class`). Per-agent metrics are
    /// intentionally absent to avoid cardinality blow-up.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();

        // Total messages
        out.push_str("# HELP natshub_messages_total Total envelopes routed by the control plane\n");
        out.push_str("# TYPE natshub_messages_total counter\n");
        out.push_str(&format!(
            "natshub_messages_total {}\n",
            self.messages_total.load(Ordering::Relaxed)
        ));

        // By kind
        out.push_str("# HELP natshub_messages_by_kind Envelopes routed, by message kind\n");
        out.push_str("# TYPE natshub_messages_by_kind counter\n");
        let kinds = [
            ("message", 0),
            ("control", 1),
            ("human", 2),
            ("status", 3),
            ("event", 4),
        ];
        for (label, idx) in kinds {
            out.push_str(&format!(
                "natshub_messages_by_kind{{kind=\"{}\"}} {}\n",
                label,
                self.by_kind[idx].load(Ordering::Relaxed)
            ));
        }

        // By channel class
        out.push_str(
            "# HELP natshub_messages_by_channel_class Envelopes routed, by channel class\n",
        );
        out.push_str("# TYPE natshub_messages_by_channel_class counter\n");
        for class in ChannelClass::ALL {
            out.push_str(&format!(
                "natshub_messages_by_channel_class{{class=\"{}\"}} {}\n",
                class.as_str(),
                self.by_channel_class[class.index()].load(Ordering::Relaxed)
            ));
        }

        // Errors
        out.push_str("# HELP natshub_errors_total Total error events observed\n");
        out.push_str("# TYPE natshub_errors_total counter\n");
        out.push_str(&format!(
            "natshub_errors_total {}\n",
            self.errors_total.load(Ordering::Relaxed)
        ));

        // Storage mirror overflow
        out.push_str(
            "# HELP natshub_storage_mirror_dropped_total Storage writes dropped because the mirror queue was full\n",
        );
        out.push_str("# TYPE natshub_storage_mirror_dropped_total counter\n");
        out.push_str(&format!(
            "natshub_storage_mirror_dropped_total {}\n",
            self.storage_mirror_dropped.load(Ordering::Relaxed)
        ));

        // Legacy (unbound) subject arrivals
        out.push_str(
            "# HELP natshub_unbound_sends_total Messages on legacy self-asserted subjects\n",
        );
        out.push_str("# TYPE natshub_unbound_sends_total counter\n");
        out.push_str(&format!(
            "natshub_unbound_sends_total {}\n",
            self.unbound_sends_total.load(Ordering::Relaxed)
        ));

        out
    }
}
