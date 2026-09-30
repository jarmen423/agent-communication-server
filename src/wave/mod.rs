//! Wave helpers — task validation (scope overlap, dependency cycles) and the
//! merge gate. Wave *orchestration* lives in `crate::orchestrator`, inside
//! hub-server; there is no client-side spawn loop anymore.

mod gate;
mod validate;

pub use gate::evaluate_merge_gate;
pub use validate::{validate_tasks, WaveTaskInput};
