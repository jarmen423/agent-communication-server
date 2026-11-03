//! An in-memory [`Storage`] for the manifest's unit tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::protocol::Envelope;
use crate::storage::{
    AgentFilter, AgentRecord, EnvelopeRecord, HistoryQuery, LockRecord, ManifestRow, SessionFilter,
    SessionRecord, Storage, WaveRecord, WaveTaskRecord,
};

/// Keeps manifest rows in memory; can be told to fail. The manifest touches
/// nothing else on `Storage`.
#[derive(Default)]
pub(super) struct RowStore {
    pub(super) rows: Mutex<Vec<ManifestRow>>,
    pub(super) fail: AtomicBool,
}

impl RowStore {
    fn check(&self) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(anyhow!("storage is down"));
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for RowStore {
    async fn load_identity_manifest(&self) -> Result<Vec<ManifestRow>> {
        self.check()?;
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn save_identity_manifest_row(&self, row: &ManifestRow) -> Result<()> {
        self.check()?;
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|r| r.identity != row.identity);
        rows.push(row.clone());
        Ok(())
    }
    async fn replace_identity_manifest(&self, rows: &[ManifestRow]) -> Result<()> {
        self.check()?;
        *self.rows.lock().unwrap() = rows.to_vec();
        Ok(())
    }
    async fn register_agent(&self, _: AgentRecord) -> Result<()> {
        unimplemented!()
    }
    async fn deregister_agent(&self, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn touch_agent(&self, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn find_agents(&self, _: &AgentFilter) -> Result<Vec<AgentRecord>> {
        unimplemented!()
    }
    async fn get_agent(&self, _: &str) -> Result<Option<AgentRecord>> {
        unimplemented!()
    }
    async fn store_envelope(&self, _: &Envelope) -> Result<()> {
        unimplemented!()
    }
    async fn query_history(&self, _: &HistoryQuery) -> Result<Vec<EnvelopeRecord>> {
        unimplemented!()
    }
    async fn get_envelope(&self, _: &str) -> Result<Option<EnvelopeRecord>> {
        unimplemented!()
    }
    async fn link_reply(&self, _: &str, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn get_thread(&self, _: &str) -> Result<Vec<EnvelopeRecord>> {
        unimplemented!()
    }
    async fn list_pending(&self, _: &str) -> Result<Vec<EnvelopeRecord>> {
        unimplemented!()
    }
    async fn create_session(&self, _: SessionRecord) -> Result<()> {
        unimplemented!()
    }
    async fn update_session_status(&self, _: &str, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn get_session(&self, _: &str) -> Result<Option<SessionRecord>> {
        unimplemented!()
    }
    async fn list_sessions(&self, _: &SessionFilter) -> Result<Vec<SessionRecord>> {
        unimplemented!()
    }
    async fn create_wave(&self, _: WaveRecord) -> Result<()> {
        unimplemented!()
    }
    async fn update_wave_status(&self, _: &str, _: &str) -> Result<()> {
        unimplemented!()
    }
    async fn get_wave(&self, _: &str) -> Result<Option<WaveRecord>> {
        unimplemented!()
    }
    async fn list_waves(&self, _: Option<&str>) -> Result<Vec<WaveRecord>> {
        unimplemented!()
    }
    async fn create_wave_task(&self, _: WaveTaskRecord) -> Result<()> {
        unimplemented!()
    }
    async fn update_wave_task_status(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<()> {
        unimplemented!()
    }
    async fn get_wave_task(&self, _: &str, _: &str) -> Result<Option<WaveTaskRecord>> {
        unimplemented!()
    }
    async fn list_wave_tasks(&self, _: &str) -> Result<Vec<WaveTaskRecord>> {
        unimplemented!()
    }
    async fn create_lock(&self, _: LockRecord) -> Result<()> {
        unimplemented!()
    }
    async fn get_lock(&self, _: &str) -> Result<Option<LockRecord>> {
        unimplemented!()
    }
    async fn list_locks(&self, _: Option<&str>) -> Result<Vec<LockRecord>> {
        unimplemented!()
    }
    async fn delete_lock(&self, _: &str) -> Result<bool> {
        unimplemented!()
    }
    async fn renew_lock(&self, _: &str, _: u64, _: DateTime<Utc>) -> Result<bool> {
        unimplemented!()
    }
    async fn migrate(&self) -> Result<()> {
        unimplemented!()
    }
    async fn ping(&self) -> Result<()> {
        unimplemented!()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
