//! TUI event types — unified enum for the select! loop.

use crossterm::event::KeyEvent;

use crate::protocol::Envelope;

use super::model::Snapshot;

/// Events flowing into the unified TUI event loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A live envelope arrived on `channel.>`.
    Envelope(Box<Envelope>),
    /// Periodic API refresh completed.
    Snapshot(Snapshot),
    /// API refresh failed (keep last good snapshot).
    SnapshotError(String),
    /// Terminal key press.
    Key(KeyEvent),
    /// Terminal resize.
    Resize(u16, u16),
    /// Render tick (idle redraw cap).
    Tick,
    /// Lazily-fetched task counts for a selected wave (P1-3).
    WaveTasks {
        wave_id: String,
        done: usize,
        total: usize,
    },
}
