//! `hub-admin manifest`: drive a running hub's identity manifest through its
//! API ops, for operators without a provisioning system. The caller must be a
//! `--manifest-writer` or an `--api-admin` on that hub.
//!
//! Output is the op's `data` as JSON. A refusal is an error, so the exit code
//! is 1. Records are validated here first, so a typo fails before connecting.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use nats_hub::manifest::{check_identity, AcceptPolicy, ManifestRecord, OwnerKind};
use nats_hub::ApiClient;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

#[derive(Args)]
pub(super) struct ManifestArgs {
    /// NATS URL. Falls back to env NATS_URL, then nats://127.0.0.1:4222.
    #[arg(long, global = true)]
    nats_url: Option<String>,
    /// Identity to call the hub as: a manifest writer or API admin. Falls back
    /// to env NATS_HUB_IDENTITY; one of the two is required.
    #[arg(long = "as", global = true)]
    caller: Option<String>,
    #[command(subcommand)]
    cmd: ManifestCmd,
}

#[derive(Subcommand)]
enum ManifestCmd {
    /// Write one identity's record.
    Put(Box<PutArgs>),
    /// Remove an identity, leaving a tombstone at its rev.
    Remove(RemoveArgs),
    /// Apply a full snapshot from a file: {"rev": N?, "records": [...]}.
    /// Live identities below the snapshot rev that it doesn't name are removed.
    Sync(SyncArgs),
    /// Show what the hub holds for one identity.
    Get(GetArgs),
    /// List live records, sorted by identity.
    List(ListArgs),
}

#[derive(Args)]
struct PutArgs {
    #[arg(long)]
    identity: String,
    /// Who owns the identity.
    #[arg(long, default_value = "user", value_parser = ["user", "tenant"])]
    owner_kind: String,
    /// Handle of the owning user (required for --owner-kind user).
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    user_id: Option<String>,
    #[arg(long)]
    device: Option<String>,
    #[arg(long)]
    device_id: Option<String>,
    #[arg(long, default_value = "device", value_parser = ["device", "connector"])]
    kind: String,
    /// Agent client, e.g. claude-code.
    #[arg(long)]
    agent: Option<String>,
    #[arg(long, default_value = "worker",
          value_parser = ["worker", "orchestrator", "admin", "service"])]
    role: String,
    /// owner | tenant | users:a,b (default: chosen from the owner kind).
    #[arg(long)]
    accept: Option<String>,
    /// Default: the current Unix time in milliseconds.
    #[arg(long)]
    rev: Option<i64>,
}

#[derive(Args)]
struct RemoveArgs {
    #[arg(long)]
    identity: String,
    /// Default: the current Unix time in milliseconds.
    #[arg(long)]
    rev: Option<i64>,
}

#[derive(Args)]
struct SyncArgs {
    /// JSON file. A missing rev, on the snapshot or on a record, defaults to
    /// the current Unix time in milliseconds.
    #[arg(long)]
    file: String,
}

#[derive(Args)]
struct GetArgs {
    #[arg(long)]
    identity: String,
}

#[derive(Args)]
struct ListArgs {
    #[arg(long)]
    limit: Option<u64>,
    /// Start after this identity (a previous page's `next_after`).
    #[arg(long)]
    after: Option<String>,
}

pub(super) fn run(args: ManifestArgs) -> Result<()> {
    let (op, params) = request(args.cmd)?;
    let Some(caller) = args.caller.or_else(|| env_value("NATS_HUB_IDENTITY")) else {
        bail!("--as (or NATS_HUB_IDENTITY) is required: a manifest writer or API admin identity");
    };
    let nats_url = args
        .nats_url
        .or_else(|| env_value("NATS_URL"))
        .unwrap_or_else(|| "nats://127.0.0.1:4222".to_string());
    let data = tokio::runtime::Runtime::new()?.block_on(async {
        let api = ApiClient::connect_with_identity(&nats_url, caller).await?;
        api.request(op, params).await
    })?;
    println!("{}", serde_json::to_string_pretty(&data)?);
    Ok(())
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn now_rev() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The op and params for a subcommand, checked as far as can be done locally.
fn request(cmd: ManifestCmd) -> Result<(&'static str, Value)> {
    Ok(match cmd {
        ManifestCmd::Put(a) => {
            let record = a.record()?;
            record.validate().map_err(bad_record)?;
            ("manifest.put", json!({"record": record}))
        }
        ManifestCmd::Remove(a) => {
            check_identity(&a.identity).map_err(bad_record)?;
            let rev = a.rev.unwrap_or_else(now_rev);
            (
                "manifest.remove",
                json!({"identity": a.identity, "rev": rev}),
            )
        }
        ManifestCmd::Sync(a) => {
            let text = std::fs::read_to_string(&a.file)
                .with_context(|| format!("read manifest file '{}'", a.file))?;
            let doc: Value = serde_json::from_str(&text)
                .with_context(|| format!("parse manifest file '{}'", a.file))?;
            ("manifest.sync", snapshot(&doc, now_rev())?)
        }
        ManifestCmd::Get(a) => ("manifest.get", json!({"identity": a.identity})),
        ManifestCmd::List(a) => ("manifest.list", json!({"limit": a.limit, "after": a.after})),
    })
}

fn bad_record(why: String) -> anyhow::Error {
    anyhow::anyhow!("bad manifest record: {why}")
}

/// Sync params from a manifest file, with missing revs filled in.
fn snapshot(doc: &Value, now: i64) -> Result<Value> {
    let rev = match doc.get("rev") {
        None | Some(Value::Null) => now,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| bad_record("rev: must be an integer".to_string()))?,
    };
    let Some(values) = doc.get("records").and_then(Value::as_array) else {
        return Err(bad_record("records: must be an array".to_string()));
    };
    let mut records = Vec::with_capacity(values.len());
    for value in values {
        let mut fields: Map<String, Value> = value
            .as_object()
            .cloned()
            .ok_or_else(|| bad_record("record: must be an object".to_string()))?;
        if fields.get("rev").map_or(true, Value::is_null) {
            fields.insert("rev".to_string(), json!(rev));
        }
        let record: ManifestRecord = serde_json::from_value(Value::Object(fields))
            .map_err(|e| bad_record(format!("record: {e}")))?;
        record.validate().map_err(bad_record)?;
        records.push(record);
    }
    Ok(json!({"rev": rev, "records": records}))
}

impl PutArgs {
    fn record(self) -> Result<ManifestRecord> {
        Ok(ManifestRecord {
            identity: self.identity,
            owner_kind: wire_name::<OwnerKind>(&self.owner_kind)?,
            user: self.user,
            user_id: self.user_id,
            device: self.device,
            device_id: self.device_id,
            kind: wire_name(&self.kind)?,
            agent: self.agent,
            role: wire_name(&self.role)?,
            accept: self.accept.as_deref().map(parse_accept).transpose()?,
            rev: self.rev.unwrap_or_else(now_rev),
        })
    }
}

/// An enum from its wire name, so the names live in one place.
fn wire_name<T: DeserializeOwned>(name: &str) -> Result<T> {
    serde_json::from_value(json!(name)).with_context(|| format!("unknown value {name:?}"))
}

fn parse_accept(value: &str) -> Result<AcceptPolicy> {
    match value {
        "owner" => Ok(AcceptPolicy::Owner),
        "tenant" => Ok(AcceptPolicy::Tenant),
        _ => match value.strip_prefix("users:") {
            Some(users) => Ok(AcceptPolicy::Users {
                users: users.split(',').map(str::to_string).collect(),
            }),
            None => bail!("--accept must be owner, tenant or users:a,b"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_forms_parse() {
        assert_eq!(parse_accept("owner").unwrap(), AcceptPolicy::Owner);
        assert_eq!(parse_accept("tenant").unwrap(), AcceptPolicy::Tenant);
        assert_eq!(
            parse_accept("users:a,b").unwrap(),
            AcceptPolicy::Users {
                users: vec!["a".to_string(), "b".to_string()]
            }
        );
        assert!(parse_accept("everyone").is_err());
    }

    #[test]
    fn snapshot_fills_missing_revs() {
        let doc = json!({"records": [
            {"identity": "a", "owner_kind": "tenant"},
            {"identity": "b", "owner_kind": "tenant", "rev": 5},
        ]});
        let params = snapshot(&doc, 100).unwrap();
        assert_eq!(params["rev"], 100);
        assert_eq!(params["records"][0]["rev"], 100);
        assert_eq!(params["records"][1]["rev"], 5);

        let params = snapshot(
            &json!({"rev": 7, "records": [
                {"identity": "a", "owner_kind": "tenant"},
            ]}),
            100,
        )
        .unwrap();
        assert_eq!(params["rev"], 7);
        assert_eq!(params["records"][0]["rev"], 7);
    }

    #[test]
    fn snapshot_refuses_a_bad_record() {
        let doc = json!({"records": [{"identity": "a", "owner_kind": "user"}]});
        let err = snapshot(&doc, 1).unwrap_err().to_string();
        assert!(err.contains("user: required"), "{err}");
        assert!(snapshot(&json!({"records": 3}), 1).is_err());
        assert!(snapshot(&json!({"rev": "x", "records": []}), 1).is_err());
    }
}
