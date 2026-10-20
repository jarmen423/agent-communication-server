//! Durable delivery permissions (W2-A acceptance criterion 7).
//!
//! Renders a real config with `hub-admin render-config --durable`, boots a
//! private nats-server from it and `hub-server --durable
//! --require-bound-identity` as the `hub-server` user, then checks that an
//! agent can attach to and read only its own inbox consumer and can't reach
//! another identity's stored DMs through `$JS.API.>`.

mod delivery_support;

use std::time::Duration;

use delivery_support::*;
use nats_hub::connect_opts::connect_with_hub_opts;
use nats_hub::{HubClient, HubConnectOptions};
use serde_json::json;

const HUB_PW: &str = "hub-pw-1";

fn opts(id: &str) -> HubConnectOptions {
    HubConnectOptions::default().with_user_password(id, format!("pw-{id}"))
}

/// Raw connection as `id` (reply inbox `_INBOX.<id>`).
async fn raw(url: &str, id: &str) -> async_nats::Client {
    connect_with_hub_opts(url, &opts(id).with_inbox_identity(id))
        .await
        .unwrap()
}

/// `true` when the request got any answer within a second. A permission
/// violation is never forwarded, so it shows up as no answer at all.
async fn answered(nc: &async_nats::Client, subject: &str, body: serde_json::Value) -> bool {
    let req = nc.request(
        subject.to_string(),
        serde_json::to_vec(&body).unwrap().into(),
    );
    matches!(
        tokio::time::timeout(Duration::from_secs(1), req).await,
        Ok(Ok(_))
    )
}

async fn render_stack() -> Option<DurableStack> {
    let dir = tempfile::tempdir().ok()?;
    let agents = dir.path().join("agents.txt");
    std::fs::write(
        &agents,
        "alice worker pw:pw-alice\nbob worker pw:pw-bob\ncarol worker pw:pw-carol\nprober worker pw:pw-prober\n",
    )
    .ok()?;
    let port = free_port();
    let conf = dir.path().join("nats.conf");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hub-admin"))
        .args(["render-config", "--agents", agents.to_str()?])
        .args(["--port", &port.to_string(), "--ws-port", "0"])
        .args(["--hub-password", HUB_PW, "--durable", "--js-store-dir"])
        .arg(dir.path().join("js"))
        .args(["--out", conf.to_str()?])
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stack = DurableStack::with_config(&conf, port).await?;
    std::mem::forget(dir); // keep the config + store for the server's lifetime
    Some(stack)
}

#[tokio::test]
async fn agents_read_only_their_own_stored_dms() {
    let _g = STACK_LOCK.lock().await;
    let Some(stack) = render_stack().await else {
        eprintln!("skipping: nats-server binary not found");
        return;
    };
    let _hub = stack.hub_env(
        &["--durable", "--require-bound-identity"],
        &[("NATS_USER", "hub-server"), ("NATS_PASSWORD", HUB_PW)],
    );
    wait_routing_as(&stack.url, "prober", opts("prober")).await;

    // bob attaches once (hub-server creates inbox-bob), then goes offline.
    {
        let bob = HubClient::connect_with_opts(&stack.url, "bob", opts("bob"))
            .await
            .unwrap();
        std::env::set_var("NATS_HUB_DURABLE_INBOX", "require");
        let _rx = bob.subscribe_inbox().await.expect("bob attaches");
        bob.drain().await.unwrap();
    }
    let alice = HubClient::connect_with_opts(&stack.url, "alice", opts("alice"))
        .await
        .unwrap();
    let secret = alice
        .send_to("bob", "chat", json!({"secret": "for bob"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let carol = raw(&stack.url, "carol").await;
    // Positive control: carol may look up her own consumer once attached.
    assert!(answered(&carol, "hub.delivery.carol.inbox", json!({})).await);
    assert!(
        answered(
            &carol,
            "$JS.API.CONSUMER.INFO.HUB_INBOX.inbox-carol",
            json!({})
        )
        .await,
        "carol can read her own consumer info"
    );
    // Everything that would reach bob's stored DMs is refused.
    for (subject, body) in [
        ("hub.delivery.bob.inbox", json!({})),
        ("$JS.API.CONSUMER.INFO.HUB_INBOX.inbox-bob", json!({})),
        (
            "$JS.API.CONSUMER.MSG.NEXT.HUB_INBOX.inbox-bob",
            json!({"batch": 1, "expires": 500_000_000}),
        ),
        (
            "$JS.API.CONSUMER.CREATE.HUB_INBOX.inbox-carol.channel.inbox.bob",
            json!({"stream_name": "HUB_INBOX", "config": {"name": "inbox-carol",
                   "filter_subject": "channel.inbox.bob", "ack_policy": "explicit"}}),
        ),
        (
            "$JS.API.CONSUMER.CREATE.HUB_INBOX.inbox-carol.channel.inbox.carol",
            json!({"stream_name": "HUB_INBOX", "config": {"name": "inbox-carol",
                   "filter_subject": "channel.inbox.carol",
                   "deliver_subject": "channel.inbox.bob"}}),
        ),
        (
            "$JS.API.CONSUMER.DURABLE.CREATE.HUB_INBOX.x",
            json!({"stream_name": "HUB_INBOX", "config": {"durable_name": "x"}}),
        ),
        ("$JS.API.STREAM.MSG.GET.HUB_INBOX", json!({"seq": 1})),
        ("$JS.API.DIRECT.GET.HUB_INBOX", json!({"seq": 1})),
        ("$JS.API.STREAM.INFO.HUB_INBOX", json!({})),
        ("$JS.API.STREAM.PURGE.HUB_INBOX", json!({})),
        ("$JS.API.CONSUMER.DELETE.HUB_INBOX.inbox-bob", json!({})),
    ] {
        assert!(
            !answered(&carol, subject, body).await,
            "carol must not be able to use {subject}"
        );
    }

    // bob reconnects and gets the DM that waited for him.
    let bob = HubClient::connect_with_opts(&stack.url, "bob", opts("bob"))
        .await
        .unwrap();
    let mut inbox = bob.subscribe_inbox().await.unwrap();
    let got = collect(&mut inbox, 1, Duration::from_secs(10)).await;
    std::env::remove_var("NATS_HUB_DURABLE_INBOX");
    assert_eq!(got.first().map(|e| e.meta.id.clone()), Some(secret));
    assert_eq!(got[0].meta.from, "alice");
}
