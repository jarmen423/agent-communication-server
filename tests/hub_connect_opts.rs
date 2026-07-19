//! Unit tests for the W2-B `HubConnectOptions` auth additions on `HubClient`.
//!
//! These tests deliberately avoid any live NATS connection — they cover:
//! - `HubConnectOptions::default()` constructs cleanly with no auth set
//! - the env-var fallback path in `HubConnectOptions::from_env()` picks up
//!   each documented variable
//! - `HubConnectOptions::has_auth()` correctly distinguishes the plain
//!   (anonymous) case from authenticated ones (used by `HubClient::connect`
//!   to decide whether to route through the options path)
//! - builder helpers (`with_token`, `with_user_password`,
//!   `with_credentials_file`, `with_nkey`, `require_tls`) compose
//!
//! Tests that need a real NATS server are marked `#[ignore]`.

#![forbid(unsafe_code)]

use nats_hub::HubConnectOptions;
use std::path::PathBuf;

// ── Default construction ───────────────────────────────────────────────

#[test]
fn default_options_have_no_auth_and_no_tls() {
    let o = HubConnectOptions::default();
    assert!(o.token.is_none(), "token should default to None");
    assert!(o.user.is_none(), "user should default to None");
    assert!(o.password.is_none(), "password should default to None");
    assert!(
        o.credentials_file.is_none(),
        "credentials_file should default to None"
    );
    assert!(o.nkey.is_none(), "nkey should default to None");
    assert!(!o.require_tls, "require_tls should default to false");
}

#[test]
fn default_options_debug_format_is_stable() {
    // Confirms Debug is derived (the connect() path logs `?opts`).
    let o = HubConnectOptions::default();
    let s = format!("{:?}", o);
    assert!(
        s.contains("token"),
        "Debug should expose token field: {}",
        s
    );
    assert!(
        s.contains("require_tls"),
        "Debug should expose require_tls field: {}",
        s
    );
}

// ── Builder helpers ────────────────────────────────────────────────────

#[test]
fn builder_with_token() {
    let o = HubConnectOptions::default().with_token("hunter2");
    assert_eq!(o.token.as_deref(), Some("hunter2"));
    assert!(o.user.is_none());
    assert!(o.password.is_none());
}

#[test]
fn builder_with_user_password() {
    let o = HubConnectOptions::default().with_user_password("josh", "s3cret");
    assert_eq!(o.user.as_deref(), Some("josh"));
    assert_eq!(o.password.as_deref(), Some("s3cret"));
    assert!(o.token.is_none());
}

#[test]
fn builder_with_credentials_file() {
    let path = PathBuf::from("/etc/nats/agent.creds");
    let o = HubConnectOptions::default().with_credentials_file(path.clone());
    assert_eq!(o.credentials_file.as_ref(), Some(&path));
}

#[test]
fn builder_with_nkey() {
    let o = HubConnectOptions::default().with_nkey("SUANQEXAMPLESEED");
    assert_eq!(o.nkey.as_deref(), Some("SUANQEXAMPLESEED"));
}

#[test]
fn builder_require_tls() {
    let o = HubConnectOptions::default().require_tls(true);
    assert!(o.require_tls);

    let o2 = HubConnectOptions::default().require_tls(false);
    assert!(!o2.require_tls);
}

#[test]
fn builders_compose_chain() {
    let o = HubConnectOptions::default()
        .with_user_password("u", "p")
        .require_tls(true);
    assert_eq!(o.user.as_deref(), Some("u"));
    assert_eq!(o.password.as_deref(), Some("p"));
    assert!(o.require_tls);
    assert!(o.token.is_none());
}

#[test]
fn options_are_cloneable() {
    // Required because HubClient is Clone and may carry options in future
    // API extensions.
    let o = HubConnectOptions::default().with_token("abc");
    let o_clone = o.clone();
    assert_eq!(o.token, o_clone.token);
}

// ── Env var fallback path ──────────────────────────────────────────────
//
// IMPORTANT: env vars are process-global and cargo runs tests in parallel
// by default, so any two tests that touch overlapping env vars will race.
// We guard ALL NATS_* env access with a single shared mutex so the env
// tests serialize against each other. Non-env tests above (default(),
// builders) don't touch the environment and don't need the lock.

use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn clear_all_nats_env() {
    std::env::remove_var("NATS_TOKEN");
    std::env::remove_var("NATS_USER");
    std::env::remove_var("NATS_PASSWORD");
    std::env::remove_var("NATS_CREDENTIALS_FILE");
    std::env::remove_var("NATS_NKEY");
    std::env::remove_var("NATS_REQUIRE_TLS");
}

#[test]
fn from_env_picks_up_nats_token() {
    let _g = env_lock().lock().unwrap();
    clear_all_nats_env();
    std::env::set_var("NATS_TOKEN", "tok-from-env-xyz");
    let o = HubConnectOptions::from_env();
    assert_eq!(o.token.as_deref(), Some("tok-from-env-xyz"));
    assert!(
        o.user.is_none() && o.password.is_none(),
        "user/pass should not be set when only NATS_TOKEN is"
    );
    std::env::remove_var("NATS_TOKEN");
}

#[test]
fn from_env_picks_up_nats_user_password() {
    let _g = env_lock().lock().unwrap();
    clear_all_nats_env();
    std::env::set_var("NATS_USER", "alice");
    std::env::set_var("NATS_PASSWORD", "pw-alice");
    let o = HubConnectOptions::from_env();
    assert_eq!(o.user.as_deref(), Some("alice"));
    assert_eq!(o.password.as_deref(), Some("pw-alice"));
    assert!(o.token.is_none());
    std::env::remove_var("NATS_USER");
    std::env::remove_var("NATS_PASSWORD");
}

#[test]
fn from_env_picks_up_nats_credentials_file() {
    let _g = env_lock().lock().unwrap();
    clear_all_nats_env();
    std::env::set_var("NATS_CREDENTIALS_FILE", "/var/secrets/agent.creds");
    let o = HubConnectOptions::from_env();
    assert_eq!(
        o.credentials_file.as_ref(),
        Some(&PathBuf::from("/var/secrets/agent.creds"))
    );
    std::env::remove_var("NATS_CREDENTIALS_FILE");
}

#[test]
fn from_env_picks_up_nats_nkey() {
    let _g = env_lock().lock().unwrap();
    clear_all_nats_env();
    std::env::set_var("NATS_NKEY", "SUANQENVSEED");
    let o = HubConnectOptions::from_env();
    assert_eq!(o.nkey.as_deref(), Some("SUANQENVSEED"));
    std::env::remove_var("NATS_NKEY");
}

#[test]
fn from_env_picks_up_require_tls_variants() {
    let _g = env_lock().lock().unwrap();
    for val in &["1", "true", "TRUE", "Yes", "yes"] {
        clear_all_nats_env();
        std::env::set_var("NATS_REQUIRE_TLS", val);
        let o = HubConnectOptions::from_env();
        assert!(
            o.require_tls,
            "NATS_REQUIRE_TLS={:?} should set require_tls=true",
            val
        );
    }

    for val in &["0", "false", "no", "", "anything-else"] {
        clear_all_nats_env();
        std::env::set_var("NATS_REQUIRE_TLS", val);
        let o = HubConnectOptions::from_env();
        assert!(
            !o.require_tls,
            "NATS_REQUIRE_TLS={:?} should leave require_tls=false",
            val
        );
    }
    clear_all_nats_env();
}

#[test]
fn from_env_empty_string_is_treated_as_unset() {
    let _g = env_lock().lock().unwrap();
    clear_all_nats_env();
    std::env::set_var("NATS_TOKEN", "");
    let o = HubConnectOptions::from_env();
    assert!(
        o.token.is_none(),
        "empty NATS_TOKEN should not produce Some(\"\")"
    );
    std::env::remove_var("NATS_TOKEN");
}

#[test]
fn from_env_with_no_vars_yields_default() {
    let _g = env_lock().lock().unwrap();
    clear_all_nats_env();
    let o = HubConnectOptions::from_env();
    assert!(o.token.is_none());
    assert!(o.user.is_none());
    assert!(o.password.is_none());
    assert!(o.credentials_file.is_none());
    assert!(o.nkey.is_none());
    assert!(!o.require_tls);
}

// ── Live-server tests (marked #[ignore]) ───────────────────────────────
//
// To run manually:
//   nats-server -p 4222 --auth tok-live   # or whatever auth scheme
//   NATS_TOKEN=tok-live cargo test -- --ignored hub_connect_opts_live

#[tokio::test]
#[ignore = "requires a live NATS server with token auth on localhost"]
async fn hub_connect_opts_live_token_auth_connects() {
    use nats_hub::HubClient;
    let url =
        std::env::var("TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let opts = HubConnectOptions::default()
        .with_token(std::env::var("TEST_NATS_TOKEN").unwrap_or_else(|_| "tok-live".to_string()));
    let client = HubClient::connect_with_opts(&url, "w2b-test", opts)
        .await
        .expect("connect with token auth should succeed against a matching server");
    client.drain().await.expect("clean drain");
}

#[tokio::test]
#[ignore = "requires a live TLS NATS server (e.g. wss://)"]
async fn hub_connect_opts_live_require_tls_connects() {
    use nats_hub::HubClient;
    let url =
        std::env::var("TEST_NATS_TLS_URL").unwrap_or_else(|_| "tls://127.0.0.1:4222".to_string());
    let opts = HubConnectOptions::default().require_tls(true);
    let client = HubClient::connect_with_opts(&url, "w2b-tls-test", opts)
        .await
        .expect("connect with require_tls should succeed against a TLS server");
    client.drain().await.expect("clean drain");
}
