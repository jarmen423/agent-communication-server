//! hub-worker — universal agent worker that subscribes to a channel,
//! executes a command for each task envelope, and publishes results back.
//!
//! Usage:
//!   hub-worker --identity worker-1 --execute "codex" --nats-url nats://127.0.0.1:4222
//!   hub-worker --identity worker-1 --execute "cat"  # echo for testing
//!   hub-worker --identity worker-1 --execute "python llm_worker.py"
//!
//! The worker subscribes to channel.inbox.<identity> (DMs addressed to it).
//! For each envelope received:
//!   1. Extracts payload.prompt (or payload.text, or payload.command)
//!   2. Passes it to the --execute command via stdin
//!   3. Captures stdout as the result
//!   4. Publishes a result envelope back to the sender (via send_reply)
//!   5. Publishes status updates (working/done/error) on its status channel

use anyhow::{Context, Result};
use clap::Parser;
use nats_hub::{Envelope, HubClient, MessageKind};
use std::process::Stdio;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

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
        "hub-worker starting"
    );

    let client = HubClient::connect(&args.nats_url, &args.identity).await?;

    // Register on the bus
    client
        .register(vec!["worker".into(), "execute".into()])
        .await?;
    info!(identity = %args.identity, "registered on bus");

    // Start heartbeat loop (optional)
    if args.heartbeat_secs > 0 {
        let hb_client = client.clone();
        let interval = args.heartbeat_secs;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                if let Err(e) = hb_client.heartbeat().await {
                    warn!(error = %e, "heartbeat failed");
                }
            }
        });
    }

    // Subscribe to our inbox (DMs)
    let mut inbox_rx = client.subscribe_inbox().await?;
    info!(identity = %args.identity, "subscribed to inbox");

    // Optionally subscribe to a broadcast channel too
    let mut broadcast_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Envelope>> = if let Some(ref ch) = args.channel {
        let rx = client.subscribe_channel(ch).await?;
        info!(channel = %ch, "also subscribed to broadcast channel");
        Some(rx)
    } else {
        None
    };

    info!("hub-worker ready, waiting for tasks...");

    loop {
        let env = if let Some(ref mut brx) = broadcast_rx {
            // Select between inbox and broadcast
            tokio::select! {
                Some(env) = inbox_rx.recv() => env,
                Some(env) = brx.recv() => env,
            }
        } else {
            match inbox_rx.recv().await {
                Some(env) => env,
                None => {
                    info!("inbox stream closed, shutting down");
                    break;
                }
            }
        };

        info!(
            id = %env.meta.id,
            from = %env.meta.from,
            kind = ?env.meta.kind,
            "received task"
        );

        // Extract the prompt from the payload
        let prompt = extract_prompt(&env);
        if prompt.is_empty() {
            warn!(id = %env.meta.id, "no prompt found in payload, skipping");
            continue;
        }

        // Send status: working
        let _ = client
            .send_status(&env.meta.channel, "working")
            .await;

        // Execute the command
        match execute_command(&args.execute, &prompt).await {
            Ok(output) => {
                info!(id = %env.meta.id, output_len = output.len(), "task completed");

                // Send the result back to the sender
                let result_payload = serde_json::json!({
                    "result": output,
                    "task_id": env.meta.id,
                    "status": "done",
                });

                if let Err(e) = client.send_reply(&env, result_payload).await {
                    error!(error = %e, "failed to send result");
                }

                // Send status: done
                let _ = client
                    .send_status(&env.meta.channel, "done")
                    .await;
            }
            Err(e) => {
                error!(id = %env.meta.id, error = %e, "task failed");

                // Send error back to the sender
                let error_payload = serde_json::json!({
                    "error": e.to_string(),
                    "task_id": env.meta.id,
                    "status": "error",
                });

                if let Err(e2) = client.send_reply(&env, error_payload).await {
                    error!(error = %e2, "failed to send error reply");
                }

                // Send status: error
                let _ = client
                    .send_status(&env.meta.channel, "error")
                    .await;
            }
        }
    }

    // Clean up
    let _ = client.drain().await;
    info!("hub-worker stopped");
    Ok(())
}

/// Extract the prompt from an envelope payload.
/// Tries payload.prompt, then payload.text, then payload.command.
fn extract_prompt(env: &Envelope) -> String {
    if let Some(p) = env.payload.get("prompt").and_then(|v| v.as_str()) {
        return p.to_string();
    }
    if let Some(t) = env.payload.get("text").and_then(|v| v.as_str()) {
        return t.to_string();
    }
    if let Some(c) = env.payload.get("command").and_then(|v| v.as_str()) {
        return c.to_string();
    }
    String::new()
}

/// Execute a command with the prompt passed via stdin, capture stdout.
async fn execute_command(command: &str, prompt: &str) -> Result<String> {
    debug!(%command, prompt_len = prompt.len(), "executing command");

    // Parse the command into program + args (simple split on spaces)
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        anyhow::bail!("empty execute command");
    }

    let mut cmd = tokio::process::Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn command: {command}"))?;

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(prompt.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    // Wait for the process and capture output
    let output = child.wait_with_output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "command exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout)
}
