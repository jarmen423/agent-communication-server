#!/usr/bin/env bash
# scripts/dogfood_identity.sh
#
# Iteration-2 T1 dogfood: prove identity is bound to credentials end-to-end.
#
# Topology: one nats-server on a loopback TCP port, configured with
# per-agent users via `hub-admin render-config` (the same file it emits is
# the one nats-server runs — the tooling is dogfooded too). hub-server runs
# with --require-bound-identity --api-admin boss.
#
#   users:  hub-server (service), alice, bob, carol (workers), boss (admin)
#
# Claims proven (each numbered check must print PASS):
#   1. alice CANNOT publish on hub.pub.bob.>   (publish ACL denies)
#   2. alice CANNOT subscribe channel.inbox.bob (subscribe ACL denies)
#   3. alice CANNOT publish legacy hub.send.*   (ACL denies — bound only)
#   4. alice sends a DM forged as meta.from="bob" → bob's inbox receives it
#      with meta.from rewritten to "alice" (router overwrite)
#   5. hub.api.bob.thread.pending returns only envelopes addressed to bob
#   6. hub.api.bob.thread.pending {identity:"alice"} → forbidden
#   7. hub.api.bob.history.query returns only envelopes bob can see
#   8. non-admin (carol) calling write op session.update_status → forbidden
#   9. admin (boss) calling the same write op → dispatched (not forbidden)
#  10. admin (boss) can read anyone's thread.pending
#  11. legacy hub.api.<op> subject → rejected under --require-bound-identity
#
# Self-contained: writes temp dir + conf, starts everything, tears down on exit.
# Prereqs: cargo build --bin hub-server --bin hub-admin; .tools/bin/nats-server.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
export PATH="${REPO_ROOT}/.tools/bin:${PATH}"
HUB_SERVER_BIN="${HUB_SERVER_BIN:-${CARGO_TARGET_DIR}/debug/hub-server}"
HUB_ADMIN_BIN="${HUB_ADMIN_BIN:-${CARGO_TARGET_DIR}/debug/hub-admin}"
PYTHON_BIN="${PYTHON_BIN:-${REPO_ROOT}/.venv/bin/python3}"
NATS_SERVER_BIN="${NATS_SERVER_BIN:-nats-server}"

TCP_PORT="${TCP_PORT:-14242}"
HTTP_PORT="${HTTP_PORT:-18242}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nats-dogfood-identity-XXXXXX")"
CONF_PATH="${TMP_DIR}/nats.conf"
AGENTS_PATH="${TMP_DIR}/agents.txt"
DB_PATH="${TMP_DIR}/db"
LOG_DIR="${TMP_DIR}/logs"
mkdir -p "${LOG_DIR}" "${DB_PATH}"

CHILD_PIDS=()
cleanup() {
    local code=$?
    echo "--- cleanup (exit=${code}) ---"
    for pid in "${CHILD_PIDS[@]:-}"; do [[ -n "${pid:-}" ]] && kill "${pid}" 2>/dev/null || true; done
    sleep 0.4
    for pid in "${CHILD_PIDS[@]:-}"; do [[ -n "${pid:-}" ]] && kill -9 "${pid}" 2>/dev/null || true; done
    pkill -f "nats-server.*${TMP_DIR}" 2>/dev/null || true
    rm -rf "${TMP_DIR}" 2>/dev/null || true
}
trap cleanup EXIT

die() { echo "FAIL: $*" >&2; exit 1; }
wait_for_port() {
    local port="$1" label="$2"
    for _ in $(seq 1 30); do
        (echo >"/dev/tcp/127.0.0.1/${port}") 2>/dev/null && { echo "[wait] ${label} on ${port} up"; return 0; }
        sleep 0.3
    done
    die "timeout waiting for ${label} on ${port}"
}

echo "=== T1 dogfood: identity bound to credentials ==="
echo "TMP_DIR=${TMP_DIR}  TCP=${TCP_PORT}"

for f in "${HUB_SERVER_BIN}" "${HUB_ADMIN_BIN}"; do [[ -x "${f}" ]] || die "missing ${f}"; done
command -v "${NATS_SERVER_BIN}" >/dev/null || die "nats-server not on PATH"
[[ -x "${PYTHON_BIN}" ]] || die "python not executable: ${PYTHON_BIN}"

# ── 1. Agents file → hub-admin render-config → nats.conf ─────────────
cat >"${AGENTS_PATH}" <<EOF
# <id> <role> <secret> [extra subscribe channels]
alice  worker       pw:pw-alice
bob    worker       pw:pw-bob
carol  worker       pw:pw-carol
boss   admin        pw:pw-boss
EOF
"${HUB_ADMIN_BIN}" render-config \
    --agents "${AGENTS_PATH}" \
    --port "${TCP_PORT}" \
    --ws-port 0 \
    --hub-password "pw-hubserver" \
    --out "${CONF_PATH}" || die "hub-admin render-config failed"
echo "[conf] rendered ${CONF_PATH} via hub-admin"
"${NATS_SERVER_BIN}" -t -c "${CONF_PATH}" || die "nats-server -t rejected rendered config"

# ── 2. Start nats-server + hub-server (bound mode) ───────────────────
"${NATS_SERVER_BIN}" -c "${CONF_PATH}" >"${LOG_DIR}/nats.log" 2>&1 &
CHILD_PIDS+=("$!")
wait_for_port "${TCP_PORT}" "nats-server"

NATS_USER=hub-server NATS_PASSWORD=pw-hubserver \
RUST_LOG=warn "${HUB_SERVER_BIN}" \
    --nats-url "nats://127.0.0.1:${TCP_PORT}" \
    --db-path "${DB_PATH}" \
    --require-bound-identity \
    --api-admin boss \
    >"${LOG_DIR}/hub-server.log" 2>&1 &
CHILD_PIDS+=("$!")
sleep 2
kill -0 "${CHILD_PIDS[-1]}" 2>/dev/null || { tail -30 "${LOG_DIR}/hub-server.log" >&2; die "hub-server exited early"; }
echo "[start] hub-server pid=${CHILD_PIDS[-1]} (--require-bound-identity --api-admin boss)"

# ── 3. Probe: all eleven claims ──────────────────────────────────────
NATS_PORT="${TCP_PORT}" PYTHONPATH="${REPO_ROOT}" "${PYTHON_BIN}" - <<'PY'
import asyncio, json, sys, uuid
import nats

PORT = int(__import__("os").environ["NATS_PORT"])
URL = f"nats://127.0.0.1:{PORT}"
fails = []

def check(name, cond, detail=""):
    print(f"  [{'PASS' if cond else 'FAIL'}] {name}" + (f" — {detail}" if detail and not cond else ""))
    if not cond:
        fails.append(name)

async def connect(user, pw):
    errs = []
    async def on_err(e):
        errs.append(e)
    nc = await nats.connect(URL, user=user, password=pw, allow_reconnect=False,
                            error_cb=on_err)
    return nc, errs

def env(frm, to, channel, text):
    return json.dumps({"meta": {"id": uuid.uuid4().hex, "from": frm, "to": to,
                                "channel": channel,
                                "timestamp": "2026-01-01T00:00:00Z", "kind": "message"},
                       "payload": {"text": text}}).encode()

async def api(nc, ident, op, params=None):
    body = json.dumps({"op": op, "params": params or {}}).encode()
    try:
        r = await nc.request(f"hub.api.{ident}.{op}", body, timeout=5)
        return json.loads(r.data)
    except Exception as e:
        return {"ok": False, "error": f"request failed: {e}"}

async def main():
    # --- alice: forge + snoop attempts --------------------------------
    alice, aerrs = await connect("alice", "pw-alice")
    await asyncio.sleep(0.2)

    await alice.publish("hub.pub.bob.chat", env("bob", None, "chat", "spoof-subject"))
    await alice.publish("hub.send.chat", env("alice", None, "chat", "legacy-subject"))
    await alice.flush()
    await asyncio.sleep(0.5)
    await alice.subscribe("channel.inbox.bob")  # denied: only bob may read it
    await asyncio.sleep(0.5)
    atext = " | ".join(map(str, aerrs))
    denied = lambda needle: any("permissions violation" in str(e).lower()
                                and needle in str(e) for e in aerrs)
    check("1 alice cannot publish hub.pub.bob.>", denied("hub.pub.bob"), atext)
    check("2 alice cannot subscribe channel.inbox.bob", denied("channel.inbox.bob"), atext)
    check("3 alice cannot publish legacy hub.send.>", denied("hub.send"), atext)

    # --- bob receives the forged DM with meta.from rewritten ----------
    bob, _berrs = await connect("bob", "pw-bob")
    inbox = []
    async def on_msg(m):
        inbox.append(m)
    sub = await bob.subscribe("channel.inbox.bob", cb=on_msg)
    await asyncio.sleep(0.3)
    forged = env("bob", "bob", "chat", "i-am-totally-bob")  # from field lies
    await alice.publish("hub.pub.alice.chat", forged)
    await alice.flush()
    for _ in range(40):
        if inbox: break
        await asyncio.sleep(0.25)
    got = json.loads(inbox[0].data) if inbox else {}
    check("4 meta.from forged 'bob' rewritten to 'alice'",
          got.get("meta", {}).get("from") == "alice"
          and got.get("payload", {}).get("text") == "i-am-totally-bob",
          f"received meta.from={got.get('meta', {}).get('from')!r}")

    # seed a second DM so pending has a row
    await alice.publish("hub.pub.alice.chat", env("alice", "bob", "chat", "pending-seed"))
    await alice.flush()
    await asyncio.sleep(1.0)  # async DB mirror

    # --- bob API calls are scoped to bob ------------------------------
    r = await api(bob, "bob", "thread.pending", {"identity": "bob"})
    pending = (r.get("data") or {}).get("pending") or []
    check("5 thread.pending returns only bob's DMs",
          r.get("ok") is True and len(pending) >= 1
          and all(p.get("to_identity") == "bob" for p in pending),
          f"resp={json.dumps(r)[:200]}")
    r = await api(bob, "bob", "thread.pending", {"identity": "alice"})
    check("6 thread.pending for another identity is forbidden",
          r.get("ok") is False and "forbidden" in str(r.get("error", "")),
          f"resp={json.dumps(r)[:200]}")
    r = await api(bob, "bob", "history.query", {})
    envs = (r.get("data") or {}).get("envelopes") or []
    visible = lambda e: e.get("to_identity") in (None, "", "bob") or e.get("from_identity") == "bob"
    check("7 history.query returns only envelopes bob can see",
          r.get("ok") is True and all(visible(e) for e in envs),
          f"resp={json.dumps(r)[:200]}")

    # --- write ops: non-admin forbidden, admin dispatched -------------
    carol, _cerrs = await connect("carol", "pw-carol")
    r = await api(carol, "carol", "session.update_status",
                  {"session_id": "nonexistent", "status": "x"})
    check("8 non-admin write op is forbidden",
          r.get("ok") is False and "forbidden" in str(r.get("error", "")),
          f"resp={json.dumps(r)[:200]}")

    boss, _ = await connect("boss", "pw-boss")
    r = await api(boss, "boss", "session.update_status",
                  {"session_id": "nonexistent", "status": "x"})
    check("9 admin write op is dispatched (not forbidden)",
          "forbidden" not in str(r.get("error", "")),
          f"resp={json.dumps(r)[:200]}")
    r = await api(boss, "boss", "thread.pending", {"identity": "alice"})
    check("10 admin may read any identity's pending",
          r.get("ok") is True, f"resp={json.dumps(r)[:200]}")

    # --- legacy api subject rejected ----------------------------------
    # alice's ACL only allows hub.api.alice.> — a legacy hub.api.agent.find
    # publish is denied at the NATS layer (and even if it reached the hub,
    # require-bound would reject it). Either rejection satisfies the claim.
    before = len(aerrs)
    try:
        r = await alice.request("hub.api.agent.find",
                                json.dumps({"op": "agent.find", "params": {}}).encode(), timeout=5)
        rj = json.loads(r.data)
    except Exception as e:
        rj = {"ok": False, "error": str(e)}
    await asyncio.sleep(0.3)
    acl_denied = any("hub.api.agent.find" in str(e) and "permissions violation" in str(e).lower()
                     for e in aerrs[before:])
    check("11 legacy hub.api.<op> rejected under require-bound",
          rj.get("ok") is False
          and (acl_denied or "bound" in str(rj.get("error", "")) or "timeout" in str(rj.get("error", ""))),
          f"resp={json.dumps(rj)[:200]}")

    for nc in (alice, bob, carol, boss):
        await nc.close()
    if fails:
        print("FAILED CHECKS:", ", ".join(fails))
        sys.exit(1)
    print("ALL CHECKS PASSED")

asyncio.run(main())
PY
RC=$?

echo
echo "=== hub-server.log tail ==="
tail -10 "${LOG_DIR}/hub-server.log" || true

if [[ "${RC}" -eq 0 ]]; then
    echo; echo "==================== PASS ===================="; exit 0
else
    echo; echo "==================== FAIL ===================="; exit 1
fi
