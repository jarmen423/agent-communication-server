#!/usr/bin/env bash
# Start a local dev stack in the foreground; Ctrl+C stops everything.
#   nats-server :4222 (+ WebSocket :8080 via config/nats-server.conf if present)
#   hub-server  (DB: .tools/run/nats_hub.db, visualizer at http://127.0.0.1:9191/)
#   echo workers: echo-1, echo-2 (no LLM needed)
#
# Env: NATS_PORT (4222), WS_ADDR (127.0.0.1:9191), WORKERS ("echo-1 echo-2"), NO_BUILD=1
set -euo pipefail
source "$(dirname "$0")/lib.sh"
require_nats_server
cd "$REPO_ROOT"

NATS_PORT="${NATS_PORT:-4222}"
WS_ADDR="${WS_ADDR:-127.0.0.1:9191}"
WORKERS="${WORKERS:-echo-1 echo-2}"
RUN_DIR="$TOOLS_DIR/run"
mkdir -p "$RUN_DIR"
PY="${PYTHON:-$REPO_ROOT/.venv/bin/python}"
[[ -x "$PY" ]] || { echo "no .venv — run: make setup" >&2; exit 1; }

if [[ -z "${NO_BUILD:-}" ]]; then
  echo "==> cargo build (hub-server + CLIs)"
  cargo build --quiet --bins
fi

pids=()
cleanup() {
  echo; echo "==> stopping"
  for p in "${pids[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

if (exec 3<>"/dev/tcp/127.0.0.1/$NATS_PORT") 2>/dev/null; then
  echo "==> nats-server already listening on :$NATS_PORT — reusing it"
else
  echo "==> nats-server :$NATS_PORT (log: $RUN_DIR/nats.log)"
  nats-server -a 127.0.0.1 -p "$NATS_PORT" -js -sd "$RUN_DIR/js" >"$RUN_DIR/nats.log" 2>&1 &
  pids+=("$!")
  wait_port 127.0.0.1 "$NATS_PORT" || { cat "$RUN_DIR/nats.log"; exit 1; }
fi
export NATS_URL="nats://127.0.0.1:$NATS_PORT"

echo "==> hub-server (db: $RUN_DIR/nats_hub.db, visualizer: http://$WS_ADDR/)"
RUST_LOG="${RUST_LOG:-info}" "$CARGO_TARGET_DIR/debug/hub-server" \
  --nats-url "$NATS_URL" --db-path "$RUN_DIR/nats_hub.db" \
  --ws-addr "$WS_ADDR" --static-dir "$REPO_ROOT/visualizer" \
  >"$RUN_DIR/hub.log" 2>&1 &
pids+=("$!")
sleep 1

for w in $WORKERS; do
  echo "==> echo worker $w (log: $RUN_DIR/$w.log)"
  "$PY" echo_worker.py --identity "$w" --nats-url "$NATS_URL" >"$RUN_DIR/$w.log" 2>&1 &
  pids+=("$!")
done

cat <<MSG

nats-hub dev stack is up.   NATS_URL=$NATS_URL
  try:  $CARGO_TARGET_DIR/debug/hub-delegate --to echo-1 --prompt "hello" --verbose
        $CARGO_TARGET_DIR/debug/hub-agents
        open http://$WS_ADDR/
  logs: $RUN_DIR/
Ctrl+C to stop.
MSG
wait
