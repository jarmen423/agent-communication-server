#!/usr/bin/env bash
# Run a command against an isolated, throwaway stack:
#   nats-server (random port, JetStream in a temp dir) + hub-server (temp DB).
# Exports NATS_URL / TEST_NATS_URL for the command. Never touches :4222 or
# your real nats_hub.db. Everything is torn down when the command exits.
#
# Usage: scripts/dev/with_stack.sh <command> [args...]
#   e.g. scripts/dev/with_stack.sh cargo test --features tui
#        scripts/dev/with_stack.sh .venv/bin/python -m pytest tests/python
#        KEEP_LOGS=1 scripts/dev/with_stack.sh cargo test --test inbox_routing
set -euo pipefail
source "$(dirname "$0")/lib.sh"
require_nats_server
cd "$REPO_ROOT"
[[ $# -gt 0 ]] || { echo "usage: $0 <command> [args...]" >&2; exit 2; }

tmp="$(mktemp -d)"
pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [[ -n "${KEEP_LOGS:-}" ]]; then echo "logs kept in $tmp"; else rm -rf "$tmp"; fi
}
trap cleanup EXIT INT TERM

if [[ -z "${NO_BUILD:-}" ]]; then
  echo "==> building hub-server"
  cargo build --quiet --bin hub-server
fi

port="$(free_port)"
echo "==> nats-server on :$port"
nats-server -a 127.0.0.1 -p "$port" -js -sd "$tmp/js" >"$tmp/nats.log" 2>&1 &
pids+=("$!")
wait_port 127.0.0.1 "$port" || { echo "nats-server failed to start"; cat "$tmp/nats.log"; exit 1; }

export NATS_URL="nats://127.0.0.1:$port"
export TEST_NATS_URL="$NATS_URL"
export NATS_HUB_TEST_STACK=1

# Bound-identity mode (iteration 2 / T1): set NATS_HUB_REQUIRE_BOUND=1 to run
# hub-server with --require-bound-identity + a test API admin. API clients then
# need an identity — ApiClient/tools pick up NATS_HUB_IDENTITY=test-admin, and
# tests can branch on the same env var for mode-dependent assertions.
HUB_ARGS=(--nats-url "$NATS_URL" --db-path "$tmp/db")
if [[ -n "${NATS_HUB_REQUIRE_BOUND:-}" ]]; then
  HUB_ARGS+=(--require-bound-identity --api-admin test-admin)
  export NATS_HUB_IDENTITY=test-admin
fi

echo "==> hub-server (db: $tmp/db)"
RUST_LOG="${HUB_LOG:-warn}" "$CARGO_TARGET_DIR/debug/hub-server" \
  "${HUB_ARGS[@]}" >"$tmp/hub.log" 2>&1 &
pids+=("$!")
sleep 1
if ! kill -0 "${pids[-1]}" 2>/dev/null; then
  echo "hub-server exited early:"; cat "$tmp/hub.log"; exit 1
fi

echo "==> $*"
set +e
"$@"
status=$?
set -e
if (( status != 0 )); then
  echo "---- hub-server log (tail) ----"; tail -n 40 "$tmp/hub.log"
fi
exit "$status"
