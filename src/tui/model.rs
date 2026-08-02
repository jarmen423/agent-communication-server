//! TUI data model — row types, live state, focus enum.
//!
//! Pure data + unit-testable merge logic. No terminal, no NATS.

use chrono::{DateTime, Utc};

use crate::protocol::{Envelope, MessageKind};
use crate::storage::{AgentRecord, SessionRecord, WaveRecord};

// ── Focus ──────────────────────────────────────────────────────

/// Which panel has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelFocus {
    Agents,
    Sessions,
    Waves,
    Feed,
}

impl PanelFocus {
    /// All panels in tab order.
    pub const ALL: [PanelFocus; 4] = [Self::Agents, Self::Sessions, Self::Waves, Self::Feed];

    /// Next panel (wraps).
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Previous panel (wraps).
    pub fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// Jump by 1-based index (1=Agents … 4=Feed).
    pub fn from_index(n: u8) -> Option<Self> {
        Self::ALL.get(n as usize).copied()
    }

    /// Human label for the footer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Sessions => "Sessions",
            Self::Waves => "Waves",
            Self::Feed => "Feed",
        }
    }
}

// ── Agent rows ─────────────────────────────────────────────────

/// Derived liveness from DB `last_seen` + live stream signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Working,
    Idle,
    Error,
    Offline,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Error => "error",
            Self::Offline => "offline",
        }
    }
}

/// One row in the Agents panel.
#[derive(Debug, Clone)]
pub struct AgentRow {
    pub identity: String,
    pub capabilities: Vec<String>,
    pub last_seen: DateTime<Utc>,
    pub status: AgentStatus,
}

/// Live signals tracked per-agent from the `channel.>` stream.
#[derive(Debug, Clone, Default)]
pub struct AgentLiveState {
    /// Last event_type seen (started, completed, error, …).
    pub last_event_type: Option<String>,
    /// Last status payload value (thinking, working, …).
    pub last_status: Option<String>,
    /// When we last saw any traffic from this agent.
    pub last_activity: Option<DateTime<Utc>>,
}

/// Merge DB record + live state into a display row.
///
/// Priority: live error > live working > live status > DB last_seen age.
pub fn merge_agent_status(
    record: &AgentRecord,
    live: Option<&AgentLiveState>,
    alive_secs: i64,
) -> AgentStatus {
    let age = (Utc::now() - record.last_seen).num_seconds();
    let db_offline = age > alive_secs;

    if let Some(live) = live {
        if let Some(ref et) = live.last_event_type {
            match et.as_str() {
                "error" => return AgentStatus::Error,
                "started" | "progress" | "milestone" => return AgentStatus::Working,
                "completed" => {
                    // Completed → idle unless DB says offline
                    return if db_offline {
                        AgentStatus::Offline
                    } else {
                        AgentStatus::Idle
                    };
                }
                _ => {}
            }
        }
        if let Some(ref s) = live.last_status {
            match s.as_str() {
                "working" | "thinking" => return AgentStatus::Working,
                "error" => return AgentStatus::Error,
                _ => {}
            }
        }
    }

    if db_offline {
        AgentStatus::Offline
    } else {
        AgentStatus::Idle
    }
}

// ── Feed lines ─────────────────────────────────────────────────

/// One line in the message feed.
#[derive(Debug, Clone)]
pub struct FeedLine {
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub from: String,
    pub channel: String,
    pub summary: String,
}

impl FeedLine {
    /// Build from a live envelope.
    pub fn from_envelope(env: &Envelope) -> Self {
        let summary = match env.meta.kind {
            MessageKind::Event => crate::events::event_summary(env),
            MessageKind::Status => env
                .payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            _ => short_payload(&env.payload),
        };
        Self {
            timestamp: env.meta.timestamp,
            kind: format!("{:?}", env.meta.kind).to_lowercase(),
            from: env.meta.from.clone(),
            channel: env.meta.channel.clone(),
            summary,
        }
    }

    /// True if this line matches a substring filter (case-insensitive).
    pub fn matches_filter(&self, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        let f = filter.to_lowercase();
        self.from.to_lowercase().contains(&f)
            || self.channel.to_lowercase().contains(&f)
            || self.summary.to_lowercase().contains(&f)
            || self.kind.to_lowercase().contains(&f)
    }
}

/// Extract a short human-readable string from a JSON payload.
fn short_payload(payload: &serde_json::Value) -> String {
    // Try common keys first
    for key in ["message", "prompt", "text", "result", "status", "action"] {
        if let Some(v) = payload.get(key).and_then(|v| v.as_str()) {
            return truncate_str(v, 120);
        }
    }
    // Fall back to compact JSON
    let s = serde_json::to_string(payload).unwrap_or_default();
    truncate_str(&s, 120)
}

/// Truncate a string to `max` chars, appending `…` if truncated.
pub fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

// ── Snapshot ───────────────────────────────────────────────────

/// A point-in-time snapshot from the query API.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub agents: Vec<AgentRecord>,
    pub sessions: Vec<SessionRecord>,
    pub waves: Vec<WaveRecord>,
    /// Message rate data points (last 5 min, per-minute).
    pub rate_points: Vec<crate::analytics::DataPoint>,
    /// When this snapshot was taken.
    pub taken_at: DateTime<Utc>,
}

/// Wave display row with task counts.
#[derive(Debug, Clone)]
pub struct WaveRow {
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub done: usize,
    pub total: usize,
}

impl WaveRow {
    pub fn from_record(w: &WaveRecord) -> Self {
        Self {
            wave_id: w.wave_id.clone(),
            goal: truncate_str(&w.goal, 40),
            status: w.status.clone(),
            done: 0,
            total: 0,
        }
    }
}

// ── Connection state ───────────────────────────────────────────

/// NATS connection health for the status bar.
///
/// async-nats auto-reconnects transparently, so the live connection never
/// surfaces a "disconnected"/"reconnecting" state here — it is effectively
/// always `Connected`. Significant outages are surfaced instead via the
/// refresh ticker's failed `ping` (footer ⚠ warning), not this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connected,
}

impl ConnState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "ok",
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(identity: &str, secs_ago: i64) -> AgentRecord {
        AgentRecord {
            identity: identity.to_string(),
            capabilities: vec![],
            last_seen: Utc::now() - chrono::Duration::seconds(secs_ago),
            registered_at: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn test_merge_status_offline() {
        let rec = make_record("a", 200);
        assert_eq!(merge_agent_status(&rec, None, 120), AgentStatus::Offline);
    }

    #[test]
    fn test_merge_status_idle() {
        let rec = make_record("a", 10);
        assert_eq!(merge_agent_status(&rec, None, 120), AgentStatus::Idle);
    }

    #[test]
    fn test_merge_status_working_from_event() {
        let rec = make_record("a", 10);
        let live = AgentLiveState {
            last_event_type: Some("started".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_agent_status(&rec, Some(&live), 120),
            AgentStatus::Working
        );
    }

    #[test]
    fn test_merge_status_error_from_event() {
        let rec = make_record("a", 10);
        let live = AgentLiveState {
            last_event_type: Some("error".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_agent_status(&rec, Some(&live), 120),
            AgentStatus::Error
        );
    }

    #[test]
    fn test_merge_status_completed_idle() {
        let rec = make_record("a", 10);
        let live = AgentLiveState {
            last_event_type: Some("completed".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_agent_status(&rec, Some(&live), 120),
            AgentStatus::Idle
        );
    }

    #[test]
    fn test_merge_status_status_payload() {
        let rec = make_record("a", 10);
        let live = AgentLiveState {
            last_status: Some("thinking".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_agent_status(&rec, Some(&live), 120),
            AgentStatus::Working
        );
    }

    #[test]
    fn test_feed_line_filter() {
        let line = FeedLine {
            timestamp: Utc::now(),
            kind: "event".to_string(),
            from: "worker-1".to_string(),
            channel: "session.abc".to_string(),
            summary: "implement handler".to_string(),
        };
        assert!(line.matches_filter(""));
        assert!(line.matches_filter("worker"));
        assert!(line.matches_filter("session"));
        assert!(line.matches_filter("handler"));
        assert!(!line.matches_filter("zzz"));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        let long = "a".repeat(200);
        let t = truncate_str(&long, 120);
        assert!(t.chars().count() <= 120);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn test_panel_focus_cycle() {
        assert_eq!(PanelFocus::Agents.next(), PanelFocus::Sessions);
        assert_eq!(PanelFocus::Feed.next(), PanelFocus::Agents);
        assert_eq!(PanelFocus::Agents.prev(), PanelFocus::Feed);
        assert_eq!(PanelFocus::from_index(0), Some(PanelFocus::Agents));
        assert_eq!(PanelFocus::from_index(3), Some(PanelFocus::Feed));
        assert_eq!(PanelFocus::from_index(4), None);
    }
}
