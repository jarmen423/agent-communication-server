#!/usr/bin/env bash
# scripts/dogfood_prod_config.sh
#
# Hardening H1 #10: run the production-shaped configuration end to end.
# The NATS config is generated with `hub-admin render-config` (per-agent
# users, per-identity reply inboxes, allow_responses for responders) and
# hub-server runs with --require-bound-identity. Every process connects as
# its own NATS user.
#
#   users:  hub-server (router/API/orchestrator), bob (orchestrator),
#           carol (worker, plays the attacker), echo-1 (echo worker),
#           slow-1 (claude worker backed by a fake CLI that hangs),
#           sup-1 (service: worker supervisor)
#
# Core checks (must PASS):
#   1. carol cannot subscribe channel.inbox.bob
#   2. carol cannot subscribe _INBOX.bob.>           (bob's API replies)
#   3. carol cannot subscribe _INBOX.>               (everyone's replies)
#   4. carol cannot publish into _INBOX.bob.*        (forge a reply)
#   5. carol cannot publish legacy hub.send.*
#   6. carol's own bound API call works (reply on _INBOX.carol)
#   7. a DM carol forges as meta.from="echo-1" reaches bob as from="carol"
#   8. the supervisor (service, allow_responses) answers bob's hub.worker.list
#   9. carol (worker) cannot call the supervisor (hub.worker.* publish denied)
#  10. echo-1 and slow-1 registered under their own identities
# Integration checks (need H2 + H3; reported XFAIL on this branch alone,
# hard failures with STRICT_INTEGRATION=1):
#  11. a 2-task wave (t2 depends on t1, both on echo-1) completes
#  12. wave.cancel on a running task: slow-1 reports status "cancelled" on
#      the task channel and its backend process is gone
#
# Self-contained: random ports, temp dir, everything torn down on exit.
# Prereqs: cargo build --bin hub-server --bin hub-admin; make setup.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
export PATH="${REPO_ROOT}/.tools/bin:${PATH}"
HUB_SERVER_BIN="${HUB_SERVER_BIN:-${CARGO_TARGET_DIR}/debug/hub-server}"
HUB_ADMIN_BIN="${HUB_ADMIN_BIN:-${CARGO_TARGET_DIR}/debug/hub-admin}"
PYTHON_BIN="${PYTHON_BIN:-${REPO_ROOT}/.venv/bin/python3}"
NATS_SERVER_BIN="${NATS_SERVER_BIN:-nats-server}"
FAKE_BIN="${REPO_ROOT}/tests/python/fixtures/bin"

OPERATOR=0
if [[ "${1:-}" == --operator ]]; then
    OPERATOR=1
    if [[ -z "${NATS_HUB_OPERATOR_TEST_STACK:-}" ]]; then
        exec "$REPO_ROOT/scripts/dev/operator_stack.sh" --nats-only "$0" --operator
    fi
    shift
fi
[[ $# -eq 0 ]] || { echo "Usage: $0 [--operator]" >&2; exit 2; }

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nats-dogfood-prod-XXXXXX")"
CONF_PATH="${TMP_DIR}/nats.conf"
AGENTS_PATH="${TMP_DIR}/agents.txt"
LOG_DIR="${TMP_DIR}/logs"
mkdir -p "${LOG_DIR}" "${TMP_DIR}/db" "${TMP_DIR}/work"

# Every child runs in its own session so cleanup can kill whole process
# groups (the fake CLI spawns a grandchild) by PID — no pkill patterns.
CHILD_PIDS=()
cleanup() {
    local code=$?
    echo "--- cleanup (exit=${code}) ---"
    for pid in "${CHILD_PIDS[@]:-}"; do [[ -n "${pid:-}" ]] && kill -TERM -- "-${pid}" 2>/dev/null; done
    sleep 0.5
    for pid in "${CHILD_PIDS[@]:-}"; do [[ -n "${pid:-}" ]] && kill -KILL -- "-${pid}" 2>/dev/null; done
    if [[ -n "${KEEP_LOGS:-}" ]]; then echo "logs kept in ${TMP_DIR}"; else rm -rf "${TMP_DIR}"; fi
}
trap cleanup EXIT

die() { echo "FAIL: $*" >&2; exit 1; }
free_port() { "${PYTHON_BIN}" -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
wait_for_port() {
    local port="$1" label="$2"
    for _ in $(seq 1 50); do
        (echo >"/dev/tcp/127.0.0.1/${port}") 2>/dev/null && { echo "[wait] ${label} on ${port} up"; return 0; }
        sleep 0.2
    done
    die "timeout waiting for ${label} on ${port}"
}
# spawn <log-name> <cmd...> — start in a new session, remember the PID.
spawn() {
    local name="$1"; shift
    setsid "$@" >"${LOG_DIR}/${name}.log" 2>&1 &
    CHILD_PIDS+=("$!")
}

for f in "${HUB_SERVER_BIN}" "${HUB_ADMIN_BIN}"; do [[ -x "${f}" ]] || die "missing ${f}"; done
command -v "${NATS_SERVER_BIN}" >/dev/null || die "nats-server not on PATH (make setup)"
[[ -x "${PYTHON_BIN}" ]] || die "python not executable: ${PYTHON_BIN} (make setup)"

TCP_PORT="${TCP_PORT:-$(free_port)}"
URL="nats://127.0.0.1:${TCP_PORT}"
(( OPERATOR )) && URL="$NATS_URL"
echo "=== H1 dogfood: production config (hub-admin + --require-bound-identity) ==="
echo "TMP_DIR=${TMP_DIR}  NATS=${URL}"

# ── 1. agents file → hub-admin render-config → nats.conf ─────────────
if (( ! OPERATOR )); then
cat >"${AGENTS_PATH}" <<EOF
# <id> <role> <secret> [ws-only] [extra subscribe channels]
bob     orchestrator  pw:pw-bob
carol   worker        pw:pw-carol
echo-1  worker        pw:pw-echo
slow-1  worker        pw:pw-slow
sup-1   service       pw:pw-sup
EOF
"${HUB_ADMIN_BIN}" render-config --agents "${AGENTS_PATH}" --port "${TCP_PORT}" \
    --ws-port 0 --hub-password pw-hub --out "${CONF_PATH}" || die "render-config failed"
"${NATS_SERVER_BIN}" -t -c "${CONF_PATH}" >/dev/null || die "nats-server -t rejected the rendered config"
echo "[conf] rendered + validated ${CONF_PATH}"

# ── 2. nats-server, hub-server, workers, supervisor ──────────────────
spawn nats "${NATS_SERVER_BIN}" -c "${CONF_PATH}"
wait_for_port "${TCP_PORT}" "nats-server"

fi

HUB_DURABLE=()
(( OPERATOR )) && HUB_DURABLE=(--durable)
spawn hub-server env NATS_USER=hub-server NATS_PASSWORD=pw-hub NATS_CREDENTIALS_FILE="${OPERATOR_CREDS_DIR:+${OPERATOR_CREDS_DIR}/hub-server.creds}" RUST_LOG="${HUB_LOG:-warn}" \
    "${HUB_SERVER_BIN}" --nats-url "${URL}" --db-path "${TMP_DIR}/db" \
    --require-bound-identity --wave-liveness-secs 60 "${HUB_DURABLE[@]}"
HUB_PID="${CHILD_PIDS[-1]}"
if (( OPERATOR )); then
    spawn hub-T_B env -u NATS_PASSWORD NATS_USER=hub-server \
        NATS_CREDENTIALS_FILE="$OPERATOR_DIR/creds/T_B/hub-server.creds" NATS_HUB_IDENTITY=hub-server \
        "$HUB_SERVER_BIN" --nats-url "$URL" --db-path "$TMP_DIR/db-b" \
        --durable --require-bound-identity
fi
sleep 1.5
kill -0 "${HUB_PID}" 2>/dev/null || { cat "${LOG_DIR}/hub-server.log" >&2; die "hub-server exited early"; }

cd "${REPO_ROOT}"
spawn echo-1 env NATS_USER=echo-1 NATS_PASSWORD=pw-echo NATS_CREDENTIALS_FILE="${OPERATOR_CREDS_DIR:+${OPERATOR_CREDS_DIR}/echo-1.creds}" PYTHONUNBUFFERED=1 \
    "${PYTHON_BIN}" echo_worker.py --identity echo-1 --nats-url "${URL}"
spawn slow-1 env NATS_USER=slow-1 NATS_PASSWORD=pw-slow NATS_CREDENTIALS_FILE="${OPERATOR_CREDS_DIR:+${OPERATOR_CREDS_DIR}/slow-1.creds}" PYTHONUNBUFFERED=1 \
    PATH="${FAKE_BIN}:${PATH}" FAKE_CLAUDE_MODE=sleep FAKE_CLAUDE_PIDFILE="${TMP_DIR}/slow.pid" \
    "${PYTHON_BIN}" claude_worker.py --identity slow-1 --nats-url "${URL}" \
    --repo "${TMP_DIR}/work"
spawn sup-1 env NATS_USER=sup-1 NATS_PASSWORD=pw-sup NATS_CREDENTIALS_FILE="${OPERATOR_CREDS_DIR:+${OPERATOR_CREDS_DIR}/sup-1.creds}" PYTHONUNBUFFERED=1 \
    "${PYTHON_BIN}" worker_supervisor.py --nats-url "${URL}" \
    --log-dir "${TMP_DIR}/sup-logs"
echo "[start] hub-server + echo-1 + slow-1 + sup-1 (each as its own NATS user)"

# ── 3. probe ─────────────────────────────────────────────────────────
NATS_URL="${URL}" SLOW_PIDFILE="${TMP_DIR}/slow.pid" \
STRICT_INTEGRATION="${STRICT_INTEGRATION:-}" PYTHONPATH="${REPO_ROOT}" \
"${PYTHON_BIN}" - <<'PY'
import asyncio, json, os, sys, time, uuid
from datetime import datetime, timezone
import nats

URL = os.environ["NATS_URL"]
STRICT = os.environ.get("STRICT_INTEGRATION") == "1"
fails, xfails = [], []

def check(name, cond, detail="", integration=False):
    if cond:
        print(f"  [PASS] {name}")
    elif integration and not STRICT:
        print(f"  [XFAIL] {name} — expected to pass after H2/H3 integration"
              + (f" ({detail})" if detail else ""))
        xfails.append(name)
    else:
        print(f"  [FAIL] {name}" + (f" — {detail}" if detail else ""))
        fails.append(name)

async def connect(user):
    errs = []
    async def on_err(e):
        errs.append(str(e))
    auth = ({"user_credentials": f"{os.environ['OPERATOR_CREDS_DIR']}/{user}.creds"}
            if os.environ.get("OPERATOR_CREDS_DIR") else {"user": user, "password": f"pw-{user}"})
    nc = await nats.connect(URL, **auth, allow_reconnect=False,
                            error_cb=on_err, inbox_prefix=f"_INBOX.{user}")
    return nc, errs

def denied(errs, needle):
    # nats-py lower-cases server errors
    return any("permissions violation" in e.lower() and needle.lower() in e.lower()
               for e in errs)

def env(frm, to, channel, payload):
    return json.dumps({"meta": {"id": str(uuid.uuid4()), "from": frm, "to": to,
                                "channel": channel, "kind": "message", "reply_to": None,
                                "timestamp": datetime.now(timezone.utc).isoformat()},
                       "payload": payload}).encode()

async def api(nc, ident, op, params=None, timeout=5):
    body = json.dumps({"op": op, "params": params or {}}).encode()
    try:
        r = await nc.request(f"hub.api.{ident}.{op}", body, timeout=timeout)
        return json.loads(r.data)
    except Exception as e:
        return {"ok": False, "error": f"request failed: {type(e).__name__}: {e}"}

async def eventually(fn, within=30.0, every=0.5):
    deadline = time.monotonic() + within
    while True:
        v = await fn()
        if v or time.monotonic() > deadline:
            return v
        await asyncio.sleep(every)

def now():
    return datetime.now(timezone.utc).isoformat()

def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False

async def wave_flow(bob):
    """11: 2-task wave on echo-1 runs to completion."""
    wid = f"w-{uuid.uuid4().hex[:8]}"
    r = await api(bob, "bob", "wave.create", {
        "wave": {"wave_id": wid, "goal": "dogfood", "status": "pending",
                 "orchestrator": "bob", "created_at": now(), "metadata": {}},
        "tasks": [{"task_id": "t1", "worker": "echo-1", "goal": "first",
                   "write_scope": ["a"], "dependencies": []},
                  {"task_id": "t2", "worker": "echo-1", "goal": "second",
                   "write_scope": ["b"], "dependencies": ["t1"]}]})
    if not r.get("ok"):
        return False, f"wave.create: {r}"
    r = await api(bob, "bob", "wave.spawn", {"wave_id": wid, "timeout_secs": 120})
    if not r.get("ok"):
        return False, f"wave.spawn: {r}"
    async def done():
        s = await api(bob, "bob", "wave.status", {"wave_id": wid})
        gate = ((s.get("data") or {}).get("summary") or {}).get("merge_gate")
        return gate if gate in ("completed", "failed", "cancelled") else None
    gate = await eventually(done, within=60)
    return gate == "completed", f"merge_gate={gate}"

async def cancel_flow(bob):
    """12: wave.cancel on a running slow-1 task → worker reports cancelled."""
    wid = f"w-{uuid.uuid4().hex[:8]}"
    task_sub = await bob.subscribe(f"channel.wave.{wid}.task.slow")
    await bob.flush()
    r = await api(bob, "bob", "wave.create", {
        "wave": {"wave_id": wid, "goal": "cancel me", "status": "pending",
                 "orchestrator": "bob", "created_at": now(), "metadata": {}},
        "tasks": [{"task_id": "slow", "worker": "slow-1", "goal": "hang forever",
                   "write_scope": ["c"], "dependencies": []}]})
    if not r.get("ok"):
        return False, f"wave.create: {r}"
    r = await api(bob, "bob", "wave.spawn", {"wave_id": wid, "timeout_secs": 120})
    if not r.get("ok"):
        return False, f"wave.spawn: {r}"
    pidfile = os.environ["SLOW_PIDFILE"]
    async def backend_started():
        try:
            return json.loads(open(pidfile).read())
        except Exception:
            return None
    pids = await eventually(backend_started, within=30)
    if not pids:
        return False, "fake claude backend never started"
    r = await api(bob, "bob", "wave.cancel", {"wave_id": wid})
    if not r.get("ok"):
        return False, f"wave.cancel: {r}"
    deadline = time.monotonic() + 15
    status = None
    while time.monotonic() < deadline and status != "cancelled":
        try:
            m = await task_sub.next_msg(timeout=1)
        except Exception:
            continue
        e = json.loads(m.data)
        if e["meta"]["from"] == "slow-1" and e["meta"].get("kind") == "message":
            status = (e.get("payload") or {}).get("status") or status
    async def backend_gone():
        return not any(pid_alive(p) for p in pids.values())
    gone = await eventually(backend_gone, within=10)
    return status == "cancelled" and gone, f"worker status={status!r} backend_gone={gone}"

async def main():
    carol, cerrs = await connect("carol")
    bob, berrs = await connect("bob")
    await asyncio.sleep(0.3)

    # ── inbox / reply isolation ──────────────────────────────────────
    await carol.subscribe("channel.inbox.bob")
    await carol.subscribe("_INBOX.bob.>")
    await carol.subscribe("_INBOX.>")
    await carol.publish("_INBOX.bob.forged", b'{"ok":true,"data":"forged"}')
    await carol.publish("hub.send.chat", env("carol", None, "chat", {"t": "legacy"}))
    await carol.flush()
    await asyncio.sleep(0.6)
    detail = " | ".join(cerrs)
    check("1 carol cannot subscribe channel.inbox.bob", denied(cerrs, "channel.inbox.bob"), detail)
    check("2 carol cannot subscribe _INBOX.bob.>", denied(cerrs, '"_INBOX.bob.>"'), detail)
    check("3 carol cannot subscribe _INBOX.>", denied(cerrs, '"_INBOX.>"'), detail)
    check("4 carol cannot publish into _INBOX.bob.*", denied(cerrs, "_INBOX.bob.forged"), detail)
    check("5 carol cannot publish legacy hub.send.*", denied(cerrs, "hub.send.chat"), detail)
    r = await api(carol, "carol", "ping")
    check("6 carol's own bound API call works", r.get("ok") is True, json.dumps(r)[:200])

    # ── forged from is rewritten ─────────────────────────────────────
    inbox = await bob.subscribe("channel.inbox.bob")
    await bob.flush()
    await carol.publish("hub.pub.carol.chat", env("echo-1", "bob", "chat", {"t": "i-am-echo"}))
    await carol.flush()
    try:
        got = json.loads((await inbox.next_msg(timeout=5)).data)
    except Exception as e:
        got = {"error": str(e)}
    check("7 forged meta.from rewritten to the sender",
          got.get("meta", {}).get("from") == "carol"
          and got.get("payload", {}).get("t") == "i-am-echo", json.dumps(got)[:200])

    # ── responders: supervisor via allow_responses; workers can't call it ──
    async def sup_list():
        try:
            r = await bob.request("hub.worker.list", b"{}", timeout=2)
            return json.loads(r.data)
        except Exception:
            return None
    r = await eventually(sup_list, within=20)
    check("8 supervisor answers bob's hub.worker.list", bool(r) and r.get("ok") is True,
          json.dumps(r)[:200])
    before = len(cerrs)
    try:
        await carol.request("hub.worker.list", b"{}", timeout=1)
    except Exception:
        pass
    await asyncio.sleep(0.3)
    check("9 carol (worker) cannot call the supervisor",
          denied(cerrs[before:], "hub.worker.list"), " | ".join(cerrs[before:]))

    async def registered():
        r = await api(bob, "bob", "agent.find", {"capabilities": []})
        ids = {a.get("identity") for a in (r.get("data") or {}).get("agents") or []}
        return {"echo-1", "slow-1"} <= ids and ids
    ids = await eventually(registered, within=30)
    check("10 echo-1 and slow-1 registered as themselves", bool(ids), f"agents={ids}")

    # ── waves (integration with H2/H3) ───────────────────────────────
    ok, detail = await wave_flow(bob)
    check("11 2-task wave completes", ok, detail, integration=True)
    ok, detail = await cancel_flow(bob)
    check("12 wave.cancel → worker reports cancelled, backend gone", ok, detail,
          integration=True)

    for nc in (carol, bob):
        await nc.close()
    print(f"\n{len(fails)} failed, {len(xfails)} xfail (integration)")
    if fails:
        print("FAILED CHECKS:", ", ".join(fails))
        sys.exit(1)
    print("ALL CORE CHECKS PASSED")

asyncio.run(main())
PY
RC=$?
if (( OPERATOR )); then
    echo "=== Operator-mode tenant isolation (two durable hub-servers) ==="
    "$PYTHON_BIN" -m pytest -q -s "$REPO_ROOT/tests/python/test_operator_isolation.py" || RC=1
fi

echo
for f in hub-server echo-1 slow-1 sup-1; do
    echo "=== ${f}.log (tail) ==="; tail -5 "${LOG_DIR}/${f}.log" 2>/dev/null || true
done
if [[ "${RC}" -eq 0 ]]; then
    echo; echo "==================== PASS ===================="; exit 0
else
    echo; echo "==================== FAIL ===================="; exit 1
fi
