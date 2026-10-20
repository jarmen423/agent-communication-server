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
//!   hub-delegate --to worker-1 --resume <task_id>   # wait again (durable hub)
//!
//! Flow:
//!   1. Build the task envelope; task channel = task.<first 8 chars of its id>
//!   2. Subscribe to channel.task.<short-uuid> (for status + reply)
//!   3. Send task envelope to hub.send.task.<short-uuid> with meta.to=worker
//!      and payload {prompt, task_channel}
//!   4. Router routes to channel.inbox.<worker>
//!   5. Worker receives, processes, publishes status/events + one result to
//!      channel.task.<short-uuid> (reply contract, refocus.md §6)
//!   6. hub-delegate takes the first `message` from the worker whose
//!      meta.reply_to or payload.task_id equals the task id, and prints it
//!      (results from anyone else are ignored — HARDENING.md C5)
//!
//! Ctrl-C (refocus-iteration-2.md §4.2): the first one DMs the worker
//! `kind = control {"action": "cancel", "task_id"}` and waits up to 10s for
//! the `cancelled` result; a second Ctrl-C exits at once. A `--timeout`
//! also sends the cancel, so the worker doesn't keep running unattended.
//!
//! Resume (`--resume <task_id>`, refocus-iteration-2 W2-A): re-attach to a
//! task delegated earlier (by this or another process, with the same
//! `--from`). On a `--durable` hub the task channel is kept in the tasks
//! stream, so a result published while nobody listened is read back by
//! sequence; live traffic is followed as usual. A resume timeout does not
//! cancel the task (Ctrl-C still does). While waiting, the task channel is
//! also re-read every few seconds, so a result published during a
//! connection blip is not missed.
//!
//! stdout carries only the result; all logs and progress go to stderr.
//!
//! Exit codes: 0 done · 1 worker reported an error · 2 timeout · 3 channel
//! closed · 4 cancelled · 130 interrupted without a cancel confirmation

use anyhow::{Context, Result};
use clap::Parser;
use nats_hub::client::{is_task_result, task_channel_for, ChannelCatchUp};
use nats_hub::{Envelope, HubClient, MessageKind};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

/// How long the first Ctrl-C waits for the worker's `cancelled` result.
const CANCEL_WAIT: Duration = Duration::from_secs(10);
/// How often the stored task channel is re-read while waiting.
const CATCH_UP_EVERY: Duration = Duration::from_secs(3);

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
        required_unless_present_any = ["prompt_file", "resume"],
        conflicts_with_all = ["prompt_file", "resume"]
    )]
    prompt: Option<String>,

    /// Read the prompt from this file (avoids the command-line length limit)
    #[arg(long, value_name = "PATH", conflicts_with = "resume")]
    prompt_file: Option<PathBuf>,

    /// Wait again for the result of an already-delegated task (its id, as
    /// printed by --no-wait / the delegation log). Reads back what the
    /// worker published while nobody listened (needs a --durable hub).
    #[arg(long, value_name = "TASK_ID", conflicts_with = "no_wait")]
    resume: Option<String>,

    /// Task channel to resume (default: derived from the task id, as
    /// hub-delegate and the MCP server name it).
    #[arg(long, value_name = "CHANNEL", requires = "resume")]
    channel: Option<String>,

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
async fn read_prompt(args: &Args) -> Result<String> {
    let prompt = match (&args.prompt, &args.prompt_file) {
        (_, Some(path)) => tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading --prompt-file {}", path.display()))?,
        (Some(p), None) if p == "-" => {
            let mut buf = String::new();
            tokio::io::stdin()
                .read_to_string(&mut buf)
                .await
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
    let prompt = match args.resume {
        None => Some(read_prompt(&args).await?),
        Some(_) => None,
    };
    let client = HubClient::connect(&args.nats_url, &args.from).await?;
    let mut interrupts = Interrupts::new()?;

    let (task_id, task_channel, mut task_rx) = match (&args.resume, prompt) {
        (Some(task_id), _) => {
            let task_channel = args
                .channel
                .clone()
                .unwrap_or_else(|| task_channel_for(task_id));
            let task_rx = client.subscribe_channel(&task_channel).await?;
            info!(%task_id, %task_channel, worker = %args.to, "resuming task");
            (task_id.clone(), task_channel, task_rx)
        }
        (None, prompt) => {
            let prompt = prompt.unwrap_or_default();
            let mut env = Envelope::new(
                args.from.clone(),
                "task",
                MessageKind::Message,
                serde_json::Value::Null,
            )
            .to(&args.to);
            let task_channel = task_channel_for(&env.meta.id);
            info!(
                to = %args.to,
                task_channel = %task_channel,
                prompt_len = prompt.len(),
                prompt = %prompt.chars().take(80).collect::<String>(),
                "delegating task"
            );

            // Subscribe to the task channel BEFORE sending (reply contract
            // rule 1). SUB and PUB travel on the same connection, so the
            // server has the subscription before the task is routed.
            let task_rx = client.subscribe_channel(&task_channel).await?;
            debug!(%task_channel, "subscribed to task channel");

            // The channel travels in payload.task_channel; meta.reply_to is
            // only ever a message id (rule 2), so it stays unset on the task.
            env.meta.channel = task_channel.clone();
            env.payload = serde_json::json!({
                "prompt": prompt,
                "task_channel": task_channel,
            });
            client.send(&env).await?;
            info!(task_id = %env.meta.id, "task sent to {}", args.to);
            eprintln!(
                "[hub-delegate] task {id} → {to} (resume: hub-delegate --resume {id} --to {to} --from {from})",
                id = env.meta.id,
                to = args.to,
                from = args.from
            );

            if args.no_wait {
                println!("{}", env.meta.id);
                let _ = client.drain().await;
                return Ok(());
            }
            (env.meta.id, task_channel, task_rx)
        }
    };
    let mut catch_up = ChannelCatchUp::new(&task_channel);
    let mut catch_up_tick = tokio::time::interval(CATCH_UP_EVERY);
    catch_up_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut deadline =
        (args.timeout > 0).then(|| Instant::now() + Duration::from_secs(args.timeout));
    let mut cancel_sent = false;
    let wait_until = |d: Option<Instant>| async move {
        match d {
            Some(d) => tokio::time::sleep_until(d).await,
            None => std::future::pending().await,
        }
    };

    let result = 'wait: loop {
        let incoming = tokio::select! {
            recv = task_rx.recv() => match recv {
                Some(env) if catch_up.first_sight(&env) => vec![env],
                Some(_) => continue, // already read back from the stream
                None => {
                    eprintln!("\n[hub-delegate] task channel closed unexpectedly");
                    match exit(&client, 3).await {}
                }
            },
            _ = catch_up_tick.tick() => match catch_up.poll(&client).await {
                Ok(envs) => envs,
                Err(e) => {
                    debug!(error = %e, "task channel catch-up failed");
                    continue;
                }
            },
            _ = wait_until(deadline) => {
                if cancel_sent {
                    eprintln!("\n[hub-delegate] no cancel confirmation from {} within {}s", args.to, CANCEL_WAIT.as_secs());
                    match exit(&client, 130).await {}
                }
                if args.resume.is_some() {
                    eprintln!(
                        "\n[hub-delegate] TIMEOUT after {}s — no result yet for {task_id} (task left running)",
                        args.timeout
                    );
                    match exit(&client, 2).await {}
                }
                // Don't leave the worker running a task nobody waits for.
                send_cancel(&client, &args.to, &task_channel, &task_id).await;
                eprintln!(
                    "\n[hub-delegate] TIMEOUT after {}s — no reply from {to}; cancel sent",
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

        for env in incoming {
            // Rule 5: the result is the first `message` correlated to our
            // task and sent by the worker we delegated to (C5); every
            // status/event envelope is progress.
            if is_task_result(&env, &task_id, &args.to) {
                break 'wait env;
            }
            if env.meta.kind == MessageKind::Message
                && env.meta.reply_to.as_deref() == Some(task_id.as_str())
            {
                eprintln!(
                    "[hub-delegate] ignoring a result for {task_id} from {} (delegated to {})",
                    env.meta.from, args.to
                );
                continue;
            }
            if args.verbose {
                print_progress(&env);
            }
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
