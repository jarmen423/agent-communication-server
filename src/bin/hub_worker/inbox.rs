//! The worker's inbox: bus receivers plus the bounded queue of tasks waiting
//! behind the running one (split out of hub_worker.rs).
//!
//! Only *task* envelopes are ever queued (see [`task_prompt`]), so a cancel
//! can only match a task. A cancel is honoured only when it comes from the
//! sender of the task it names (HARDENING.md C5).
//!
//! A `session_start` for a session whose task is already queued or running
//! (an orchestrator re-dispatch) is not run twice: its id becomes an alias
//! of that task, so a cancel by either id works (C3).
//!
//! Session actions are gated by [`Sessions::admit`]: only the session's owner
//! (the sender of its `session_start`) may send to or close it (C5).

use nats_hub::client::{looks_like_task_result, task_channel};
use nats_hub::{Envelope, MessageKind};
use std::collections::{HashMap, VecDeque};
use tracing::warn;

pub type EnvRx = tokio::sync::mpsc::UnboundedReceiver<Envelope>;

/// Inbox + optional broadcast channel, plus tasks that arrived while a task
/// was running. `direct` = arrived on our inbox.
pub struct Inbox {
    pub inbox: EnvRx,
    pub broadcast: Option<EnvRx>,
    queued: VecDeque<(Envelope, String)>,
    max_queued: usize,
    /// alias envelope id → (task id it stands for, the alias's sender)
    aliases: HashMap<String, (String, String)>,
    /// Session owners (C5 for session actions).
    pub sessions: Sessions,
}

/// What a cancel request resolves to.
pub enum CancelTarget {
    /// The running task (directly or through an alias).
    Running,
    /// A queued task, removed from the queue.
    Queued(Envelope),
    /// Known, but `by` didn't send it (C5); carries the task's sender.
    Denied(String),
    /// Unknown or finished.
    Unknown,
}

/// Sessions this worker has seen started, with their owners (bounded).
const MAX_SESSIONS: usize = 1024;

#[derive(Default)]
pub struct Sessions {
    owners: VecDeque<(String, String)>, // (session id, owner), oldest first
}

impl Sessions {
    fn owner(&self, session: &str) -> Option<&str> {
        self.owners
            .iter()
            .find(|(s, _)| s == session)
            .map(|(_, o)| o.as_str())
    }

    /// C5 for sessions. `Ok` lets `env` through (non-session envelopes
    /// always pass); `Err(reason)` means ignore it. A first `session_start`
    /// records its sender as the owner; the owner's `session_close` forgets
    /// the session. `session_send`/`session_close` for a session this worker
    /// never saw started are refused: there is no owner to check against.
    pub fn admit(&mut self, env: &Envelope) -> Result<(), String> {
        let Some(action) = env.payload.get("action").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if !matches!(action, "session_start" | "session_send" | "session_close") {
            return Ok(());
        }
        let session = env
            .payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| env.meta.channel.strip_prefix("session."))
            .map(str::to_string);
        let Some(session) = session else {
            return Err(format!("{action} without a session id"));
        };
        let from = env.meta.from.as_str();
        match (action, self.owner(&session)) {
            ("session_start", None) => {
                if self.owners.len() >= MAX_SESSIONS {
                    self.owners.pop_front();
                }
                self.owners.push_back((session, from.to_string()));
                Ok(())
            }
            (_, Some(owner)) if owner != from => Err(format!(
                "{action} for session {session} from {from}: owner is {owner}"
            )),
            ("session_close", Some(_)) => {
                self.owners.retain(|(s, _)| *s != session);
                Ok(())
            }
            (_, Some(_)) => Ok(()),
            (_, None) => Err(format!("{action} for unknown session {session}")),
        }
    }
}

/// The session id of a `session_start` task (C3 dedupe key).
pub fn session_start_id(env: &Envelope) -> Option<&str> {
    (env.payload.get("action").and_then(|v| v.as_str()) == Some("session_start"))
        .then(|| env.payload.get("session_id").and_then(|v| v.as_str()))
        .flatten()
        .filter(|s| !s.is_empty())
}

impl Inbox {
    pub fn new(inbox: EnvRx, broadcast: Option<EnvRx>, max_queued: usize) -> Self {
        Self {
            inbox,
            broadcast,
            queued: VecDeque::new(),
            max_queued,
            aliases: HashMap::new(),
            sessions: Sessions::default(),
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

    /// C3: if `task` re-dispatches the session of the running task or of a
    /// queued one, register it as an alias of that task and return true
    /// (the caller then neither queues nor runs it).
    pub fn alias_redispatch(&mut self, task: &Envelope, running: &Envelope) -> bool {
        let Some(session) = session_start_id(task) else {
            return false;
        };
        let target = if session_start_id(running) == Some(session) {
            running.meta.id.clone()
        } else {
            match self
                .queued
                .iter()
                .find(|(e, _)| session_start_id(e) == Some(session))
            {
                Some((e, _)) => e.meta.id.clone(),
                None => return false,
            }
        };
        self.aliases
            .insert(task.meta.id.clone(), (target, task.meta.from.clone()));
        true
    }

    /// Resolve a cancel for `id` from `by` against the running task and the
    /// queue (C5: only the sender of the named envelope may cancel it). A
    /// queued match is removed so a `cancelled` result can be sent.
    pub fn resolve_cancel(
        &mut self,
        id: &str,
        by: &str,
        running: Option<&Envelope>,
    ) -> CancelTarget {
        let (target, alias_owner) = match self.aliases.get(id) {
            Some((target, owner)) => (target.clone(), Some(owner.clone())),
            None => (id.to_string(), None),
        };
        if let Some(run) = running.filter(|r| r.meta.id == target) {
            let owner = alias_owner.unwrap_or_else(|| run.meta.from.clone());
            return if owner == by {
                CancelTarget::Running
            } else {
                CancelTarget::Denied(owner)
            };
        }
        let Some(pos) = self.queued.iter().position(|(e, _)| e.meta.id == target) else {
            return CancelTarget::Unknown;
        };
        let owner = alias_owner.unwrap_or_else(|| self.queued[pos].0.meta.from.clone());
        if owner != by {
            return CancelTarget::Denied(owner);
        }
        self.forget(&target);
        match self.queued.remove(pos) {
            Some((task, _)) => CancelTarget::Queued(task),
            None => CancelTarget::Unknown,
        }
    }

    /// Drop the aliases of a task that finished or was cancelled.
    pub fn forget(&mut self, task_id: &str) {
        self.aliases.retain(|_, (target, _)| target != task_id);
    }
}

/// Log a denied cancel (C5).
pub fn warn_denied(task_id: &str, by: &str, owner: &str) {
    warn!(%task_id, %by, %owner, "cancel from someone other than the task's sender, ignoring");
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
        assert!(
            matches!(q.resolve_cancel(&a_id, "mallory", None), CancelTarget::Denied(o) if o == "boss")
        );
        assert!(matches!(
            q.resolve_cancel("nope", "boss", None),
            CancelTarget::Unknown
        ));
        assert!(
            matches!(q.resolve_cancel(&a_id, "boss", None), CancelTarget::Queued(t) if t.meta.id == a_id)
        );
        assert_eq!(q.pop_queued().unwrap().0.meta.id, b_id);
        assert!(q.pop_queued().is_none());
    }

    #[test]
    fn only_the_session_owner_may_send_or_close() {
        let mut s = Sessions::default();
        let act = |from: &str, action: &str| {
            task(
                from,
                json!({"action": action, "session_id": "s1", "message": "m"}),
            )
        };
        assert!(s.admit(&task("x", json!({"prompt": "plain"}))).is_ok());
        assert!(
            s.admit(&act("boss", "session_send")).is_err(),
            "unknown session"
        );
        assert!(s.admit(&act("boss", "session_start")).is_ok());
        assert!(s.admit(&act("mallory", "session_send")).is_err());
        assert!(s.admit(&act("mallory", "session_start")).is_err());
        assert!(s.admit(&act("mallory", "session_close")).is_err());
        assert!(s.admit(&act("boss", "session_send")).is_ok());
        // Session id from the channel when the payload has none (HubClient::close_session).
        let close = Envelope::new(
            "boss",
            "session.s1",
            MessageKind::Message,
            json!({"action": "session_close"}),
        );
        assert!(s.admit(&close).is_ok());
        assert!(s.admit(&act("boss", "session_send")).is_err(), "closed");
    }

    #[test]
    fn redispatched_session_start_aliases_the_running_task() {
        let mut q = inbox(4);
        let start = |from: &str| {
            task(
                from,
                json!({"action": "session_start", "session_id": "w1--t1",
                              "prompt": "go", "task_channel": "wave.w1.task.t1"}),
            )
        };
        let running = start("orch");
        let again = start("orch");
        assert!(q.alias_redispatch(&again, &running));
        assert!(q.full().is_none() && q.pop_queued().is_none(), "not queued");
        // A cancel by the newest id (C2) stops the running task.
        assert!(matches!(
            q.resolve_cancel(&again.meta.id, "orch", Some(&running)),
            CancelTarget::Running
        ));
        assert!(matches!(
            q.resolve_cancel(&again.meta.id, "eve", Some(&running)),
            CancelTarget::Denied(_)
        ));
        let other = task(
            "orch",
            json!({"action": "session_start", "session_id": "w1--t2",
                                        "prompt": "x"}),
        );
        assert!(!q.alias_redispatch(&other, &running), "different session");
        let plain = task("orch", json!({"prompt": "x"}));
        assert!(!q.alias_redispatch(&plain, &running));
        // A re-dispatch of a *queued* session aliases the queued task.
        q.queue(other.clone(), "x".into());
        let other_again = task(
            "orch",
            json!({"action": "session_start", "session_id": "w1--t2",
                                              "prompt": "x"}),
        );
        assert!(q.alias_redispatch(&other_again, &running));
        assert!(
            matches!(q.resolve_cancel(&other_again.meta.id, "orch", Some(&running)),
                         CancelTarget::Queued(t) if t.meta.id == other.meta.id)
        );
        q.forget(&running.meta.id);
        assert!(matches!(
            q.resolve_cancel(&again.meta.id, "orch", None),
            CancelTarget::Unknown
        ));
    }
}
