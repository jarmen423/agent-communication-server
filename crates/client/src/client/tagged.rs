//! Subscriptions that keep the NATS subject alongside each envelope.
//!
//! Needed wherever the *subject* is the trustworthy part (iteration-2
//! contract §4.1): NATS permissions pin `hub.presence.<identity>` to that
//! identity's credential, while the payload is self-asserted.

use anyhow::{Context, Result};
use futures_util::StreamExt;

use super::HubClient;
use crate::protocol::Envelope;

impl HubClient {
    /// Like [`HubClient::subscribe_subject`], but yields `(subject, envelope)`.
    pub async fn subscribe_subject_tagged(
        &self,
        subject: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<(String, Envelope)>> {
        let subject_owned = subject.to_string();
        let mut sub = self
            .nats
            .subscribe(subject_owned.clone())
            .await
            .with_context(|| format!("subscribe to {subject_owned} failed"))?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                match Envelope::from_json_bytes(&msg.payload) {
                    Ok(env) => {
                        if tx.send((msg.subject.to_string(), env)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to decode envelope on {subject_owned}");
                    }
                }
            }
        });
        Ok(rx)
    }
}
