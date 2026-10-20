//! hub-admin — mint per-agent NATS credentials + render nats-server configs.
//!
//! The hub pins identity to credentials at the NATS layer (contract §4.1):
//! every agent connects as a `users[]` entry whose publish permissions are
//! bound to its own identity — `hub.pub.<id>.>`, `hub.register.<id>`,
//! `hub.presence.<id>`, `hub.api.<id>.>` — so it can only ever send *as*
//! itself. Inboxes are private: `channel.inbox.<id>` is the only inbox a
//! user may subscribe, and request-reply uses the per-identity prefix
//! `_INBOX.<id>` (contract C1) — a user may subscribe `_INBOX.<id>.>` only
//! and has no publish right on `_INBOX` at all. Users that answer requests
//! (role `service`, and the hub-server user) get `allow_responses: true`
//! instead. NATS ACLs cannot express "all channels except inboxes" (deny
//! beats allow), so task/session/wave channels are visible to every agent
//! and broadcast channels are granted per-agent with `--channel`.
//!
//! Roles (real API-admin privilege lives in `hub-server --api-admin`):
//!   worker        an execution agent (echo-worker, claude-worker, …)
//!   orchestrator  delegates tasks / owns sessions & waves; may also send
//!                 worker-supervisor requests (`hub.worker.>`)
//!   admin         an orchestrator the operator also lists via --api-admin
//!   service       hub-side helper that answers requests (worker supervisor):
//!                 `hub.worker.>`, presence/register feeds, allow_responses
//!
//! Reserved identities (`hub-server`, `hub-orchestrator`, `visualizer`) and
//! identities that collide with a query-API op namespace are refused.
//!
//! Usage:
//!   hub-admin add-agent <id> [--role R] [--nkey] [--channel CH]...
//!   hub-admin render-config --agents FILE [--out PATH] [--port N]
//!                           [--ws-port N] [--tls-cert P --tls-key P --tls-ca P]
//!                           [--durable [--js-store-dir D] [--js-max-file 10G]]
//!
//! Durable delivery: every agent may also attach to and read its own
//! JetStream inbox consumer and `DIRECT.GET` the tasks stream — never create
//! consumers or read another identity's stored DMs (docs/DELIVERY.md).
//!
//! The agents file is one agent per line:
//!   <id> <role> <secret> [ws-only] [extra subscribe channels...]
//! where <secret> is `pw:<password>`, `nkey:<public-key>`, or a bare
//! password, and the `ws-only` token restricts the credential to the
//! WebSocket listener. `#` comments and blank lines are ignored. The
//! rendered config also emits a `hub-server` service user.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

#[path = "hub_admin/acl.rs"]
mod acl;
use acl::{
    check_channel, check_identity, check_secret, hub_server_entry, user_entry, JsNames, ROLES,
};

/// Stream names used in the durable-delivery grants (match hub-server's
/// `--delivery-config`).
#[derive(Args, Clone)]
struct JsArgs {
    /// Inbox stream name (hub-server `inbox.name`).
    #[arg(long, default_value = nats_hub::delivery::config::DEFAULT_INBOX_STREAM)]
    inbox_stream: String,
    /// Tasks stream name (hub-server `tasks.name`).
    #[arg(long, default_value = nats_hub::delivery::config::DEFAULT_TASKS_STREAM)]
    tasks_stream: String,
}

impl JsArgs {
    fn names(&self) -> Result<JsNames> {
        JsNames::new(&self.inbox_stream, &self.tasks_stream)
    }
}

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
    #[command(flatten)]
    js: JsArgs,
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
    /// Enable JetStream for `hub-server --durable` (adds a `jetstream {}`
    /// block; see docs/DELIVERY.md).
    #[arg(long)]
    durable: bool,
    /// JetStream store directory (with --durable).
    #[arg(
        long,
        default_value = "/var/lib/nats-hub/jetstream",
        requires = "durable"
    )]
    js_store_dir: String,
    /// JetStream file-storage cap, e.g. `10G` (with --durable).
    #[arg(long, default_value = "10G", requires = "durable")]
    js_max_file: String,
    /// JetStream memory-storage cap, e.g. `256M` (with --durable).
    #[arg(long, default_value = "256M", requires = "durable")]
    js_max_mem: String,
    #[command(flatten)]
    js: JsArgs,
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
    check_identity(&a.identity)?;
    if !ROLES.contains(&a.role.as_str()) {
        bail!("--role must be worker|orchestrator|admin|service");
    }
    let channels = a
        .channels
        .iter()
        .map(|ch| check_channel(ch))
        .collect::<Result<Vec<_>>>()?;

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
        user_entry(
            &a.identity,
            &a.role,
            &secret,
            &channels,
            a.ws_only,
            &a.js.names()?
        )
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
    let mut rows: Vec<AgentRow> = Vec::new();
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
        let line_no = n + 1;
        check_identity(id).with_context(|| format!("agents file line {line_no}"))?;
        if rows.iter().any(|r| r.identity == id) {
            bail!("agents file line {line_no}: duplicate identity {id:?}");
        }
        if !ROLES.contains(&role) {
            bail!("agents file line {line_no}: unknown role {role:?}");
        }
        check_secret(secret.strip_prefix("pw:").unwrap_or(secret))
            .with_context(|| format!("agents file line {line_no}"))?;
        // Optional flag token `ws-only` restricts the credential to the
        // WebSocket listener; every other trailing token is an extra
        // subscribe channel.
        let mut ws_only = false;
        let mut channels = Vec::new();
        for tok in fields {
            if tok == "ws-only" {
                ws_only = true;
            } else {
                channels.push(
                    check_channel(tok).with_context(|| format!("agents file line {line_no}"))?,
                );
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
    check_secret(&hub_pw).context("--hub-password")?;
    let js = a.js.names()?;
    for v in [&a.js_store_dir, &a.js_max_file, &a.js_max_mem] {
        check_secret(v).context("--js-* value")?; // no quotes/newlines
    }

    let mut out = String::new();
    out.push_str(&format!(
        "# nats-server.conf — generated by hub-admin render-config\n\
         # per-agent users with identity-bound permissions (contract §4.1)\n\
         # run hub-server with: --require-bound-identity --api-admin <admins>\n\n\
         server_name: nats-hub\nport: {}\n\n",
        a.port
    ));
    if a.durable {
        out.push_str(&format!(
            "# durable delivery (hub-server --durable; docs/DELIVERY.md)\n\
             jetstream {{\n  store_dir: \"{}\"\n  max_file_store: {}\n  max_memory_store: {}\n}}\n\n",
            a.js_store_dir, a.js_max_file, a.js_max_mem
        ));
    }
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
            &js,
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
