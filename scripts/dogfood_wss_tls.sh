#!/usr/bin/env bash
# scripts/dogfood_wss_tls.sh
#
# W3-C: prove wss:// + token + CA trust for remote adapter, with hub-server
# and hub-delegate joining a token-gated TCP listener via NATS_TOKEN env
# (ControlPlane/ApiClient/HubClient all honor HubConnectOptions::from_env).
#
# Topology:
#   nats-server
#     TCP :token-gated  (hub-server + hub-delegate use NATS_TOKEN)
#     WS  :TLS + token  (adapter uses wss:// + --token + --ca-file)
#
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/data/cargo-targets/jfrie/nats}"
HUB_SERVER_BIN="${HUB_SERVER_BIN:-${CARGO_TARGET_DIR}/debug/hub-server}"
HUB_DELEGATE_BIN="${HUB_DELEGATE_BIN:-${CARGO_TARGET_DIR}/debug/hub-delegate}"
PYTHON_BIN="${PYTHON_BIN:-$HOME/.hermes/hermes-agent/venv/bin/python3}"
ADAPTER_SCRIPT="${REPO_ROOT}/remote_agent_adapter.py"
NATS_SERVER_BIN="${NATS_SERVER_BIN:-nats-server}"
CERT="${REPO_ROOT}/config/tls-example/nats-server.crt"
KEY="${REPO_ROOT}/config/tls-example/nats-server.key"
TOKEN="dogfood-token-w3c-wss"

TCP_PORT="${TCP_PORT:-14232}"
WS_PORT="${WS_PORT:-18090}"
HTTP_PORT="${HTTP_PORT:-18232}"

TMP_DIR="$(mktemp -d /data/tmp/nats-dogfood-wss-XXXXXX)"
CONF_PATH="${TMP_DIR}/nats.conf"
LOG_DIR="${TMP_DIR}/logs"
mkdir -p "${LOG_DIR}"

CHILD_PIDS=()
cleanup() {
    local code=$?
    echo "--- cleanup (exit=${code}) ---"
    for pid in "${CHILD_PIDS[@]:-}"; do
        [[ -z "${pid:-}" ]] && continue
        kill "${pid}" 2>/dev/null || true
    done
    sleep 0.4
    for pid in "${CHILD_PIDS[@]:-}"; do
        [[ -z "${pid:-}" ]] && continue
        kill -9 "${pid}" 2>/dev/null || true
    done
    pkill -f "nats-server.*${TMP_DIR}" 2>/dev/null || true
    pkill -f "remote_agent_adapter.*dogfood-wss" 2>/dev/null || true
    pkill -f "hub-server.*${TMP_DIR}" 2>/dev/null || true
    rm -rf "${TMP_DIR}" 2>/dev/null || true
}
trap cleanup EXIT

die() { echo "FAIL: $*" >&2; exit 1; }

need() {
    [[ -x "$1" ]] || [[ -f "$1" ]] || die "missing $1"
}

need "${HUB_SERVER_BIN}"
need "${HUB_DELEGATE_BIN}"
need "${ADAPTER_SCRIPT}"
need "${CERT}"
need "${KEY}"
command -v "${NATS_SERVER_BIN}" >/dev/null || die "nats-server not on PATH"
[[ -x "${PYTHON_BIN}" ]] || die "python not executable: ${PYTHON_BIN}"

# Absolute cert paths (nats-server resolves relative to CWD)
CERT_ABS="$(cd "$(dirname "${CERT}")" && pwd)/$(basename "${CERT}")"
KEY_ABS="$(cd "$(dirname "${KEY}")" && pwd)/$(basename "${KEY}")"

cat >"${CONF_PATH}" <<EOF
server_name: "w3c-dogfood-wss"
port: ${TCP_PORT}
host: "127.0.0.1"
http_port: ${HTTP_PORT}

websocket {
    port: ${WS_PORT}
    host: "127.0.0.1"
    same_origin: false
    handshake_timeout: "5s"
    tls {
        cert_file: "${CERT_ABS}"
        key_file: "${KEY_ABS}"
    }
}

authorization {
    token: "${TOKEN}"
}

max_payload: 10485760
log_file: "${LOG_DIR}/nats.log"
logtime: true
EOF

echo "=== W3-C dogfood: wss:// + token + CA (full-stack auth) ==="
echo "TMP_DIR=${TMP_DIR}"
echo "TCP_PORT=${TCP_PORT} (token)  WS_PORT=${WS_PORT} (TLS+token)"

"${NATS_SERVER_BIN}" -c "${CONF_PATH}" >"${LOG_DIR}/nats.stdout" 2>&1 &
CHILD_PIDS+=($!)
echo "[start] nats-server pid=${CHILD_PIDS[-1]}"

for i in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":${TCP_PORT} "; then
        echo "[wait] nats TCP :${TCP_PORT} up (${i}s)"
        break
    fi
    sleep 0.2
    [[ $i -eq 30 ]] && die "nats TCP never listened"
done
for i in $(seq 1 30); do
    if ss -ltn 2>/dev/null | grep -q ":${WS_PORT} "; then
        echo "[wait] nats WS  :${WS_PORT} up (${i}s)"
        break
    fi
    sleep 0.2
    [[ $i -eq 30 ]] && die "nats WS never listened"
done

echo "=== negative: wss without token must fail ==="
if "${PYTHON_BIN}" - <<PY 2>"${LOG_DIR}/neg.err"
import asyncio, ssl, nats
async def main():
    ctx = ssl.create_default_context(cafile="${CERT_ABS}")
    # hostname is 127.0.0.1 — cert SAN includes IP:127.0.0.1
    await nats.connect(servers=["wss://127.0.0.1:${WS_PORT}"], tls=ctx, connect_timeout=3)
asyncio.run(main())
print("UNEXPECTED_OK")
PY
then
    die "tokenless wss connect unexpectedly succeeded"
fi
if ! grep -qiE "Authorization|auth|permissions|timeout|Error" "${LOG_DIR}/neg.err"; then
    cat "${LOG_DIR}/neg.err" >&2
    die "expected auth error on tokenless connect"
fi
echo "[neg] PASS — wss without token rejected"

export NATS_TOKEN="${TOKEN}"
# hub-server / hub-delegate on token TCP
"${HUB_SERVER_BIN}" --nats-url "nats://127.0.0.1:${TCP_PORT}" \
    >"${LOG_DIR}/hub-server.log" 2>&1 &
CHILD_PIDS+=($!)
echo "[start] hub-server pid=${CHILD_PIDS[-1]} (NATS_TOKEN set)"
sleep 1
kill -0 "${CHILD_PIDS[-1]}" 2>/dev/null || {
    cat "${LOG_DIR}/hub-server.log" >&2
    die "hub-server exited early"
}

PYTHONUNBUFFERED=1 "${PYTHON_BIN}" -u "${ADAPTER_SCRIPT}" \
    --identity dogfood-wss \
    --nats-url "wss://127.0.0.1:${WS_PORT}" \
    --token "${TOKEN}" \
    --ca-file "${CERT_ABS}" \
    --backend shell \
    --execute echo \
    >"${LOG_DIR}/adapter.log" 2>&1 &
CHILD_PIDS+=($!)
echo "[start] adapter pid=${CHILD_PIDS[-1]} (wss + token + ca)"

for i in $(seq 1 40); do
    if grep -q "connected to NATS as dogfood-wss" "${LOG_DIR}/adapter.log" 2>/dev/null; then
        echo "[wait] adapter connected (${i} checks)"
        break
    fi
    sleep 0.25
    [[ $i -eq 40 ]] && {
        cat "${LOG_DIR}/adapter.log" >&2
        die "adapter never connected"
    }
done

echo "=== round-trip hub-delegate → dogfood-wss ==="
set +e
DELEGATE_OUT="$("${HUB_DELEGATE_BIN}" \
    --nats-url "nats://127.0.0.1:${TCP_PORT}" \
    --to dogfood-wss \
    --from dogfood-boss \
    --prompt "ping-w3c-wss" \
    --timeout 30 \
    --verbose 2>&1)"
DELEGATE_RC=$?
set -e
echo "--- delegate output ---"
echo "${DELEGATE_OUT}"
echo "--- end ---"

[[ ${DELEGATE_RC} -eq 0 ]] || die "hub-delegate exit=${DELEGATE_RC}"
echo "${DELEGATE_OUT}" | grep -q "ping-w3c-wss" || die "echo payload missing"
echo "${DELEGATE_OUT}" | grep -qi "completed" || die "completed event missing"

echo
echo "==================== PASS ===================="
exit 0
