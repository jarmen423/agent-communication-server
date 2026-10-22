use clap::Parser;

#[derive(Parser)]
#[command(name = "hub-server", about = "Run the nats-hub control plane router")]
pub(super) struct Args {
    /// Seconds allowed for graceful shutdown after SIGTERM or SIGINT.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..))]
    pub(super) shutdown_timeout: u32,

    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    pub(super) nats_url: String,

    /// Path to the SurrealDB persistent store. When provided, the server
    /// attaches a Storage backend for the agent registry and message
    /// history. Omit (or pass an empty value) to run in-memory only.
    #[cfg(feature = "storage-surreal")]
    #[arg(long, default_value = "nats_hub.db")]
    pub(super) db_path: String,

    /// Address for the Prometheus-compatible `/metrics` endpoint.
    #[arg(long)]
    pub(super) metrics_addr: Option<String>,

    /// Address for the WebSocket bridge (visualizer). When set (e.g.
    /// `127.0.0.1:9191`), serves a live event stream at /ws and static
    /// files from --static-dir.
    #[arg(long)]
    pub(super) ws_addr: Option<String>,

    /// Directory to serve static files from (visualizer HTML/JS/CSS).
    #[arg(long)]
    pub(super) static_dir: Option<String>,

    /// Shared secret required on the WS upgrade as `?token=` in the URL.
    /// Falls back to env HUB_WS_TOKEN. Required for non-loopback --ws-addr
    /// unless --ws-insecure is given.
    #[arg(long)]
    pub(super) ws_token: Option<String>,

    /// Additional allowed `Origin` for the WS endpoint (repeatable).
    /// Loopback spellings (localhost/127.0.0.1/[::1]) are allowed by
    /// default; entries are normalized — case, trailing `/` and default
    /// ports don't matter.
    #[arg(long = "ws-allow-origin")]
    pub(super) ws_allow_origin: Vec<String>,

    /// Sender identity stamped on envelopes the bridge publishes to the bus.
    /// Falls back to env HUB_WS_IDENTITY, then `human`.
    #[arg(long)]
    pub(super) ws_identity: Option<String>,

    /// Allow a non-loopback --ws-addr without --ws-token. INSECURE: only
    /// for trusted networks.
    #[arg(long)]
    pub(super) ws_insecure: bool,

    /// Drop messages on the legacy self-asserted subjects (`hub.send.>`,
    /// bare `hub.register`/`hub.presence`, `hub.api.<op>`). Every sender
    /// must use the bound subjects that pin identity into the subject
    /// (contract §4.1). Default off during the migration window.
    #[arg(long)]
    pub(super) require_bound_identity: bool,

    /// Durable delivery over JetStream: sends survive a router restart,
    /// DMs wait for offline agents, task channels are kept for resume, and
    /// several hub-servers share one routing consumer. Needs a nats-server
    /// with JetStream enabled. Off = plain core NATS (see docs/DELIVERY.md).
    #[arg(long)]
    pub(super) durable: bool,

    /// JSON file with stream names/limits for --durable (any subset of the
    /// fields in docs/DELIVERY.md). Falls back to env NATS_HUB_DELIVERY_CONFIG.
    #[arg(long, value_name = "PATH", requires = "durable")]
    pub(super) delivery_config: Option<std::path::PathBuf>,

    /// Identity allowed to call query-API write ops (wave.*/session.*
    /// mutations). Repeatable; env `NATS_HUB_API_ADMINS` (comma-separated)
    /// is merged in.
    #[arg(long = "api-admin")]
    pub(super) api_admin: Vec<String>,

    /// Directory the `backup.export` op (and `hub-server backup`) writes
    /// into. Without it the op is disabled. Callers only pick a file name.
    #[arg(long)]
    pub(super) backup_dir: Option<std::path::PathBuf>,

    /// Worker liveness TTL (seconds) for wave orchestration: a worker
    /// silent longer than this loses its running wave tasks. Falls back
    /// to env NATS_HUB_WAVE_LIVENESS_SECS, then 90.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) wave_liveness_secs: Option<u64>,

    /// Lock holder liveness TTL (seconds) for file locks: a holder silent
    /// on hub.presence longer than this loses its locks. Falls back to env
    /// NATS_HUB_LOCK_LIVENESS_SECS, then 90.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) lock_liveness_secs: Option<u64>,

    /// Envelope retention: prune history older than N days, in bounded
    /// batches from a background task. Off by default. Envelopes on
    /// channels a live wave/session still uses are never pruned.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) retention_days: Option<u64>,

    /// Envelope retention: cap total stored envelopes at N (oldest
    /// unprotected rows go first). Off by default.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) retention_max_envelopes: Option<u64>,

    /// How long terminal (closed/failed/cancelled) waves and sessions are
    /// kept, in days — record plus all of their channels' envelopes.
    /// Defaults to max(4 * --retention-days, 30) when --retention-days is
    /// set; terminal state is never pruned otherwise.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) retention_terminal_days: Option<u64>,

    /// Max rows deleted per retention statement. Default 1000.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) retention_batch_size: Option<usize>,

    /// Seconds between retention passes. Default 3600.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    pub(super) retention_interval_secs: Option<u64>,

    /// Operator subcommands. Bare `hub-server` (no subcommand) runs the
    /// router — all flags above apply to that mode.
    #[command(subcommand)]
    pub(super) command: Option<HubCommand>,
}

#[derive(clap::Subcommand)]
pub(super) enum HubCommand {
    /// Write a consistent backup of the RUNNING hub-server's database by
    /// calling the admin-only `backup.export` query-API op. The export
    /// lands on the hub host as <--backup-dir of that hub-server>/<--name>
    /// (the file must not already exist).
    Backup {
        /// NATS server URL
        #[arg(long, default_value = "nats://127.0.0.1:4222")]
        nats_url: String,

        /// Caller identity for the bound api subject (defaults to
        /// NATS_HUB_IDENTITY). Must be in the hub's --api-admin list when
        /// API authorization is enforced.
        #[arg(long)]
        identity: Option<String>,

        /// File name inside the running hub-server's --backup-dir
        /// ([A-Za-z0-9._-], no '/'). Default: hub-<UTC timestamp>.surql
        #[arg(long)]
        name: Option<String>,
    },

    /// Restore a `hub-server backup` export into a FRESH --db-path.
    /// Offline: the hub-server owning that path must be stopped first
    /// (RocksDB is single-writer), and the path must be empty.
    #[cfg(feature = "storage-surreal")]
    Restore {
        /// Fresh SurrealDB path to restore into (must not exist or be empty)
        #[arg(long)]
        db_path: String,

        /// The .surql export file produced by `hub-server backup`
        #[arg(long)]
        from: std::path::PathBuf,
    },
}
