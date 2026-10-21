//! hub-lock — advisory file locks across agents (docs/FILE_LOCKS.md).
//!
//! Announce "I'm working on these paths" in a scope (a repo or project
//! key); others see who holds what and can wait until paths are free.
//! This is coordination, not filesystem locking.
//!
//! Usage:
//!   hub-lock acquire --scope myrepo --path src/foo.rs --path src/bar/ [--ttl-secs 900] [--reason "..."] [--from me]
//!   hub-lock renew --lock-id <id> [--ttl-secs 900]
//!   hub-lock release --lock-id <id> [--force]
//!   hub-lock list --scope myrepo        (or --all for every scope)
//!   hub-lock get --lock-id <id>
//!   hub-lock wait --scope myrepo --path src/foo.rs [--timeout-secs 300]
//!
//! The holder is always the caller's bound identity (--from or
//! NATS_HUB_IDENTITY). On a permissive hub a legacy caller may still pass
//! --from (it is sent as the self-asserted holder).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nats_hub_client::locks::{wait_for_locks, AcquireOptions, WaitOutcome};
use nats_hub_client::{ApiClient, HubClient};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "hub-lock", about = "Advisory file locks across agents")]
struct Args {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,

    /// Caller identity — the lock holder. Falls back to
    /// NATS_HUB_IDENTITY; on a bound hub this is the only source of the
    /// holder identity (the payload is never trusted).
    #[arg(long, global = true)]
    from: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Acquire a lock on paths in a scope (all-or-nothing).
    Acquire {
        #[arg(long)]
        scope: String,
        /// Repo-relative file or directory prefix (trailing /). Repeatable.
        #[arg(long = "path", required = true)]
        paths: Vec<String>,
        #[arg(long)]
        ttl_secs: Option<u64>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        task_id: Option<String>,
    },
    /// Extend a held lock's expiry.
    Renew {
        #[arg(long)]
        lock_id: String,
        #[arg(long)]
        ttl_secs: Option<u64>,
    },
    /// Release a held lock (--force: admin release of anyone's lock).
    Release {
        #[arg(long)]
        lock_id: String,
        #[arg(long)]
        force: bool,
    },
    /// List live locks in a scope (or everywhere with --all).
    List {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show one lock.
    Get {
        #[arg(long)]
        lock_id: String,
    },
    /// Block until no other holder locks these paths (or timeout).
    Wait {
        #[arg(long)]
        scope: String,
        #[arg(long = "path", required = true)]
        paths: Vec<String>,
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
        #[arg(long, default_value_t = 2.0)]
        poll_secs: f64,
    },
}

async fn api(nats_url: &str, from: Option<&str>) -> Result<ApiClient> {
    match from {
        Some(id) => ApiClient::connect_with_identity(nats_url, id).await,
        None => ApiClient::connect(nats_url).await,
    }
}

fn print_lock(l: &nats_hub_client::LockRecord) {
    println!(
        "{}  {}  paths=[{}]  expires {}  ttl {}s{}{}",
        l.lock_id,
        l.holder,
        l.paths.join(", "),
        l.expires_at.format("%H:%M:%SZ"),
        l.ttl_secs,
        l.reason
            .as_deref()
            .map(|r| format!("  reason \"{r}\""))
            .unwrap_or_default(),
        l.task_id
            .as_deref()
            .map(|t| format!("  task {t}"))
            .unwrap_or_default(),
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let from = args.from.as_deref();

    match &args.command {
        Command::Acquire {
            scope,
            paths,
            ttl_secs,
            reason,
            task_id,
        } => {
            let api = api(&args.nats_url, from).await?;
            let out = api
                .lock_acquire(
                    scope,
                    paths,
                    AcquireOptions {
                        ttl_secs: *ttl_secs,
                        reason: reason.clone(),
                        task_id: task_id.clone(),
                        holder: from.map(str::to_string),
                    },
                )
                .await?;
            if out.granted {
                let lock = out.lock.context("granted without lock")?;
                println!("ACQUIRED  scope={scope}  channel={}", out.channel);
                print_lock(&lock);
            } else {
                eprintln!("CONFLICT — {} blocking lock(s):", out.conflicts.len());
                for c in &out.conflicts {
                    print_lock(c);
                }
                std::process::exit(1);
            }
        }
        Command::Renew { lock_id, ttl_secs } => {
            let lock = api(&args.nats_url, from)
                .await?
                .lock_renew(lock_id, *ttl_secs)
                .await?;
            println!("RENEWED");
            print_lock(&lock);
        }
        Command::Release { lock_id, force } => {
            api(&args.nats_url, from)
                .await?
                .lock_release(lock_id, *force)
                .await?;
            println!("RELEASED {lock_id}");
        }
        Command::List { scope, all, json } => {
            if !all && scope.is_none() {
                bail!("pass --scope <scope> or --all");
            }
            let out = api(&args.nats_url, from)
                .await?
                .lock_list(scope.as_deref())
                .await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&out.locks)?);
            } else {
                if let Some(ch) = &out.channel {
                    eprintln!("scope={} channel={ch}", scope.as_deref().unwrap());
                }
                if out.locks.is_empty() {
                    println!("no live locks");
                }
                for l in &out.locks {
                    print_lock(l);
                }
            }
        }
        Command::Get { lock_id } => {
            match api(&args.nats_url, from).await?.lock_get(lock_id).await? {
                Some(l) => print_lock(&l),
                None => bail!("lock '{lock_id}' not found (released or expired)"),
            }
        }
        Command::Wait {
            scope,
            paths,
            timeout_secs,
            poll_secs,
        } => {
            let identity = from
                .map(str::to_string)
                .or_else(|| std::env::var("NATS_HUB_IDENTITY").ok())
                .context("wait needs an identity: pass --from or set NATS_HUB_IDENTITY")?;
            let hub = HubClient::connect(&args.nats_url, &identity).await?;
            let api = ApiClient::connect_with_identity(&args.nats_url, &identity).await?;
            eprintln!("waiting on [{}] in scope {scope} (up to {timeout_secs}s)", paths.join(", "));
            match wait_for_locks(
                &hub,
                &api,
                scope,
                paths,
                Duration::from_secs(*timeout_secs),
                Duration::from_secs_f64(poll_secs.max(0.2)),
            )
            .await?
            {
                WaitOutcome::Free => println!("FREE — no conflicting locks in {scope}"),
                WaitOutcome::TimedOut(conflicts) => {
                    eprintln!("TIMEOUT — still blocked by:");
                    for c in &conflicts {
                        print_lock(c);
                    }
                    std::process::exit(2);
                }
            }
        }
    }
    Ok(())
}
