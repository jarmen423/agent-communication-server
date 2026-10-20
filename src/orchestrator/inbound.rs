//! Envelopes on `channel.wave.>`: worker events and terminal results that
//! drive task transitions (only from the task's assigned worker, and only
//! while the task is `running`).

use tokio::time::Instant;
use tracing::{debug, warn};

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
        if env.meta.from == ORCHESTRATOR_IDENTITY {
            return; // our own progress publishes echo back on the wildcard
        }
        let Some(rt) = self.waves.get(&wave_id) else {
            return;
        };
        // Proof of life only for a worker this wave is waiting on: anyone
        // else posting on the wave's channels must not be able to keep a
        // (different) worker's task alive or grow `last_seen`.
        let is_worker = rt
            .tasks
            .values()
            .any(|t| t.worker == env.meta.from && !is_terminal(&t.status));
        if is_worker {
            self.last_seen.insert(env.meta.from.clone(), Instant::now());
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
        if env.meta.from != worker {
            warn!(
                %wave_id, %task_id, sender = %env.meta.from, expected = %worker,
                "ignoring wave task envelope from foreign sender"
            );
            return;
        }
        if task_status != "running" {
            // Not dispatched yet, or already terminal: nothing to change.
            debug!(%wave_id, %task_id, status = %task_status, "ignoring envelope for a task that isn't running");
            return;
        }

        match env.meta.kind {
            MessageKind::Event => match event_type(&env) {
                Some(event_types::COMPLETED) => {
                    let result = json_text(event_data(&env).get("result"));
                    self.complete_task(&wave_id, &task_id, result).await;
                }
                // Workers mark a §4.2 cancellation with an `error` event
                // carrying `cancelled: true`; that is not a task failure.
                Some(event_types::ERROR)
                    if event_data(&env).get("cancelled").and_then(|v| v.as_bool())
                        == Some(true) =>
                {
                    self.cancel_task(&wave_id, &task_id, false).await;
                }
                Some(event_types::ERROR) => {
                    let err = json_text(event_data(&env).get("error"));
                    self.fail_task(&wave_id, &task_id, non_empty_or(err, "task failed"), false)
                        .await;
                }
                Some(event_types::MILESTONE)
                    if event_data(&env).get("name").and_then(|v| v.as_str())
                        == Some("verify_passed") =>
                {
                    self.record_verify(&wave_id, &task_id, "passed").await;
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
                    self.fail_task(&wave_id, &task_id, non_empty_or(err, "task failed"), false)
                        .await;
                }
                Some("cancelled") => self.cancel_task(&wave_id, &task_id, false).await,
                _ => {}
            },
            _ => {}
        }
    }
}
