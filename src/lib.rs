//! nats-hub: NATS-based communication layer with control plane routing.
//!
//! ## Architecture
//!
//! ```text
//!  ┌───────────────┐    ┌───────────────────┐    ┌───────────────┐
//!  │  Agent Client  │──▶│   NATS Server     │◀──│  Agent Client  │
//!  │  (publisher)   │   │   (JetStream off) │    │  (subscriber)  │
//!  └───────────────┘    └───────┬───────────┘    └───────────────┘
//!                              │
//!                       ┌──────▼───────┐
//!                       │  Control     │
//!                       │  Plane Worker│  (subscribes to hub.>,
//!                       │  (router)    │   routes to channel.>)
//!                       └──────┬───────┘
//!                              │
//!         ┌────────────┬───────┼────────┬────────────┐
//!         ▼            ▼                ▼            ▼
//!   channel.agentA  channel.agentB  channel.human  channel.logs
//! ```

pub mod client;
pub mod protocol;
pub mod router;
pub mod storage;

pub use client::HubClient;
pub use protocol::{Envelope, MessageKind, Meta};
pub use router::ControlPlane;

#[cfg(feature = "storage-surreal")]
pub use storage::{AgentFilter, AgentRecord, EnvelopeRecord, HistoryQuery, Storage, SurrealStorage};