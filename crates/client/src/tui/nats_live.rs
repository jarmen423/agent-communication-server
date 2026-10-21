//! NATS live listener — spawns a background task that pushes
//! decoded envelopes from `channel.>` into an mpsc channel.

use tokio::sync::mpsc;
use tracing::info;

use crate::client::HubClient;
use crate::protocol::Envelope;

/// Spawn the live listener. Returns a receiver that yields envelopes
/// until the NATS subscription ends or the receiver is dropped.
///
/// The underlying `async_nats::Client` auto-reconnects transparently, so the
/// stream pauses on a server outage rather than ending; there is no manual
/// reconnect to manage here. Dropped-envelope accounting (ring-buffer full)
/// lives in `App::ingest_envelope` (`feed_dropped`).
pub async fn spawn_live_listener(
    client: &HubClient,
) -> anyhow::Result<mpsc::UnboundedReceiver<Envelope>> {
    let rx = client.subscribe_all().await?;
    info!("TUI live listener subscribed to channel.>");
    Ok(rx)
}
