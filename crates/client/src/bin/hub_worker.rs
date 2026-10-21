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
//! ignored, and so is a cancel from anyone but the task's sender
//! (HARDENING.md C5). Tasks still run one at a time; the inbox is read while
//! a task runs so a cancel can reach it. Other tasks wait in a bounded queue
//! (`--max-queued`); one that doesn't fit gets an `error` result at once.
//!
//! SIGTERM and Ctrl-C stop the worker; the running command's process group
//! is killed on the way out.
//!
//! Logs go to stderr.

#[path = "hub_worker/exec.rs"]
mod exec;
#[path = "hub_worker/inbox.rs"]
mod inbox;

use anyhow::Result;
use clap::Parser;
use exec::{execute_command, ExecOutcome};
use inbox::{cancel_target, task_prompt, warn_denied, CancelTarget, Inbox};
use nats_hub_client::client::{task_channel, TaskOutcome};
use nats_hub_client::{Envelope, HubClient, MessageKind};
use serde_json::json;
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

    /// Max tasks waiting behind the running one; more get an error result
    #[arg(long, default_value_t = 256, value_parser = clap::value_parser!(u64).range(1..))]
    max_queued: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    // Installed first, so a SIGTERM during startup is not lost either.
    let mut stop = Stop::install()?;

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

    // Stopping drops the loop and with it the running command, whose
    // process group is then killed (exec::GroupGuard).
    let max_queued = usize::try_from(args.max_queued).unwrap_or(usize::MAX);
    let mut inbox = Inbox::new(inbox_rx, broadcast_rx, max_queued);
    tokio::select! {
        _ = run_loop(&client, &args, &mut inbox) => {}
        sig = stop.recv() => info!(signal = sig, "shutting down"),
    }

    let _ = client.drain().await;
    info!("hub-worker stopped");
    Ok(())
}

/// SIGTERM + SIGINT (Ctrl-C) as one stream.
struct Stop {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
}

impl Stop {
    fn install() -> Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            term: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        tokio::select! {
            _ = self.term.recv() => "SIGTERM",
            _ = tokio::signal::ctrl_c() => "SIGINT",
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            "SIGINT"
        }
    }
}

async fn run_loop(client: &HubClient, args: &Args, inbox: &mut Inbox) {
    loop {
        if let Some((task, prompt)) = inbox.pop_queued() {
            handle_task(client, args, task, prompt, inbox).await;
            continue;
        }
        let Some((env, direct)) = inbox.recv().await else {
            break;
        };
        if let Some(id) = cancel_target(&env, direct) {
            // Nothing runs between tasks; only a queued task can match.
            on_cancel(client, inbox, id, &env.meta.from, None).await;
            continue;
        }
        if !admit_session(inbox, &env) {
            continue;
        }
        match task_prompt(&env, direct) {
            Ok(prompt) => handle_task(client, args, env, prompt, inbox).await,
            Err(why) => debug!(id = %env.meta.id, why, "not a task, ignoring"),
        }
    }
    info!("inbox stream closed, shutting down");
}

/// C5 for session actions: false (logged) when `env` acts on a session it
/// doesn't own. Progress/control kinds are never session actions.
fn admit_session(inbox: &mut Inbox, env: &Envelope) -> bool {
    if matches!(
        env.meta.kind,
        MessageKind::Status | MessageKind::Event | MessageKind::Control
    ) {
        return true;
    }
    match inbox.sessions.admit(env) {
        Ok(()) => true,
        Err(why) => {
            warn!(id = %env.meta.id, %why, "session action ignored");
            false
        }
    }
}

/// Apply a cancel for `id` from `by`. Returns true if it targets the running task.
async fn on_cancel(
    client: &HubClient,
    inbox: &mut Inbox,
    id: &str,
    by: &str,
    running: Option<&Envelope>,
) -> bool {
    match inbox.resolve_cancel(id, by, running) {
        CancelTarget::Running => return true,
        CancelTarget::Queued(task) => publish_cancelled(client, &task).await,
        CancelTarget::Denied(owner) => warn_denied(id, by, &owner),
        CancelTarget::Unknown => {
            debug!(task_id = %id, "cancel for unknown/finished task, ignoring")
        }
    }
    false
}

/// While `running` runs: alias a re-dispatch of a known session (C3), queue
/// another task, or answer at once if the queue is full.
async fn queue_or_reject(
    client: &HubClient,
    inbox: &mut Inbox,
    env: Envelope,
    direct: bool,
    running: &Envelope,
) {
    if !admit_session(inbox, &env) {
        return;
    }
    let prompt = match task_prompt(&env, direct) {
        Ok(prompt) => prompt,
        Err(why) => {
            debug!(id = %env.meta.id, why, "not a task, ignoring");
            return;
        }
    };
    if inbox.alias_redispatch(&env, running) {
        info!(id = %env.meta.id, "session_start for a session already in flight: aliased, not re-run");
        return;
    }
    let Some(max) = inbox.full() else {
        return inbox.queue(env, prompt);
    };
    warn!(id = %env.meta.id, max, "queue full, rejecting task");
    let err = format!(
        "worker {} is busy: {max} tasks already queued",
        client.identity()
    );
    if let Err(e) = client.send_task_result(&env, TaskOutcome::Error(err)).await {
        error!(error = %e, "failed to send queue-full result");
    }
    let _ = client.send_task_status(&env, "error").await;
}

/// Run one task and publish its status + terminal result (refocus.md §6).
async fn handle_task(
    client: &HubClient,
    args: &Args,
    env: Envelope,
    prompt: String,
    inbox: &mut Inbox,
) {
    info!(id = %env.meta.id, from = %env.meta.from, kind = ?env.meta.kind, "running task");

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
    // (from its sender) fires `cancel_tx`; other tasks wait in the queue.
    let mut bus_open = true;
    let outcome = loop {
        tokio::select! {
            outcome = &mut run => break outcome,
            next = inbox.recv(), if bus_open => match next {
                None => bus_open = false,
                Some((other, other_direct)) => match cancel_target(&other, other_direct) {
                    Some(id) => {
                        if on_cancel(client, inbox, id, &other.meta.from, Some(&env)).await {
                            if let Some(tx) = cancel_tx.take() {
                                info!(task_id = %id, by = %other.meta.from, "cancel requested");
                                let _ = tx.send(());
                            }
                        }
                    }
                    None => queue_or_reject(client, inbox, other, other_direct, &env).await,
                },
            },
        }
    };

    inbox.forget(&env.meta.id);
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
