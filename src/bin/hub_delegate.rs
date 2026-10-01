//! hub-delegate — one command to delegate a task to an agent worker.
//!
//! Creates a task channel, sends the task to the worker's inbox,
//! and waits for the result. Optionally prints status updates.
//!
//! Usage:
//!   hub-delegate --to worker-1 --prompt "implement phase 3"
//!   hub-delegate --to worker-1 --prompt "fix the bug" --timeout 120
//!   hub-delegate --to worker-1 --prompt "review this" --verbose
//!   hub-delegate --to worker-1 --prompt-file task.md      # no 128 KiB argv limit
//!   git diff | hub-delegate --to worker-1 --prompt -      # prompt from stdin
//!   hub-delegate --to worker-1 --prompt "do work" --no-wait   # fire and forget
//!
//! Flow:
//!   1. Generate task UUID → task channel = task.<short-uuid>
//!   2. Subscribe to channel.task.<short-uuid> (for status + reply)
//!   3. Send task envelope to hub.send.task.<short-uuid> with meta.to=worker
//!      and payload {prompt, task_channel}
//!   4. Router routes to channel.inbox.<worker>
//!   5. Worker receives, processes, publishes status/events + one result to
//!      channel.task.<short-uuid> (reply contract, refocus.md §6)
//!   6. hub-delegate takes the first `message` whose meta.reply_to or
//!      payload.task_id equals the task id, and prints it
//!
//! Ctrl-C (refocus-iteration-2.md §4.2): the first one DMs the worker
//! `kind = control {"action": "cancel", "task_id"}` and waits up to 10s for
//! the `cancelled` result; a second Ctrl-C exits at once.
//!
//! stdout carries only the result; all logs and progress go to stderr.
//!
//! Exit codes: 0 done · 1 worker reported an error · 2 timeout · 3 channel
//! closed · 4 cancelled · 130 interrupted without a cancel confirmation

use anyhow::{Context, Result};
use clap::Parser;
use nats_hub::client::is_task_result;
use nats_hub::{Envelope, HubClient, MessageKind};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

/// How long the first Ctrl-C waits for the worker's `cancelled` result.
const CANCEL_WAIT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(
    name = "hub-delegate",
    about = "Delegate a task to an agent worker via nats-hub"
)]
struct Args {
    /// Recipient agent identity (the worker to send the task to)
    #[arg(long)]
    to: String,

    /// The prompt/task to send to the worker (`-` reads it from stdin)
    #[arg(
        long,
        required_unless_present = "prompt_file",
        conflicts_with = "prompt_file"
    )]
    prompt: Option<String>,

    /// Read the prompt from this file (avoids the command-line length limit)
    #[arg(long, value_name = "PATH")]
    prompt_file: Option<PathBuf>,

    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Timeout in seconds to wait for a reply (0 = wait forever)
    #[arg(long, default_value_t = 120)]
    timeout: u64,

    /// Don't wait for a reply (fire and forget)
    #[arg(long)]
    no_wait: bool,

    /// Print status updates (working/done/error) as they arrive
    #[arg(long)]
    verbose: bool,

    /// Your identity (the sender)
    #[arg(long, default_value = "orchestrator")]
    from: String,
}

/// The prompt from `--prompt TEXT`, `--prompt -` (stdin) or `--prompt-file`.
fn read_prompt(args: &Args) -> Result<String> {
    let prompt = match (&args.prompt, &args.prompt_file) {
        (_, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("reading --prompt-file {}", path.display()))?,
        (Some(p), None) if p == "-" => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading the prompt from stdin")?;
            buf
        }
        (Some(p), None) => p.clone(),
        (None, None) => anyhow::bail!("--prompt or --prompt-file is required"),
    };
    anyhow::ensure!(!prompt.trim().is_empty(), "the prompt is empty");
    Ok(prompt)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Logs on stderr: stdout is the result, so `x=$(hub-delegate …)` works
    // even with RUST_LOG set.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let prompt = read_prompt(&args)?;

    // Generate a short task UUID for the task channel
    let task_uuid = uuid::Uuid::new_v4();
    let task_short = &task_uuid.to_string()[..8];
    let task_channel = format!("task.{task_short}");

    info!(
        to = %args.to,
        task_channel = %task_channel,
        prompt_len = prompt.len(),
        prompt = %prompt.chars().take(80).collect::<String>(),
        "delegating task"
    );

    let client = HubClient::connect(&args.nats_url, &args.from).await?;

    // Subscribe to the task channel BEFORE sending (reply contract rule 1).
    // SUB and PUB travel on the same connection, so the server has the
    // subscription before the task is routed to the worker.
    let mut task_rx = client.subscribe_channel(&task_channel).await?;
    debug!(%task_channel, "subscribed to task channel");

    // The channel travels in payload.task_channel; meta.reply_to is only
    // ever a message id (rule 2), so it stays unset on the task itself.
    let payload = serde_json::json!({
        "prompt": prompt,
        "task_channel": task_channel,
    });

    let env = Envelope::new(
        args.from.clone(),
        &task_channel,
        MessageKind::Message,
        payload,
    )
    .to(&args.to);

    let task_id = env.meta.id.clone();
    let mut interrupts = Interrupts::new()?;
    client.send(&env).await?;
    info!(%task_id, "task sent to {}", args.to);

    if args.no_wait {
        println!("{task_id}");
        let _ = client.drain().await;
        return Ok(());
    }

    let mut deadline =
        (args.timeout > 0).then(|| Instant::now() + Duration::from_secs(args.timeout));
    let mut cancel_sent = false;
    let wait_until = |d: Option<Instant>| async move {
        match d {
            Some(d) => tokio::time::sleep_until(d).await,
            None => std::future::pending().await,
        }
    };

    let result = loop {
        let recv = tokio::select! {
            recv = task_rx.recv() => recv,
            _ = wait_until(deadline) => {
                if cancel_sent {
                    eprintln!("\n[hub-delegate] no cancel confirmation from {} within {}s", args.to, CANCEL_WAIT.as_secs());
                    match exit(&client, 130).await {}
                }
                eprintln!(
                    "\n[hub-delegate] TIMEOUT after {}s — no reply from {to}",
                    args.timeout,
                    to = args.to
                );
                match exit(&client, 2).await {}
            }
            () = interrupts.recv() => {
                if cancel_sent {
                    eprintln!("\n[hub-delegate] interrupted again — exiting without waiting");
                    match exit(&client, 130).await {}
                }
                eprintln!(
                    "\n[hub-delegate] cancelling task {task_id} on {} (Ctrl-C again to exit now)",
                    args.to
                );
                send_cancel(&client, &args.to, &task_channel, &task_id).await;
                cancel_sent = true;
                deadline = Some(Instant::now() + CANCEL_WAIT);
                continue;
            }
        };

        let Some(env) = recv else {
            eprintln!("\n[hub-delegate] task channel closed unexpectedly");
            match exit(&client, 3).await {}
        };

        // Rule 5: the result is the first `message` correlated to our task;
        // every status/event envelope is progress.
        if is_task_result(&env, &task_id) {
            break env;
        }
        if args.verbose {
            print_progress(&env);
        }
    };

    info!(%task_id, "reply received from {}", result.meta.from);
    let status = result
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("done");
    let text = |key: &str| result.payload.get(key).and_then(|v| v.as_str());

    match status {
        "error" => {
            let err = text("error")
                .or_else(|| text("result"))
                .unwrap_or("unknown error");
            eprintln!("\n[hub-delegate] task failed: {err}");
            match exit(&client, 1).await {}
        }
        "cancelled" => {
            eprintln!("\n[hub-delegate] task cancelled by {}", result.meta.from);
            match exit(&client, 4).await {}
        }
        _ => {}
    }

    println!("{}", text("result").unwrap_or("no result field"));
    let _ = client.drain().await;
    Ok(())
}

/// DM the worker the §4.2 cancel request for `task_id`.
async fn send_cancel(client: &HubClient, worker: &str, channel: &str, task_id: &str) {
    let cancel = Envelope::new(
        client.identity(),
        channel,
        MessageKind::Control,
        serde_json::json!({"action": "cancel", "task_id": task_id}),
    )
    .to(worker);
    if let Err(e) = client.send(&cancel).await {
        eprintln!("[hub-delegate] failed to send cancel: {e:#}");
    }
}

/// Drain the connection (flushes pending publishes) and exit.
async fn exit(client: &HubClient, code: i32) -> std::convert::Infallible {
    let _ = client.drain().await;
    std::process::exit(code);
}

/// SIGINT as a stream, installed before the task is sent so no Ctrl-C is
/// missed (a fresh `ctrl_c()` per wait could drop one between waits).
struct Interrupts {
    #[cfg(unix)]
    sig: tokio::signal::unix::Signal,
}

impl Interrupts {
    fn new() -> Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            sig: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .context("installing the SIGINT handler")?,
        })
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        self.sig.recv().await;
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `--verbose`: print a progress envelope from the task channel to stderr.
fn print_progress(env: &Envelope) {
    let field = |key: &str| env.payload.get(key).and_then(|v| v.as_str());
    match env.meta.kind {
        MessageKind::Status => {
            eprintln!(
                "[status] {} — {}",
                env.meta.from,
                field("status").unwrap_or("unknown")
            );
        }
        MessageKind::Event => {
            eprintln!(
                "[event] {} — {}",
                env.meta.from,
                field("event_type").unwrap_or("event")
            );
        }
        ref kind => debug!(from = %env.meta.from, ?kind, "ignoring uncorrelated envelope"),
    }
}
