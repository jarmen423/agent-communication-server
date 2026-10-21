//! Wave helpers — task validation (scope overlap, dependency cycles) and the
//! merge gate. Wave *orchestration* lives in `crate::orchestrator`, inside
//! hub-server; there is no client-side spawn loop anymore.

mod gate;

pub use gate::evaluate_merge_gate;
pub use nats_hub_client::wave::*;
