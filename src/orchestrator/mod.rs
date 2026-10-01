//! Server-side wave orchestrator — lives inside hub-server.
//!
//! This is the single wave orchestration implementation (the old client-side
//! loops in `hub-wave` and `mcp_server/` are gone). The orchestrator owns
//! every running wave's state machine:
//!
//! - task state is persisted through [`Storage`] (`wave_tasks`), so a
//!   `hub-server` restart resumes `running` waves and re-dispatches
//!   in-flight tasks (at-least-once `session_start` DMs);
//! - only envelopes from a task's assigned worker (`meta.from`) may change
//!   its state — a foreign sender is logged and ignored;
//! - a worker silent for longer than `liveness_ttl` loses its task
//!   (`failed`), which fails the wave (fail-fast);
//! - the worker's `milestone verify_passed` event is recorded on the task
//!   (`verify_result`), as is a task ending without that milestone;
//! - progress is published on the existing `channel.wave.<id>` subjects so
//!   `hub-watch --wave <id>` keeps working.
//!
//! Clients never touch this module directly: `hub.api` ops (`wave.spawn`,
//! `wave.status`, `wave.cancel`) reach it through the [`api`] command
//! handle installed by hub-server.

mod api;
mod engine;

pub use api::{orchestrator_handle, OrchCommand, OrchestratorHandle};
pub use engine::{
    snapshot_json, OrchestratorConfig, WaveOrchestrator, DEFAULT_LIVENESS_SECS,
    ORCHESTRATOR_IDENTITY,
};
