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
///
/// # Reply inbox (contract C1)
///
/// Every connection uses the request-reply inbox prefix `_INBOX.<id>`
/// instead of the shared `_INBOX`, so generated NATS permissions can let each
/// user subscribe `_INBOX.<id>.>` only (no agent can read another's replies).
/// `<id>` is chosen by [`HubConnectOptions::inbox_prefix`]: the NATS `user`
/// when valid, else the credentials JWT `name` (the permission key), else
/// `inbox_identity`, else the default
/// `_INBOX`.
#[derive(Clone, Default)]
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
    /// Identity whose reply inbox `_INBOX.<identity>` this connection uses
    /// when `user` is not itself a valid identity (nkey/creds/token auth).
    /// [`crate::HubClient`] sets it to the client identity; `from_env`
    /// fills it from `NATS_HUB_IDENTITY`.
    pub inbox_identity: Option<String>,
}

impl std::fmt::Debug for HubConnectOptions {
    // Manual impl: never print secrets (token, password, nkey seed) into logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |o: &Option<String>| o.as_ref().map(|_| "<redacted>");
        f.debug_struct("HubConnectOptions")
            .field("token", &redact(&self.token))
            .field("user", &self.user)
            .field("password", &redact(&self.password))
            .field("credentials_file", &self.credentials_file)
            .field("nkey", &redact(&self.nkey))
            .field("require_tls", &self.require_tls)
            .field("inbox_identity", &self.inbox_identity)
            .finish()
    }
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

    /// Convenience builder: set the identity used for the reply inbox prefix.
    #[must_use]
    pub fn with_inbox_identity(mut self, identity: impl Into<String>) -> Self {
        self.inbox_identity = Some(identity.into());
        self
    }

    /// Request-reply inbox prefix for this connection (contract C1):
    /// `_INBOX.<user>` when `user` is a valid identity, else
    /// the credentials JWT `name`, else `_INBOX.<inbox_identity>` when valid,
    /// else `None` (the
    /// async-nats default `_INBOX`).
    pub fn inbox_prefix(&self) -> Option<String> {
        // The credential name is the permission key even when a HubClient
        // has a different logical sender (the hub's internal wave client).
        let credential_identity = self
            .credentials_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|contents| credentials_identity(&contents));
        let prefix = [
            self.user.as_deref(),
            credential_identity.as_deref(),
            self.inbox_identity.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|id| crate::protocol::valid_identity(id))
        .map(crate::protocol::subjects::reply_inbox_prefix);
        prefix
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
    /// | `NATS_HUB_IDENTITY` | `inbox_identity` (when a valid identity) |
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
        if let Ok(v) = std::env::var("NATS_HUB_IDENTITY") {
            if crate::protocol::valid_identity(&v) {
                o.inbox_identity = Some(v);
            }
        }
        o
    }
}

/// Open an `async_nats::Client` using [`HubConnectOptions`].
///
/// Auth precedence: credentials_file > nkey > user+password > token.
/// The reply inbox prefix follows [`HubConnectOptions::inbox_prefix`].
pub async fn connect_with_hub_opts(
    url: &str,
    opts: &HubConnectOptions,
) -> Result<async_nats::Client> {
    let mut co = async_nats::ConnectOptions::new();

    if let Some(creds_path) = &opts.credentials_file {
        // async-nats' credentials_file() snapshots both JWT and seed. The
        // callback instead reloads one complete file on every authentication,
        // including reconnect after JWT expiry. Writers must rename atomically.
        let path = creds_path.clone();
        co = async_nats::ConnectOptions::with_auth_callback(move |nonce| {
            let path = path.clone();
            async move {
                let contents = tokio::fs::read_to_string(&path).await.map_err(|e| {
                    async_nats::AuthError::new(format!(
                        "cannot read NATS credentials file {}: {e}",
                        path.display()
                    ))
                })?;
                credentials_auth(&contents, &nonce)
            }
        });
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
    let inbox_prefix = opts.inbox_prefix();
    if let Some(prefix) = &inbox_prefix {
        co = co.custom_inbox_prefix(prefix);
    }

    debug!(%url, has_auth = opts.has_auth(), require_tls = opts.require_tls, ?inbox_prefix, "async_nats connect_with_options");
    async_nats::connect_with_options(url, co)
        .await
        .with_context(|| format!("failed to connect to NATS at {url}"))
}

/// Parse a decorated credentials file without including secrets in errors.
fn credentials_auth(
    contents: &str,
    nonce: &[u8],
) -> Result<async_nats::Auth, async_nats::AuthError> {
    let field = |marker: &str| {
        contents
            .lines()
            .skip_while(|line| !line.contains(marker))
            .nth(1)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let invalid = || async_nats::AuthError::new("invalid NATS credentials file");
    let jwt = field("BEGIN NATS USER JWT").ok_or_else(invalid)?;
    let seed = field("BEGIN USER NKEY SEED").ok_or_else(invalid)?;
    let key = nkeys::KeyPair::from_seed(seed).map_err(|_| invalid())?;
    let signature = key.sign(nonce).map_err(|_| invalid())?;
    let mut auth = async_nats::Auth::new();
    auth.jwt = Some(jwt.into());
    auth.signature = Some(signature);
    Ok(auth)
}

/// Read a JWT name solely as a local inbox hint; NATS verifies authentication.
/// Keep this dependency-free for the embeddable client crate.
fn credentials_identity(contents: &str) -> Option<String> {
    let jwt = contents
        .lines()
        .skip_while(|l| !l.contains("BEGIN NATS USER JWT"))
        .nth(1)?;
    let payload = jwt.trim().split('.').nth(1)?;
    let mut bytes = Vec::new();
    let (mut bits, mut count) = (0_u32, 0_u32);
    for c in payload.bytes().take(16 * 1024) {
        let n = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(n);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let name = claims.get("name")?.as_str()?;
    crate::protocol::valid_identity(name).then(|| name.to_owned())
}

#[cfg(test)]
mod credential_inbox_tests {
    use super::*;

    #[test]
    fn jwt_name_is_the_inbox_permission_key() {
        // {"name":"hub-server"}, with an arbitrary header/signature. This is
        // a hint only; the authentication callback/NATS verifies the creds.
        let contents = "-----BEGIN NATS USER JWT-----\na.eyJuYW1lIjoiaHViLXNlcnZlciJ9.b\n";
        assert_eq!(
            credentials_identity(contents).as_deref(),
            Some("hub-server")
        );
        assert!(credentials_identity("not a creds file").is_none());
        assert!(credentials_identity("BEGIN NATS USER JWT\na.bad!.b").is_none());
        let path = std::env::temp_dir().join(format!("acs-inbox-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, contents).unwrap();
        let opts = HubConnectOptions::default()
            .with_credentials_file(&path)
            .with_inbox_identity("internal-wave-client");
        assert_eq!(opts.inbox_prefix().as_deref(), Some("_INBOX.hub-server"));
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod server_addr_tests {
    use std::str::FromStr;

    /// A ws(s):// URL without a port means the scheme's port. async-nats
    /// before 0.50 dialed 4222 for it (and for an explicit `:443`, which
    /// URL parsing drops), so Rust clients couldn't reach `wss://<host>`
    /// behind a TLS proxy or Cloudflare.
    #[test]
    fn websocket_urls_default_to_the_scheme_port() {
        let port = |url: &str| async_nats::ServerAddr::from_str(url).unwrap().port();
        assert_eq!(port("wss://hub.example.com"), 443);
        assert_eq!(port("wss://hub.example.com:443"), 443);
        assert_eq!(port("wss://hub.example.com:8443"), 8443);
        assert_eq!(port("ws://hub.example.com"), 80);
        assert_eq!(port("nats://hub.example.com"), 4222);
        assert_eq!(port("hub.example.com"), 4222);
    }
}
