# W2-B — Rust HubClient auth via ConnectOptions + env

## Outcome

`HubClient::connect` remains backward-compatible and now picks up auth/TLS from
the environment. Programmatic callers use `HubClient::connect_with_opts` +
`HubConnectOptions` (builders + `from_env()`).

## Files

| Path | Change |
|------|--------|
| `src/connect_opts.rs` | **NEW** — `HubConnectOptions`, `connect_with_hub_opts` |
| `src/client.rs` | `connect` env routing; `connect_with_opts`; uses connect_opts |
| `src/lib.rs` | `pub mod connect_opts`; re-export `HubConnectOptions` |
| `tests/hub_connect_opts.rs` | **NEW** — 16 unit tests + 2 ignored live tests |

## API surface

```rust
pub struct HubConnectOptions {
    pub token: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub credentials_file: Option<PathBuf>,
    pub nkey: Option<String>,
    pub require_tls: bool,
}

impl HubConnectOptions {
    pub fn with_token(self, token) -> Self;
    pub fn with_user_password(self, user, password) -> Self;
    pub fn with_credentials_file(self, path) -> Self;
    pub fn with_nkey(self, seed) -> Self;
    pub fn require_tls(self, required: bool) -> Self;
    pub fn has_auth(&self) -> bool;
    pub fn from_env() -> Self;
}

impl HubClient {
    // unchanged signature; reads NATS_* env when set
    pub async fn connect(url, identity) -> Result<Self>;
    pub async fn connect_with_opts(url, identity, opts: HubConnectOptions) -> Result<Self>;
}
```

Auth precedence: credentials_file > nkey > user+password > token.  
`require_tls` always applied when true.

## Env var matrix

| Env | Field |
|-----|--------|
| `NATS_TOKEN` | token |
| `NATS_USER` | user |
| `NATS_PASSWORD` | password |
| `NATS_CREDENTIALS_FILE` | credentials_file (no tilde expand) |
| `NATS_NKEY` | nkey |
| `NATS_REQUIRE_TLS=1\|true\|yes` | require_tls |

All hub-* CLIs that call `HubClient::connect` inherit env auth with no bin edits.

## Verification

```
cargo test --test hub_connect_opts
# 16 passed; 0 failed; 2 ignored
```

## Residual risks

- Path tilde/`$HOME` not expanded on credentials_file — expand at call site.
- Live token/TLS connect tests are `#[ignore]` until dogfood script (W2-E).
- `test_agent_activity` analytics failure is **pre-existing** (reproduced on clean tree without W2-B); not caused by this task.
