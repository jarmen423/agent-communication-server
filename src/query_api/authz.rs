//! Query-API authorization (contract §4.1, T1).
//!
//! Callers reach the API on one of two subject forms:
//!
//! - **Bound** `hub.api.<identity>.<op>` — `<identity>` is one NATS token
//!   ([`crate::protocol::valid_identity`]) naming the caller. Operator ACLs
//!   pin the token to the connection's credentials (`hub.api.<id>.>` publish
//!   permission per user), so the subject is the identity proof.
//! - **Legacy** `hub.api.<op>` — self-asserted, unauthenticated. Rejected
//!   under `--require-bound-identity`. Otherwise it is fully privileged only
//!   on a permissive hub (neither knob set); once `--api-admin` is set a
//!   legacy caller is a non-admin that owns nothing ([`Scope::Anonymous`]):
//!   no writes, broadcast envelopes only, no waves/sessions/pending.
//!
//! Ambiguity rule: after `hub.api.`, a first token that names a known op
//! namespace (`wave`, `session`, `agent`, `history`, `thread`, `envelope`,
//! `stats`, `ping`) keeps the legacy meaning — `hub.api.wave.get` is op
//! `wave.get`, not a call from agent "wave". Agents must not be named after
//! an op namespace; `hub-admin` refuses to mint such identities.
//!
//! Policy applied at dispatch (handler bodies are unchanged):
//!
//! - **Write ops** (`wave.create`, `wave.create_task`, `wave.update_status`,
//!   `wave.update_task_status`, `session.create`, `session.update_status`,
//!   and any future `wave.*`/`session.*` op not in the read set) are
//!   owner-scoped for bound callers (see `write_authz`); admins listed via
//!   `--api-admin` / `NATS_HUB_API_ADMINS` may do anything.
//! - **Read scoping** for a bound caller `C`:
//!   - `history.query`, `thread.get` — return only envelopes where
//!     `from_identity == C`, `to_identity == C`, or `to_identity` is empty
//!     (a broadcast message on any channel, including `task.*`,
//!     `session.*`, `wave.*` channels). The filter runs over an
//!     over-fetched result (`scoped_reads`), so pages are not short just
//!     because invisible rows filled the `LIMIT`.
//!   - `thread.pending` — `params.identity` must equal `C` (admins may
//!     query anyone's pending inbox).
//!   - `envelope.get` — same visibility rule; an invisible record returns a
//!     "not found" error (existence is not leaked).
//!   - `wave.get`, `wave.list_tasks`, `wave.get_task` — visible iff `C` is
//!     the wave's `orchestrator` or a `worker` on one of its tasks.
//!   - `wave.list` — filtered to waves where `C` is orchestrator or worker.
//!   - `session.get` — visible iff `C` is `orchestrator` or `worker`.
//!   - `session.list` — same filter.
//!   - `agent.*`, `stats.*`, `ping` — unscoped metadata/aggregates.
//! - Admins bypass all read scoping.
//!
//! Enforcement activates when `--require-bound-identity` or `--api-admin`
//! is configured; with neither, the dispatch is permissive exactly as
//! before (all existing callers keep working).

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::Value;

use crate::protocol::valid_identity;
use crate::storage::Storage;
use crate::ApiResponse;

/// Op namespaces that appear right after `hub.api.` in the legacy form.
/// A bound subject's identity token may not collide with these.
pub const OP_NAMESPACES: &[&str] = &[
    "wave", "session", "agent", "history", "thread", "envelope", "stats", "ping",
];

/// Read ops under `wave.*` / `session.*`; any other op in those namespaces
/// is treated as a write.
const WAVE_READ_OPS: &[&str] = &[
    "wave.get",
    "wave.list",
    "wave.list_tasks",
    "wave.get_task",
    "wave.status",
];
const SESSION_READ_OPS: &[&str] = &["session.get", "session.list"];

/// How the caller reached the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// Bound identity from `hub.api.<identity>.<op>`.
    Bound(String),
    /// Legacy `hub.api.<op>` call (self-asserted, privileged for compat).
    Legacy,
}

/// Whose view a request is answered with (derived from [`Caller`] and the
/// hub's [`ApiAuthz`] by [`ApiAuthz::scope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Unscoped: a permissive hub, or an admin caller.
    Full,
    /// A bound non-admin caller: sees and writes what it owns or takes part in.
    Identity(&'a str),
    /// A legacy caller on an enforcing hub: owns nothing, writes nothing,
    /// sees only broadcast envelopes and unscoped metadata.
    Anonymous,
}

impl Scope<'_> {
    /// The scoped identity, if any (`None` for `Full` and `Anonymous`).
    pub fn identity(&self) -> Option<&str> {
        match self {
            Scope::Identity(id) => Some(id),
            _ => None,
        }
    }
}

/// Authorization configuration for the query API.
#[derive(Debug, Clone, Default)]
pub struct ApiAuthz {
    /// Drop legacy `hub.api.<op>` calls (`--require-bound-identity`).
    pub require_bound: bool,
    /// Identities allowed to invoke write ops (`--api-admin`, repeatable;
    /// `NATS_HUB_API_ADMINS` comma-separated env is merged in by hub-server).
    pub admins: BTreeSet<String>,
}

impl ApiAuthz {
    /// Fully permissive configuration: no identity check, no scoping.
    /// Equivalent to the pre-T1 dispatch behavior.
    pub fn permissive() -> Self {
        Self::default()
    }

    /// True when either enforcement knob is on.
    pub fn enforcing(&self) -> bool {
        self.require_bound || !self.admins.is_empty()
    }

    /// Is `identity` an API admin?
    pub fn is_admin(&self, identity: &str) -> bool {
        self.admins.contains(identity)
    }

    /// The view `caller` gets on this hub (see [`Scope`]).
    pub fn scope<'a>(&self, caller: &'a Caller) -> Scope<'a> {
        if !self.enforcing() {
            // Neither --require-bound-identity nor --api-admin: the hub is
            // permissive for everyone (module docs). Bound callers are the
            // default for every client now, so scoping them here would make
            // a default hub hide waves/sessions/DMs from their own operators.
            return Scope::Full;
        }
        match caller {
            Caller::Bound(id) if self.is_admin(id) => Scope::Full,
            Caller::Bound(id) => Scope::Identity(id),
            Caller::Legacy => Scope::Anonymous,
        }
    }

    /// Parse `hub.api.<rest>` into (caller, op). Errors are returned as a
    /// ready-made [`ApiResponse`] the listener can send back verbatim.
    pub fn parse_subject(&self, subject: &str) -> Result<(Caller, String), ApiResponse> {
        let Some(rest) = subject.strip_prefix("hub.api.") else {
            // Not an api subject — treat the whole subject as the op
            // (matches the pre-T1 `req.op` fallback path).
            return Ok((Caller::Legacy, String::new()));
        };

        let first = rest.split('.').next().unwrap_or_default();
        let legacy = || {
            if self.require_bound {
                Err(ApiResponse::err(format!(
                    "legacy hub.api.<op> rejected: this hub requires bound identity \
                     subjects — call hub.api.<identity>.{rest} instead"
                )))
            } else {
                Ok((Caller::Legacy, rest.to_string()))
            }
        };

        // A first token that is an op namespace keeps the legacy meaning.
        if OP_NAMESPACES.contains(&first) {
            return legacy();
        }
        // Single token: not a bound subject — treat as a legacy op so the
        // dispatch reports "unknown operation" for typos as before.
        if !rest.contains('.') {
            return legacy();
        }
        // Bound: `hub.api.<identity>.<op>`.
        if !valid_identity(first) {
            return Err(ApiResponse::err(format!(
                "invalid identity '{first}' in api subject"
            )));
        }
        let op = rest[first.len() + 1..].to_string();
        Ok((Caller::Bound(first.to_string()), op))
    }

    /// Classify `op` for authorization purposes.
    fn op_kind(op: &str) -> OpKind {
        match op {
            "history.query" => OpKind::EnvelopeList("envelopes"),
            "thread.get" => OpKind::EnvelopeList("thread"),
            "thread.pending" => OpKind::ThreadPending,
            "envelope.get" => OpKind::EnvelopeGet,
            "wave.get" | "wave.list_tasks" | "wave.get_task" | "wave.status" => OpKind::WaveRecord,
            "wave.list" => OpKind::WaveList,
            "session.get" => OpKind::SessionRecord,
            "session.list" => OpKind::SessionList,
            _ if Self::is_write_op(op) => OpKind::Write,
            _ => OpKind::Unscoped,
        }
    }

    /// Write ops mutate stored state — they are restricted to admins.
    /// Any `wave.*`/`session.*` op outside the read sets is a write
    /// (forward-compatible with new ops added by later refocus tasks).
    pub fn is_write_op(op: &str) -> bool {
        if let Some(ns) = op.split('.').next() {
            match ns {
                "wave" => !WAVE_READ_OPS.contains(&op),
                "session" => !SESSION_READ_OPS.contains(&op),
                _ => false,
            }
        } else {
            false
        }
    }

    /// Envelope visibility rule (documented in the module comment):
    /// broadcasts are public; a DM is visible to its sender and recipient
    /// (never to [`Scope::Anonymous`]).
    pub(super) fn envelope_visible(scope: Scope<'_>, record: &Value) -> bool {
        let caller = match scope {
            Scope::Full => return true,
            Scope::Identity(id) => Some(id),
            Scope::Anonymous => None,
        };
        let from = record.get("from_identity").and_then(Value::as_str);
        let to = record
            .get("to_identity")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        match (to, caller) {
            (None, _) => true, // broadcast on a channel — public
            (Some(t), Some(c)) => t == c || from == Some(c),
            (Some(_), None) => false,
        }
    }

    /// Wave visibility: `caller` is the orchestrator or one of its task
    /// workers. Non-existent waves are treated as invisible.
    async fn wave_visible(&self, storage: &dyn Storage, caller: &str, wave_id: &str) -> bool {
        match storage.get_wave(wave_id).await {
            Ok(Some(wave)) if wave.orchestrator == caller => true,
            Ok(_) => storage
                .list_wave_tasks(wave_id)
                .await
                .map(|tasks| tasks.iter().any(|t| t.worker == caller))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Session visibility: `caller` is its orchestrator or worker.
    async fn session_visible(&self, storage: &dyn Storage, caller: &str, session_id: &str) -> bool {
        storage
            .get_session(session_id)
            .await
            .map(|s| s.is_some_and(|s| s.orchestrator == caller || s.worker == caller))
            .unwrap_or(false)
    }

    /// Apply authorization to a dispatched request. `resp` is the handler's
    /// raw response; it is filtered/rejected here for scoped callers.
    ///
    /// `params` is the request's params object (needed by ops whose target
    /// is named there, e.g. `wave_id`, `session_id`, `identity`).
    pub async fn authorize(
        &self,
        storage: &Arc<dyn Storage>,
        caller: &Caller,
        op: &str,
        params: &Value,
        resp: ApiResponse,
    ) -> ApiResponse {
        let scope = self.scope(caller);
        if scope == Scope::Full || !resp.ok {
            return resp; // unscoped, or a handler error (left untouched)
        }
        // `None` = anonymous legacy caller: owns no wave/session/inbox.
        let identity = scope.identity();

        match Self::op_kind(op) {
            // Writes were already gated before dispatch (owner-scoped,
            // `write_authz`); a response here means the write was allowed.
            OpKind::Write => resp,
            OpKind::Unscoped => resp,
            OpKind::EnvelopeList(key) => {
                let mut resp = resp;
                if let Some(data) = resp.data.as_mut() {
                    if let Some(arr) = data.get_mut(key).and_then(Value::as_array_mut) {
                        arr.retain(|rec| Self::envelope_visible(scope, rec));
                    }
                }
                resp
            }
            OpKind::ThreadPending => {
                let target = params.get("identity").and_then(Value::as_str);
                if identity.is_none() || target != identity {
                    return ApiResponse::err(
                        "forbidden: thread.pending only returns the caller's own pending \
                         messages (admins may query any identity)",
                    );
                }
                resp
            }
            OpKind::EnvelopeGet => {
                let visible = resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("envelope"))
                    .map(|e| Self::envelope_visible(scope, e))
                    .unwrap_or(false);
                if visible {
                    resp
                } else {
                    ApiResponse::err("envelope not found")
                }
            }
            OpKind::WaveRecord => {
                let wave_id = params.get("wave_id").and_then(Value::as_str);
                let visible = match (wave_id, identity) {
                    (Some(id), Some(me)) => self.wave_visible(storage.as_ref(), me, id).await,
                    (Some(_), None) => false,
                    (None, _) => true, // missing param → handler already errored
                };
                if visible {
                    resp
                } else {
                    ApiResponse::err("wave not found")
                }
            }
            OpKind::SessionRecord => {
                let session_id = params.get("session_id").and_then(Value::as_str);
                let visible = match (session_id, identity) {
                    (Some(id), Some(me)) => self.session_visible(storage.as_ref(), me, id).await,
                    (Some(_), None) => false,
                    (None, _) => true,
                };
                if visible {
                    resp
                } else {
                    ApiResponse::err("session not found")
                }
            }
            OpKind::WaveList => {
                let mut resp = resp;
                let waves: Vec<Value> = resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("waves"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut kept = Vec::with_capacity(waves.len());
                for w in waves {
                    let Some(me) = identity else { break };
                    // Fast path: the serialized record already names the
                    // orchestrator — skip the DB probe for that common case.
                    if w.get("orchestrator").and_then(Value::as_str) == Some(me) {
                        kept.push(w);
                        continue;
                    }
                    let id = w.get("wave_id").and_then(Value::as_str).unwrap_or_default();
                    if self.wave_visible(storage.as_ref(), me, id).await {
                        kept.push(w);
                    }
                }
                if let Some(data) = resp.data.as_mut() {
                    data["waves"] = Value::Array(kept);
                }
                resp
            }
            OpKind::SessionList => {
                let mut resp = resp;
                if let Some(data) = resp.data.as_mut() {
                    if let Some(arr) = data.get_mut("sessions").and_then(Value::as_array_mut) {
                        arr.retain(|s| {
                            identity.is_some()
                                && (s.get("orchestrator").and_then(Value::as_str) == identity
                                    || s.get("worker").and_then(Value::as_str) == identity)
                        });
                    }
                }
                resp
            }
        }
    }
}

enum OpKind {
    /// Mutating op — gated before dispatch (owner-scoped).
    Write,
    /// Unscoped metadata/aggregates.
    Unscoped,
    /// Response data.<key> is an array of envelope records to filter.
    EnvelopeList(&'static str),
    /// thread.pending — params.identity must equal caller.
    ThreadPending,
    /// envelope.get — visibility check on data.envelope.
    EnvelopeGet,
    /// wave.get / wave.list_tasks / wave.get_task — params.wave_id scoped.
    WaveRecord,
    /// wave.list — filter data.waves by visibility.
    WaveList,
    /// session.get — params.session_id scoped.
    SessionRecord,
    /// session.list — filter data.sessions.
    SessionList,
}
