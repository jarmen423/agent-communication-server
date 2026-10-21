#!/usr/bin/env bash
# Visualizer smoke test: throwaway nats + hub-server (--ws-addr + --static-dir
# visualizer/ + WS token) + echo worker, then the Playwright script.
# Usage: tests/e2e_visualizer/run_smoke.sh   (run `npm ci` in this dir first)
set -euo pipefail
cd "$(dirname "$0")"
REPO_ROOT="$(cd ../.. && pwd)"
source "$REPO_ROOT/scripts/dev/lib.sh"
require_nats_server
wait_tcp() { python3 -c 'import socket,sys,time
host,port=sys.argv[1],int(sys.argv[2])
for _ in range(200):
    try:
        socket.create_connection((host,port),0.25).close(); sys.exit(0)
    except OSError: time.sleep(0.1)
sys.exit(1)' "$1" "$2"; }
cd "$REPO_ROOT"

if [[ -z "${NO_BUILD:-}" ]]; then
  echo "==> building workspace binaries"
  cargo build --quiet --workspace --bins --features tui
fi

tmp="$(mktemp -d)"; pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

nats_port="$(free_port)"; ws_port="$(free_port)"
token="smoke-token-$$"
nats-server -a 127.0.0.1 -p "$nats_port" -js -sd "$tmp/js" >"$tmp/nats.log" 2>&1 &
pids+=("$!")
wait_tcp 127.0.0.1 "$nats_port"
export NATS_URL="nats://127.0.0.1:$nats_port"

HUB_WS_TOKEN="$token" "$CARGO_TARGET_DIR/debug/hub-server" \
  --nats-url "$NATS_URL" --db-path "$tmp/db" \
  --ws-addr "127.0.0.1:$ws_port" --static-dir "$REPO_ROOT/visualizer" \
  >"$tmp/hub.log" 2>&1 &
pids+=("$!")
sleep 1; kill -0 "${pids[-1]}" 2>/dev/null || { cat "$tmp/hub.log"; exit 1; }

"$REPO_ROOT/.venv/bin/python" echo_worker.py --identity echo-1 --nats-url "$NATS_URL" \
  >"$tmp/echo.log" 2>&1 &
pids+=("$!")

export VIS_URL="http://127.0.0.1:$ws_port/?token=$token"
export HUB_DELEGATE="$CARGO_TARGET_DIR/debug/hub-delegate"
export ARTIFACTS="$REPO_ROOT/tests/e2e_visualizer/artifacts"
mkdir -p "$ARTIFACTS"
node "$REPO_ROOT/tests/e2e_visualizer/smoke.mjs"
