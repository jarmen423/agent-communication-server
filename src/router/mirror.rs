//! Bounded storage mirror: one writer task drains a bounded queue of writes
//! (envelopes, registrations, heartbeats) into `Storage`, in order.
//!
//! The router's hot path only does a non-blocking `try_send`. If the DB falls
//! behind and the queue is full, the write is dropped and counted (on the
//! mirror and, when attached, on `MetricsCollector`) instead of spawning an
//! unbounded number of tasks or stalling routing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::analytics::metrics::MetricsCollector;
use crate::protocol::Envelope;
use crate::storage::{AgentRecord, Storage};

/// Default queue capacity (writes buffered while the DB catches up).
pub const DEFAULT_MIRROR_CAPACITY: usize = 4096;

/// One queued storage write.
#[derive(Debug)]
pub(crate) enum MirrorOp {
    Envelope(Box<Envelope>),
    Register(AgentRecord),
    Touch(String),
}

impl MirrorOp {
    fn label(&self) -> &'static str {
        match self {
            MirrorOp::Envelope(_) => "envelope",
            MirrorOp::Register(_) => "register",
            MirrorOp::Touch(_) => "touch",
        }
    }
}

pub(crate) struct StorageMirror {
    tx: mpsc::Sender<MirrorOp>,
    /// Taken by `start()`; `None` once the writer is running.
    rx: Mutex<Option<mpsc::Receiver<MirrorOp>>>,
    storage: Arc<dyn Storage>,
    dropped: AtomicU64,
    metrics: Option<Arc<MetricsCollector>>,
    stop: CancellationToken,
    writer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    completed: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
}

impl StorageMirror {
    pub(crate) fn new(
        storage: Arc<dyn Storage>,
        capacity: usize,
        metrics: Option<Arc<MetricsCollector>>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
            storage,
            dropped: AtomicU64::new(0),
            metrics,
            stop: CancellationToken::new(),
            writer: Mutex::new(None),
            completed: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Spawn the single writer task (idempotent). Must run inside a Tokio
    /// runtime. Writes enqueued before `start()` are kept (up to capacity).
    pub(crate) async fn start(&self) {
        let Some(mut rx) = self.rx.lock().await.take() else {
            return;
        };
        let storage = self.storage.clone();
        let stop = self.stop.clone();
        let completed = self.completed.clone();
        let failed = self.failed.clone();
        let metrics = self.metrics.clone();
        *self.writer.lock().await = Some(tokio::spawn(async move {
            loop {
                let op = tokio::select! {
                    biased;
                    _ = stop.cancelled(), if !rx.is_closed() => {
                        rx.close();
                        continue;
                    }
                    op = rx.recv() => match op {
                        Some(op) => op,
                        None => break,
                    }
                };
                if apply(storage.as_ref(), op).await {
                    completed.fetch_add(1, Ordering::Relaxed);
                } else {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(m) = &metrics {
                    m.storage_mirror_pending.fetch_sub(1, Ordering::Relaxed);
                }
            }
            debug!("storage mirror writer stopped");
        }));
    }

    pub(crate) async fn drain(&self) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let before = self.completed.load(Ordering::Relaxed);
        self.stop.cancel();
        if let Some(writer) = self.writer.lock().await.take() {
            writer.await?;
        }
        let flushed = self.completed.load(Ordering::Relaxed) - before;
        let failed = self.failed.load(Ordering::Relaxed);
        info!(
            queued_writes_flushed = flushed,
            writes_failed = failed,
            duration_ms = started.elapsed().as_millis(),
            "storage mirror drained"
        );
        anyhow::ensure!(failed == 0, "{failed} storage mirror writes failed");
        Ok(())
    }

    /// Queue a write without blocking. Returns `false` (and counts the drop)
    /// when the queue is full.
    pub(crate) fn enqueue(&self, op: MirrorOp) -> bool {
        // Count before send so the writer cannot complete before the increment.
        if let Some(m) = &self.metrics {
            m.storage_mirror_pending.fetch_add(1, Ordering::Relaxed);
        }
        match self.tx.try_send(op) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(op))
            | Err(mpsc::error::TrySendError::Closed(op)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(m) = &self.metrics {
                    m.storage_mirror_pending.fetch_sub(1, Ordering::Relaxed);
                    m.record_mirror_drop();
                }
                // Log the first drop and then every power of two, so a
                // sustained overload can't flood the log.
                if n.is_power_of_two() {
                    warn!(
                        dropped_total = n,
                        op = op.label(),
                        "storage mirror queue full; dropping write (routing unaffected)"
                    );
                }
                false
            }
        }
    }

    /// Writes dropped on overflow so far.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

async fn apply(storage: &dyn Storage, op: MirrorOp) -> bool {
    let res = match &op {
        MirrorOp::Envelope(env) => storage.store_envelope(env).await,
        MirrorOp::Register(rec) => storage.register_agent(rec.clone()).await,
        MirrorOp::Touch(ident) => storage.touch_agent(ident).await,
    };
    if let Err(e) = &res {
        warn!(error = %e, op = op.label(), "storage mirror write failed (non-fatal)");
    }
    res.is_ok()
}
