//! `hub-admin` hardening (H1 #1, #3, #8): per-identity reply inboxes,
//! responders via `allow_responses`, the hub-server user's subscriptions,
//! reserved identities and extra-channel validation. Runs the real binary;
//! when `.tools/bin/nats-server` exists the rendered config is also
//! validated with `nats-server -t`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn hub_admin(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hub-admin"))
        .args(args)
        .output()
        .expect("run hub-admin")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn tmp_file(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hub-admin-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn render(agents: &str) -> Output {
    let path = tmp_file("agents.txt", agents);
    hub_admin(&[
        "render-config",
        "--agents",
        path.to_str().unwrap(),
        "--ws-port",
        "0",
        "--hub-password",
        "pw-hub",
    ])
}

/// The `{ user: "<id>", … }` block of a rendered config.
fn user_block<'a>(conf: &'a str, id: &str) -> &'a str {
    let start = conf
        .find(&format!("user: \"{id}\""))
        .unwrap_or_else(|| panic!("no user {id} in:\n{conf}"));
    let end = conf[start..].find("} },").map_or(conf.len(), |e| start + e);
    &conf[start..end]
}

#[test]
fn rendered_permissions_use_per_identity_reply_inboxes() {
    let out = render("alice worker pw:pw-a chat\nsup service pw:pw-s\nboss admin pw:pw-b\n");
    assert!(out.status.success(), "{}", stderr(&out));
    let conf = String::from_utf8(out.stdout).unwrap();

    assert!(
        !conf.contains("\"_INBOX.>\""),
        "shared _INBOX.> grant left:\n{conf}"
    );
    let alice = user_block(&conf, "alice");
    assert!(alice.contains("\"_INBOX.alice.>\""), "{alice}");
    assert!(alice.contains("\"channel.chat\""), "{alice}");
    assert!(
        !alice.contains("allow_responses"),
        "workers answer no requests: {alice}"
    );
    assert!(
        !alice.contains("hub.worker"),
        "workers can't drive the supervisor: {alice}"
    );
    assert!(user_block(&conf, "boss").contains("\"hub.worker.>\""));
    let sup = user_block(&conf, "sup");
    assert!(
        sup.contains("\"_INBOX.sup.>\"") && sup.contains("allow_responses: true"),
        "{sup}"
    );

    // hub-server: may follow channel.wave.> (orchestrator), replies via
    // allow_responses, own inbox only.
    let hub = user_block(&conf, "hub-server");
    for needle in [
        "\"channel.>\"",
        "\"_INBOX.hub-server.>\"",
        "allow_responses: true",
    ] {
        assert!(hub.contains(needle), "hub-server lacks {needle}: {hub}");
    }

    if let Some(nats) = nats_server() {
        let conf_path = tmp_file("nats.conf", &conf);
        let t = Command::new(nats)
            .args(["-t", "-c", conf_path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(t.status.success(), "nats-server -t: {}", stderr(&t));
    }
}

#[test]
fn reserved_identities_are_refused() {
    for id in [
        "hub-server",
        "hub-orchestrator",
        "visualizer",
        "wave",
        "ping",
    ] {
        let out = hub_admin(&["add-agent", id]);
        assert!(!out.status.success(), "add-agent {id} must fail");
        let out = render(&format!("{id} worker pw:x\n"));
        assert!(!out.status.success(), "render-config with {id} must fail");
    }
    let out = render("alice worker pw:a\nalice worker pw:b\n");
    assert!(!out.status.success() && stderr(&out).contains("duplicate"));
}

#[test]
fn bad_extra_channels_are_refused() {
    for ch in [
        "*",
        ">",
        "chat.*",
        "chat.>",
        "a..b",
        ".chat",
        "chat.",
        "inbox",
        "inbox.bob",
        "channel.inbox.bob",
        "x\"y",
    ] {
        let out = hub_admin(&["add-agent", "alice", "--channel", ch]);
        assert!(
            !out.status.success(),
            "add-agent --channel {ch:?} must fail"
        );
        let out = render(&format!("alice worker pw:x {ch}\n"));
        assert!(
            !out.status.success(),
            "render-config channel {ch:?} must fail"
        );
    }
    let out = hub_admin(&["add-agent", "alice", "--channel", "channel.agents.team-1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"channel.agents.team-1\""));
}

#[test]
fn secrets_that_break_the_config_are_refused() {
    for secret in ["pw:a\"b", "pw:a\\b", "pw:"] {
        let out = render(&format!("alice worker {secret}\n"));
        assert!(!out.status.success(), "secret {secret:?} must fail");
    }
}

fn nats_server() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tools/bin/nats-server");
    p.exists().then_some(p)
}
