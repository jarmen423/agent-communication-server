//! NATS permission blocks rendered by `hub-admin` (contract §4.1 + C1) and
//! the input checks that keep them tight.

use anyhow::{bail, Result};

use nats_hub::protocol::{reserved_identity, valid_identity};
use nats_hub::query_api::authz::OP_NAMESPACES;

pub(crate) const ROLES: &[&str] = &["worker", "orchestrator", "admin", "service"];

/// Stream names the durable-delivery grants refer to (must match
/// hub-server's `--delivery-config`; defaults match its defaults).
#[derive(Clone, Debug)]
pub(crate) struct JsNames {
    pub inbox: String,
    pub tasks: String,
}

impl Default for JsNames {
    fn default() -> Self {
        Self {
            inbox: nats_hub::delivery::config::DEFAULT_INBOX_STREAM.into(),
            tasks: nats_hub::delivery::config::DEFAULT_TASKS_STREAM.into(),
        }
    }
}

impl JsNames {
    pub(crate) fn new(inbox: &str, tasks: &str) -> Result<Self> {
        for n in [inbox, tasks] {
            if !nats_hub::delivery::config::valid_name(n) {
                bail!("invalid stream name {n:?}: use [A-Za-z0-9_-]+");
            }
        }
        Ok(Self {
            inbox: inbox.into(),
            tasks: tasks.into(),
        })
    }
}

/// Durable-delivery grants for one agent (docs/DELIVERY.md → Permissions).
/// Agents never create JetStream objects: hub-server creates the inbox
/// consumer on `hub.delivery.<id>.inbox`. The agent may then only look up,
/// pull from and ack **its own** consumer, and read the tasks stream by
/// sequence (`DIRECT.GET`; the same visibility `channel.task.>` already
/// gives). No `CONSUMER.CREATE`, no `STREAM.MSG.GET` on the inbox stream,
/// no other identity's consumer. All harmless on a hub without JetStream
/// (nobody answers), so they are always rendered.
fn delivery_publish_allow(identity: &str, js: &JsNames) -> Vec<String> {
    let consumer = nats_hub::delivery::inbox_consumer_name(identity);
    vec![
        format!("hub.delivery.{identity}.>"),
        format!("$JS.API.CONSUMER.INFO.{}.{consumer}", js.inbox),
        format!("$JS.API.CONSUMER.MSG.NEXT.{}.{consumer}", js.inbox),
        format!("$JS.ACK.{}.{consumer}.>", js.inbox),
        format!("$JS.API.DIRECT.GET.{}", js.tasks),
    ]
}

/// Refuse identities an agent credential must never carry: invalid tokens,
/// API op namespaces (bound API subjects would parse as legacy ops) and the
/// hub's own component identities.
pub(crate) fn check_identity(identity: &str) -> Result<()> {
    if !valid_identity(identity) {
        bail!("invalid identity {identity:?}: must be one or more of [A-Za-z0-9_-]");
    }
    if OP_NAMESPACES.contains(&identity) {
        bail!(
            "identity {identity:?} collides with a query-API op namespace — bound API \
             subjects for it would be parsed as legacy ops; pick another name"
        );
    }
    if reserved_identity(identity) {
        bail!("identity {identity:?} is reserved for a hub component; pick another name");
    }
    Ok(())
}

/// Normalize an extra broadcast channel (`chat` or `channel.chat`) and
/// refuse anything that would widen the ACL: wildcards, empty tokens,
/// characters outside `[A-Za-z0-9_-]` per token, or any overlap with the
/// per-identity inboxes (`inbox`, `inbox.<id>`).
pub(crate) fn check_channel(raw: &str) -> Result<String> {
    let ch = raw.strip_prefix("channel.").unwrap_or(raw);
    if ch.split('.').any(|tok| !valid_identity(tok)) {
        bail!(
            "channel {raw:?} is invalid: every token must be [A-Za-z0-9_-]+ \
             (no '*', '>', empty tokens or quotes)"
        );
    }
    if ch.split('.').next() == Some("inbox") {
        bail!("channel {raw:?} overlaps the inboxes — inboxes are per-identity and automatic");
    }
    Ok(ch.to_string())
}

/// Refuse secrets that would break out of the quoted config string.
pub(crate) fn check_secret(secret: &str) -> Result<()> {
    if secret.is_empty() || secret.contains(['"', '\\', '\n', '\r']) {
        bail!("secret must be non-empty and contain no quotes, backslashes or newlines");
    }
    Ok(())
}

/// Per-agent subscribe allowlist. `channels` are extra broadcast channels
/// already normalized by [`check_channel`].
fn subscribe_allow(identity: &str, role: &str, channels: &[String]) -> Vec<String> {
    let mut allow = vec![
        format!("channel.inbox.{identity}"),
        "channel.task.>".into(),
        "channel.session.>".into(),
        "channel.wave.>".into(),
        format!("_INBOX.{identity}.>"), // own request-reply inbox only (C1)
    ];
    if role == "service" {
        allow.extend([
            "hub.presence.*".into(),
            "hub.register.*".into(),
            "hub.worker.>".into(),
        ]);
    }
    allow.extend(channels.iter().map(|ch| format!("channel.{ch}")));
    allow
}

/// Per-agent publish allowlist — every entry is bound to the identity,
/// except `hub.worker.>` (worker-supervisor requests: list/models/ensure/
/// stop), which services and orchestrating roles (orchestrator, admin) may
/// call; plain workers may not.
fn publish_allow(identity: &str, role: &str, js: &JsNames) -> Vec<String> {
    let mut allow = vec![
        format!("hub.pub.{identity}.>"),
        format!("hub.register.{identity}"),
        format!("hub.presence.{identity}"),
        format!("hub.api.{identity}.>"),
    ];
    if role != "worker" {
        allow.push("hub.worker.>".into());
    }
    allow.extend(delivery_publish_allow(identity, js));
    allow
}

/// Shared config-mode and scoped-signing-key permission policy.
pub(crate) fn permissions(
    identity: &str,
    role: &str,
    channels: &[String],
    js: &JsNames,
) -> serde_json::Value {
    let (publish, subscribe) = if role == "hub-server" {
        (
            vec![
                "channel.>".into(),
                "hub.>".into(),
                "$JS.API.>".into(),
                "$JS.ACK.>".into(),
            ],
            vec![
                "hub.>".into(),
                "channel.>".into(),
                format!("_INBOX.{identity}.>"),
            ],
        )
    } else {
        (
            publish_allow(identity, role, js),
            subscribe_allow(identity, role, channels),
        )
    };
    let mut policy = serde_json::json!({"pub": {"allow": publish}, "sub": {"allow": subscribe}});
    if matches!(role, "service" | "hub-server") {
        // Config allow_responses:true defaults: one response within two seconds.
        policy["resp"] = serde_json::json!({"max": 1, "ttl": 2_000_000_000u64});
    }
    policy
}

fn quoted_list(items: &serde_json::Value) -> String {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One rendered `users[]` entry.
pub(crate) fn user_entry(
    identity: &str,
    role: &str,
    secret: &str,
    channels: &[String],
    ws_only: bool,
    js: &JsNames,
) -> String {
    let cred = if let Some(pubkey) = secret.strip_prefix("nkey:") {
        format!("nkey: {pubkey}")
    } else {
        let pw = secret.strip_prefix("pw:").unwrap_or(secret);
        format!("password: \"{pw}\"")
    };
    let conn_type = if ws_only {
        "\n      allowed_connection_types: [\"WEBSOCKET\"],"
    } else {
        ""
    };
    // Responders reply to the request's reply subject without any
    // publish right on `_INBOX` (contract C1).
    let responses = if role == "service" {
        "\n        allow_responses: true,"
    } else {
        ""
    };
    let policy = permissions(identity, role, channels, js);
    format!(
        r#"    # role: {role}
    {{ user: "{identity}", {cred},{conn_type}
      permissions: {{
        publish: {{ allow: [{}] }},
        subscribe: {{ allow: [{}] }},{responses}
      }} }},"#,
        quoted_list(&policy["pub"]["allow"]),
        quoted_list(&policy["sub"]["allow"]),
    )
}

/// The hub-server's own service user: router, query API, wave orchestrator
/// and ws bridge all connect with it.
///
/// - publish `channel.>` (router delivery) and `hub.>` (orchestrator and
///   bridge sends on `hub.pub.<their-id>.>`, bridge → supervisor requests on
///   `hub.worker.*`); API replies go out via `allow_responses`.
/// - subscribe `hub.>` (router, query API, orchestrator heartbeats),
///   `channel.>` (the orchestrator follows `channel.wave.>`; the router already
///   sees every envelope on `hub.pub.*.>`, so this adds no visibility) and
///   its own reply inbox `_INBOX.hub-server.>` (every in-process client
///   connects as user `hub-server`, so they all share this prefix).
/// - publish `$JS.API.>` / `$JS.ACK.>`: durable delivery (`--durable`)
///   creates the streams and consumers and acks ingress; harmless without
///   JetStream.
pub(crate) fn hub_server_entry(password: &str) -> String {
    let policy = permissions("hub-server", "hub-server", &[], &JsNames::default());
    let publish = quoted_list(&policy["pub"]["allow"]);
    let subscribe = quoted_list(&policy["sub"]["allow"]);
    format!(
        r#"    # role: hub-server (router, query API, wave orchestrator, ws bridge)
    {{ user: "hub-server", password: "{password}",
      permissions: {{
        publish: {{ allow: [{publish}] }},
        subscribe: {{ allow: [{subscribe}] }},
        allow_responses: true,
      }} }},"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn templates_reproduce_config_mode_allowlists() {
        for identity in ["bob", "agent_2", "hub-server"] {
            for role in ["worker", "orchestrator", "admin", "service", "hub-server"] {
                let template =
                    permissions("{{name()}}", role, &["chat".into()], &JsNames::default());
                let expanded: serde_json::Value =
                    serde_json::from_str(&template.to_string().replace("{{name()}}", identity))
                        .unwrap();
                // Frozen pre-H0 config-mode policy: catches simultaneous
                // broadening of the config renderer and JWT templates.
                let expected = if role == "hub-server" {
                    json!({"pub": {"allow": ["channel.>", "hub.>", "$JS.API.>", "$JS.ACK.>"]},
                        "sub": {"allow": ["hub.>", "channel.>", format!("_INBOX.{identity}.>")]},
                        "resp": {"max": 1, "ttl": 2_000_000_000u64}})
                } else {
                    let mut publish = vec![
                        format!("hub.pub.{identity}.>"),
                        format!("hub.register.{identity}"),
                        format!("hub.presence.{identity}"),
                        format!("hub.api.{identity}.>"),
                    ];
                    if role != "worker" {
                        publish.push("hub.worker.>".into());
                    }
                    publish.extend([
                        format!("hub.delivery.{identity}.>"),
                        format!("$JS.API.CONSUMER.INFO.HUB_INBOX.inbox-{identity}"),
                        format!("$JS.API.CONSUMER.MSG.NEXT.HUB_INBOX.inbox-{identity}"),
                        format!("$JS.ACK.HUB_INBOX.inbox-{identity}.>"),
                        "$JS.API.DIRECT.GET.HUB_TASKS".into(),
                    ]);
                    let mut subscribe = vec![
                        format!("channel.inbox.{identity}"),
                        "channel.task.>".into(),
                        "channel.session.>".into(),
                        "channel.wave.>".into(),
                        format!("_INBOX.{identity}.>"),
                    ];
                    if role == "service" {
                        subscribe.extend([
                            "hub.presence.*".into(),
                            "hub.register.*".into(),
                            "hub.worker.>".into(),
                        ]);
                    }
                    subscribe.push("channel.chat".into());
                    let mut expected =
                        json!({"pub": {"allow": publish}, "sub": {"allow": subscribe}});
                    if role == "service" {
                        expected["resp"] = json!({"max": 1, "ttl": 2_000_000_000u64});
                    }
                    expected
                };
                assert_eq!(expanded, expected, "{role}/{identity}");
                assert_eq!(
                    expanded,
                    permissions(identity, role, &["chat".into()], &JsNames::default())
                );
            }
        }
    }
}
