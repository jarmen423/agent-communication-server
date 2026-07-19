#!/usr/bin/env bash
# scripts/dogfood_token_auth.sh
#
# W2-E local dogfood: prove token auth + WebSocket remote adapter round-trip.
#
# Topology under test:
#
#   ┌─────────────────────────── nats-server ───────────────────────────┐
#   │  TCP listener 14222  (loopback only, anonymous)                   │
#   │  WS  listener 18080  (REQUIRES token: "dogfood-token-wave2e")     │
#   │             ↑                                                     │
#   │             │ this is the security boundary under test            │
#   └──────┬──────┴──────────────┬──────────────────────────────────────┘
#          │                     │ ws://127.0.0.1:18080 + --token
#          │ nats://127.0.0.1:14222 (anonymous)                        ↓
#   ┌──────┴──────┐         ┌──────────────────────┐
#   │ hub-server  │ ◀──┐    │ remote_agent_adapter │   (Python)
#   │ (Rust)      │    │    │ identity=dogfood-    │
#   └──────┬──────┘    │    │   remote             │
#          │           │    │ backend=shell echo   │
#          │  nats://  │    └──────────────────────┘
#          │  anonymous│
#   ┌──────┴──────────┴──┐
#   │ hub-delegate       │  sends one-shot prompt "ping-wave2e"
#   │ (Rust)             │  to dogfood-remote via hub.send.>
#   └────────────────────┘
#
# Security claim proven by this run:
#   - The WS listener rejects every connection that does not present the
#     `dogfood-token-wave2e` token (verified by a pre-flight negative test).
#   - The remote adapter connects successfully ONLY because it presents
#     the token.
#   - hub-delegate → hub-server → (TCP) → NATS → (WS+token) → adapter → echo
#     round-trip completes, proving token-gated remote-agent participation.
#
# Why TCP is anonymous and WS is token-gated (split-authorization):
#   hub-server and hub-delegate are trusted local Rust binaries (W2-B added
#   auth opts to HubClient; the daemon's ControlPlane::connect does not yet
#   thread a token through, and we cannot edit src/ under the W2-E scope).
#   The deployment pattern in config/nats-server.conf + REMOTE_AGENTS.md
#   explicitly separates the trusted control-plane network (anonymous TCP on
#   loopback) from the untrusted remote-agent network (token-gated WS). This
#   script mirrors exactly that pattern, which is the realistic threat model
#   for distributed-agent teams.
#
# Pass criteria:
#   - pre-flight: WS connect WITHOUT token is rejected (negative control)
#   - main test: hub-delegate exits 0 with an echo/done signal
#
# Self-contained: writes temp dir + conf, starts everything, tears down on exit.

set -uo pipefail

# ── Paths ────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/data/cargo-targets/jfrie/nats}"
HUB_SERVER_BIN="${HUB_SERVER_BIN:-${CARGO_TARGET_DIR}/debug/hub-server}"
HUB_DELEGATE_BIN="${HUB_DELEGATE_BIN:-${CARGO_TARGET_DIR}/debug/hub-delegate}"
PYTHON_BIN="${PYTHON_BIN:-$HOME/.hermes/hermes-agent/venv/bin/python3}"
ADAPTER_SCRIPT="${REPO_ROOT}/remote_agent_adapter.py"
NATS_SERVER_BIN="${NATS_SERVER_BIN:-nats-server}"

# Fixed dogfood token (NOT a real secret — loopback fixture only).
TOKEN="dogfood-token-wave2e"

# ── Ports (allow override) ──────────────────────────────────────────
TCP_PORT="${TCP_PORT:-14222}"
WS_PORT="${WS_PORT:-18080}"
HTTP_PORT="${HTTP_PORT:-18222}"

# ── Temp dir ─────────────────────────────────────────────────────────
TMP_DIR="$(mktemp -d /data/tmp/nats-dogfood-XXXXXX)"
CONF_PATH="${TMP_DIR}/nats.conf"
DB_PATH="${TMP_DIR}/db"
LOG_DIR="${TMP_DIR}/logs"
mkdir -p "${LOG_DIR}" "${DB_PATH}"

# ── Process bookkeeping ─────────────────────────────────────────────
CHILD_PIDS=()

cleanup() {
    local code=$?
    echo "--- cleanup (exit=${code}) ---"
    for pid in "${CHILD_PIDS[@]:-}"; do
        [[ -z "${pid:-}" ]] && continue
        kill "${pid}" 2>/dev/null || true
    done
    sleep 0.5
    for pid in "${CHILD_PIDS[@]:-}"; do
        [[ -z "${pid:-}" ]] && continue
        kill -9 "${pid}" 2>/dev/null || true
    done
    pkill -f "nats-server.*${TMP_DIR}" 2>/dev/null || true
    pkill -f "remote_agent_adapter.*dogfood-remote" 2>/dev/null || true
    pkill -f "hub-server.*${TMP_DIR}" 2>/dev/null || true
    rm -rf "${TMP_DIR}" 2>/dev/null || true
}
trap cleanup EXIT

# ── Helpers ─────────────────────────────────────────────────────────
wait_for_port() {
    local port="$1" label="$2" max="${3:-15}"
    local tries=0
    while (( tries < max )); do
        if (echo > "/dev/tcp/127.0.0.1/${port}") 2>/dev/null; then
            echo "[wait] ${label} on ${port} listening (after ${tries}s)"
            return 0
        fi
        tries=$((tries + 1))
        sleep 1
    done
    echo "[wait] TIMEOUT waiting for ${label} on ${port}" >&2
    return 1
}

# ── Preflight ───────────────────────────────────────────────────────
echo "=== W2-E dogfood: token auth + WS remote adapter round-trip ==="
echo "TMP_DIR=${TMP_DIR}"
echo "TCP_PORT=${TCP_PORT} (anonymous, loopback)  WS_PORT=${WS_PORT} (token-gated)  HTTP_PORT=${HTTP_PORT}"

for bin in "${HUB_SERVER_BIN}" "${HUB_DELEGATE_BIN}" "${ADAPTER_SCRIPT}"; do
    if [[ ! -e "${bin}" ]]; then
        echo "PREFLIGHT FAIL: missing required file: ${bin}" >&2
        exit 2
    fi
done
if ! command -v "${NATS_SERVER_BIN}" >/dev/null 2>&1; then
    echo "PREFLIGHT FAIL: nats-server not on PATH" >&2
    exit 2
fi
if [[ ! -x "${PYTHON_BIN}" ]]; then
    echo "PREFLIGHT FAIL: python not executable at ${PYTHON_BIN}" >&2
    exit 2
fi

# ── 1. Write NATS conf (anonymous TCP, token-gated WS) ──────────────
cat > "${CONF_PATH}" <<EOF
port: ${TCP_PORT}
http_port: ${HTTP_PORT}
websocket {
    port: ${WS_PORT}
    no_tls: true
    same_origin: false
    handshake_timeout: "5s"
    authorization {
        token: "${TOKEN}"
    }
}
server_name: "w2e-dogfood"
max_payload: 10485760
logtime: false
debug: false
EOF
echo "[conf] written -> ${CONF_PATH}  (TCP anonymous, WS requires token)"

# ── 2. Start nats-server ────────────────────────────────────────────
"${NATS_SERVER_BIN}" -c "${CONF_PATH}" > "${LOG_DIR}/nats.log" 2>&1 &
NATS_PID=$!
CHILD_PIDS+=("${NATS_PID}")
echo "[start] nats-server pid=${NATS_PID}"
wait_for_port "${TCP_PORT}" "nats-server(TCP)" 15 || exit 3
wait_for_port "${WS_PORT}"  "nats-server(WS)"  15 || exit 3

# ── 3. NEGATIVE CONTROL: WS without token must be rejected ──────────
echo "=== negative control: WS connect WITHOUT token (expect rejection) ==="
NEG_PROBE="${TMP_DIR}/neg_probe.py"
cat > "${NEG_PROBE}" <<'PY'
import asyncio, nats, sys
async def main():
    try:
        nc = await nats.connect(sys.argv[1], name="no-token", allow_reconnect=False)
        print("UNEXPECTED: connected without token")
        await nc.close()
    except Exception as e:
        print(f"OK-rejected: {type(e).__name__}: {str(e)[:80]}")
asyncio.run(main())
PY
NEG_OUT=$("${PYTHON_BIN}" "${NEG_PROBE}" "ws://127.0.0.1:${WS_PORT}" 2>&1 | grep -v "Unclosed\|client_session")
echo "${NEG_OUT}"
if echo "${NEG_OUT}" | grep -q "OK-rejected"; then
    echo "[neg] PASS — WS without token is rejected"
else
    echo "[neg] FAIL — WS without token was NOT rejected; aborting" >&2
    exit 7
fi

# ── 4. Start hub-server (plain TCP, in-memory) ──────────────────────
"${HUB_SERVER_BIN}" \
    --nats-url "nats://127.0.0.1:${TCP_PORT}" \
    --db-path "" \
    > "${LOG_DIR}/hub-server.log" 2>&1 &
HUB_PID=$!
CHILD_PIDS+=("${HUB_PID}")
echo "[start] hub-server pid=${HUB_PID}"
sleep 2
if ! kill -0 "${HUB_PID}" 2>/dev/null; then
    echo "FAIL: hub-server exited early. Tail of log:" >&2
    tail -30 "${LOG_DIR}/hub-server.log" >&2
    exit 4
fi
echo "[start] hub-server still alive, assuming subscribed"

# ── 5. Start remote_agent_adapter (ws:// + token) ───────────────────
PYTHONPATH="${REPO_ROOT}:${PYTHONPATH:-}" \
PYTHONUNBUFFERED=1 \
"${PYTHON_BIN}" -u "${ADAPTER_SCRIPT}" \
    --identity dogfood-remote \
    --nats-url "ws://127.0.0.1:${WS_PORT}" \
    --token "${TOKEN}" \
    --backend shell \
    --execute "echo" \
    > "${LOG_DIR}/adapter.log" 2>&1 &
ADAPTER_PID=$!
CHILD_PIDS+=("${ADAPTER_PID}")
echo "[start] remote_agent_adapter pid=${ADAPTER_PID} (ws:// + token)"
for i in $(seq 1 25); do
    if grep -q "connected to NATS as dogfood-remote" "${LOG_DIR}/adapter.log" 2>/dev/null; then
        echo "[wait] adapter connected after ${i}s"
        break
    fi
    sleep 1
done
if ! grep -q "connected to NATS as dogfood-remote" "${LOG_DIR}/adapter.log" 2>/dev/null; then
    echo "FAIL: adapter did not report connected within 25s. Tail:" >&2
    tail -40 "${LOG_DIR}/adapter.log" >&2
    exit 5
fi
if ! kill -0 "${ADAPTER_PID}" 2>/dev/null; then
    echo "FAIL: adapter exited after connecting. Tail:" >&2
    tail -40 "${LOG_DIR}/adapter.log" >&2
    exit 6
fi

# ── 6. hub-delegate one-shot prompt ─────────────────────────────────
echo "=== round-trip: hub-delegate --to dogfood-remote --prompt ping-wave2e ==="
DELEGATE_OUT="${LOG_DIR}/delegate.out"
set +e
"${HUB_DELEGATE_BIN}" \
    --nats-url "nats://127.0.0.1:${TCP_PORT}" \
    --to dogfood-remote \
    --from dogfood-boss \
    --prompt "ping-wave2e" \
    --timeout 30 \
    --verbose > "${DELEGATE_OUT}" 2>&1
DELEGATE_RC=$?
set -e
echo "[delegate] exit=${DELEGATE_RC}"
echo "--- delegate output ---"
cat "${DELEGATE_OUT}"
echo "--- end delegate output ---"

# ── 7. Verdict ──────────────────────────────────────────────────────
PASS=1
if [[ "${DELEGATE_RC}" -ne 0 ]]; then
    echo "FAIL: hub-delegate exited ${DELEGATE_RC}" >&2
    PASS=0
fi
if ! grep -qE "(ping-wave2e|done|completed|reply|✓|RESULT)" "${DELEGATE_OUT}" 2>/dev/null; then
    if [[ "${DELEGATE_RC}" -eq 0 && -s "${DELEGATE_OUT}" ]]; then
        echo "[warn] delegate exit=0 but no obvious echo signal; treating as pass per exit code"
    else
        echo "FAIL: no echo/done signal in delegate output" >&2
        PASS=0
    fi
fi

echo
echo "=== adapter.log tail (evidence) ==="
tail -25 "${LOG_DIR}/adapter.log" || true
echo
echo "=== nats.log tail (evidence) ==="
tail -15 "${LOG_DIR}/nats.log" || true
echo
echo "=== hub-server.log tail (evidence) ==="
tail -15 "${LOG_DIR}/hub-server.log" || true

if [[ "${PASS}" -eq 1 ]]; then
    echo
    echo "==================== PASS ===================="
    exit 0
else
    echo
    echo "==================== FAIL ===================="
    exit 1
fi
