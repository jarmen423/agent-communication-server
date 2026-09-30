#!/usr/bin/env bash
# Start the full nats-hub stack: nats-server + hub-server + hermes worker.
# Usage: ./scripts/start_stack.sh [--identity NAME] [--repo PATH] [--model MODEL]
set -euo pipefail

IDENTITY="${IDENTITY:-hermes-worker-1}"
REPO="${REPO:-$(pwd)}"
MODEL="${MODEL:-}"
NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
DB_PATH="${DB_PATH:-$(pwd)/nats_hub.db}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${BIN_DIR:-${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug}"
# Python with nats-py + the hermes CLI deps. Defaults to the repo venv (make setup).
HERMES_PY="${HERMES_PY:-$REPO_ROOT/.venv/bin/python}"
export PATH="$REPO_ROOT/.tools/bin:$PATH"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --identity) IDENTITY="$2"; shift 2 ;;
    --repo)     REPO="$2";     shift 2 ;;
    --model)    MODEL="$2";    shift 2 ;;
    *) echo "unknown flag: $1"; exit 1 ;;
  esac
done

echo "=== nats-hub stack ==="
echo "  identity:  $IDENTITY"
echo "  repo:      $REPO"
echo "  nats:      $NATS_URL"
echo "  db:        $DB_PATH"
echo "  binaries:  $BIN_DIR"

# 1. nats-server (skip if already running)
if pgrep -x nats-server >/dev/null 2>&1; then
  echo "[nats-server] already running (pid $(pgrep -x nats-server))"
else
  echo "[nats-server] starting on :4222"
  nats-server -p 4222 --jetstream &
  sleep 1
fi

# 2. hub-server (skip if already running)
if pgrep -f "hub-server.*--db-path" >/dev/null 2>&1; then
  echo "[hub-server] already running (pid $(pgrep -f 'hub-server.*--db-path'))"
else
  echo "[hub-server] starting with db=$DB_PATH"
  "$BIN_DIR/hub-server" --db-path "$DB_PATH" &
  sleep 1
fi

# 3. hermes worker
WORKER_ARGS="--identity $IDENTITY --repo $REPO --nats-url $NATS_URL"
[[ -n "$MODEL" ]] && WORKER_ARGS+=" --model $MODEL"

echo "[hermes-worker] starting as $IDENTITY"
exec "$HERMES_PY" "$(dirname "$0")/../hermes_worker.py" $WORKER_ARGS
