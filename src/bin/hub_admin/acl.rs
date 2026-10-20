//! NATS permission blocks rendered by `hub-admin` (contract §4.1 + C1) and
//! the input checks that keep them tight.

use anyhow::{bail, Result};

use nats_hub::protocol::{reserved_identity, valid_identity};
use nats_hub::query_api::authz::OP_NAMESPACES;

pub(crate) const ROLES: &[&str] = &["worker", "orchestrator", "admin", "service"];

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

/// Per-agent publish allowlist — every entry is bound to the identity.
fn publish_allow(identity: &str, role: &str) -> Vec<String> {
    let mut allow = vec![
        format!("hub.pub.{identity}.>"),
        format!("hub.register.{identity}"),
        format!("hub.presence.{identity}"),
        format!("hub.api.{identity}.>"),
    ];
    if role == "service" {
        allow.push("hub.worker.>".into());
    }
    allow
}

fn quoted_list(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
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
    format!(
        r#"    # role: {role}
    {{ user: "{identity}", {cred},{conn_type}
      permissions: {{
        publish: {{ allow: [{}] }},
        subscribe: {{ allow: [{}] }},{responses}
      }} }},"#,
        quoted_list(&publish_allow(identity, role)),
        quoted_list(&subscribe_allow(identity, role, channels)),
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
pub(crate) fn hub_server_entry(password: &str) -> String {
    format!(
        r#"    # role: hub-server (router, query API, wave orchestrator, ws bridge)
    {{ user: "hub-server", password: "{password}",
      permissions: {{
        publish: {{ allow: ["channel.>", "hub.>"] }},
        subscribe: {{ allow: ["hub.>", "channel.>", "_INBOX.hub-server.>"] }},
        allow_responses: true,
      }} }},"#
    )
}
