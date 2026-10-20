//! Worker liveness: heartbeats, registry seeding, and the TTL sweep.

use tokio::time::Instant;
use tracing::warn;

use crate::protocol::{subjects, Envelope};
use crate::storage::AgentFilter;

use super::engine::WaveOrchestrator;
use super::state::instant_from_datetime;

impl WaveOrchestrator {
    /// Record a heartbeat. On the bound `hub.presence.<identity>` subject the
    /// identity comes from the subject (pinned to the credential by NATS
    /// permissions); only the legacy subject falls back to the payload.
    pub(super) fn on_presence(&mut self, subject: &str, env: &Envelope) {
        let bound = subject
            .strip_prefix(subjects::PRESENCE)
            .and_then(|rest| rest.strip_prefix('.'))
            .filter(|id| !id.is_empty());
        let ident = bound.or_else(|| env.payload.get("identity").and_then(|v| v.as_str()));
        if let Some(ident) = ident {
            self.last_seen.insert(ident.to_string(), Instant::now());
        }
    }

    /// Seed `last_seen` from the persisted agent registry so a restart
    /// doesn't instantly kill workers that were alive moments ago.
    pub(super) async fn seed_liveness(&mut self) {
        let within = self.config.liveness_ttl.as_secs().max(1) as i64;
        match self
            .storage
            .find_agents(&AgentFilter::new().alive_within(within))
            .await
        {
            Ok(agents) => {
                for a in agents {
                    let inst = instant_from_datetime(a.last_seen);
                    self.last_seen
                        .entry(a.identity)
                        .and_modify(|e| {
                            if inst > *e {
                                *e = inst;
                            }
                        })
                        .or_insert(inst);
                }
            }
            Err(e) => warn!(error = %e, "failed to seed worker liveness from storage"),
        }
    }

    pub(super) async fn sweep(&mut self) {
        let now = Instant::now();
        let ttl = self.config.liveness_ttl;
        let wave_ids: Vec<String> = self.waves.keys().cloned().collect();
        for wave_id in wave_ids {
            let (expired, dead) = {
                let Some(rt) = self.waves.get(&wave_id) else {
                    continue;
                };
                let dead: Vec<(String, String)> = rt
                    .tasks
                    .values()
                    .filter(|t| t.status == "running")
                    .filter(|t| {
                        let baseline = rt
                            .dispatched
                            .get(&t.task_id)
                            .copied()
                            .into_iter()
                            .chain(self.last_seen.get(&t.worker).copied())
                            .max()
                            .unwrap_or(now);
                        now.duration_since(baseline) > ttl
                    })
                    .map(|t| (t.task_id.clone(), t.worker.clone()))
                    .collect();
                (now >= rt.deadline, dead)
            };
            if expired {
                self.shutdown_wave(&wave_id, "failed", "wave timed out")
                    .await;
                continue;
            }
            for (task_id, worker) in dead {
                warn!(%wave_id, %task_id, %worker, "worker liveness TTL expired");
                self.fail_task(
                    &wave_id,
                    &task_id,
                    format!(
                        "worker '{worker}' silent for >{}s (liveness TTL)",
                        ttl.as_secs()
                    ),
                )
                .await;
            }
        }
    }
}
