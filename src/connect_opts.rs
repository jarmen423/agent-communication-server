//! NATS connection options for authenticated / TLS hubs.
//!
//! Shared by [`crate::HubClient`] so CLIs and embedders can pass token,
//! user/password, credentials files, NKEYs, and `require_tls` without
//! depending on `async_nats` types directly.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing::debug;

/// Authentication + transport options for [`crate::HubClient::connect_with_opts`].
///
/// Mirrors the subset of `async_nats::ConnectOptions` auth knobs needed to
/// reach an authenticated or `wss://` hub from any machine. All fields are
/// optional; `Default::default()` is equivalent to the legacy unauthenticated
/// connect path used by [`crate::HubClient::connect`].
///
/// # Field precedence
///
/// When multiple auth methods are set, the following precedence is applied
/// (first match wins, matching how the nats-server negotiates NKEY/creds
/// before username/password before token):
///
/// 1. `credentials_file` — NATS user JWT + NKEY seed file (highest)
/// 2. `nkey`             — raw NKEY seed
/// 3. `user` + `password`
/// 4. `token`
///
/// `require_tls` is orthogonal and always applied.
#[derive(Clone, Debug, Default)]
pub struct HubConnectOptions {
    /// Static auth token (`--token` on nats-server). Lowest auth precedence.
    pub token: Option<String>,
    /// Username for user/password auth. Used with `password`.
    pub user: Option<String>,
    /// Password for user/password auth.
    pub password: Option<String>,
    /// Path to a NATS `.creds` file (JWT + NKEY seed). Highest auth precedence.
    /// `~` and env vars are NOT expanded — expand before constructing.
    pub credentials_file: Option<PathBuf>,
    /// Raw NKEY seed string (e.g. `SUANQ...`). Second-highest auth precedence.
    pub nkey: Option<String>,
    /// Force TLS (`async_nats::ConnectOptions::require_tls`). Required for
    /// `wss://` / TLS-only deployments. Default `false`.
    pub require_tls: bool,
}

impl HubConnectOptions {
    /// Convenience builder: set the token auth method.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Convenience builder: set user + password.
    #[must_use]
    pub fn with_user_password(
        mut self,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.user = Some(user.into());
        self.password = Some(password.into());
        self
    }

    /// Convenience builder: set a credentials file path.
    #[must_use]
    pub fn with_credentials_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.credentials_file = Some(path.into());
        self
    }

    /// Convenience builder: set an NKEY seed.
    #[must_use]
    pub fn with_nkey(mut self, seed: impl Into<String>) -> Self {
        self.nkey = Some(seed.into());
        self
    }

    /// Convenience builder: force TLS (set true for wss:// hubs).
    #[must_use]
    pub fn require_tls(mut self, required: bool) -> Self {
        self.require_tls = required;
        self
    }

    /// Returns true when any auth field is set (not a plain anonymous connect).
    pub fn has_auth(&self) -> bool {
        self.token.is_some()
            || self.user.is_some()
            || self.password.is_some()
            || self.credentials_file.is_some()
            || self.nkey.is_some()
    }

    /// Build options from environment variables.
    ///
    /// | Env var | Field |
    /// |---------|-------|
    /// | `NATS_TOKEN` | `token` |
    /// | `NATS_USER` | `user` |
    /// | `NATS_PASSWORD` | `password` |
    /// | `NATS_CREDENTIALS_FILE` | `credentials_file` (as-is; no tilde expand) |
    /// | `NATS_NKEY` | `nkey` |
    /// | `NATS_REQUIRE_TLS=1|true|yes` | `require_tls` |
    pub fn from_env() -> Self {
        let mut o = Self::default();

        if let Ok(v) = std::env::var("NATS_TOKEN") {
            if !v.is_empty() {
                o.token = Some(v);
            }
        }
        if let Ok(v) = std::env::var("NATS_USER") {
            if !v.is_empty() {
                o.user = Some(v);
            }
        }
        if let Ok(v) = std::env::var("NATS_PASSWORD") {
            if !v.is_empty() {
                o.password = Some(v);
            }
        }
        if let Ok(v) = std::env::var("NATS_CREDENTIALS_FILE") {
            if !v.is_empty() {
                o.credentials_file = Some(PathBuf::from(v));
            }
        }
        if let Ok(v) = std::env::var("NATS_NKEY") {
            if !v.is_empty() {
                o.nkey = Some(v);
            }
        }
        if let Ok(v) = std::env::var("NATS_REQUIRE_TLS") {
            let lower = v.trim().to_ascii_lowercase();
            if lower == "1" || lower == "true" || lower == "yes" {
                o.require_tls = true;
            }
        }
        o
    }
}

/// Open an `async_nats::Client` using [`HubConnectOptions`].
///
/// Auth precedence: credentials_file > nkey > user+password > token.
pub async fn connect_with_hub_opts(
    url: &str,
    opts: &HubConnectOptions,
) -> Result<async_nats::Client> {
    let mut co = async_nats::ConnectOptions::new();

    if let Some(creds_path) = &opts.credentials_file {
        co = co
            .credentials_file(creds_path)
            .await
            .with_context(|| format!("loading NATS credentials file at {:?}", creds_path))?;
    } else if let Some(seed) = &opts.nkey {
        co = co.nkey(seed.clone());
    } else if let (Some(user), Some(pass)) = (opts.user.as_ref(), opts.password.as_ref()) {
        co = co.user_and_password(user.clone(), pass.clone());
    } else if let Some(token) = &opts.token {
        co = co.token(token.clone());
    }

    if opts.require_tls {
        co = co.require_tls(true);
    }

    debug!(%url, has_auth = opts.has_auth(), require_tls = opts.require_tls, "async_nats connect_with_options");
    async_nats::connect_with_options(url, co)
        .await
        .with_context(|| format!("failed to connect to NATS at {url}"))
}
