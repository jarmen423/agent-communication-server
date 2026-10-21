//! Apache-2.0 NATS hub client, shared wire types, and pure validation helpers.
//! No server implementation or database dependency is included.

pub mod analytics;
pub mod client;
pub mod connect_opts;
pub mod delivery;
pub mod events;
pub mod protocol;
pub mod query_api;
pub mod query_api_client;
pub mod storage;
pub mod wave;

#[cfg(feature = "tui")]
pub mod tui;

pub use analytics::{ActivityStats, ChannelStats, DataPoint, Interval, LatencyStats, TimeRange};
pub use client::{AgentInfo, AgentRegistry, HubClient};
pub use connect_opts::HubConnectOptions;
pub use events::{
    event_payload, event_summary, format_event_line, resolve_watch_target, WatchQuery, WatchTarget,
};
pub use protocol::{subjects, Envelope, MessageKind, Meta};
pub use query_api::{ApiRequest, ApiResponse};
pub use query_api_client::ApiClient;
pub use storage::{
    AgentFilter, AgentRecord, EnvelopeRecord, HistoryQuery, SessionFilter, SessionRecord,
    WaveRecord, WaveTaskRecord,
};
pub use wave::{validate_tasks, WaveTaskInput};
