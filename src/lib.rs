//! # nats-hub
//!
//! A NATS-based communication layer with control plane routing for
//! agent-to-agent and human-to-agent messaging. Built in Rust with
//! `async-nats` and optional SurrealDB persistence.
//!
//! ## Quick Start
//!
//! ```no_run
//! use nats_hub::{HubClient, MessageKind};
//! use serde_json::json;
//!
//! # async fn example() -> anyhow::Result<()> {
//! // Connect with an identity — auto-stamped on every message
//! let client = HubClient::connect("nats://127.0.0.1:4222", "my-agent").await?;
//!
//! // Broadcast to a channel (all subscribers see it)
//! client.send_message("agents.broadcast", json!({"hello": "world"})).await?;
//!
//! // Direct message a specific agent (private DM via inbox routing)
//! client.send_to("worker-1", "tasks", json!({"prompt": "do work"})).await?;
//!
//! // Subscribe to your inbox (private messages addressed to you)
//! let mut inbox = client.subscribe_inbox().await?;
//! while let Some(env) = inbox.recv().await {
//!     println!("got DM from {}: {}", env.meta.from, env.payload);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Architecture
//!
//! ```text
//!  Agent ──hub.send.<channel>──▶ Router ──channel.<name>──▶ Subscribers
//!                                    │
//!                     meta.to set?   │
//!                     ├── yes → channel.inbox.<to>   (private DM)
//!                     └── no  → channel.<channel>     (broadcast)
//!
//!  Router ──async mirror──▶ SurrealDB (message history, agent registry)
//! ```
//!
//! ## Communication Patterns
//!
//! - **Broadcast**: `send_message(channel, payload)` — all subscribers see it
//! - **DM**: `send_to(agent, channel, payload)` — only that agent's inbox
//! - **Reply**: `send_reply(&original, payload)` — DM + correlation ID
//! - **Task channel**: `hub-delegate` creates isolated `task.<uuid>` channels
//!
//! ## Feature Flags
//!
//! - `default` (includes `storage-surreal`): SurrealDB persistence backend
//! - `storage-surreal`: SurrealDB with embedded RocksDB
//! - `no-storage`: Pure NATS transport, no persistence (lighter dependency tree)
//!
//! ## Storage Trait
//!
//! When `storage-surreal` is enabled, the [`Storage`] trait provides:
//! - `store_envelope()` / `query_history()` — message history
//! - `register_agent()` / `find_agents()` — agent registry
//! - `get_thread()` / `link_reply()` — conversation threading
//! - `list_pending()` — unanswered messages for an agent

pub mod client;
pub mod connect_opts;
pub mod events;
pub mod protocol;
pub mod query_api;
pub mod query_api_client;
pub mod router;
pub mod storage;
pub mod wave;
pub mod ws_bridge;

// ── Public API: core types ────────────────────────────────────

pub use client::{AgentInfo, AgentRegistry, HubClient};
pub use connect_opts::HubConnectOptions;
pub use events::{
    event_payload, event_summary, format_event_line, resolve_watch_target, WatchQuery, WatchTarget,
};
pub use protocol::{subjects, Envelope, MessageKind, Meta};
pub use router::{ControlPlane, RoutingTable};

// ── Public API: storage types (feature-gated) ─────────────────

#[cfg(feature = "storage-surreal")]
pub use storage::{
    AgentFilter, AgentRecord, EnvelopeRecord, HistoryQuery, SessionFilter, SessionRecord, Storage,
    SurrealStorage, WaveRecord, WaveTaskRecord,
};

// The analytics module is always compiled (its `metrics` submodule has no
// storage dependency, so `MetricsCollector` is available in `--features
// no-storage` mode). The storage-dependent `Analytics` trait + `SurrealAnalytics`
// re-exports below are gated.
pub mod analytics;
#[cfg(feature = "storage-surreal")]
pub use analytics::{
    ActivityStats, Analytics, ChannelStats, DataPoint, Interval, LatencyStats, SurrealAnalytics,
    TimeRange,
};

// Always available — no storage dependency.
pub use analytics::metrics::MetricsCollector;

pub use wave::{evaluate_merge_gate, spawn_wave, validate_tasks, SpawnOutcome, WaveTaskInput};

pub use query_api::{ApiRequest, ApiResponse};
pub use query_api_client::ApiClient;
