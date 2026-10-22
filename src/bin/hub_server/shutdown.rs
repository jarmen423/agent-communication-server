//! Signal supervision and task ownership. One deadline covers all cleanup.
use anyhow::Result;
use nats_hub::MetricsCollector;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::args::Args;

#[derive(Default)]
pub(super) struct Services {
    pub intake_stop: CancellationToken,
    pub background_stop: CancellationToken,
    pub intake: Vec<JoinHandle<Result<()>>>,
    pub background: Vec<JoinHandle<()>>,
}

impl Services {
    pub async fn finish_intake(&mut self) -> Result<()> {
        for task in self.intake.drain(..) {
            task.await??;
        }
        Ok(())
    }

    pub async fn stop_background(&mut self) -> Result<()> {
        self.background_stop.cancel();
        for task in self.background.drain(..) {
            task.await?;
        }
        Ok(())
    }
}

struct Signals {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
}

impl Signals {
    fn install() -> Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            term: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            #[cfg(unix)]
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        })
    }

    async fn recv(&mut self) -> Result<()> {
        #[cfg(unix)]
        tokio::select! {
            _ = self.term.recv() => {},
            _ = self.interrupt.recv() => {},
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}

pub(super) async fn serve(args: Args) -> Result<()> {
    // Install both handlers before connecting or opening the database.
    let mut signals = Signals::install()?;
    let metrics = Arc::new(MetricsCollector::default());
    let timeout = Duration::from_secs(args.shutdown_timeout.into());
    let mut services = Services::default();
    let stop = services.intake_stop.clone();
    let server = super::run_server(args, &mut services, metrics.clone());
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => return result,
        signal = signals.recv() => signal?,
    }
    let started = Instant::now();
    tracing::info!(target: "nats_hub::shutdown",
        queued_writes = metrics.storage_mirror_pending.load(Ordering::Relaxed),
        "shutdown requested"
    );
    stop.cancel();
    tokio::select! {
        biased;
        _ = signals.recv() => force(&metrics, started, "second signal"),
        _ = tokio::time::sleep(timeout) => force(&metrics, started, "timeout"),
        result = &mut server => match result {
            Ok(()) => {
                tracing::info!(target: "nats_hub::shutdown",duration_ms = started.elapsed().as_millis(), "shutdown complete");
                Ok(())
            }
            Err(e) => {
                tracing::error!(target: "nats_hub::shutdown",error = %e, "shutdown failed");
                force(&metrics, started, "cleanup error")
            }
        },
    }
}

fn force(metrics: &MetricsCollector, started: Instant, reason: &str) -> ! {
    let dropped = metrics.storage_mirror_pending.load(Ordering::Relaxed);
    metrics
        .storage_mirror_shutdown_dropped
        .fetch_add(dropped, Ordering::Relaxed);
    tracing::error!(target: "nats_hub::shutdown",
        reason,
        mirror_writes_dropped = dropped,
        duration_ms = started.elapsed().as_millis(),
        "shutdown forced"
    );
    // A hung DB operation or a second signal must not block runtime teardown.
    std::process::exit(1)
}
