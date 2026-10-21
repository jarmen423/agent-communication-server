//! Subscription target resolution for `hub-watch`.

use anyhow::{bail, Result};

/// Which scope to watch — decoupled from CLI parsing.
#[derive(Debug, Clone, Default)]
pub struct WatchQuery {
    pub session: Option<String>,
    pub wave: Option<String>,
    pub agent: Option<String>,
    pub channel: Option<String>,
    pub all: bool,
}

/// Resolved NATS subscription target for event watching.
#[derive(Debug, Clone)]
pub struct WatchTarget {
    pub label: String,
    pub subject: String,
    pub agent_filter: Option<String>,
    /// When set, only envelopes whose `meta.channel` starts with this prefix pass.
    pub channel_prefix: Option<String>,
}

/// Resolve a watch query into a NATS subject + optional sender filter.
pub fn resolve_watch_target(query: &WatchQuery) -> Result<WatchTarget> {
    if let Some(session) = &query.session {
        return Ok(WatchTarget {
            label: format!("session.{session}"),
            subject: format!("channel.session.{session}"),
            agent_filter: None,
            channel_prefix: None,
        });
    }
    if let Some(wave) = &query.wave {
        // `channel.wave.{id}.>` misses the wave-level broadcast subject
        // `channel.wave.{id}` — use a prefix filter on `channel.>` instead.
        return Ok(WatchTarget {
            label: format!("wave.{wave}"),
            subject: "channel.>".to_string(),
            agent_filter: None,
            channel_prefix: Some(format!("wave.{wave}")),
        });
    }
    if let Some(agent) = &query.agent {
        return Ok(WatchTarget {
            label: format!("agent {agent}"),
            subject: "channel.>".to_string(),
            agent_filter: Some(agent.clone()),
            channel_prefix: None,
        });
    }
    if let Some(channel) = &query.channel {
        return Ok(WatchTarget {
            label: channel.clone(),
            subject: format!("channel.{channel}"),
            agent_filter: None,
            channel_prefix: None,
        });
    }
    if query.all {
        return Ok(WatchTarget {
            label: "all channels".to_string(),
            subject: "channel.>".to_string(),
            agent_filter: None,
            channel_prefix: None,
        });
    }
    bail!("specify one of --session, --wave, --agent, --channel, or --all")
}
