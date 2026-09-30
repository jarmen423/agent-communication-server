//! Command channel between the query-API handlers and the orchestrator.
//!
//! `handle_request` dispatches `hub.api` ops against `&dyn Storage` only —
//! it knows nothing about the orchestrator. A process-wide `OnceLock`
//! handle (installed by hub-server when it starts the orchestrator) lets
//! the wave ops forward `spawn`/`cancel` commands and await the reply.

use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};

/// A command forwarded to the orchestrator loop. Each variant carries a
/// oneshot reply so API errors reach the caller verbatim.
#[derive(Debug)]
pub enum OrchCommand {
    /// Start orchestrating a pending wave (dispatch ready tasks, drive it
    /// to a terminal status).
    Spawn {
        wave_id: String,
        timeout_secs: u64,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    /// Cancel a wave: non-terminal tasks → `cancelled` and running tasks'
    /// workers get the §4.2 cancel DM.
    Cancel {
        wave_id: String,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}

/// Clone-able sender into the orchestrator's command queue.
pub type OrchestratorHandle = mpsc::Sender<OrchCommand>;

static HANDLE: OnceLock<OrchestratorHandle> = OnceLock::new();

/// Install the process-wide handle. Called once by hub-server at startup.
pub(crate) fn install_handle(handle: OrchestratorHandle) {
    let _ = HANDLE.set(handle);
}

/// The installed handle, if an orchestrator runs in this process.
pub fn orchestrator_handle() -> Option<OrchestratorHandle> {
    HANDLE.get().cloned()
}
