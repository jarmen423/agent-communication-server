//! Query API client — used by CLI tools to route DB ops through hub-server.
//!
//! Instead of opening their own SurrealDB connection (which fails due to
//! RocksDB single-writer lock), CLI tools use this client to send requests
//! to hub-server's query API via NATS request-reply.

use anyhow::{bail, Result};
use serde_json::Value;
use std::time::Duration;

/// Client for the query API. Connects to NATS and sends requests to `hub.api.<op>`.
pub struct ApiClient {
    nats: async_nats::Client,
    timeout: Duration,
}

impl ApiClient {
    /// Connect to NATS for query-API request/reply.
    ///
    /// Uses [`crate::HubConnectOptions::from_env`] so CLIs honor the same
    /// `NATS_TOKEN` / user / credentials / TLS env vars as `HubClient`.
    pub async fn connect(nats_url: &str) -> Result<Self> {
        let opts = crate::HubConnectOptions::from_env();
        let nats = crate::connect_opts::connect_with_hub_opts(nats_url, &opts).await?;
        Ok(Self {
            nats,
            timeout: Duration::from_secs(10),
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Send a query API request and get the response data.
    /// Returns error if the server returns an error or times out.
    pub async fn request(&self, op: &str, params: Value) -> Result<Value> {
        let subject = crate::query_api::subject(op);
        let payload = serde_json::to_vec(&serde_json::json!({
            "op": op,
            "params": params,
        }))?;

        let reply = tokio::time::timeout(self.timeout, self.nats.request(subject, payload.into()))
            .await
            .map_err(|_| {
                anyhow::anyhow!("query API timeout ({:?}) for op '{op}'", self.timeout)
            })??;

        let resp: crate::query_api::ApiResponse = serde_json::from_slice(&reply.payload)?;

        if !resp.ok {
            let msg = resp.error.unwrap_or_else(|| "unknown error".to_string());
            bail!("query API '{op}' failed: {msg}")
        }

        Ok(resp.data.unwrap_or(Value::Null))
    }
}
