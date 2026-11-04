mod agents;
mod api;
mod daemon;
mod exec;
mod install;
mod local;
mod migration;
mod runtime;
mod tenants;
mod worker;

use anyhow::Result;
use clap::{Parser, Subcommand};
use reqwest::Method;
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "ACS device login, credential manager and agent connector"
)]
struct Cli {
    #[arg(long, global = true)]
    tenant: Option<String>,
    #[command(subcommand)]
    command: Command,
}

/// Hosted ACS. Self-host and dev pass `--server` or set `ACS_SERVER`.
const HOSTED_SERVER: &str = "https://acs.agentmemorylabs.com";

#[derive(Subcommand)]
enum Command {
    /// Sign this device in (opens the browser; --headless prints the URL)
    Login {
        /// Control plane; self-host/dev: e.g. http://127.0.0.1:9300
        #[arg(long, env = "ACS_SERVER", default_value = HOSTED_SERVER)]
        server: String,
        #[arg(long)]
        headless: bool,
        #[arg(long, env = "ACS_DEVICE_NAME")]
        device_name: Option<String>,
        #[arg(long, env = "ACS_SERVICE_KEY", hide_env_values = true)]
        key: Option<String>,
    },
    /// Revoke this device and remove its local state
    Logout,
    /// List available tenants or select the default
    Tenant {
        #[command(subcommand)]
        command: TenantCommand,
    },
    /// Show the device, its identities and their credential expiry
    Status,
    /// Print `export` lines for one identity: eval "$(acs env <id>)"
    Env { identity: String },
    /// Run a command as one identity: acs exec <id> -- hub-delegate …
    Exec {
        identity: String,
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Add or remove an identity on this device
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Install and start the credential daemon as a user service
    Up {
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the credential daemon
    Down,
    /// Connect an MCP client (Claude Code, Codex, Hermes) as one identity
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Run a worker as one identity
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
}

#[derive(Subcommand)]
enum TenantCommand {
    List,
    Use { tenant: String },
}

#[derive(Subcommand)]
enum AgentCommand {
    Add {
        id: String,
        #[arg(long, default_value = "orchestrator", value_parser = ["worker", "orchestrator"])]
        role: String,
    },
    Rm {
        id: String,
    },
}
#[derive(Subcommand)]
enum McpCommand {
    Install {
        #[arg(value_parser = ["claude", "codex", "hermes"])]
        client: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long, env = "ACS_MCP_SERVER")]
        server: Option<PathBuf>,
        #[arg(long, env = "ACS_PYTHON")]
        python: Option<PathBuf>,
    },
}
#[derive(Subcommand)]
enum WorkerCommand {
    /// acs worker run claude --identity <id> -- --repo <path>
    Run {
        #[arg(value_parser = clap::builder::PossibleValuesParser::new(worker::BACKENDS))]
        backend: String,
        #[arg(long)]
        identity: String,
        #[arg(last = true)]
        args: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let selected = cli
        .tenant
        .clone()
        .or_else(|| std::env::var("ACS_TENANT").ok());
    let root = local::root()?;
    migration::run(&root).await?;
    let scoped = matches!(
        cli.command,
        Command::Env { .. }
            | Command::Exec { .. }
            | Command::Worker { .. }
            | Command::Agent { .. }
            | Command::Mcp { .. }
    );
    let device_root = root.clone();
    let root = if scoped {
        tenants::resolve(
            &root,
            selected.as_deref(),
            matches!(
                cli.command,
                Command::Agent {
                    command: AgentCommand::Rm { .. }
                }
            ),
        )
        .await?
    } else {
        root
    };
    if matches!(cli.command, Command::Up { .. } | Command::Down) {
        if let Some(t) = cli.tenant.as_deref() {
            tenants::select(&root, Some(t))?;
        }
    } else if matches!(cli.command, Command::Status) && selected.is_some() {
        tenants::select(&root, selected.as_deref())?;
    }
    match cli.command {
        Command::Up { foreground: true } => daemon::foreground(&root).await?,
        Command::Up { foreground: false } => daemon::up(&root).await?,
        Command::Down => daemon::down(&root).await?,
        Command::Worker {
            command:
                WorkerCommand::Run {
                    backend,
                    identity,
                    args,
                },
        } => worker::run(&root, &backend, &identity, &args).await?,
        Command::Env { identity } => exec::print(&root, &identity)?,
        Command::Exec { identity, command } => exec::run(&root, &identity, &command).await?,
        command => {
            let _lock = local::lock(&device_root, "operation.lock")?;
            match command {
                Command::Login {
                    server,
                    headless,
                    device_name,
                    key,
                } => api::login(&root, &server, headless, key, device_name).await?,
                Command::Tenant { command } => {
                    tenants::command(
                        &root,
                        match &command {
                            TenantCommand::List => None,
                            TenantCommand::Use { tenant } => Some(tenant),
                        },
                    )
                    .await?
                }
                Command::Logout => {
                    daemon::down(&root).await?;
                    if !root.join("logout.revoked").exists() {
                        let mut state = tenants::device_state(&root)?;
                        let api = api::authorized(&root, &mut state).await?;
                        api.request(
                            Method::DELETE,
                            "/v1/device",
                            Some(&state.access_token),
                            json!({}),
                        )
                        .await?;
                        // If a locked keychain prevents cleanup, the next logout
                        // retries local removal without needing the revoked token.
                        local::atomic_private(&root.join("logout.revoked"), b"revoked")?;
                    }
                    local::clear(&root)?;
                    println!("Logged out; device revoked and local state removed");
                }
                Command::Status => {
                    for tenant in tenants::names(&root)? {
                        if selected.as_ref().is_some_and(|t| t != &tenant) {
                            continue;
                        }
                        let s = local::load(&tenants::path(&root, &tenant)?)?;
                        println!(
                            "Server: {}\nTenant: {}\nDevice: {}\nNATS: {}\nDaemon: {}",
                            s.server,
                            s.tenant,
                            s.device,
                            s.nats_url,
                            if daemon::running(&root) {
                                "running"
                            } else {
                                "stopped"
                            }
                        );
                        for (id, a) in s.agents {
                            println!(
                                "{id}: {} credential {}",
                                a.role,
                                local::expiry(a.expires_at)
                            );
                        }
                    }
                }
                Command::Agent {
                    command: AgentCommand::Add { id, role },
                } => agents::add(&root, &id, &role).await?,
                Command::Agent {
                    command: AgentCommand::Rm { id },
                } => agents::remove(&root, &id).await?,
                Command::Mcp {
                    command:
                        McpCommand::Install {
                            client,
                            identity,
                            home,
                            server,
                            python,
                        },
                } => {
                    install::mcp(&root, &client, identity.as_deref(), home, server, python).await?;
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}
