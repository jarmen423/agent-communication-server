//! Own the embedded SDK's runtime so closing storage can await its cleanup.
//!
//! SurrealDB 2.x exposes no async close/join API: dropping its final handle
//! closes an internal channel, then a detached task shuts down RocksDB. A
//! dedicated runtime lets us wait for those tasks before the process exits.
use anyhow::{anyhow, Context, Result};
use nats_hub::SurrealStorage;
use std::sync::Arc;
use tokio::sync::oneshot;

pub(super) struct Database {
    pub storage: Arc<SurrealStorage>,
    stop: oneshot::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Database {
    pub async fn open(path: &str) -> Result<Self> {
        let path = path.to_owned();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("hub-storage".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        let _ = ready_tx.send(Err(anyhow!(e)));
                        return;
                    }
                };
                let metrics = runtime.metrics();
                runtime.block_on(async move {
                    let storage = SurrealStorage::connect(&path).await;
                    let _ = ready_tx.send(storage);
                    let _ = stopped.await;
                    // Only the SDK runs on this runtime. Zero tasks means its
                    // router, datastore shutdown and background work all ended.
                    while metrics.num_alive_tasks() != 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                });
            })
            .context("start embedded storage runtime")?;
        let storage = Arc::new(
            ready_rx
                .await
                .context("embedded storage runtime stopped")??,
        );
        Ok(Self {
            storage,
            stop,
            thread,
        })
    }

    /// Call after every storage user has stopped and released its handles.
    /// The signal supervisor bounds this await with the shutdown deadline.
    pub async fn close(self) -> Result<()> {
        drop(self.storage);
        let _ = self.stop.send(());
        tokio::task::spawn_blocking(move || self.thread.join())
            .await?
            .map_err(|_| anyhow!("embedded storage runtime panicked"))?;
        tracing::info!(target: "nats_hub::shutdown", "storage closed");
        Ok(())
    }
}
