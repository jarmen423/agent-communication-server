#!/usr/bin/env bash
# scripts/dogfood_durable.sh
#
# W2-A: durable delivery on the production-shaped configuration. The NATS
# config comes from `hub-admin render-config --durable` (per-agent users,
# JetStream on), hub-server runs `--durable --require-bound-identity`, and
# every process connects as its own NATS user.
#
#   users:  hub-server, bob (orchestrator), carol (worker, the attacker),
#           echo-1 (echo worker)
#
# Checks (all must PASS):
#   1. router outage: bob delegates while hub-server is DOWN; after the
#      restart the task is routed, run, and `hub-delegate --resume` gets it
#   2. offline DM: bob delegates while echo-1 is DOWN; echo-1 starts later,
#      runs the stored task, and `--resume` gets the result
#   3. carol can attach to and read her own inbox consumer
#   4. carol can't attach as bob, look up / pull from / delete bob's inbox
#      consumer, create any consumer, or read HUB_INBOX by sequence
#   5. carol can't replay a stored envelope as echo-1 by aiming a
#      DIRECT.GET reply at hub.pub.echo-1.* (the router drops it)
#
# Self-contained: random ports, temp dir, everything torn down on exit.
# Prereqs: cargo build --bin hub-server --bin hub-admin --bin hub-delegate;
# make setup. Kills only the PIDs it started.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
export PATH="${REPO_ROOT}/.tools/bin:${PATH}"
BIN="${CARGO_TARGET_DIR}/debug"
PYTHON_BIN="${PYTHON_BIN:-${REPO_ROOT}/.venv/bin/python3}"
NATS_SERVER_BIN="${NATS_SERVER_BIN:-nats-server}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/nats-dogfood-durable-XXXXXX")"
LOG_DIR="${TMP_DIR}/logs"
mkdir -p "${LOG_DIR}" "${TMP_DIR}/db"

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

FAILS=0
die() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; FAILS=$((FAILS + 1)); }
free_port() { "${PYTHON_BIN}" -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
wait_for_port() {
    for _ in $(seq 1 50); do
        (echo >"/dev/tcp/127.0.0.1/$1") 2>/dev/null && return 0
        sleep 0.2
    done
    die "timeout waiting for port $1"
}
# spawn <var> <log-name> <cmd...>: start in a new session; PID into $var.
spawn() {
    local var="$1" name="$2"; shift 2
    setsid "$@" >>"${LOG_DIR}/${name}.log" 2>&1 &
    CHILD_PIDS+=("$!")
    printf -v "${var}" '%s' "$!"
}
stop() { kill -TERM -- "-$1" 2>/dev/null; for _ in $(seq 1 50); do kill -0 "$1" 2>/dev/null || return 0; sleep 0.1; done; kill -KILL -- "-$1" 2>/dev/null; }

for f in hub-server hub-admin hub-delegate; do [[ -x "${BIN}/${f}" ]] || die "missing ${BIN}/${f}"; done
command -v "${NATS_SERVER_BIN}" >/dev/null || die "nats-server not on PATH (make setup)"

PORT="$(free_port)"
URL="nats://127.0.0.1:${PORT}"
echo "=== W2-A dogfood: durable delivery (hub-admin --durable + --require-bound-identity) ==="
echo "TMP_DIR=${TMP_DIR}  NATS=${URL}"

cat >"${TMP_DIR}/agents.txt" <<EOF
bob     orchestrator  pw:pw-bob
carol   worker        pw:pw-carol
echo-1  worker        pw:pw-echo
EOF
"${BIN}/hub-admin" render-config --agents "${TMP_DIR}/agents.txt" --port "${PORT}" --ws-port 0 \
    --hub-password pw-hub --durable --js-store-dir "${TMP_DIR}/js" --out "${TMP_DIR}/nats.conf" \
    || die "render-config failed"
"${NATS_SERVER_BIN}" -t -c "${TMP_DIR}/nats.conf" >/dev/null || die "nats-server -t rejected the config"

spawn NATS_PID nats "${NATS_SERVER_BIN}" -c "${TMP_DIR}/nats.conf"
wait_for_port "${PORT}"

start_hub() {
    spawn HUB_PID hub-server env NATS_USER=hub-server NATS_PASSWORD=pw-hub RUST_LOG="${HUB_LOG:-warn}" \
        "${BIN}/hub-server" --nats-url "${URL}" --db-path "${TMP_DIR}/db" \
        --durable --require-bound-identity
}
start_echo() {
    spawn ECHO_PID echo-1 env NATS_USER=echo-1 NATS_PASSWORD=pw-echo PYTHONUNBUFFERED=1 \
        "${PYTHON_BIN}" "${REPO_ROOT}/echo_worker.py" --identity echo-1 --nats-url "${URL}"
}
# delegate <args...> as bob; stdout = hub-delegate stdout
delegate() {
    NATS_USER=bob NATS_PASSWORD=pw-bob "${BIN}/hub-delegate" --nats-url "${URL}" \
        --from bob --to echo-1 "$@" 2>>"${LOG_DIR}/delegate.log"
}

start_hub
sleep 2
start_echo
# First boot: wait until a round trip works.
first="$(delegate --prompt "warm up" --timeout 30)" || { tail -n 30 "${LOG_DIR}"/*.log; die "warm-up delegation failed"; }
echo "[warm-up] ${first}"

# ── 1. router outage ────────────────────────────────────────────────
stop "${HUB_PID}"
echo "[1] hub-server stopped (pid ${HUB_PID})"
task1="$(delegate --prompt "sent while the router was down" --no-wait)" || fail "1: --no-wait failed"
echo "[1] delegated ${task1} while the router was down"
sleep 1
start_hub
echo "[1] hub-server restarted (pid ${HUB_PID})"
out1="$(delegate --resume "${task1}" --timeout 30)"
echo "[1] resume → ${out1}"
[[ "${out1}" == "echo: nwod saw retuor eht elihw tnes" ]] && pass "1 router outage: send routed after restart" \
    || fail "1 router outage: got '${out1}'"

# ── 2. offline DM ───────────────────────────────────────────────────
stop "${ECHO_PID}"
echo "[2] echo-1 stopped (pid ${ECHO_PID})"
task2="$(delegate --prompt "for an offline worker" --no-wait)" || fail "2: --no-wait failed"
echo "[2] delegated ${task2} while echo-1 was offline"
sleep 2
start_echo
echo "[2] echo-1 restarted (pid ${ECHO_PID})"
out2="$(delegate --resume "${task2}" --timeout 30)"
echo "[2] resume → ${out2}"
[[ "${out2}" == "echo: rekrow enilffo na rof" ]] && pass "2 offline DM delivered on reconnect" \
    || fail "2 offline DM: got '${out2}'"

# ── 3–5. permissions + replay guard ─────────────────────────────────
NATS_URL="${URL}" PYTHONPATH="${REPO_ROOT}" "${PYTHON_BIN}" - <<'PY' || FAILS=$((FAILS + 1))
import asyncio, json, os, sys, uuid
import nats

URL = os.environ["NATS_URL"]
fails = []

def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        fails.append(name)

async def answered(nc, subject, body):
    try:
        await nc.request(subject, json.dumps(body).encode(), timeout=1)
        return True
    except Exception:
        return False

async def main():
    errs = []
    async def on_err(e):
        errs.append(str(e))
    carol = await nats.connect(URL, user="carol", password="pw-carol",
                               inbox_prefix=b"_INBOX.carol", error_cb=on_err)
    bob = await nats.connect(URL, user="bob", password="pw-bob", inbox_prefix=b"_INBOX.bob")

    r = await carol.request("hub.delivery.carol.inbox", b"{}", timeout=3)
    check("3 carol attaches her own inbox", json.loads(r.data).get("ok") is True, r.data)
    check("3 carol reads her own consumer info",
          await answered(carol, "$JS.API.CONSUMER.INFO.HUB_INBOX.inbox-carol", {}))

    for subject, body in [
        ("hub.delivery.bob.inbox", {}),
        ("$JS.API.CONSUMER.INFO.HUB_INBOX.inbox-bob", {}),
        ("$JS.API.CONSUMER.MSG.NEXT.HUB_INBOX.inbox-bob", {"batch": 1, "expires": 500_000_000}),
        ("$JS.API.CONSUMER.DELETE.HUB_INBOX.inbox-bob", {}),
        ("$JS.API.CONSUMER.CREATE.HUB_INBOX.inbox-carol.channel.inbox.bob",
         {"stream_name": "HUB_INBOX", "config": {"name": "inbox-carol", "filter_subject": "channel.inbox.bob"}}),
        ("$JS.API.CONSUMER.CREATE.HUB_INBOX.inbox-carol.channel.inbox.carol",
         {"stream_name": "HUB_INBOX", "config": {"name": "inbox-carol", "filter_subject": "channel.inbox.carol",
                                                 "deliver_subject": "channel.inbox.bob"}}),
        ("$JS.API.CONSUMER.DURABLE.CREATE.HUB_INBOX.x", {"stream_name": "HUB_INBOX", "config": {"durable_name": "x"}}),
        ("$JS.API.STREAM.MSG.GET.HUB_INBOX", {"seq": 1}),
        ("$JS.API.DIRECT.GET.HUB_INBOX", {"seq": 1}),
        ("$JS.API.STREAM.INFO.HUB_INBOX", {}),
    ]:
        check(f"4 carol denied {subject}", not await answered(carol, subject, body))
    check("4 server reported permission violations", any("ermissions" in e for e in errs), errs[:2])

    # 5. replay: carol publishes on a task channel, then aims a DIRECT.GET
    # reply at echo-1's ingress subject. Bob watches the channel.
    ch = f"task.replay{uuid.uuid4().hex[:6]}"
    sub = await bob.subscribe(f"channel.{ch}")
    env = {"meta": {"id": str(uuid.uuid4()), "from": "carol", "channel": ch, "to": None,
                    "timestamp": "2026-01-01T00:00:00Z", "kind": "message", "reply_to": None},
           "payload": {"status": "done", "result": "forged"}}
    await carol.publish(f"hub.pub.carol.{ch}", json.dumps(env).encode())
    first = await sub.next_msg(timeout=5)
    check("5 carol's own send arrives as carol", json.loads(first.data)["meta"]["from"] == "carol")
    req = json.dumps({"seq": 1, "next_by_subj": f"channel.{ch}"}).encode()
    await carol.publish("$JS.API.DIRECT.GET.HUB_TASKS", req, reply=f"hub.pub.echo-1.{ch}")
    await carol.flush()
    try:
        m = await sub.next_msg(timeout=2)
        check("5 replay as echo-1 dropped by the router", False, json.loads(m.data)["meta"])
    except Exception:
        check("5 replay as echo-1 dropped by the router", True)

    await carol.close(); await bob.close()
    sys.exit(1 if fails else 0)

asyncio.run(main())
PY

grep -h "dropping replayed message" "${LOG_DIR}/hub-server.log" | tail -n 1 | sed 's/^/[hub-server log] /'
echo
if (( FAILS )); then echo "=== durable dogfood: ${FAILS} FAILED ==="; exit 1; fi
echo "=== durable dogfood: ALL PASS ==="
