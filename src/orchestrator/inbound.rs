//! Envelopes on `channel.wave.>`: worker events and terminal results that
//! drive task transitions (only from the task's assigned worker).

use tokio::time::Instant;
use tracing::warn;

use crate::events::{event_data, event_type, event_types};
use crate::protocol::{Envelope, MessageKind};

use super::engine::WaveOrchestrator;
use super::state::{
    is_terminal, json_text, non_empty_or, parse_wave_channel, ORCHESTRATOR_IDENTITY,
};

impl WaveOrchestrator {
    pub(super) async fn on_wave_envelope(&mut self, env: Envelope) {
        let Some((wave_id, chan_task)) = parse_wave_channel(&env.meta.channel) else {
            return;
        };
        if !self.waves.contains_key(&wave_id) {
            return;
        }
        // Any envelope on a tracked wave's channels proves its sender alive.
        self.last_seen.insert(env.meta.from.clone(), Instant::now());
        if env.meta.from == ORCHESTRATOR_IDENTITY {
            return; // our own progress publishes echo back on the wildcard
        }

        let task_id = chan_task
            .or_else(|| {
                event_data(&env)
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                env.payload
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        let Some(task_id) = task_id else { return };

        let (worker, task_status) = {
            let Some(rt) = self.waves.get(&wave_id) else {
                return;
            };
            match rt.tasks.get(&task_id) {
                Some(t) => (t.worker.clone(), t.status.clone()),
                None => return,
            }
        };
        if is_terminal(&task_status) {
            return;
        }
        if env.meta.from != worker {
            warn!(
                %wave_id, %task_id, sender = %env.meta.from, expected = %worker,
                "ignoring wave task envelope from foreign sender"
            );
            return;
        }

        match env.meta.kind {
            MessageKind::Event => match event_type(&env) {
                Some(event_types::COMPLETED) => {
                    let result = json_text(event_data(&env).get("result"));
                    self.complete_task(&wave_id, &task_id, result).await;
                }
                Some(event_types::ERROR) => {
                    let err = json_text(event_data(&env).get("error"));
                    self.fail_task(&wave_id, &task_id, non_empty_or(err, "task failed"))
                        .await;
                }
                Some(event_types::MILESTONE) => {
                    if event_data(&env).get("name").and_then(|v| v.as_str())
                        == Some("verify_passed")
                    {
                        self.record_verify(&wave_id, &task_id, "passed").await;
                    }
                }
                _ => {}
            },
            // Terminal task result per the reply contract (refocus.md §6).
            MessageKind::Message => match env.payload.get("status").and_then(|v| v.as_str()) {
                Some("done") => {
                    let result = json_text(env.payload.get("result"));
                    self.complete_task(&wave_id, &task_id, result).await;
                }
                Some("error") => {
                    let err = json_text(env.payload.get("error"));
                    self.fail_task(&wave_id, &task_id, non_empty_or(err, "task failed"))
                        .await;
                }
                Some("cancelled") => self.worker_cancelled(&wave_id, &task_id).await,
                _ => {}
            },
            _ => {}
        }
    }
}
