//! Query intake and tracked in-flight requests.
use super::{encode_response, handle_request_authorized, ApiAuthz, API_PREFIX};
use crate::Storage;
use anyhow::Result;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Stop intake on cancellation, then finish every already dispatched request.
/// The owner drains the shared NATS connection after storage is released.
pub async fn serve_api_listener(
    storage: Arc<dyn Storage>,
    client: async_nats::Client,
    authz: ApiAuthz,
    shutdown: CancellationToken,
) -> Result<()> {
    let mut sub = client.subscribe(format!("{API_PREFIX}.>")).await?;
    let mut requests = JoinSet::new();
    debug!(require_bound = authz.require_bound, "query API listening");
    loop {
        let msg = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            done = requests.join_next(), if !requests.is_empty() => {
                if let Some(done) = done { done?; }
                continue;
            }
            msg = sub.next() => match msg {
                Some(msg) => msg,
                None => break,
            }
        };
        let (storage, client, authz) = (storage.clone(), client.clone(), authz.clone());
        requests.spawn(async move {
            let resp =
                handle_request_authorized(&storage, msg.subject.as_str(), &msg.payload, &authz)
                    .await;
            if let Some(reply) = msg.reply {
                let bytes = encode_response(&resp, client.server_info().max_payload);
                if let Err(e) = client.publish(reply, bytes.into()).await {
                    warn!(error = %e, "query API: failed to publish reply");
                }
            }
        });
    }
    drop(sub);
    while let Some(done) = requests.join_next().await {
        done?;
    }
    Ok(())
}
