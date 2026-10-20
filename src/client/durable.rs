//! Inbox subscription: durable (JetStream consumer) when the hub offers it,
//! else the plain core-NATS subscription. Either way redeliveries are
//! dropped by `meta.id` (see [`SeenIds`]).
//!
//! `NATS_HUB_DURABLE_INBOX` picks the mode:
//!
//! | value              | behaviour |
//! |--------------------|-----------|
//! | unset / `auto`     | attach to the durable inbox if the hub is `--durable`, else core |
//! | `0`/`off`/`false`  | always core (the pre-JetStream behaviour) |
//! | `1`/`on`/`require` | durable or fail |
//!
//! A hub without `--durable` answers the attach request with "no
//! responders" at once, so `auto` costs one round trip there.

use std::time::Duration;

use anyhow::{bail, Result};
use async_nats::jetstream::{self, consumer::PullConsumer};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::dedupe::SeenIds;
use super::HubClient;
use crate::delivery::{self, Attach, InboxTarget};
use crate::protocol::{subjects, Envelope};

/// How long each step of the attach handshake may take.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(2);
/// Back-off between re-attach attempts after the consumer stream ends.
const REATTACH_DELAY: Duration = Duration::from_secs(2);

/// Inbox delivery mode (`NATS_HUB_DURABLE_INBOX`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxMode {
    Auto,
    Off,
    Require,
}

impl InboxMode {
    pub fn from_env() -> Self {
        Self::parse(std::env::var("NATS_HUB_DURABLE_INBOX").ok().as_deref())
    }

    pub fn parse(v: Option<&str>) -> Self {
        match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("0" | "off" | "false" | "no") => Self::Off,
            Some("1" | "on" | "true" | "yes" | "require") => Self::Require,
            _ => Self::Auto,
        }
    }
}

impl HubClient {
    /// Subscribe to this agent's inbox (`channel.inbox.<identity>`).
    ///
    /// On a `--durable` hub this reads the identity's JetStream inbox
    /// consumer, so DMs sent while this client was offline arrive first, in
    /// order, and each is acked once handed to the returned channel. On a
    /// core hub it is a plain subscription. Mode: [`InboxMode::from_env`].
    pub async fn subscribe_inbox(&self) -> Result<mpsc::UnboundedReceiver<Envelope>> {
        match InboxMode::from_env() {
            InboxMode::Off => self.subscribe_inbox_live().await,
            mode => match self.subscribe_inbox_durable().await? {
                Some(rx) => Ok(rx),
                None if mode == InboxMode::Require => bail!(
                    "NATS_HUB_DURABLE_INBOX=require but the hub offers no durable inbox \
                     for {} (is hub-server running with --durable?)",
                    self.identity
                ),
                None => self.subscribe_inbox_live().await,
            },
        }
    }

    /// Plain core-NATS inbox subscription (live DMs only), with dedupe.
    pub async fn subscribe_inbox_live(&self) -> Result<mpsc::UnboundedReceiver<Envelope>> {
        let mut raw = self
            .subscribe_subject(&subjects::inbox(&self.identity))
            .await?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut seen = SeenIds::default();
            while let Some(env) = raw.recv().await {
                if !seen.insert(&env.meta.id) {
                    debug!(id = %env.meta.id, "inbox: duplicate dropped");
                    continue;
                }
                if tx.send(env).is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }

    /// Durable inbox, or `Ok(None)` when the hub has none for us.
    pub async fn subscribe_inbox_durable(
        &self,
    ) -> Result<Option<mpsc::UnboundedReceiver<Envelope>>> {
        let Some(consumer) = self.inbox_consumer().await else {
            return Ok(None);
        };
        info!(identity = %self.identity, "inbox: reading durable consumer");
        let (tx, rx) = mpsc::unbounded_channel();
        let client = self.clone();
        tokio::spawn(async move { client.pump_inbox(consumer, tx).await });
        Ok(Some(rx))
    }

    /// Find our inbox consumer: ask hub-server (which creates it), else,
    /// when no hub-server answers (router down), bind an existing one.
    async fn inbox_consumer(&self) -> Option<PullConsumer> {
        let js = jetstream::new(self.nats.clone());
        let target = match delivery::attach_inbox(&self.nats, &self.identity, ATTACH_TIMEOUT).await
        {
            Attach::Attached(t) => t,
            outcome => {
                debug!(
                    ?outcome,
                    "inbox attach not answered; trying an existing consumer"
                );
                InboxTarget {
                    stream: delivery::client_inbox_stream(),
                    consumer: delivery::inbox_consumer_name(&self.identity),
                }
            }
        };
        let get = js.get_consumer_from_stream(&target.consumer, &target.stream);
        match tokio::time::timeout(ATTACH_TIMEOUT, get).await {
            Ok(Ok(c)) => Some(c),
            Ok(Err(e)) => {
                debug!(error = %e, "no durable inbox consumer");
                None
            }
            Err(_) => {
                debug!("durable inbox consumer lookup timed out");
                None
            }
        }
    }

    /// Forward consumer messages to `tx`, acking each after it is queued.
    /// Survives reconnects and a deleted consumer (re-attaches); stops when
    /// the receiver is dropped (the unacked message is redelivered later).
    async fn pump_inbox(&self, mut consumer: PullConsumer, tx: mpsc::UnboundedSender<Envelope>) {
        let mut seen = SeenIds::default();
        loop {
            match consumer.messages().await {
                Ok(mut msgs) => {
                    while let Some(item) = msgs.next().await {
                        let msg = match item {
                            Ok(m) => m,
                            Err(e) => {
                                debug!(error = %e, "inbox consumer stream error");
                                if matches!(
                                    e.kind(),
                                    jetstream::consumer::pull::MessagesErrorKind::ConsumerDeleted
                                ) {
                                    break;
                                }
                                continue;
                            }
                        };
                        match Envelope::from_json_bytes(&msg.payload) {
                            Ok(env) if seen.insert(&env.meta.id) => {
                                if tx.send(env).is_err() {
                                    return;
                                }
                            }
                            Ok(env) => debug!(id = %env.meta.id, "inbox: redelivery dropped"),
                            Err(e) => warn!(error = %e, "inbox: undecodable envelope acked"),
                        }
                        if let Err(e) = msg.ack().await {
                            debug!(error = %e, "inbox ack failed (will be redelivered)");
                        }
                    }
                }
                Err(e) => debug!(error = %e, "inbox consumer messages() failed"),
            }
            if tx.is_closed() {
                return;
            }
            tokio::time::sleep(REATTACH_DELAY).await;
            match self.inbox_consumer().await {
                Some(c) => consumer = c,
                None => warn!(identity = %self.identity, "inbox consumer gone; retrying"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InboxMode;

    #[test]
    fn mode_parsing() {
        assert_eq!(InboxMode::parse(None), InboxMode::Auto);
        assert_eq!(InboxMode::parse(Some("auto")), InboxMode::Auto);
        assert_eq!(InboxMode::parse(Some("0")), InboxMode::Off);
        assert_eq!(InboxMode::parse(Some("OFF")), InboxMode::Off);
        assert_eq!(InboxMode::parse(Some("require")), InboxMode::Require);
        assert_eq!(InboxMode::parse(Some("1")), InboxMode::Require);
    }
}
