//! hub-worker — universal agent worker that subscribes to its inbox,
//! executes a command for each task envelope, and publishes results back.
//!
//! Usage:
//!   hub-worker --identity worker-1 --execute "codex" --nats-url nats://127.0.0.1:4222
//!   hub-worker --identity worker-1 --execute "cat"  # echo for testing
//!   hub-worker --identity worker-1 --execute "python llm_worker.py" --timeout-secs 900
//!
//! The worker subscribes to channel.inbox.<identity> (DMs addressed to it),
//! then registers on the bus. For each task envelope it follows the reply
//! contract (refocus.md §6):
//!   1. Extracts payload.prompt (or .text, .command, .message)
//!   2. With payload.task_channel: publishes `status: working` on that channel
//!   3. Streams the prompt to the --execute command's stdin while reading its
//!      stdout/stderr concurrently (no pipe deadlock on large I/O)
//!   4. Kills the command's process group after --timeout-secs → `status: error`
//!   5. Publishes exactly one terminal result with `meta.reply_to = <task id>`
//!      and payload `{status, task_id, result, error}` — on the task channel,
//!      or as a DM to the sender when there is no task_channel.
//!
//! Cancel (refocus-iteration-2.md §4.2): a DM `kind = control` with payload
//! `{"action": "cancel", "task_id": <task envelope id>}` kills the running
//! command's process group (or drops a queued task) and publishes the
//! terminal result with `status: "cancelled"`. Unknown/finished ids are
//! ignored. Tasks still run one at a time; the inbox is read while a task
//! runs so a cancel can reach it (other envelopes are queued).
//!
//! Logs go to stderr.

#[path = "hub_worker/exec.rs"]
mod exec;

use anyhow::Result;
use clap::Parser;
use exec::{execute_command, ExecOutcome};
use nats_hub::client::{looks_like_task_result, task_channel, TaskOutcome};
use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;
use std::collections::VecDeque;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// Seconds between the first re-announcements (then every --heartbeat-secs).
const ANNOUNCE_BACKOFF_SECS: [u64; 4] = [1, 2, 4, 8];

fn capabilities() -> Vec<String> {
    vec!["worker".into(), "execute".into()]
}

#[derive(Parser)]
#[command(name = "hub-worker", about = "Universal agent worker for nats-hub")]
struct Args {
    /// Worker identity (agent name)
    #[arg(long)]
    identity: String,

    /// Command to execute for each task (receives prompt via stdin, result via stdout)
    #[arg(long)]
    execute: String,

    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Also subscribe to this broadcast channel (in addition to inbox)
    #[arg(long)]
    channel: Option<String>,

    /// Heartbeat interval in seconds (0 = disabled)
    #[arg(long, default_value = "30")]
    heartbeat_secs: u64,

    /// Kill the command (and its process group) after this many seconds
    /// and report `status: error` (0 = no timeout)
    #[arg(long, default_value_t = 600)]
    timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    info!(
        identity = %args.identity,
        execute = %args.execute,
        nats_url = %args.nats_url,
        timeout_secs = args.timeout_secs,
        "hub-worker starting"
    );

    let client = HubClient::connect(&args.nats_url, &args.identity).await?;

    // Subscribe first, then announce: once we are visible on the bus we can
    // already receive tasks.
    let inbox_rx = client.subscribe_inbox().await?;
    info!(identity = %args.identity, "subscribed to inbox");

    let broadcast_rx = match args.channel {
        Some(ref ch) => {
            let rx = client.subscribe_channel(ch).await?;
            info!(channel = %ch, "also subscribed to broadcast channel");
            Some(rx)
        }
        None => None,
    };

    client.register(capabilities()).await?;
    info!(identity = %args.identity, "registered on bus");

    if args.heartbeat_secs > 0 {
        let hb_client = client.clone();
        let interval = Duration::from_secs(args.heartbeat_secs);
        // Heartbeat + (upserting) re-registration: fast while hub-server may
        // still be starting (a registration sent before it listens is lost),
        // then every interval. Also recovers from router restarts.
        tokio::spawn(async move {
            let mut backoff = ANNOUNCE_BACKOFF_SECS
                .iter()
                .map(|s| Duration::from_secs(*s))
                .filter(|d| *d < interval);
            loop {
                tokio::time::sleep(backoff.next().unwrap_or(interval)).await;
                let beat = hb_client.heartbeat().await;
                let reg = hb_client.register(capabilities()).await;
                if let Err(e) = beat.and(reg) {
                    warn!(error = %e, "heartbeat failed");
                }
            }
        });
    }

    info!("hub-worker ready, waiting for tasks...");

    // Dropping the loop on Ctrl+C drops any running child (kill_on_drop).
    let mut inbox = Inbox {
        inbox: inbox_rx,
        broadcast: broadcast_rx,
        queued: VecDeque::new(),
    };
    tokio::select! {
        _ = run_loop(&client, &args, &mut inbox) => {}
        _ = tokio::signal::ctrl_c() => info!("interrupted, shutting down"),
    }

    let _ = client.drain().await;
    info!("hub-worker stopped");
    Ok(())
}

type EnvRx = tokio::sync::mpsc::UnboundedReceiver<Envelope>;

/// Inbox + optional broadcast channel, plus envelopes that arrived while a
/// task was running. `direct` = arrived on our inbox.
struct Inbox {
    inbox: EnvRx,
    broadcast: Option<EnvRx>,
    queued: VecDeque<(Envelope, bool)>,
}

impl Inbox {
    /// Next envelope from the bus (not the queue). None once closed.
    async fn recv(&mut self) -> Option<(Envelope, bool)> {
        match self.broadcast {
            Some(ref mut brx) => tokio::select! {
                Some(env) = self.inbox.recv() => Some((env, true)),
                Some(env) = brx.recv() => Some((env, false)),
                else => None,
            },
            None => self.inbox.recv().await.map(|env| (env, true)),
        }
    }

    /// Drop a queued task on cancel. Returns it so a result can be sent.
    fn take_queued(&mut self, id: &str) -> Option<Envelope> {
        let pos = self.queued.iter().position(|(e, _)| e.meta.id == id)?;
        self.queued.remove(pos).map(|(e, _)| e)
    }
}

/// The task id of a §4.2 cancel request (only DMs count).
fn cancel_target(env: &Envelope, direct: bool) -> Option<&str> {
    let is_cancel = direct
        && env.meta.kind == MessageKind::Control
        && env.payload.get("action").and_then(|v| v.as_str()) == Some("cancel");
    is_cancel
        .then(|| env.payload.get("task_id").and_then(|v| v.as_str()))
        .flatten()
}

async fn run_loop(client: &HubClient, args: &Args, inbox: &mut Inbox) {
    loop {
        let next = match inbox.queued.pop_front() {
            Some(queued) => Some(queued),
            None => inbox.recv().await,
        };
        let Some((env, direct)) = next else { break };
        if let Some(id) = cancel_target(&env, direct) {
            // Nothing runs between tasks; only a queued task can match.
            match inbox.take_queued(id) {
                Some(task) => publish_cancelled(client, &task).await,
                None => debug!(task_id = %id, "cancel for unknown/finished task, ignoring"),
            }
            continue;
        }
        handle_task(client, args, env, direct, inbox).await;
    }
    info!("inbox stream closed, shutting down");
}

/// Run one task and publish its status + terminal result (refocus.md §6).
async fn handle_task(
    client: &HubClient,
    args: &Args,
    env: Envelope,
    direct: bool,
    inbox: &mut Inbox,
) {
    info!(id = %env.meta.id, from = %env.meta.from, kind = ?env.meta.kind, "received envelope");

    // Progress/bookkeeping kinds are never tasks. Never answer a result
    // either: two workers DMing each other results would ping-pong forever.
    let progress = matches!(
        env.meta.kind,
        MessageKind::Status | MessageKind::Event | MessageKind::Control
    );
    if progress || looks_like_task_result(&env) {
        debug!(id = %env.meta.id, "not a task, ignoring");
        return;
    }
    // Without a task channel the reply is a DM (rule 6) — only for envelopes
    // sent to us, not for chatter on the broadcast channel.
    if !direct && task_channel(&env).is_none() {
        debug!(id = %env.meta.id, "broadcast without task_channel, ignoring");
        return;
    }
    let prompt = extract_prompt(&env);
    if prompt.is_empty() {
        warn!(id = %env.meta.id, "no prompt found in payload, skipping");
        return;
    }

    if let Err(e) = client.send_task_status(&env, "working").await {
        warn!(error = %e, "failed to send working status");
    }

    let timeout = (args.timeout_secs > 0).then(|| Duration::from_secs(args.timeout_secs));
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let mut cancel_tx = Some(cancel_tx);
    let cancelled = async {
        // A dropped sender (never cancelled) must not look like a cancel.
        if cancel_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let run = execute_command(&args.execute, &prompt, timeout, cancelled);
    tokio::pin!(run);

    // Keep reading the bus while the command runs: a cancel for this task
    // fires `cancel_tx`; everything else waits in the queue.
    let mut bus_open = true;
    let outcome = loop {
        tokio::select! {
            outcome = &mut run => break outcome,
            next = inbox.recv(), if bus_open => match next {
                None => bus_open = false,
                Some((other, other_direct)) => match cancel_target(&other, other_direct) {
                    Some(id) if id == env.meta.id => {
                        info!(task_id = %id, by = %other.meta.from, "cancel requested");
                        if let Some(tx) = cancel_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                    Some(id) => match inbox.take_queued(id) {
                        Some(task) => publish_cancelled(client, &task).await,
                        None => debug!(task_id = %id, "cancel for unknown task, ignoring"),
                    },
                    None => inbox.queued.push_back((other, other_direct)),
                },
            },
        }
    };

    let outcome = match outcome {
        ExecOutcome::Done(output) => {
            info!(id = %env.meta.id, output_len = output.len(), "task completed");
            TaskOutcome::Done(output)
        }
        ExecOutcome::Error(e) => {
            error!(id = %env.meta.id, error = %e, "task failed");
            TaskOutcome::Error(e)
        }
        ExecOutcome::Cancelled => {
            info!(id = %env.meta.id, "task cancelled, process group killed");
            publish_cancelled(client, &env).await;
            return;
        }
    };
    let final_status = match outcome {
        TaskOutcome::Done(_) => "done",
        TaskOutcome::Error(_) => "error",
    };

    if let Err(e) = client.send_task_result(&env, outcome).await {
        error!(error = %e, "failed to send result");
    }
    let _ = client.send_task_status(&env, final_status).await;
}

/// The terminal `status: "cancelled"` result (§4.2), routed like
/// `HubClient::send_task_result` (task channel, else DM to the sender).
async fn publish_cancelled(client: &HubClient, task: &Envelope) {
    let payload = json!({
        "status": "cancelled",
        "task_id": task.meta.id,
        "result": null,
        "error": "cancelled",
    });
    let result = match task_channel(task) {
        Some(channel) => Envelope::new(client.identity(), channel, MessageKind::Message, payload),
        None => Envelope::new(
            client.identity(),
            task.meta.channel.clone(),
            MessageKind::Message,
            payload,
        )
        .to(&task.meta.from),
    }
    .reply_to(&task.meta.id);
    if let Err(e) = client.send(&result).await {
        error!(error = %e, "failed to send cancelled result");
    }
    let _ = client.send_task_status(task, "cancelled").await;
}

/// Extract the prompt from an envelope payload.
/// Tries payload.prompt, .text, .command, then .message (human bridges).
fn extract_prompt(env: &Envelope) -> String {
    ["prompt", "text", "command", "message"]
        .iter()
        .find_map(|k| env.payload.get(*k).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string()
}
