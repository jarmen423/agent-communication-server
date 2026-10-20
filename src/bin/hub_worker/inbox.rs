//! The worker's inbox: bus receivers plus the bounded queue of tasks waiting
//! behind the running one (split out of hub_worker.rs).
//!
//! Only *task* envelopes are ever queued (see [`task_prompt`]), so a cancel
//! can only match a task. A cancel is honoured only when it comes from the
//! sender of the task it names (HARDENING.md C5).

use nats_hub::client::{looks_like_task_result, task_channel};
use nats_hub::{Envelope, MessageKind};
use std::collections::VecDeque;
use tracing::warn;

pub type EnvRx = tokio::sync::mpsc::UnboundedReceiver<Envelope>;

/// Inbox + optional broadcast channel, plus tasks that arrived while a task
/// was running. `direct` = arrived on our inbox.
pub struct Inbox {
    pub inbox: EnvRx,
    pub broadcast: Option<EnvRx>,
    queued: VecDeque<(Envelope, String)>,
    max_queued: usize,
}

impl Inbox {
    pub fn new(inbox: EnvRx, broadcast: Option<EnvRx>, max_queued: usize) -> Self {
        Self {
            inbox,
            broadcast,
            queued: VecDeque::new(),
            max_queued,
        }
    }

    /// Next envelope from the bus (not the queue). None once closed.
    pub async fn recv(&mut self) -> Option<(Envelope, bool)> {
        match self.broadcast {
            Some(ref mut brx) => tokio::select! {
                Some(env) = self.inbox.recv() => Some((env, true)),
                Some(env) = brx.recv() => Some((env, false)),
                else => None,
            },
            None => self.inbox.recv().await.map(|env| (env, true)),
        }
    }

    /// The oldest queued task and its prompt.
    pub fn pop_queued(&mut self) -> Option<(Envelope, String)> {
        self.queued.pop_front()
    }

    /// The queue bound, when the queue is full (the next task is rejected).
    pub fn full(&self) -> Option<usize> {
        (self.queued.len() >= self.max_queued).then_some(self.max_queued)
    }

    /// Queue a task (with its prompt) behind the running one. Check
    /// [`Inbox::full`] first; this never drops a task.
    pub fn queue(&mut self, task: Envelope, prompt: String) {
        self.queued.push_back((task, prompt));
    }

    /// Drop a queued task on cancel — only when `by` sent it (C5). Returns
    /// it so a `cancelled` result can be sent.
    pub fn take_queued(&mut self, id: &str, by: &str) -> Option<Envelope> {
        let pos = self.queued.iter().position(|(e, _)| e.meta.id == id)?;
        if self.queued[pos].0.meta.from != by {
            warn!(task_id = %id, %by, owner = %self.queued[pos].0.meta.from,
                  "cancel from someone other than the task's sender, ignoring");
            return None;
        }
        self.queued.remove(pos).map(|(e, _)| e)
    }
}

/// The task id of a §4.2 cancel request (only DMs count).
pub fn cancel_target(env: &Envelope, direct: bool) -> Option<&str> {
    let is_cancel = direct
        && env.meta.kind == MessageKind::Control
        && env.payload.get("action").and_then(|v| v.as_str()) == Some("cancel");
    is_cancel
        .then(|| env.payload.get("task_id").and_then(|v| v.as_str()))
        .flatten()
}

/// The prompt if `env` is a task this worker should run, else `Err(reason)`.
///
/// Progress/bookkeeping kinds are never tasks. Never answer a result either:
/// two workers DMing each other results would ping-pong forever. Without a
/// task channel the reply is a DM (rule 6) — only for envelopes sent to us,
/// not for chatter on the broadcast channel.
pub fn task_prompt(env: &Envelope, direct: bool) -> Result<String, &'static str> {
    if matches!(
        env.meta.kind,
        MessageKind::Status | MessageKind::Event | MessageKind::Control
    ) || looks_like_task_result(env)
    {
        return Err("not a task");
    }
    if !direct && task_channel(env).is_none() {
        return Err("broadcast without task_channel");
    }
    let prompt = ["prompt", "text", "command", "message"]
        .iter()
        .find_map(|k| env.payload.get(*k).and_then(|v| v.as_str()))
        .unwrap_or_default();
    if prompt.is_empty() {
        return Err("no prompt in payload");
    }
    Ok(prompt.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(from: &str, payload: serde_json::Value) -> Envelope {
        Envelope::new(from, "task.t", MessageKind::Message, payload).to("w")
    }

    fn inbox(max: usize) -> Inbox {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Inbox::new(rx, None, max)
    }

    #[test]
    fn only_tasks_have_a_prompt() {
        let t = task("boss", json!({"prompt": "hi", "task_channel": "task.t"}));
        assert_eq!(task_prompt(&t, true).as_deref(), Ok("hi"));
        let status = Envelope::new("x", "task.t", MessageKind::Status, json!({"prompt": "hi"}));
        assert!(task_prompt(&status, true).is_err());
        let result = task(
            "w2",
            json!({"task_id": "t", "status": "done", "prompt": "x"}),
        );
        assert!(task_prompt(&result, true).is_err());
        assert!(task_prompt(&task("boss", json!({"prompt": "hi"})), false).is_err());
        assert!(task_prompt(&task("boss", json!({"prompt": ""})), true).is_err());
    }

    #[test]
    fn queue_is_bounded_and_cancel_needs_the_sender() {
        let mut q = inbox(2);
        let a = task("boss", json!({"prompt": "a"}));
        let b = task("boss", json!({"prompt": "b"}));
        let (a_id, b_id) = (a.meta.id.clone(), b.meta.id.clone());
        assert_eq!(q.full(), None);
        q.queue(a, "a".into());
        q.queue(b, "b".into());
        assert_eq!(q.full(), Some(2));
        assert!(q.take_queued(&a_id, "mallory").is_none(), "third party");
        assert!(q.take_queued("nope", "boss").is_none());
        assert_eq!(q.take_queued(&a_id, "boss").unwrap().meta.id, a_id);
        assert_eq!(q.pop_queued().unwrap().0.meta.id, b_id);
        assert!(q.pop_queued().is_none());
    }
}
