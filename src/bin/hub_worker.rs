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

use anyhow::{Context, Result};
use clap::Parser;
use nats_hub::client::{looks_like_task_result, task_channel, TaskOutcome};
use nats_hub::{Envelope, HubClient, MessageKind};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

/// Max chars of stderr quoted in an error result.
const STDERR_TAIL: usize = 2000;

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
    tokio::select! {
        _ = run_loop(&client, &args, inbox_rx, broadcast_rx) => {}
        _ = tokio::signal::ctrl_c() => info!("interrupted, shutting down"),
    }

    let _ = client.drain().await;
    info!("hub-worker stopped");
    Ok(())
}

type EnvRx = tokio::sync::mpsc::UnboundedReceiver<Envelope>;

async fn run_loop(
    client: &HubClient,
    args: &Args,
    mut inbox_rx: EnvRx,
    broadcast_rx: Option<EnvRx>,
) {
    let mut broadcast_rx = broadcast_rx;
    loop {
        // `direct` = arrived on our inbox (vs. the optional broadcast channel).
        let (env, direct) = match broadcast_rx {
            Some(ref mut brx) => tokio::select! {
                Some(env) = inbox_rx.recv() => (env, true),
                Some(env) = brx.recv() => (env, false),
                else => break,
            },
            None => match inbox_rx.recv().await {
                Some(env) => (env, true),
                None => break,
            },
        };
        handle_task(client, args, env, direct).await;
    }
    info!("inbox stream closed, shutting down");
}

/// Run one task and publish its status + terminal result (refocus.md §6).
async fn handle_task(client: &HubClient, args: &Args, env: Envelope, direct: bool) {
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
    let outcome = match execute_command(&args.execute, &prompt, timeout).await {
        Ok(output) => {
            info!(id = %env.meta.id, output_len = output.len(), "task completed");
            TaskOutcome::Done(output)
        }
        Err(e) => {
            error!(id = %env.meta.id, error = %e, "task failed");
            TaskOutcome::Error(format!("{e:#}"))
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

/// Extract the prompt from an envelope payload.
/// Tries payload.prompt, .text, .command, then .message (human bridges).
fn extract_prompt(env: &Envelope) -> String {
    ["prompt", "text", "command", "message"]
        .iter()
        .find_map(|k| env.payload.get(*k).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// Execute a command, streaming `prompt` to stdin while concurrently
/// collecting stdout and stderr. Returns stdout on exit status 0.
async fn execute_command(command: &str, prompt: &str, timeout: Option<Duration>) -> Result<String> {
    debug!(%command, prompt_len = prompt.len(), "executing command");

    // Parse the command into program + args (simple split on whitespace)
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some((program, rest)) = parts.split_first() else {
        anyhow::bail!("empty execute command");
    };

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group, so a timeout can kill the whole tree.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn command: {command}"))?;

    let run = collect_output(&mut child, prompt.as_bytes().to_vec());
    let (status, stdout, stderr) = match timeout {
        None => run.await?,
        Some(limit) => match tokio::time::timeout(limit, run).await {
            Ok(res) => res?,
            Err(_) => {
                kill_tree(&mut child).await;
                anyhow::bail!("command timed out after {}s: {command}", limit.as_secs());
            }
        },
    };

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr = stderr.trim();
        let tail_start = stderr
            .char_indices()
            .rev()
            .nth(STDERR_TAIL)
            .map_or(0, |(i, _)| i);
        anyhow::bail!("command exited with {status}: {}", &stderr[tail_start..]);
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Write stdin, read stdout + stderr, and wait — all concurrently.
async fn collect_output(
    child: &mut Child,
    input: Vec<u8>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut stdin = child.stdin.take().context("child stdin not piped")?;
    let mut stdout = child.stdout.take().context("child stdout not piped")?;
    let mut stderr = child.stderr.take().context("child stderr not piped")?;

    let write = async move {
        let res = stdin.write_all(&input).await;
        drop(stdin); // close → EOF for the child
        match res {
            // The child may exit without consuming all input; not our error.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            other => other,
        }
    };
    let read_out = async {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.map(|_| buf)
    };
    let read_err = async {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await.map(|_| buf)
    };

    let (w, out, err) = tokio::join!(write, read_out, read_err);
    w.context("writing prompt to stdin")?;
    let out = out.context("reading stdout")?;
    let err = err.context("reading stderr")?;
    let status = child.wait().await.context("waiting for command")?;
    Ok((status, out, err))
}

/// SIGKILL the child's process group (unix), then the child itself, and reap it.
async fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = tokio::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    let _ = child.kill().await;
}
