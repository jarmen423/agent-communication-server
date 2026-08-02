//! TUI application state — owns all data, no rendering logic.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};

use super::model::{AgentLiveState, AgentRow, ConnState, FeedLine, PanelFocus, Snapshot, WaveRow};

/// Central application state.
pub struct App {
    // ── Focus & navigation ───────────────────────────────────
    pub focus: PanelFocus,
    pub agent_sel: usize,
    pub session_sel: usize,
    pub wave_sel: usize,
    pub feed_scroll: usize,
    pub feed_follow: bool,

    // ── Data ─────────────────────────────────────────────────
    pub agents: Vec<AgentRow>,
    pub sessions: Vec<crate::storage::SessionRecord>,
    pub waves: Vec<WaveRow>,
    pub feed: VecDeque<FeedLine>,
    pub feed_cap: usize,
    pub feed_filter: String,
    pub filter_editing: bool,

    // ── Live state ───────────────────────────────────────────
    pub live_map: HashMap<String, AgentLiveState>,
    pub alive_secs: i64,

    // ── Connection / health ──────────────────────────────────
    pub nats_state: ConnState,
    pub last_snapshot_at: Option<DateTime<Utc>>,
    pub snapshot_error: Option<String>,
    pub feed_dropped: u64,
    pub total_envelopes: u64,
    /// Set by the `r` key; the run loop drains it to trigger an immediate
    /// API refresh (P0-2). Handler stays pure — it only flips this flag.
    pub refresh_requested: bool,

    // ── Stats ────────────────────────────────────────────────
    pub rate_points: Vec<crate::analytics::DataPoint>,

    // ── Wave task counts (lazy, P1-3) ────────────────────────
    /// wave_id -> (done, total), cached across refreshes.
    pub wave_counts: HashMap<String, (usize, usize)>,

    // ── UI state ─────────────────────────────────────────────
    pub show_help: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(feed_cap: usize, alive_secs: i64) -> Self {
        Self {
            focus: PanelFocus::Agents,
            agent_sel: 0,
            session_sel: 0,
            wave_sel: 0,
            feed_scroll: 0,
            feed_follow: true,
            agents: Vec::new(),
            sessions: Vec::new(),
            waves: Vec::new(),
            feed: VecDeque::with_capacity(feed_cap),
            feed_cap,
            feed_filter: String::new(),
            filter_editing: false,
            live_map: HashMap::new(),
            alive_secs,
            nats_state: ConnState::Connected,
            last_snapshot_at: None,
            snapshot_error: None,
            feed_dropped: 0,
            total_envelopes: 0,
            refresh_requested: false,
            rate_points: Vec::new(),
            wave_counts: HashMap::new(),
            show_help: false,
            should_quit: false,
        }
    }

    /// Apply a snapshot from the API refresh.
    pub fn apply_snapshot(&mut self, snap: Snapshot) {
        // Merge agents: DB records + live state
        self.agents = snap
            .agents
            .iter()
            .map(|rec| {
                let live = self.live_map.get(&rec.identity);
                let status = super::model::merge_agent_status(rec, live, self.alive_secs);
                AgentRow {
                    identity: rec.identity.clone(),
                    capabilities: rec.capabilities.clone(),
                    last_seen: rec.last_seen,
                    status,
                }
            })
            .collect();

        self.sessions = snap.sessions;
        self.waves = snap
            .waves
            .iter()
            .map(|w| {
                let mut row = WaveRow::from_record(w);
                // Restore lazily-fetched task counts across refreshes (P1-3).
                if let Some(&(done, total)) = self.wave_counts.get(&row.wave_id) {
                    row.done = done;
                    row.total = total;
                }
                row
            })
            .collect();
        self.rate_points = snap.rate_points;
        self.last_snapshot_at = Some(snap.taken_at);
        self.snapshot_error = None;

        // Clamp selection indices
        self.clamp_selections();

        // Bound the live map: drop entries for agents no longer present and stale.
        self.prune_live_map();
    }

    /// Remove `live_map` entries that are both absent from the current agent set
    /// and stale (no recent activity). Keeps agent-set members regardless of age,
    /// and keeps recently-active entries even if not in the current snapshot.
    pub fn prune_live_map(&mut self) {
        let in_agents: std::collections::HashSet<&str> =
            self.agents.iter().map(|a| a.identity.as_str()).collect();
        let now = Utc::now();
        let alive = self.alive_secs;
        self.live_map.retain(|id, state| {
            let present = in_agents.contains(id.as_str());
            let recent = state
                .last_activity
                .map(|t| (now - t).num_seconds() <= alive)
                .unwrap_or(false);
            present || recent
        });
    }

    /// Ingest a live envelope into the feed + update live agent state.
    pub fn ingest_envelope(&mut self, env: &crate::protocol::Envelope) {
        self.total_envelopes += 1;

        // Update live agent state
        let from = &env.meta.from;
        let live = self.live_map.entry(from.clone()).or_default();
        live.last_activity = Some(env.meta.timestamp);

        if env.meta.kind == crate::protocol::MessageKind::Event {
            if let Some(et) = crate::events::event_type(env) {
                live.last_event_type = Some(et.to_string());
            }
        }
        if env.meta.kind == crate::protocol::MessageKind::Status {
            if let Some(s) = env.payload.get("status").and_then(|v| v.as_str()) {
                live.last_status = Some(s.to_string());
            }
        }

        // Push to feed ring buffer
        let line = FeedLine::from_envelope(env);
        if self.feed.len() >= self.feed_cap {
            self.feed.pop_front();
            self.feed_dropped += 1;
        }
        self.feed.push_back(line);
    }

    /// Filtered feed lines (respects `feed_filter`).
    pub fn filtered_feed(&self) -> Vec<&FeedLine> {
        if self.feed_filter.is_empty() {
            return self.feed.iter().collect();
        }
        self.feed
            .iter()
            .filter(|l| l.matches_filter(&self.feed_filter))
            .collect()
    }

    /// Age of the last snapshot in seconds (for footer display).
    pub fn snapshot_age_secs(&self) -> Option<i64> {
        self.last_snapshot_at
            .map(|t| (Utc::now() - t).num_seconds())
    }

    /// Total message rate from the last snapshot's data points.
    pub fn total_rate(&self) -> u64 {
        self.rate_points.iter().map(|p| p.count).sum()
    }

    fn clamp_selections(&mut self) {
        if !self.agents.is_empty() {
            self.agent_sel = self.agent_sel.min(self.agents.len() - 1);
        } else {
            self.agent_sel = 0;
        }
        if !self.sessions.is_empty() {
            self.session_sel = self.session_sel.min(self.sessions.len() - 1);
        } else {
            self.session_sel = 0;
        }
        if !self.waves.is_empty() {
            self.wave_sel = self.wave_sel.min(self.waves.len() - 1);
        } else {
            self.wave_sel = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Envelope, MessageKind};
    use crate::storage::AgentRecord;

    fn agent_record(identity: &str) -> AgentRecord {
        AgentRecord {
            identity: identity.to_string(),
            capabilities: Vec::new(),
            last_seen: Utc::now(),
            registered_at: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn prune_live_map_drops_stale_absent_keeps_present_and_recent() {
        let mut app = App::new(100, 120);

        // Ingest envelopes from 6 distinct senders → populates live_map.
        for i in 1..=6 {
            let env = Envelope::new(
                format!("sender{i}"),
                "ch",
                MessageKind::Message,
                serde_json::json!({"message": "x"}),
            );
            app.ingest_envelope(&env);
        }
        assert_eq!(app.live_map.len(), 6);

        // Force sender1..sender4 and sender6 stale (older than alive_secs=120).
        let stale = Some(Utc::now() - chrono::Duration::seconds(1000));
        for i in [1, 2, 3, 4, 6] {
            app.live_map
                .get_mut(&format!("sender{i}"))
                .expect("entry exists")
                .last_activity = stale;
        }
        // sender5 stays recent (its ingest timestamp is now).

        // Snapshot contains ONLY sender5 (recent) and sender6 (stale but present).
        let snap = Snapshot {
            agents: vec![agent_record("sender5"), agent_record("sender6")],
            ..Default::default()
        };
        app.apply_snapshot(snap);

        // Stale + absent → pruned.
        for i in 1..=4 {
            assert!(
                !app.live_map.contains_key(&format!("sender{i}")),
                "sender{i} should have been pruned"
            );
        }
        // Recent (even though in agent set too) → retained.
        assert!(app.live_map.contains_key("sender5"));
        // Stale but present in agent set → retained.
        assert!(app.live_map.contains_key("sender6"));

        assert_eq!(app.live_map.len(), 2);
    }
}
