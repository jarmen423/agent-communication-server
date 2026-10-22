//! HTTP device API. Never include response bodies, tokens or seeds in errors.
use crate::local::{self, State};
use anyhow::{bail, Context, Result};
use reqwest::{header::RETRY_AFTER, Client, Method, StatusCode, Url};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime},
};
use tokio::time::Instant;

const REQUEST_BUDGET: Duration = Duration::from_secs(60);
const MAX_RETRIES: usize = 60;

fn retry_delay(header: &str) -> Option<Duration> {
    let delay = header
        .trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .ok()
        .or_else(|| {
            httpdate::parse_http_date(header)
                .ok()
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })?;
    Some(delay.min(Duration::from_secs(10)))
}

pub struct Api {
    client: Client,
    pub server: String,
}

impl Api {
    pub fn new(server: &str) -> Result<Self> {
        let url = Url::parse(server).context("invalid control-plane URL")?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("control-plane URL requires HTTPS (HTTP allowed only on loopback) and no embedded credentials");
        }
        Ok(Self {
            server: server.trim_end_matches('/').to_owned(),
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> Result<Value> {
        self.request_until(method, path, token, body, Instant::now() + REQUEST_BUDGET)
            .await
    }

    async fn request_until(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Value,
        deadline: Instant,
    ) -> Result<Value> {
        let builder = self
            .client
            .request(method, format!("{}{path}", self.server));
        // RFC 8628 device requests use application/x-www-form-urlencoded.
        let mut req = if path == "/device/code" || path == "/device/token" {
            builder.form(&body)
        } else {
            builder.json(&body)
        };
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let mut retries = 0;
        loop {
            let response = tokio::time::timeout_at(
                deadline,
                req.try_clone()
                    .context("cannot retry control-plane request")?
                    .send(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("control-plane request timed out"))?
            .map_err(|_| anyhow::anyhow!("control plane unavailable"))?;
            let status = response.status();
            if status == StatusCode::SERVICE_UNAVAILABLE && retries < MAX_RETRIES {
                if let Some(delay) = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(retry_delay)
                {
                    if Instant::now() + delay < deadline {
                        retries += 1;
                        drop(response);
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                }
            }
            let bytes = tokio::time::timeout_at(deadline, response.bytes())
                .await
                .map_err(|_| anyhow::anyhow!("control-plane request timed out"))?
                .context("cannot read control-plane response")?;
            let value: Value =
                serde_json::from_slice(&bytes).context("invalid control-plane response")?;
            if !status.is_success() {
                // Error names are a fixed protocol vocabulary; don't reflect server text.
                let code = value["error"].as_str().unwrap_or("request_failed");
                let safe = match code {
                    "authorization_pending"
                    | "slow_down"
                    | "expired_token"
                    | "invalid_grant"
                    | "access_denied"
                    | "invalid_token"
                    | "identity_exists"
                    | "invalid_request"
                    | "invalid_client"
                    | "temporarily_unavailable"
                    | "rate_limited"
                    | "quota_exceeded"
                    | "key_in_use"
                    | "revocation_list_full"
                    | "unsupported_grant_type" => code,
                    _ => "request_failed",
                };
                bail!("HTTP {} {safe}", status.as_u16());
            }
            return Ok(value);
        }
    }
}

pub fn string(value: &Value, name: &str) -> Result<String> {
    value[name]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("missing {name} in response"))
}

pub fn accept(root: &Path, state: &mut State, value: &Value) -> Result<()> {
    local::store_refresh(root, &string(value, "refresh_token")?)?;
    state.access_token = string(value, "access_token")?;
    state.access_expires_at = local::now()
        + value["expires_in"]
            .as_u64()
            .context("missing token expiry")?;
    local::save(root, state)
}

pub async fn authorized(root: &Path, state: &mut State) -> Result<Api> {
    let api = Api::new(&state.server)?;
    if state.access_expires_at <= local::now() + 30 {
        let value = api
            .request(
                Method::POST,
                "/device/token",
                None,
                json!({"grant_type": "refresh_token", "refresh_token": local::refresh(root)?}),
            )
            .await?;
        accept(root, state, &value)?;
    }
    Ok(api)
}

pub async fn login(root: &Path, server: &str, headless: bool, key: Option<String>) -> Result<()> {
    if root.join("state.json").exists() {
        bail!("already logged in; log out before changing devices");
    }
    let api = Api::new(server)?;
    let value = if let Some(key) = key {
        api.request(
            Method::POST,
            "/device/token",
            None,
            json!({"grant_type": "service_key", "key": key}),
        )
        .await?
    } else {
        let code = api
            .request(
                Method::POST,
                "/device/code",
                None,
                json!({"client_id": "acs"}),
            )
            .await?;
        let url = string(&code, "verification_uri_complete")?;
        // Only open a URL belonging to the selected issuer.
        let verify = Url::parse(&url)?;
        if verify.origin() != Url::parse(&api.server)?.origin() {
            bail!("foreign verification URL refused");
        }
        println!("Open {url}");
        use std::io::Write;
        std::io::stdout().flush()?;
        if !headless {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            if tokio::process::Command::new(opener)
                .arg(&url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .is_err()
            {
                eprintln!("could not open browser; use the printed URL");
            }
        }
        let mut interval = code["interval"].as_u64().unwrap_or(5).max(1);
        let deadline = local::now() + code["expires_in"].as_u64().unwrap_or(300);
        loop {
            if local::now() >= deadline {
                bail!("device login expired; run acs login again");
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
            match api.request(Method::POST, "/device/token", None,
                json!({"grant_type": "urn:ietf:params:oauth:grant-type:device_code", "device_code": code["device_code"], "client_id": "acs"})).await {
                Ok(value) => break value,
                Err(e) if e.to_string().ends_with("authorization_pending") => (),
                Err(e) if e.to_string().ends_with("slow_down") => interval += 5,
                Err(e) => return Err(e),
            }
        }
    };
    let mut state = State {
        server: api.server,
        tenant: string(&value, "tenant")?,
        device: string(&value, "device")?,
        nats_url: string(&value, "nats_url")?,
        access_token: String::new(),
        access_expires_at: 0,
        agents: BTreeMap::new(),
    };
    accept(root, &mut state, &value)?;
    println!("Logged in: tenant {} device {}", state.tenant, state.device);
    Ok(())
}

#[cfg(test)]
mod tests;
