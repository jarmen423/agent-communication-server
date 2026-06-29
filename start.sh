#!/usr/bin/env bash
# start.sh — launch NATS server + control plane router
set -euo pipefail

cd "$(dirname "$0")"

echo "Starting NATS server…"
nats-server -c config/nats-server.conf &
NATS_PID=$!
sleep 1

echo "Starting control plane router…"
RUST_LOG=info ./target/release/hub-server &
HUB_PID=$!
sleep 1

echo ""
echo "╔════════════════════════════════════════════╗"
echo "║         nats-hub is running                ║"
echo "╚════════════════════════════════════════════╝"
echo ""
echo "  NATS server  : PID $NATS_PID (port 4222, monitor 8222)"
echo "  Control plane: PID $HUB_PID"
echo ""
echo "  Press Ctrl+C to stop both."
echo ""

trap 'kill $NATS_PID $HUB_PID 2>/dev/null; exit 0' INT TERM
wait