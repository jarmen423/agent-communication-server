//! hub-history — query message history via the query API.
//!
//! Routes DB operations through hub-server's query API (NATS request-reply)
//! to avoid RocksDB single-writer lock contention.
//!
//! Usage:
//!   hub-history                          # all recent messages (default limit 20)
//!   hub-history --channel agents.tasks    # filter by channel
//!   hub-history --from agent-alpha         # filter by sender
//!   hub-history --tail                    # live follow (subscribe to all channels)

use anyhow::Result;
use clap::Parser;
use nats_hub_client::connect_opts::{default_nats_url, env_identity};
use nats_hub_client::events::origin_label;
use nats_hub_client::{ApiClient, Envelope, HistoryQuery};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-history", about = "Query message history via query API")]
struct Args {
    /// NATS server URL (env: NATS_URL)
    #[arg(long, default_value_t = default_nats_url())]
    nats_url: String,
    #[arg(long)]
    channel: Option<String>,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    kind: Option<String>,
    /// Filter by owning user (provenance `origin.user`)
    #[arg(long)]
    user: Option<String>,
    /// Filter by agent client (provenance `origin.agent`)
    #[arg(long)]
    agent: Option<String>,
    /// Filter by self-reported model (`origin.reported.model`, unverified)
    #[arg(long)]
    model: Option<String>,
    /// Filter by self-reported session (`origin.reported.session`, unverified)
    #[arg(long)]
    session: Option<String>,
    #[arg(long, default_value = "20")]
    limit: usize,
    /// Follow new messages live after the snapshot (same filters apply)
    #[arg(long, conflicts_with = "json")]
    tail: bool,
    /// Print the stored records as JSON (provenance included)
    #[arg(long)]
    json: bool,
    /// Show verbose provenance (session, on-behalf-of) in the ORIGIN column
    #[arg(long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let api = ApiClient::connect(&args.nats_url).await?;

    let mut query = HistoryQuery::new().limit(args.limit);
    if let Some(ref ch) = args.channel {
        query = query.channel(ch.clone());
    }
    if let Some(ref from) = args.from {
        query = query.from(from.clone());
    }
    if let Some(ref kind) = args.kind {
        query = query.kind(kind.clone());
    }
    if let Some(ref user) = args.user {
        query = query.user(user.clone());
    }
    if let Some(ref agent) = args.agent {
        query = query.agent(agent.clone());
    }
    if let Some(ref model) = args.model {
        query = query.model(model.clone());
    }
    if let Some(ref session) = args.session {
        query = query.session(session.clone());
    }
    let provenance_filters: Vec<&str> = [
        ("user", args.user.is_some()),
        ("agent", args.agent.is_some()),
        ("model", args.model.is_some()),
        ("session", args.session.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();

    let resp = api
        .request("history.query", serde_json::to_value(&query)?)
        .await?;
    if let Err(message) = check_provenance_filters_applied(&resp, &provenance_filters) {
        eprintln!("{message}");
        std::process::exit(1);
    }
    if args.json {
        // Print the records exactly as the hub returned them: re-parsing
        // into EnvelopeRecord would run provenance through its lossy
        // decode and could silently print `[]` on a decode failure.
        println!("{}", envelopes_json(&resp));
        return Ok(());
    }
    let results: Vec<nats_hub_client::storage::EnvelopeRecord> = resp
        .get("envelopes")
        .and_then(|e| serde_json::from_value(e.clone()).ok())
        .unwrap_or_default();

    if results.is_empty() {
        println!("(no messages found)");
    } else {
        println!(
            "{:<36} {:<7} {:<20} {:<15} {:<30} {:<8} PAYLOAD",
            "ID", "KIND", "FROM", "CHANNEL", "ORIGIN", "TIME"
        );
        println!("{}", "-".repeat(150));
        for record in &results {
            let time = record.timestamp.format("%H:%M:%S").to_string();
            let payload_preview = preview_payload(&record.payload, 60);
            let label = record
                .origin
                .as_ref()
                .map(|origin| origin_label(origin, args.verbose))
                .unwrap_or_default();
            println!(
                "{:<36} {:<7} {:<20} {:<15} {:<30} {:<8} {}",
                record.id.chars().take(36).collect::<String>(),
                record.kind,
                truncate_str(&record.from_identity, 20),
                truncate_str(&record.channel, 15),
                label,
                time,
                payload_preview
            );
        }
        println!("\n({} message(s))", results.len());
    }

    if args.tail {
        println!("\n--- live tail (Ctrl+C to stop) ---");
        let identity = env_identity().unwrap_or_else(|| "hub-history".to_string());
        let client = nats_hub_client::HubClient::connect(&args.nats_url, &identity).await?;
        let mut rx = client.subscribe_all().await?;
        while let Some(env) = rx.recv().await {
            if !tail_matches(&env, &args) {
                continue;
            }
            let time = env.meta.timestamp.format("%H:%M:%S").to_string();
            let payload_preview = preview_payload(&env.payload, 80);
            let label = env
                .meta
                .origin
                .as_ref()
                .map(|origin| origin_label(origin, args.verbose))
                .unwrap_or_default();
            println!(
                "{:<7} {:<20} {:<15} {:<30} {:<8} {}",
                format!("{:?}", env.meta.kind).to_lowercase(),
                truncate_str(&env.meta.from, 20),
                truncate_str(&env.meta.channel, 15),
                label,
                time,
                payload_preview
            );
        }
    }
    Ok(())
}

/// Fail when a requested provenance filter is absent from the reply's
/// `applied_filters`: an older hub ignored a filter it does not know, and
/// silently listing unfiltered history would be wrong.
fn check_provenance_filters_applied(
    resp: &serde_json::Value,
    requested: &[&str],
) -> std::result::Result<(), &'static str> {
    if requested.is_empty() {
        return Ok(());
    }
    let applied: Vec<&str> = resp
        .get("applied_filters")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if requested.iter().all(|name| applied.contains(name)) {
        Ok(())
    } else {
        Err("this hub does not support provenance filters; upgrade hub-server")
    }
}

/// The stored records from a `history.query` reply, pretty-printed
/// exactly as returned (no lossy re-decode through `EnvelopeRecord`).
fn envelopes_json(resp: &serde_json::Value) -> String {
    let envelopes = resp
        .get("envelopes")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    serde_json::to_string_pretty(&envelopes).unwrap_or_else(|_| "[]".to_string())
}

/// Whether a live envelope passes the same `--channel`/`--from` and
/// provenance filters as the snapshot query, so `--tail` does not print
/// messages the snapshot was filtered against.
fn tail_matches(env: &Envelope, args: &Args) -> bool {
    if let Some(channel) = &args.channel {
        if env.meta.channel != *channel {
            return false;
        }
    }
    if let Some(from) = &args.from {
        if env.meta.from != *from {
            return false;
        }
    }
    let origin = env.meta.origin.as_ref();
    let matches = |want: &Option<String>, got: Option<&String>| {
        want.as_ref().map_or(true, |want| got == Some(want))
    };
    matches(&args.user, origin.and_then(|o| o.user.as_ref()))
        && matches(&args.agent, origin.and_then(|o| o.agent.as_ref()))
        && matches(&args.model, origin.and_then(|o| o.reported.model.as_ref()))
        && matches(
            &args.session,
            origin.and_then(|o| o.reported.session.as_ref()),
        )
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() > max {
        &s[..max.saturating_sub(2)]
    } else {
        s
    }
}

fn preview_payload(payload: &serde_json::Value, max: usize) -> String {
    let s = if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(result) = payload.get("result").and_then(|v| v.as_str()) {
        result.to_string()
    } else if let Some(status) = payload.get("status").and_then(|v| v.as_str()) {
        format!("status: {}", status)
    } else if let Some(error) = payload.get("error").and_then(|v| v.as_str()) {
        format!("error: {}", error)
    } else {
        payload.to_string()
    };
    if s.len() > max {
        format!("{}...", &s[..max.saturating_sub(3)])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_provenance_filter_needs_no_applied_filters() {
        assert!(check_provenance_filters_applied(&json!({}), &[]).is_ok());
    }

    #[test]
    fn applied_provenance_filters_pass() {
        let resp = json!({"applied_filters": ["channel", "user", "model"]});
        assert!(check_provenance_filters_applied(&resp, &["user", "model"]).is_ok());
    }

    #[test]
    fn tail_and_json_conflict() {
        assert!(Args::try_parse_from(["hub-history", "--tail", "--json"]).is_err());
        assert!(Args::try_parse_from(["hub-history", "--tail"]).is_ok());
        assert!(Args::try_parse_from(["hub-history", "--json"]).is_ok());
    }

    #[test]
    fn json_output_is_the_hub_reply_unchanged() {
        // A self-reported value with a control character (dropped by the
        // lossy EnvelopeRecord decode) and an unknown extra key both
        // survive verbatim.
        let resp = json!({"envelopes": [{
            "id": "m1",
            "origin": {"user": "alice",
                       "reported": {"model": "mo\u{7}del", "future_key": "kept"}},
            "future_record_field": 42,
        }]});
        let out = envelopes_json(&resp);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, resp["envelopes"]);
        assert_eq!(envelopes_json(&json!({})), "[]");
    }

    fn tail_envelope() -> Envelope {
        let mut env = Envelope::new(
            "worker-1",
            "chat",
            nats_hub_client::MessageKind::Message,
            json!({"text": "hi"}),
        );
        env.meta.origin = Some(nats_hub_client::Origin {
            user: Some("alice".into()),
            agent: Some("claude-code".into()),
            reported: nats_hub_client::Reported {
                model: Some("some-model".into()),
                session: Some("sess-1".into()),
                ..Default::default()
            },
            ..Default::default()
        });
        env
    }

    #[test]
    fn tail_matches_applies_channel_from_and_provenance_filters() {
        let env = tail_envelope();
        let matching = Args::try_parse_from([
            "hub-history",
            "--tail",
            "--channel",
            "chat",
            "--from",
            "worker-1",
            "--user",
            "alice",
            "--agent",
            "claude-code",
            "--model",
            "some-model",
            "--session",
            "sess-1",
        ])
        .unwrap();
        assert!(tail_matches(&env, &matching));

        for wrong in [
            ["--channel", "other"],
            ["--from", "worker-2"],
            ["--user", "bob"],
            ["--agent", "codex"],
            ["--model", "other-model"],
            ["--session", "sess-2"],
        ] {
            let args = Args::try_parse_from(["hub-history", "--tail", wrong[0], wrong[1]]).unwrap();
            assert!(!tail_matches(&env, &args), "{wrong:?} must not match");
        }

        // No filters: everything passes, with or without provenance.
        let open = Args::try_parse_from(["hub-history", "--tail"]).unwrap();
        assert!(tail_matches(&env, &open));
        let mut bare = tail_envelope();
        bare.meta.origin = None;
        assert!(tail_matches(&bare, &open));

        // A provenance filter never matches an envelope without origin.
        let filtered = Args::try_parse_from(["hub-history", "--tail", "--user", "alice"]).unwrap();
        assert!(!tail_matches(&bare, &filtered));
    }

    #[test]
    fn missing_applied_filters_fail_with_upgrade_message() {
        for resp in [
            json!({}),
            json!({"applied_filters": ["channel"]}),
            json!({"applied_filters": "user"}),
            json!({"applied_filters": ["user"]}),
        ] {
            let err = check_provenance_filters_applied(&resp, &["user", "session"])
                .expect_err("unapplied provenance filter must fail");
            assert_eq!(
                err,
                "this hub does not support provenance filters; upgrade hub-server"
            );
        }
    }
}
