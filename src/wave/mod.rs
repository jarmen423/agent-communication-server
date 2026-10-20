//! Wave helpers — task validation (scope overlap, dependency cycles) and the
//! merge gate. Wave *orchestration* lives in `crate::orchestrator`, inside
//! hub-server; there is no client-side spawn loop anymore.

mod gate;
mod ids;
mod validate;

pub use gate::evaluate_merge_gate;
pub use ids::{validate_id, wave_task_session_id, MAX_ID_LEN, SESSION_ID_SEPARATOR};
pub use validate::{validate_tasks, WaveTaskInput};
