//! hub-delegate — one command to delegate a task to an agent worker.
//!
//! Creates a task channel, sends the task to the worker's inbox,
//! and waits for the result. Optionally prints status updates.
//!
//! Usage:
//!   hub-delegate --to worker-1 --prompt "implement phase 3"
//!   hub-delegate --to worker-1 --prompt "fix the bug" --timeout 120
//!   hub-delegate --to worker-1 --prompt "review this" --verbose
//!   hub-delegate --to worker-1 --prompt "do work" --no-wait   # fire and forget
//!
//! Flow:
//!   1. Generate task UUID → task channel = task.<short-uuid>
//!   2. Subscribe to channel.task.<short-uuid> (for status + reply)
//!   3. Send task envelope to hub.send.task.<short-uuid> with meta.to=worker
//!   4. Router routes to channel.inbox.<worker>
//!   5. Worker receives, processes, publishes result to channel.task.<short-uuid>
//!   6. hub-delegate receives result and prints it

use anyhow::{Context, Result};
use clap::Parser;
use nats_hub::{Envelope, HubClient, MessageKind};
use std::time::Duration;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hub-delegate",
    about = "Delegate a task to an agent worker via nats-hub"
)]
struct Args {
    /// Recipient agent identity (the worker to send the task to)
    #[arg(long)]
    to: String,

    /// The prompt/task to send to the worker
    #[arg(long)]
    prompt: String,

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Generate a short task UUID for the task channel
    let task_uuid = uuid::Uuid::new_v4();
    let task_short = &task_uuid.to_string()[..8];
    let task_channel = format!("task.{task_short}");

    info!(
        to = %args.to,
        task_channel = %task_channel,
        prompt = %args.prompt.chars().take(80).collect::<String>(),
        "delegating task"
    );

    let client = HubClient::connect(&args.nats_url, &args.from).await?;

    // Subscribe to the task channel to receive status updates + reply
    let mut task_rx = client.subscribe_channel(&task_channel).await?;
    debug!(%task_channel, "subscribed to task channel");

    // Small delay to ensure subscription is registered before publishing
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Build and send the task envelope
    let payload = serde_json::json!({
        "prompt": args.prompt,
        "task_channel": task_channel,
    });

    let env = Envelope::new(
        args.from.clone(),
        &task_channel,
        MessageKind::Message,
        payload,
    )
    .to(&args.to)
    .reply_to(&task_channel); // worker uses this to know where to publish results

    let task_id = env.meta.id.clone();
    client.send(&env).await?;
    info!(%task_id, "task sent to {}", args.to);

    if args.no_wait {
        println!("{task_id}");
        let _ = client.drain().await;
        return Ok(());
    }

    // Wait for reply on the task channel
    let timeout = if args.timeout > 0 {
        Some(Duration::from_secs(args.timeout))
    } else {
        None
    };

    loop {
        let recv = if let Some(t) = timeout {
            match tokio::time::timeout(t, task_rx.recv()).await {
                Ok(result) => result,
                Err(_) => {
                    eprintln!(
                        "\n[hub-delegate] TIMEOUT after {}s — no reply from {to}",
                        args.timeout,
                        to = args.to
                    );
                    let _ = client.drain().await;
                    std::process::exit(2);
                }
            }
        } else {
            task_rx.recv().await
        };

        match recv {
            Some(reply) => {
                // Check if this is a status update or the final result
                if reply.meta.kind == MessageKind::Status {
                    if args.verbose {
                        let status = reply
                            .payload
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        eprintln!("[status] {} — {status}", reply.meta.from);
                    }
                    continue;
                }

                // Structured events (started/progress/completed/error) are
                // emitted on the task channel too — skip them; only the final
                // `message` (with payload.result / payload.error) is the reply.
                if reply.meta.kind == MessageKind::Event {
                    if args.verbose {
                        let etype = reply
                            .payload
                            .get("event_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("event");
                        eprintln!("[event] {} — {etype}", reply.meta.from);
                    }
                    continue;
                }

                // This is the result
                let result = reply
                    .payload
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| {
                        if let Some(err) = reply.payload.get("error").and_then(|v| v.as_str()) {
                            return err;
                        }
                        "no result field"
                    });

                let status = reply
                    .payload
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("done");

                if status == "error" {
                    eprintln!("\n[hub-delegate] task failed: {result}");
                    let _ = client.drain().await;
                    std::process::exit(1);
                }

                println!("{result}");
                info!(%task_id, "reply received from {}", reply.meta.from);
                break;
            }
            None => {
                eprintln!("\n[hub-delegate] task channel closed unexpectedly");
                let _ = client.drain().await;
                std::process::exit(3);
            }
        }
    }

    let _ = client.drain().await;
    Ok(())
}
