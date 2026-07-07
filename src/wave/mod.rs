//! Wave orchestration helpers — validation and spawn logic.

mod spawn;
mod validate;

pub use spawn::{evaluate_merge_gate, spawn_wave, SpawnOutcome};
pub use validate::{validate_tasks, WaveTaskInput};
