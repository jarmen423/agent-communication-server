#!/usr/bin/env bash
# Prove the Apache client (including every CLI and TUI) has no server/DB code,
# then keep the server library's transport-only compatibility build working.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
cd "$REPO_ROOT"
cargo check --locked -p nats-hub-client --all-targets --features tui
client_deps="$(mktemp)"
trap 'rm -f "$client_deps"' EXIT
cargo tree --locked -p nats-hub-client --features tui > "$client_deps"
if grep -Ei 'surrealdb|rocksdb|nats-hub v' "$client_deps"; then
  echo 'Client dependency tree contains server/database code' >&2
  exit 1
fi
# The acs connector is Apache-2.0 client code too.
cargo tree --locked -p acs > "$client_deps"
if grep -Ei 'surrealdb|rocksdb|nats-hub v' "$client_deps"; then
  echo 'acs dependency tree contains server/database code' >&2
  exit 1
fi
cargo check --locked -p nats-hub --lib --no-default-features --features no-storage
