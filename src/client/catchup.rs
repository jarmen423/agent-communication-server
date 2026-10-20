//! Catch-up reads of a stored task/session/wave channel (`HUB_TASKS`).
//!
//! A delegator that was disconnected (laptop asleep, process restarted)
//! missed whatever was published on its task channel meanwhile. On a
//! `--durable` hub those envelopes are kept in the tasks stream for its
//! retention window; [`ChannelCatchUp`] reads them by sequence with
//! `DIRECT.GET` (`next_by_subj`), so the caller needs no consumer and no
//! permission beyond `$JS.API.DIRECT.GET.<tasks stream>`.

use anyhow::{anyhow, Result};
use async_nats::jetstream::{self, stream::DirectGetErrorKind};

use super::dedupe::SeenIds;
use super::HubClient;
use crate::delivery;
use crate::protocol::{subjects, Envelope};

/// Most envelopes one [`ChannelCatchUp::poll`] returns.
pub const CATCH_UP_BATCH: usize = 500;

/// Incremental reader of one stored channel, plus a dedupe set shared with
/// the caller's live subscription so an envelope seen both ways counts once.
#[derive(Debug)]
pub struct ChannelCatchUp {
    subject: String,
    stream: String,
    last_seq: u64,
    seen: SeenIds,
    available: Option<bool>,
}

impl ChannelCatchUp {
    /// Reader for `channel.<channel>` on the client's tasks stream
    /// ([`delivery::client_tasks_stream`]).
    pub fn new(channel: &str) -> Self {
        Self {
            subject: subjects::channel(channel),
            stream: delivery::client_tasks_stream(),
            last_seq: 0,
            seen: SeenIds::default(),
            available: None,
        }
    }

    /// `true` the first time `env` is seen (live or caught up).
    pub fn first_sight(&mut self, env: &Envelope) -> bool {
        self.seen.insert(&env.meta.id)
    }

    /// `Some(false)` once the hub turned out to keep no stream (core hub).
    pub fn available(&self) -> Option<bool> {
        self.available
    }

    /// Stored envelopes published after the last poll that haven't been
    /// seen yet, oldest first. On a hub without the stream this returns
    /// `Ok(vec![])` and marks the reader unavailable (later polls are free).
    pub async fn poll(&mut self, client: &HubClient) -> Result<Vec<Envelope>> {
        if self.available == Some(false) {
            return Ok(Vec::new());
        }
        let js = jetstream::new(client.nats.clone());
        let stream = js
            .get_stream_no_info(&self.stream)
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let mut out = Vec::new();
        while out.len() < CATCH_UP_BATCH {
            let next = stream
                .direct_get_next_for_subject(&self.subject, Some(self.last_seq + 1))
                .await;
            let msg = match next {
                Ok(m) => m,
                Err(e) if e.kind() == DirectGetErrorKind::NotFound => break,
                Err(e) => {
                    let no_hub = matches!(
                        e.kind(),
                        DirectGetErrorKind::Request | DirectGetErrorKind::TimedOut
                    );
                    if self.available.is_none() && no_hub {
                        // No responders (no stream / no JetStream): a core hub.
                        self.available = Some(false);
                        return Ok(Vec::new());
                    }
                    return Err(anyhow!("catch-up on {}: {e}", self.subject));
                }
            };
            self.available = Some(true);
            let seq = msg
                .headers
                .as_ref()
                .and_then(|h| delivery::header(h, "Nats-Sequence"))
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| anyhow!("direct get reply without Nats-Sequence"))?;
            self.last_seq = seq;
            if let Ok(env) = Envelope::from_json_bytes(&msg.payload) {
                if self.seen.insert(&env.meta.id) {
                    out.push(env);
                }
            }
        }
        if self.available.is_none() {
            // Reached the end on the first poll: the stream exists.
            self.available = Some(true);
        }
        Ok(out)
    }
}

/// The task channel a delegator uses for task envelope `task_id`:
/// `task.<first 8 chars of the id>`. Deriving it from the id is what lets
/// `hub-delegate --resume <task_id>` (or any reconnecting delegator) find
/// the channel again from the id alone.
pub fn task_channel_for(task_id: &str) -> String {
    let short: String = task_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(8)
        .collect();
    format!("task.{short}")
}

#[cfg(test)]
mod tests {
    use super::task_channel_for;

    #[test]
    fn task_channel_is_derived_from_the_id() {
        assert_eq!(
            task_channel_for("3f2a9c1e-0000-4000-8000-000000000000"),
            "task.3f2a9c1e"
        );
        assert_eq!(task_channel_for("ab-cd.ef*gh>ijkl"), "task.abcdefgh");
    }
}
