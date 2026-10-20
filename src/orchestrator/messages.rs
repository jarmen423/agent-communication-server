//! The DMs the orchestrator sends to a task's worker. All are sent with the
//! orchestrator's own identity, so a worker that checks `cancel.meta.from`
//! against the dispatch's `meta.from` (contract C5) accepts the cancel.

use serde_json::json;

use crate::protocol::{subjects, Envelope, MessageKind};
use crate::storage::WaveTaskRecord;
use crate::wave::wave_task_session_id;

/// `session_start` for one wave task, on its task channel.
///
/// - `session_id` is `<wave_id>--<task_id>` (C4): unique per wave, stable
///   across re-dispatches, so a worker never reuses another wave's session.
/// - `task_channel` (C6) tells `hub-worker --execute` where to publish its
///   progress and terminal result, which is where the orchestrator listens.
/// - The returned envelope's `meta.id` is what the worker registers the turn
///   under; the orchestrator's cancel DM carries it (C2).
pub(super) fn session_start(identity: &str, wave_id: &str, task: &WaveTaskRecord) -> Envelope {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let mut payload = json!({
        "action": "session_start",
        "session_id": wave_task_session_id(wave_id, &task.task_id),
        "wave_id": wave_id,
        "wave_task_id": task.task_id,
        "channel": task_channel,
        "task_channel": task_channel,
        "prompt": task.goal,
        "write_scope": task.write_scope,
    });
    if let Some(ref cmd) = task.verify_cmd {
        payload["verify_cmd"] = json!(cmd);
    }
    if let Some(ref path) = task.handoff_path {
        payload["handoff_path"] = json!(path);
    }
    Envelope::new(identity, &task_channel, MessageKind::Message, payload).to(&task.worker)
}

/// §4.2 cancel: `kind=control {"action":"cancel","task_id": <dispatch id>}`.
/// `dispatch_id` is the envelope id of the latest `session_start` (C2);
/// `wave_task_id` names the wave task for humans and logs.
pub(super) fn cancel(
    identity: &str,
    wave_id: &str,
    task: &WaveTaskRecord,
    dispatch_id: &str,
) -> Envelope {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let payload = json!({
        "action": "cancel",
        "task_id": dispatch_id,
        "wave_id": wave_id,
        "wave_task_id": task.task_id,
        "session_id": wave_task_session_id(wave_id, &task.task_id),
    });
    Envelope::new(identity, &task_channel, MessageKind::Control, payload).to(&task.worker)
}

/// `session_close` once the task is terminal (C4): the worker frees the
/// session's state and subscriptions.
pub(super) fn session_close(identity: &str, wave_id: &str, task: &WaveTaskRecord) -> Envelope {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let payload = json!({
        "action": "session_close",
        "session_id": wave_task_session_id(wave_id, &task.task_id),
        "wave_id": wave_id,
        "wave_task_id": task.task_id,
    });
    Envelope::new(identity, &task_channel, MessageKind::Message, payload).to(&task.worker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn task() -> WaveTaskRecord {
        WaveTaskRecord {
            wave_id: "w1".into(),
            task_id: "t1".into(),
            worker: "bob".into(),
            goal: "do it".into(),
            status: "pending".into(),
            write_scope: vec!["src/a".into()],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: Some("true".into()),
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            verify_result: None,
        }
    }

    #[test]
    fn session_start_carries_wave_session_id_and_task_channel() {
        let env = session_start("hub-orchestrator", "w1", &task());
        assert_eq!(env.meta.from, "hub-orchestrator");
        assert_eq!(env.meta.to.as_deref(), Some("bob"));
        assert_eq!(env.meta.channel, "wave.w1.task.t1");
        assert_eq!(env.payload["session_id"], "w1--t1");
        assert_eq!(env.payload["task_channel"], "wave.w1.task.t1");
        assert_eq!(env.payload["channel"], "wave.w1.task.t1");
        assert_eq!(env.payload["verify_cmd"], "true");
    }

    #[test]
    fn cancel_targets_the_dispatch_envelope() {
        let start = session_start("hub-orchestrator", "w1", &task());
        let env = cancel("hub-orchestrator", "w1", &task(), &start.meta.id);
        assert_eq!(env.meta.kind, MessageKind::Control);
        assert_eq!(env.meta.from, start.meta.from);
        assert_eq!(env.meta.to.as_deref(), Some("bob"));
        assert_eq!(env.payload["action"], "cancel");
        assert_eq!(env.payload["task_id"], start.meta.id.as_str());
        assert_eq!(env.payload["wave_task_id"], "t1");
    }

    #[test]
    fn session_close_names_the_wave_session() {
        let env = session_close("hub-orchestrator", "w1", &task());
        assert_eq!(env.payload["action"], "session_close");
        assert_eq!(env.payload["session_id"], "w1--t1");
    }
}
