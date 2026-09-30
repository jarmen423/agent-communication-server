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
//!      and payload {prompt, task_channel}
//!   4. Router routes to channel.inbox.<worker>
//!   5. Worker receives, processes, publishes status/events + one result to
//!      channel.task.<short-uuid> (reply contract, refocus.md §6)
//!   6. hub-delegate takes the first `message` whose meta.reply_to or
//!      payload.task_id equals the task id, and prints it
//!
//! Exit codes: 0 done · 1 worker reported an error · 2 timeout · 3 channel closed

use anyhow::Result;
use clap::Parser;
use nats_hub::client::is_task_result;
use nats_hub::{Envelope, HubClient, MessageKind};
use std::time::Duration;
use tracing::{debug, info};
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

    // Subscribe to the task channel BEFORE sending (reply contract rule 1).
    // SUB and PUB travel on the same connection, so the server has the
    // subscription before the task is routed to the worker.
    let mut task_rx = client.subscribe_channel(&task_channel).await?;
    debug!(%task_channel, "subscribed to task channel");

    // The channel travels in payload.task_channel; meta.reply_to is only
    // ever a message id (rule 2), so it stays unset on the task itself.
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
    .to(&args.to);

    let task_id = env.meta.id.clone();
    client.send(&env).await?;
    info!(%task_id, "task sent to {}", args.to);

    if args.no_wait {
        println!("{task_id}");
        let _ = client.drain().await;
        return Ok(());
    }

    let deadline =
        (args.timeout > 0).then(|| tokio::time::Instant::now() + Duration::from_secs(args.timeout));

    let result = loop {
        let recv = match deadline {
            Some(d) => match tokio::time::timeout_at(d, task_rx.recv()).await {
                Ok(r) => r,
                Err(_) => {
                    eprintln!(
                        "\n[hub-delegate] TIMEOUT after {}s — no reply from {to}",
                        args.timeout,
                        to = args.to
                    );
                    let _ = client.drain().await;
                    std::process::exit(2);
                }
            },
            None => task_rx.recv().await,
        };

        let Some(env) = recv else {
            eprintln!("\n[hub-delegate] task channel closed unexpectedly");
            let _ = client.drain().await;
            std::process::exit(3);
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

    if status == "error" {
        let err = text("error")
            .or_else(|| text("result"))
            .unwrap_or("unknown error");
        eprintln!("\n[hub-delegate] task failed: {err}");
        let _ = client.drain().await;
        std::process::exit(1);
    }

    println!("{}", text("result").unwrap_or("no result field"));
    let _ = client.drain().await;
    Ok(())
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
