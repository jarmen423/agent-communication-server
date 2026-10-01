"""nats-hub MCP — ``check_providers``: which workers are alive and usable.

The provider health check from ``TODO.md``: before an orchestrator (or a
human) picks a worker, show who is live, what they run, and optionally prove
each one answers a real task.

1. **Registry** (``agent.find`` with ``alive_within_secs``): identity,
   capabilities and ``last_seen`` of every agent with a recent heartbeat.
2. **Models**: registry ``metadata`` (``model``/``models``/``provider``) when
   the router recorded it, else the worker supervisor's ``hub.worker.list``,
   else ``model:<x>``/``provider:<x>`` capabilities. The source is reported.
3. **Ping** (``ping: true``): a tiny real delegation (``PING_PROMPT``) to each
   worker, all in parallel, each bounded by ``ping_timeout``. Classified as
   ``ok`` / ``slow`` / ``error`` / ``unresponsive``. A ping that times out is
   cancelled (contract §4.2) so an LLM worker doesn't keep running it.
4. **Provider probes** (``providers``): the supervisor's live model catalog
   (``hub.worker.models``) per provider name.

Every network step has a timeout, so the tool never hangs; what it can and
can't verify is returned alongside the data (``VERIFIES``/``DOES_NOT_VERIFY``).
"""

from __future__ import annotations

import asyncio
import json
import re
import time
from datetime import datetime, timezone
from typing import Any

import hub_connection as conn
from hub_buffers import hub

PING_PROMPT = "Reply with exactly: pong"
DEFAULT_ALIVE_SECS = 120
DEFAULT_PING_TIMEOUT = 30.0
DEFAULT_SLOW_AFTER = 10.0
MAX_PING_TARGETS = 20
SUPERVISOR_TIMEOUT = 2.0
WORKER_CAPABILITY = "worker"

VERIFIES = [
    "bus presence: the agent registered and sent a heartbeat within "
    "alive_within_secs (hub registry)",
    "registered capabilities, and the model/provider wherever the worker "
    "advertises one (see model_source)",
    "with ping=true: the worker received a DM, ran its backend end to end and "
    "published a reply-contract result within ping_timeout — for an LLM "
    "worker that means its CLI is installed, authenticated and had quota at "
    "that moment",
    "with providers=[...]: the worker supervisor's live model catalog",
]
DOES_NOT_VERIFY = [
    "remaining API credits, quota or rate limits — providers don't expose them "
    "to the hub; a ping proves only that one tiny request succeeded just now",
    "that a larger or tool-using task will succeed (permissions, repo access, "
    "context limits, sandbox policy)",
    "the model of a worker that doesn't advertise one (it runs its CLI default)",
    "agents without the 'worker' capability (bridges, orchestrators) — listed "
    "but not pinged unless named in `workers`",
    "ping cost: each ping is one real, tiny request to the worker's backend",
]


def _age_secs(last_seen: Any) -> float | None:
    if not isinstance(last_seen, str):
        return None
    # chrono emits nanoseconds + "Z"; fromisoformat (3.10) wants <= 6 digits.
    text = re.sub(r"(\.\d{6})\d+", r"\1", last_seen.replace("Z", "+00:00"))
    try:
        ts = datetime.fromisoformat(text)
    except ValueError:
        return None
    if ts.tzinfo is None:
        ts = ts.replace(tzinfo=timezone.utc)
    return round((datetime.now(timezone.utc) - ts).total_seconds(), 1)


def _model_info(agent: dict, supervised: dict[str, dict]) -> dict:
    """model/provider for one agent, with where the answer came from."""
    meta = agent.get("metadata") or {}
    if isinstance(meta, dict) and (meta.get("model") or meta.get("models")
                                   or meta.get("provider")):
        models = meta.get("models") or ([meta["model"]] if meta.get("model") else [])
        return {"provider": meta.get("provider"), "models": models,
                "model_source": "registry-metadata"}
    sup = supervised.get(agent.get("identity") or "")
    if sup is not None:
        return {"provider": sup.get("provider"),
                "models": [sup["model"]] if sup.get("model") else [],
                "model_source": "supervisor"}
    caps = agent.get("capabilities") or []
    models = [c.split(":", 1)[1] for c in caps if c.startswith("model:")]
    provs = [c.split(":", 1)[1] for c in caps if c.startswith("provider:")]
    if models or provs:
        return {"provider": provs[0] if provs else None, "models": models,
                "model_source": "capabilities"}
    return {"provider": None, "models": [], "model_source": None}


async def _nats_request(subject: str, payload: dict, timeout: float) -> dict:
    """Bounded request-reply; any failure is returned as {ok: False}."""
    try:
        nc = await conn.get_nc()
        reply = await nc.request(subject, json.dumps(payload).encode(),
                                 timeout=timeout)
        resp = json.loads(reply.data)
        return resp if isinstance(resp, dict) else {"ok": False,
                                                    "error": "non-object reply"}
    except Exception as e:  # no responders, timeout, bad JSON
        return {"ok": False, "error": f"{type(e).__name__}: {e}".rstrip(": ")}


async def _supervised_workers() -> tuple[dict[str, dict], str | None]:
    """``hub.worker.list`` → {identity: child}; the error if unreachable."""
    resp = await _nats_request("hub.worker.list", {}, SUPERVISOR_TIMEOUT)
    if not resp.get("ok"):
        return {}, resp.get("error") or "supervisor error"
    return {w.get("identity"): w for w in resp.get("workers") or []
            if isinstance(w, dict)}, None


async def _probe_provider(provider: str) -> dict:
    resp = await _nats_request("hub.worker.models", {"provider": provider},
                               SUPERVISOR_TIMEOUT * 2)
    return {"provider": provider, "ok": bool(resp.get("ok")),
            "models": resp.get("models") or [], "error": resp.get("error")}


async def ping_worker(identity: str, timeout: float, slow_after: float) -> dict:
    """One tiny delegation; never raises, never exceeds ``timeout`` (+publish)."""
    h = hub()
    started = time.monotonic()
    try:
        tracker = await h.delegate_async(identity, PING_PROMPT)
    except Exception as e:
        return {"status": "error", "latency_ms": None,
                "detail": f"could not send ping: {e}"}
    try:
        try:
            await asyncio.wait_for(tracker.done.wait(), timeout=timeout)
        except asyncio.TimeoutError:
            # Contract §4.2: ask the worker to stop the ping; don't wait.
            await h.cancel_task(tracker.task_id, timeout=0.01)
            return {"status": "unresponsive", "latency_ms": None,
                    "last_status": tracker.last_status,
                    "detail": f"no result within {timeout:g}s (cancel sent)"}
        latency = time.monotonic() - started
        payload = (tracker.result or {}).get("payload") or {}
        out: dict[str, Any] = {"latency_ms": round(latency * 1000)}
        if tracker.state == "done":
            out["status"] = "slow" if latency > slow_after else "ok"
            reply = payload.get("result")
            out["reply"] = reply[:200] if isinstance(reply, str) else reply
        else:  # error | cancelled | connection lost
            out["status"] = "error"
            out["detail"] = (payload.get("error") or tracker.last_status
                             or tracker.state)
        return out
    finally:
        await h.forget_task(tracker.task_id)


async def check_providers(args: dict) -> dict:
    alive_secs = int(args.get("alive_within_secs") or DEFAULT_ALIVE_SECS)
    found = await conn.api_request("agent.find", {
        "capabilities": args.get("capability") or [],
        "alive_within_secs": alive_secs,
    })
    if not found.get("ok"):
        return found
    agents = (found.get("data") or {}).get("agents") or []
    providers = args.get("providers") or []
    (supervised, sup_err), probes = await asyncio.gather(
        _supervised_workers(),
        asyncio.gather(*(_probe_provider(p) for p in providers)),
    )

    workers: dict[str, dict] = {}
    for a in agents:
        ident = a.get("identity")
        if not ident:
            continue
        workers[ident] = {
            "identity": ident,
            "alive": True,
            "capabilities": a.get("capabilities") or [],
            "last_seen": a.get("last_seen"),
            "age_secs": _age_secs(a.get("last_seen")),
            **_model_info(a, supervised),
        }

    notes: list[str] = []
    if args.get("ping"):
        me = conn.identity()
        if args.get("workers"):
            targets = list(dict.fromkeys(args["workers"]))
        else:
            targets = [w["identity"] for w in workers.values()
                       if WORKER_CAPABILITY in w["capabilities"]
                       and w["identity"] != me]
        if len(targets) > MAX_PING_TARGETS:
            notes.append(f"pinged the first {MAX_PING_TARGETS} of "
                         f"{len(targets)} workers; pass `workers` to choose")
            targets = targets[:MAX_PING_TARGETS]
        timeout = float(args.get("ping_timeout") or DEFAULT_PING_TIMEOUT)
        slow = float(args.get("slow_after") or DEFAULT_SLOW_AFTER)
        results = await asyncio.gather(
            *(ping_worker(t, timeout, slow) for t in targets))
        for ident, res in zip(targets, results):
            entry = workers.setdefault(ident, {
                "identity": ident, "alive": False, "capabilities": [],
                "last_seen": None, "age_secs": None,
                "provider": None, "models": [], "model_source": None,
            })
            entry["ping"] = res
        for w in workers.values():
            w.setdefault("ping", {"status": "not_pinged"})

    summary: dict[str, int] = {"alive": sum(w["alive"] for w in workers.values())}
    for w in workers.values():
        if "ping" in w:
            st = w["ping"]["status"]
            summary[st] = summary.get(st, 0) + 1

    return {"ok": True, "data": {
        "workers": sorted(workers.values(), key=lambda w: w["identity"]),
        "summary": summary,
        "alive_within_secs": alive_secs,
        "supervisor": {"reachable": sup_err is None, "error": sup_err},
        "provider_probes": list(probes),
        "notes": notes,
        "verifies": VERIFIES,
        "does_not_verify": DOES_NOT_VERIFY,
    }}
