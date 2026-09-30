//! Reply-contract helpers (`refocus.md` §6).
//!
//! These make the delegate → worker → result loop hard to get wrong:
//!
//! * [`HubClient::send_task_status`] publishes progress on the task channel
//!   (`kind = status`, `meta.reply_to = <task id>`).
//! * [`HubClient::send_task_result`] publishes exactly one terminal result:
//!   broadcast on `payload.task_channel` when the task carries one (rule 4),
//!   otherwise a DM back to the sender (rule 6). Either way
//!   `meta.reply_to = <task id>` and the payload is
//!   `{status, task_id, result, error}`.
//! * [`is_task_result`] is the delegator-side matcher (rule 5).

use anyhow::Result;
use serde_json::{json, Value};

use super::HubClient;
use crate::protocol::{Envelope, MessageKind};

/// Outcome of running one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    /// Task succeeded; carries the result text.
    Done(String),
    /// Task failed; carries the error message.
    Error(String),
}

/// The task channel a delegator asked for (`payload.task_channel`), if any.
pub fn task_channel(task: &Envelope) -> Option<&str> {
    task.payload
        .get("task_channel")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Build the terminal result payload (rule 4):
/// `{"status": "done"|"error", "task_id", "result", "error"}`.
pub fn task_result_payload(task_id: &str, outcome: &TaskOutcome) -> Value {
    match outcome {
        TaskOutcome::Done(result) => json!({
            "status": "done",
            "task_id": task_id,
            "result": result,
            "error": null,
        }),
        TaskOutcome::Error(error) => json!({
            "status": "error",
            "task_id": task_id,
            "result": null,
            "error": error,
        }),
    }
}

/// Delegator-side matcher (rule 5): `kind = message` and either
/// `meta.reply_to == task_id` or `payload.task_id == task_id`.
/// `status` / `event` envelopes are progress, never the result.
pub fn is_task_result(env: &Envelope, task_id: &str) -> bool {
    env.meta.kind == MessageKind::Message
        && (env.meta.reply_to.as_deref() == Some(task_id)
            || env.payload.get("task_id").and_then(Value::as_str) == Some(task_id))
}

/// True if this envelope looks like a task result rather than a task.
/// Workers use it to avoid answering results (which could ping-pong).
pub fn looks_like_task_result(env: &Envelope) -> bool {
    env.payload.get("task_id").is_some() && env.payload.get("status").is_some()
}

impl HubClient {
    /// Publish a progress status on the task's channel (rule 3).
    ///
    /// Returns `Ok(None)` without publishing when the task has no
    /// `payload.task_channel` (plain DMs get only the terminal reply).
    pub async fn send_task_status(&self, task: &Envelope, status: &str) -> Result<Option<String>> {
        let Some(channel) = task_channel(task) else {
            return Ok(None);
        };
        let env = Envelope::new(
            self.identity.clone(),
            channel,
            MessageKind::Status,
            json!({ "status": status }),
        )
        .reply_to(&task.meta.id);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(Some(id))
    }

    /// Publish the single terminal result for `task` (rules 4 and 6).
    ///
    /// With `payload.task_channel`: broadcast `kind = message` on that channel.
    /// Without it: DM `meta.from` on the task's own channel. Both set
    /// `meta.reply_to = task.meta.id`. Returns the result envelope id.
    pub async fn send_task_result(&self, task: &Envelope, outcome: TaskOutcome) -> Result<String> {
        let payload = task_result_payload(&task.meta.id, &outcome);
        let env = match task_channel(task) {
            Some(channel) => Envelope::new(
                self.identity.clone(),
                channel,
                MessageKind::Message,
                payload,
            ),
            None => Envelope::new(
                self.identity.clone(),
                task.meta.channel.clone(),
                MessageKind::Message,
                payload,
            )
            .to(&task.meta.from),
        }
        .reply_to(&task.meta.id);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(payload: Value) -> Envelope {
        Envelope::new("boss", "task.abc", MessageKind::Message, payload).to("w")
    }

    #[test]
    fn result_payload_shapes() {
        let done = task_result_payload("t1", &TaskOutcome::Done("ok".into()));
        assert_eq!(
            done,
            json!({"status":"done","task_id":"t1","result":"ok","error":null})
        );
        let err = task_result_payload("t1", &TaskOutcome::Error("boom".into()));
        assert_eq!(
            err,
            json!({"status":"error","task_id":"t1","result":null,"error":"boom"})
        );
    }

    #[test]
    fn task_channel_extraction() {
        assert_eq!(
            task_channel(&task(json!({"task_channel": "task.x"}))),
            Some("task.x")
        );
        assert_eq!(task_channel(&task(json!({"task_channel": ""}))), None);
        assert_eq!(task_channel(&task(json!({"prompt": "hi"}))), None);
    }

    #[test]
    fn matcher_follows_rule_5() {
        let id = "task-id-1";
        let by_reply = Envelope::new("w", "task.x", MessageKind::Message, json!({})).reply_to(id);
        let by_payload = Envelope::new("w", "task.x", MessageKind::Message, json!({"task_id": id}));
        let status = Envelope::new("w", "task.x", MessageKind::Status, json!({})).reply_to(id);
        let event = Envelope::new("w", "task.x", MessageKind::Event, json!({"task_id": id}));
        let other = Envelope::new("w", "task.x", MessageKind::Message, json!({})).reply_to("x");
        assert!(is_task_result(&by_reply, id));
        assert!(is_task_result(&by_payload, id));
        assert!(!is_task_result(&status, id));
        assert!(!is_task_result(&event, id));
        assert!(!is_task_result(&other, id));
    }

    #[test]
    fn result_detection_for_loop_guard() {
        let res = task(task_result_payload("t", &TaskOutcome::Done("x".into())));
        assert!(looks_like_task_result(&res));
        assert!(!looks_like_task_result(&task(json!({"prompt": "hi"}))));
    }
}
