//! Query API client — used by CLI tools to route DB ops through hub-server.
//!
//! Instead of opening their own SurrealDB connection (which fails due to
//! RocksDB single-writer lock), CLI tools use this client to send requests
//! to hub-server's query API via NATS request-reply.

use anyhow::{bail, Result};
use serde_json::Value;
use std::time::Duration;

/// Client for the query API. Connects to NATS and sends requests to
/// `hub.api.<op>` (legacy) or `hub.api.<identity>.<op>` (bound, when an
/// identity is configured).
pub struct ApiClient {
    nats: async_nats::Client,
    timeout: Duration,
    /// Caller identity for bound api subjects (`hub.api.<identity>.<op>`).
    /// `None` → legacy `hub.api.<op>` (self-asserted; rejected by hubs
    /// running `--require-bound-identity`).
    identity: Option<String>,
}

impl ApiClient {
    /// Connect to NATS for query-API request/reply.
    ///
    /// Uses [`crate::HubConnectOptions::from_env`] so CLIs honor the same
    /// `NATS_TOKEN` / user / credentials / TLS env vars as `HubClient`.
    /// The caller identity comes from `NATS_HUB_IDENTITY`; when unset (or
    /// invalid) the client falls back to the legacy `hub.api.<op>` form.
    pub async fn connect(nats_url: &str) -> Result<Self> {
        let opts = crate::HubConnectOptions::from_env();
        let nats = crate::connect_opts::connect_with_hub_opts(nats_url, &opts).await?;
        let identity = std::env::var("NATS_HUB_IDENTITY")
            .ok()
            .filter(|i| crate::protocol::valid_identity(i));
        Ok(Self {
            nats,
            timeout: Duration::from_secs(10),
            identity,
        })
    }

    /// Connect with an explicit caller identity — requests go out on the
    /// bound subject `hub.api.<identity>.<op>` (contract §4.1).
    pub async fn connect_with_identity(
        nats_url: &str,
        identity: impl Into<String>,
    ) -> Result<Self> {
        let identity = identity.into();
        crate::protocol::require_valid_identity(&identity)?;
        let opts = crate::HubConnectOptions::from_env();
        let nats = crate::connect_opts::connect_with_hub_opts(nats_url, &opts).await?;
        Ok(Self {
            nats,
            timeout: Duration::from_secs(10),
            identity: Some(identity),
        })
    }

    /// Override the caller identity after connecting.
    pub fn with_identity(mut self, identity: impl Into<String>) -> Result<Self> {
        let identity = identity.into();
        crate::protocol::require_valid_identity(&identity)?;
        self.identity = Some(identity);
        Ok(self)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The api subject this client calls for `op`.
    fn subject_for(&self, op: &str) -> String {
        match &self.identity {
            Some(id) => crate::query_api::subject_bound(id, op),
            None => crate::query_api::subject(op),
        }
    }

    /// Send a query API request and get the response data.
    /// Returns error if the server returns an error or times out.
    pub async fn request(&self, op: &str, params: Value) -> Result<Value> {
        let subject = self.subject_for(op);
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
