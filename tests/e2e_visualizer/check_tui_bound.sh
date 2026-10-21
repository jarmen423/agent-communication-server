#!/usr/bin/env bash
# hub-tui against a bound-identity hub: with NATS_HUB_IDENTITY set to an
# --api-admin identity the TUI must pass its query-API preflight (bound
# subject hub.api.<identity>.ping) and render; without an identity the same
# hub must reject it (legacy hub.api.<op> is dropped).
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
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
  cargo build --quiet --workspace --bins --features tui
fi

tmp="$(mktemp -d)"; pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true; rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

nats_port="$(free_port)"
nats-server -a 127.0.0.1 -p "$nats_port" -js -sd "$tmp/js" >"$tmp/nats.log" 2>&1 &
pids+=("$!")
wait_tcp 127.0.0.1 "$nats_port"
export NATS_URL="nats://127.0.0.1:$nats_port"

"$CARGO_TARGET_DIR/debug/hub-server" --nats-url "$NATS_URL" --db-path "$tmp/db" \
  --require-bound-identity --api-admin test-admin >"$tmp/hub.log" 2>&1 &
pids+=("$!")
sleep 1; kill -0 "${pids[-1]}" 2>/dev/null || { cat "$tmp/hub.log"; exit 1; }

TUI="$CARGO_TARGET_DIR/debug/hub-tui"
# Positive: bound identity starts the TUI (pty via `script`, quit with q).
printf 'q' > "$tmp/keys"
# `script -e` propagates the TUI exit code; q must quit it cleanly (0).
if ! NATS_HUB_IDENTITY=test-admin timeout 25 script -qec \
  "$TUI --nats-url $NATS_URL" "$tmp/tui.out" < "$tmp/keys" >/dev/null 2>&1; then
  echo "FAIL: hub-tui with NATS_HUB_IDENTITY=test-admin did not run/quit cleanly"
  cat "$tmp/tui.out" || true; exit 1
fi
if grep -q "query API ping failed" "$tmp/tui.out"; then
  echo "FAIL: hub-tui with NATS_HUB_IDENTITY=test-admin failed bound ping"; exit 1
fi
echo "ok: hub-tui bound identity (test-admin) passed preflight"

# Negative: no identity -> legacy ping is dropped by the bound hub.
if timeout 20 "$TUI" --nats-url "$NATS_URL" >"$tmp/tui-legacy.out" 2>&1; then
  echo "FAIL: hub-tui without identity unexpectedly worked on bound hub"; exit 1
fi
grep -q "query API ping failed" "$tmp/tui-legacy.out" \
  || { echo "FAIL: legacy hub-tui failed for the wrong reason"; cat "$tmp/tui-legacy.out"; exit 1; }
echo "ok: hub-tui without identity is rejected on bound hub"
echo "TUI BOUND CHECK PASS"
