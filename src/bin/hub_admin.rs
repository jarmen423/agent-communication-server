//! hub-admin — mint per-agent NATS credentials + render nats-server configs.
//!
//! The hub pins identity to credentials at the NATS layer (contract §4.1):
//! every agent connects as a `users[]` entry whose publish permissions are
//! bound to its own identity — `hub.pub.<id>.>`, `hub.register.<id>`,
//! `hub.presence.<id>`, `hub.api.<id>.>` — so it can only ever send *as*
//! itself. Inboxes are private: `channel.inbox.<id>` is the only inbox a
//! user may subscribe. NATS ACLs cannot express "all channels except
//! inboxes" (deny beats allow), so task/session/wave channels are visible
//! to every agent and broadcast channels are granted per-agent with
//! `--channel`.
//!
//! Roles (a documentation marker — ACLs are the same for every agent;
//! real API-admin privilege lives in `hub-server --api-admin`):
//!   worker        an execution agent (echo-worker, claude-worker, …)
//!   orchestrator  delegates tasks / owns sessions & waves
//!   admin         an orchestrator the operator also lists via --api-admin
//!   service       hub-side helper (worker supervisor, ws bridge)
//!
//! Usage:
//!   hub-admin add-agent <id> [--role R] [--nkey] [--channel CH]...
//!   hub-admin render-config --agents FILE [--out PATH] [--port N]
//!                           [--ws-port N] [--tls-cert P --tls-key P --tls-ca P]
//!
//! The agents file is one agent per line:
//!   <id> <role> <secret> [ws-only] [extra subscribe channels...]
//! where <secret> is `pw:<password>`, `nkey:<public-key>`, or a bare
//! password, and the `ws-only` token restricts the credential to the
//! WebSocket listener. `#` comments and blank lines are ignored. The
//! rendered config also emits a `hub-server` service user.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use nats_hub::protocol::{require_valid_identity, valid_identity};
use nats_hub::query_api::authz::OP_NAMESPACES;

#[derive(Parser)]
#[command(name = "hub-admin", about = "Per-agent NATS credentials for nats-hub")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print a `users[]` entry for one agent.
    AddAgent(AddAgentArgs),
    /// Render a complete nats-server.conf from an agents file.
    RenderConfig(RenderConfigArgs),
}

#[derive(Args)]
struct AddAgentArgs {
    /// Agent identity (one NATS token: [A-Za-z0-9_-]+).
    identity: String,
    /// worker | orchestrator | admin | service
    #[arg(long, default_value = "worker")]
    role: String,
    /// Generate an nkey credential instead of a password.
    #[arg(long)]
    nkey: bool,
    /// Extra channel the agent may subscribe (repeatable), e.g.
    /// `--channel chat` allows `channel.chat`. Broadcast channels only.
    #[arg(long = "channel")]
    channels: Vec<String>,
    /// Restrict this credential to the WebSocket listener
    /// (allowed_connection_types: ["WEBSOCKET"]).
    #[arg(long)]
    ws_only: bool,
}

#[derive(Args)]
struct RenderConfigArgs {
    /// Agents file (see module docs for the line format).
    #[arg(long)]
    agents: String,
    /// Output path (default: stdout).
    #[arg(long)]
    out: Option<String>,
    /// NATS TCP port.
    #[arg(long, default_value_t = 4222)]
    port: u16,
    /// WebSocket port for remote agents (0 = no websocket listener).
    #[arg(long, default_value_t = 8080)]
    ws_port: u16,
    /// TLS cert/key/CA for the listeners (all-or-none).
    #[arg(long)]
    tls_cert: Option<String>,
    #[arg(long)]
    tls_key: Option<String>,
    #[arg(long)]
    tls_ca: Option<String>,
    /// Fixed password for the generated hub-server service user
    /// (default: a fresh random one is embedded in the config).
    #[arg(long)]
    hub_password: Option<String>,
}

/// Per-agent subscribe allowlist. `channels` are extra broadcast channels.
fn subscribe_allow(identity: &str, role: &str, channels: &[String]) -> Vec<String> {
    let mut allow = vec![
        format!("channel.inbox.{identity}"),
        "channel.task.>".into(),
        "channel.session.>".into(),
        "channel.wave.>".into(),
        "_INBOX.>".into(),
    ];
    if role == "service" {
        allow.extend([
            "hub.presence.*".into(),
            "hub.register.*".into(),
            "hub.worker.>".into(),
        ]);
    }
    for ch in channels {
        // accept both `chat` and `channel.chat`
        allow.push(format!(
            "channel.{}",
            ch.strip_prefix("channel.").unwrap_or(ch)
        ));
    }
    allow
}

/// Per-agent publish allowlist — every entry is bound to the identity.
fn publish_allow(identity: &str, role: &str) -> Vec<String> {
    let mut allow = vec![
        format!("hub.pub.{identity}.>"),
        format!("hub.register.{identity}"),
        format!("hub.presence.{identity}"),
        format!("hub.api.{identity}.>"),
        "_INBOX.>".into(), // request-reply (api calls, RPC responders)
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
fn user_entry(
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
    format!(
        r#"    # role: {role}
    {{ user: "{identity}", {cred},{conn_type}
      permissions: {{
        publish: {{ allow: [{}] }},
        subscribe: {{ allow: [{}] }},
      }} }},"#,
        quoted_list(&publish_allow(identity, role)),
        quoted_list(&subscribe_allow(identity, role, channels)),
    )
}

/// The hub-server's own service user (router + query API + ws bridge).
fn hub_server_entry(password: &str) -> String {
    format!(
        r#"    # role: hub-server (router, query API, ws bridge)
    {{ user: "hub-server", password: "{password}",
      permissions: {{
        publish: {{ allow: ["channel.>", "_INBOX.>", "hub.>"] }},
        subscribe: {{ allow: ["hub.>", "_INBOX.>"] }},
      }} }},"#
    )
}

fn gen_password() -> String {
    // 64 lowercase-hex chars (128 bits twice over) — no extra deps.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn cmd_add_agent(a: AddAgentArgs) -> Result<()> {
    require_valid_identity(&a.identity)?;
    if OP_NAMESPACES.contains(&a.identity.as_str()) {
        bail!(
            "identity '{}' collides with a query-API op namespace — bound API subjects \
             for it would be parsed as legacy ops; pick another name",
            a.identity
        );
    }
    if !["worker", "orchestrator", "admin", "service"].contains(&a.role.as_str()) {
        bail!("--role must be worker|orchestrator|admin|service");
    }
    for ch in &a.channels {
        if ch.starts_with("inbox.") || ch.starts_with("channel.inbox.") {
            bail!("--channel {ch:?} is an inbox — inboxes are per-identity and automatic");
        }
    }

    let (secret, note) = if a.nkey {
        let kp = nkeys::KeyPair::new_user();
        let seed = kp.seed().map_err(|e| anyhow::anyhow!("nkey seed: {e}"))?;
        let pubk = kp.public_key();
        (
            format!("nkey:{pubk}"),
            format!("# SEED (keep secret — this is the agent's credential): {seed}"),
        )
    } else {
        (gen_password(), "# password generated".to_string())
    };

    println!(
        "# agent: {}  role: {}   generated by hub-admin",
        a.identity, a.role
    );
    println!("{note}");
    println!(
        "{}",
        user_entry(&a.identity, &a.role, &secret, &a.channels, a.ws_only)
    );
    if a.role == "admin" {
        println!(
            "# admin: also pass --api-admin {} to hub-server (or NATS_HUB_API_ADMINS)",
            a.identity
        );
    }
    Ok(())
}

struct AgentRow {
    identity: String,
    role: String,
    secret: String,
    ws_only: bool,
    channels: Vec<String>,
}

fn parse_agents_file(text: &str) -> Result<Vec<AgentRow>> {
    let mut rows = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace().peekable();
        let (Some(id), Some(role), Some(secret)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!(
                "agents file line {}: expected '<id> <role> <secret> [ws-only] [channels...]'",
                n + 1
            );
        };
        if !valid_identity(id) {
            bail!("agents file line {}: invalid identity {id:?}", n + 1);
        }
        if OP_NAMESPACES.contains(&id) {
            bail!(
                "agents file line {}: identity {id:?} collides with an API op namespace",
                n + 1
            );
        }
        if !["worker", "orchestrator", "admin", "service"].contains(&role) {
            bail!("agents file line {}: unknown role {role:?}", n + 1);
        }
        // Optional flag token `ws-only` restricts the credential to the
        // WebSocket listener; every other trailing token is an extra
        // subscribe channel.
        let mut ws_only = false;
        let mut channels = Vec::new();
        for tok in fields {
            if tok == "ws-only" {
                ws_only = true;
            } else {
                channels.push(tok.to_string());
            }
        }
        rows.push(AgentRow {
            identity: id.to_string(),
            role: role.to_string(),
            secret: secret.to_string(),
            ws_only,
            channels,
        });
    }
    Ok(rows)
}

fn cmd_render_config(a: RenderConfigArgs) -> Result<()> {
    let text = std::fs::read_to_string(&a.agents)
        .with_context(|| format!("read agents file '{}'", a.agents))?;
    let agents = parse_agents_file(&text)?;
    let tls = match (&a.tls_cert, &a.tls_key, &a.tls_ca) {
        (None, None, None) => None,
        (Some(c), Some(k), ca) => Some((c.clone(), k.clone(), ca.clone())),
        _ => bail!("--tls-cert and --tls-key must be given together (--tls-ca optional)"),
    };
    let hub_pw = a.hub_password.clone().unwrap_or_else(gen_password);

    let mut out = String::new();
    out.push_str(&format!(
        "# nats-server.conf — generated by hub-admin render-config\n\
         # per-agent users with identity-bound permissions (contract §4.1)\n\
         # run hub-server with: --require-bound-identity --api-admin <admins>\n\n\
         server_name: nats-hub\nport: {}\n\n",
        a.port
    ));
    if a.ws_port != 0 {
        out.push_str("websocket {\n");
        out.push_str(&format!("  port: {}\n", a.ws_port));
        match &tls {
            Some((cert, key, ca)) => {
                out.push_str("  tls {\n");
                out.push_str(&format!("    cert_file: \"{cert}\"\n"));
                out.push_str(&format!("    key_file: \"{key}\"\n"));
                if let Some(ca) = ca {
                    out.push_str(&format!("    ca_file: \"{ca}\"\n"));
                    out.push_str("    verify: true\n");
                }
                out.push_str("  }\n");
            }
            None => out.push_str("  no_tls: true\n"),
        }
        out.push_str("}\n\n");
    }
    out.push_str("authorization {\n  users = [\n");
    out.push_str(&hub_server_entry(&hub_pw));
    out.push('\n');
    for agent in &agents {
        out.push_str(&user_entry(
            &agent.identity,
            &agent.role,
            &agent.secret,
            &agent.channels,
            agent.ws_only,
        ));
        out.push('\n');
    }
    out.push_str("  ]\n}\n\n# hub-server credential (NATS_USER/NATS_PASSWORD for the router):\n");
    out.push_str(&format!("#   user=hub-server password={hub_pw}\n"));

    match &a.out {
        Some(path) => {
            std::fs::write(path, &out).with_context(|| format!("write {path}"))?;
            eprintln!("wrote {path} ({} agents + hub-server user)", agents.len());
        }
        None => print!("{out}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::AddAgent(a) => cmd_add_agent(a),
        Cmd::RenderConfig(a) => cmd_render_config(a),
    }
}
